//! `hvw-watch`: the user service of the client. It holds the iroh endpoint
//! of the client, serves the local socket for `hvw` and `hvw-review`, and
//! watches each member for egress requests. For each request it updates
//! the workspace token, raises a toast and reports the agent as blocked.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use hvw::herdr::Herdr;
use hvw::local::{LocalRequest, answer};
use hvw::state::Store;
use hvw_link::{Config, Link, Pending, Request, WatchEvent, frame};
use iroh::endpoint::Connection;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

struct Watch {
    link: Link,
    store: Store,
    herdr: Herdr,
    /// One connection for each member, made on first use.
    conns: Mutex<HashMap<String, Connection>>,
    /// The pending requests, by member and id.
    pending: Mutex<BTreeMap<(String, u64), Pending>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,iroh=warn,noq=warn".into()),
        )
        .without_time()
        .init();
    let cfg = Config::load(&hvw::paths::link_config())?;
    let link = Link::bind(&cfg).await?;
    tracing::info!("endpoint id {}", link.id());
    let store = Store::open()?;
    // Requests of an earlier run are stale: the members send the current
    // ones again.
    store.save_pending(&[])?;
    let w = Arc::new(Watch {
        link,
        store,
        herdr: Herdr::new(),
        conns: Mutex::new(HashMap::new()),
        pending: Mutex::new(BTreeMap::new()),
    });

    let path = hvw::paths::socket()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("cannot listen on {}", path.display()))?;
    tracing::info!("listening on {}", path.display());

    let members: Vec<String> = w.link.peers().names().map(str::to_owned).collect();
    for m in members {
        tokio::spawn(watch_member(w.clone(), m));
    }
    loop {
        let (sock, _) = listener.accept().await?;
        let w = w.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_local(&w, sock).await {
                tracing::debug!("local client: {e:#}");
            }
        });
    }
}

impl Watch {
    async fn connection(&self, member: &str) -> Result<Connection> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(member)
            && c.close_reason().is_none()
        {
            return Ok(c.clone());
        }
        let c = self.link.connect(member).await?;
        conns.insert(member.to_owned(), c.clone());
        Ok(c)
    }

    async fn open(
        &self,
        member: &str,
        req: &Request,
    ) -> Result<(iroh::endpoint::SendStream, iroh::endpoint::RecvStream)> {
        let conn = self.connection(member).await?;
        let (mut send, recv) = conn.open_bi().await.context("cannot open a stream")?;
        frame::write(&mut send, req).await?;
        Ok((send, recv))
    }
}

async fn serve_local(w: &Watch, mut sock: UnixStream) -> Result<()> {
    let Some(req) = frame::read::<_, LocalRequest>(&mut sock).await? else {
        return Ok(());
    };
    match req {
        LocalRequest::Members => {
            let names: Vec<&str> = w.link.peers().names().collect();
            answer(&mut sock, Ok(json!(names))).await
        }
        LocalRequest::Open { member } => {
            let conn = match w.connection(&member).await {
                Ok(c) => c,
                Err(e) => return answer(&mut sock, Err(e)).await,
            };
            let (mut send, mut recv) = match conn.open_bi().await {
                Ok(s) => s,
                Err(e) => return answer(&mut sock, Err(e.into())).await,
            };
            answer(&mut sock, Ok(json!(null))).await?;
            let (mut sr, mut sw) = sock.into_split();
            let up = async {
                tokio::io::copy(&mut sr, &mut send).await?;
                send.finish()?;
                anyhow::Ok(())
            };
            let down = async {
                tokio::io::copy(&mut recv, &mut sw).await?;
                sw.shutdown().await?;
                anyhow::Ok(())
            };
            // The session ends when the member side ends. The client side
            // may stop early: `hvw attach` never closes its stdin.
            tokio::select! {
                r = down => r,
                r = async { up.await?; std::future::pending().await } => r,
            }
        }
    }
}

/// Watches one member for ever, and connects again after each failure.
async fn watch_member(w: Arc<Watch>, member: String) {
    let mut delay = Duration::from_secs(1);
    loop {
        match watch_once(&w, &member).await {
            Ok(()) => tracing::info!("{member} closed the watch"),
            Err(e) => tracing::info!("watch of {member}: {e:#}"),
        }
        // The requests of the member come again with the next watch.
        let gone: Vec<Pending> = {
            let mut p = w.pending.lock().await;
            let keys: Vec<_> = p.keys().filter(|(m, _)| *m == member).cloned().collect();
            keys.into_iter().filter_map(|k| p.remove(&k)).collect()
        };
        if !gone.is_empty() {
            publish(&w, &gone).await;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}

async fn watch_once(w: &Watch, member: &str) -> Result<()> {
    let (mut send, mut recv) = w.open(member, &Request::Watch).await?;
    send.finish()?;
    hvw_link::read_reply(&mut recv, |_| {}).await?;
    tracing::info!("watching {member}");
    while let Some(ev) = frame::read::<_, WatchEvent>(&mut recv).await? {
        let changed = {
            let mut p = w.pending.lock().await;
            match ev {
                WatchEvent::Pending(info) => {
                    if info.member != member {
                        bail!("{member} sent a request of {}", info.member);
                    }
                    let fresh = !p.contains_key(&(member.to_owned(), info.id));
                    p.insert((member.to_owned(), info.id), (*info).clone());
                    if fresh {
                        toast(w, &info).await;
                    }
                    *info
                }
                WatchEvent::Resolved { id, .. } | WatchEvent::Withdrawn { id, .. } => {
                    match p.remove(&(member.to_owned(), id)) {
                        Some(info) => info,
                        None => continue,
                    }
                }
            }
        };
        publish(w, std::slice::from_ref(&changed)).await;
    }
    Ok(())
}

async fn toast(w: &Watch, p: &Pending) {
    let body = format!(
        "{} {} {} — {}",
        p.refusal.method,
        p.refusal.host,
        p.refusal.path,
        p.reason.chars().take(200).collect::<String>()
    );
    if let Err(e) = w
        .herdr
        .notify(&format!("egress request: {}", p.vm), &body)
        .await
    {
        tracing::warn!("cannot raise a toast: {e:#}");
    }
}

/// Writes the pending file and updates herdr for the VMs of `changed`.
async fn publish(w: &Watch, changed: &[Pending]) {
    let all: Vec<Pending> = w.pending.lock().await.values().cloned().collect();
    if let Err(e) = w.store.save_pending(&all) {
        tracing::warn!("cannot write the pending requests: {e:#}");
    }
    let mut vms: Vec<&str> = changed.iter().map(|p| p.vm.as_str()).collect();
    vms.sort();
    vms.dedup();
    for vm in vms {
        let entry = match w.store.load(vm) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("{vm}: {e:#}");
                continue;
            }
        };
        let mine: Vec<&Pending> = all.iter().filter(|p| p.vm == vm).collect();
        let r = async {
            if let Some(ws) = &entry.workspace {
                w.herdr.set_pending(ws, mine.len()).await?;
            }
            if let Some(pane) = &entry.pane {
                match mine.first() {
                    Some(p) => {
                        let msg = format!("waits for egress to {}", p.refusal.host);
                        w.herdr.report_blocked(pane, &msg).await?;
                    }
                    None => w.herdr.release(pane).await?,
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = r.await {
            tracing::warn!("cannot update herdr for {vm}: {e:#}");
        }
    }
}
