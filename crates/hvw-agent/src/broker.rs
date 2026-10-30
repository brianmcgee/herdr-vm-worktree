//! The egress broker. `egress-request` in a VM asks it for a grant. The
//! broker accepts a request only if the gatekeeper of that VM refused a
//! matching request recently. Then it waits for the decision of the user.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use hvw_core::refusal::{self, EgressAsk};
use hvw_link::{Decision, Pending};
use serde::Deserialize;
use serde_json::json;

use crate::server::Agent;
use crate::{gatekeeper, state};

/// How far back the broker looks for a refusal.
pub const REFUSAL_WINDOW_MINUTES: u32 = 15;
/// How long a request waits for the user.
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_REASON: usize = 2000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    host: String,
    method: Option<String>,
    path: Option<String>,
    reason: String,
}

pub async fn serve(agent: Arc<Agent>) -> anyhow::Result<()> {
    let addr = SocketAddr::from((agent.cfg.bridge.address, agent.cfg.broker_port));
    let app = Router::new()
        .route("/v1/egress", post(egress))
        .with_state(agent);
    // The bridge address exists only after Incus created the bridge.
    let listener = loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => break l,
            Err(e) => {
                tracing::warn!("cannot listen on {addr}: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    tracing::info!("broker listens on {addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn reply(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

async fn egress(
    State(agent): State<Arc<Agent>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Body>,
) -> Response {
    let Some(rec) = agent.vm_by_address(peer.ip()).await else {
        return reply(
            StatusCode::FORBIDDEN,
            json!({ "error": "this address is not a worktree VM" }),
        );
    };
    let ask = EgressAsk {
        host: body.host.to_ascii_lowercase(),
        method: body.method.map(|m| m.to_ascii_uppercase()),
        path: body.path,
    };
    if let Err(e) = ask.validate() {
        return reply(StatusCode::BAD_REQUEST, json!({ "error": e }));
    }
    if body.reason.trim().is_empty() || body.reason.len() > MAX_REASON {
        return reply(
            StatusCode::BAD_REQUEST,
            json!({ "error": format!("give a reason of 1 to {MAX_REASON} bytes") }),
        );
    }
    let lines = match gatekeeper::recent_log(&rec.name, REFUSAL_WINDOW_MINUTES).await {
        Ok(l) => l,
        Err(e) => {
            return reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            );
        }
    };
    let Some(refused) = refusal::find(&ask, lines.iter().map(String::as_str)) else {
        return reply(
            StatusCode::NOT_FOUND,
            json!({ "error": format!(
                "the gatekeeper refused no matching request in the last {REFUSAL_WINDOW_MINUTES} minutes: \
                 make the request first, then ask"
            ) }),
        );
    };
    let pending = Pending {
        id: 0,
        vm: rec.name.clone(),
        member: agent.cfg.name.clone(),
        ask,
        reason: body.reason,
        refusal: refused,
        policies: rec.policies(),
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let (id, rx) = agent.add_pending(&rec.client, pending).await;
    // The guard withdraws the request when the agent in the VM gives up
    // (the connection drops and axum drops this future).
    let guard = WithdrawOnDrop {
        agent: agent.clone(),
        id,
        armed: true,
    };
    let decision = tokio::time::timeout(DECISION_TIMEOUT, rx).await;
    let mut guard = guard;
    match decision {
        Ok(Ok(d)) => {
            guard.armed = false;
            let status = if d == Decision::Deny {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::OK
            };
            reply(status, json!({ "id": id, "decision": d }))
        }
        Ok(Err(_)) => reply(
            StatusCode::GONE,
            json!({ "error": "the request was withdrawn" }),
        ),
        Err(_) => reply(
            StatusCode::GATEWAY_TIMEOUT,
            json!({ "error": "no decision in time", "id": id }),
        ),
    }
}

struct WithdrawOnDrop {
    agent: Arc<Agent>,
    id: u64,
    armed: bool,
}

impl Drop for WithdrawOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let agent = self.agent.clone();
            let id = self.id;
            tokio::spawn(async move { agent.withdraw(id).await });
        }
    }
}

/// The grant that a request asks for.
pub fn grant_of(ask: &EgressAsk) -> hvw_core::policy::Grant {
    hvw_core::policy::Grant {
        host: ask.host.clone(),
        methods: ask.method.iter().cloned().collect(),
        paths: ask.path.iter().cloned().collect(),
    }
}

/// Applies an approved request to the VM. The grant lives as long as the
/// VM. With scope policy, also returns the Nix change that adds the grant
/// to the policies of the VM, for the clan repo.
pub async fn apply(
    agent: &Agent,
    p: &Pending,
    decision: Decision,
) -> anyhow::Result<Option<String>> {
    if decision == Decision::Deny {
        return Ok(None);
    }
    let _lock = agent.lock.lock().await;
    let Some(mut rec) = state::load(&agent.cfg, &p.vm)? else {
        anyhow::bail!("VM {} is gone", p.vm);
    };
    let grant = grant_of(&p.ask);
    if !rec.grants.contains(&grant) {
        rec.grants.push(grant.clone());
    }
    let nix = (decision == Decision::Policy).then(|| {
        rec.policies()
            .iter()
            .map(|policy| hvw_core::policy::grants_as_nix(policy, std::slice::from_ref(&grant)))
            .collect::<String>()
    });
    state::save(&agent.cfg, &rec)?;
    gatekeeper::reload(&agent.cfg, &rec).await?;
    Ok(nix)
}
