//! The control plane server: VM lifecycle, source transfer, terminals and
//! egress decisions, over iroh.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use hvw_core::manifest::valid_name;
use hvw_core::policy::Effective;
use hvw_link::frame;
use hvw_link::{
    DecideResult, Decision, FetchInfo, GitUser, LaunchRepo, Link, Peer, Pending, Reply, RepoSource,
    Request, VmInfo, WatchEvent,
};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde_json::json;
use tokio::sync::{Mutex, broadcast, oneshot};

use crate::config::{Config, env_file};
use crate::incus::{Exec, Incus};
use crate::state::{RepoRecord, VmRecord};
use crate::{attach, broker, farm, gatekeeper, git, state};
use hvw_core::vm::valid_vm_name;

struct PendingEntry {
    info: Pending,
    client: String,
    tx: oneshot::Sender<Decision>,
}

pub struct Agent {
    pub cfg: Config,
    pub incus: Incus,
    /// Serialises changes of VM records and lifecycle steps.
    pub lock: Mutex<()>,
    pending: Mutex<BTreeMap<u64, PendingEntry>>,
    next_id: AtomicU64,
    /// Watch events, with the client that they belong to.
    events: broadcast::Sender<(String, WatchEvent)>,
}

impl Agent {
    pub async fn vm_by_address(&self, ip: IpAddr) -> Option<VmRecord> {
        let records = state::all(&self.cfg).ok()?;
        records
            .into_iter()
            .find(|r| r.launched && self.cfg.vm_address(r.slot).ok().map(IpAddr::V4) == Some(ip))
    }

    pub async fn add_pending(
        &self,
        client: &str,
        mut info: Pending,
    ) -> (u64, oneshot::Receiver<Decision>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        info.id = id;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(
            id,
            PendingEntry {
                info: info.clone(),
                client: client.to_owned(),
                tx,
            },
        );
        let _ = self
            .events
            .send((client.to_owned(), WatchEvent::Pending(Box::new(info))));
        (id, rx)
    }

    pub async fn withdraw(&self, id: u64) {
        if let Some(e) = self.pending.lock().await.remove(&id) {
            let _ = self
                .events
                .send((e.client, WatchEvent::Withdrawn { id, vm: e.info.vm }));
        }
    }
}

pub async fn run(cfg: Config) -> Result<()> {
    farm::check_layout(&cfg).await?;
    let link_cfg = hvw_link::Config::load(&cfg.link)?;
    let link = Link::bind(&link_cfg).await?;
    tracing::info!(id = %link.id(), "hvw-agent {} is up", cfg.name);
    let (events, _) = broadcast::channel(256);
    let agent = Arc::new(Agent {
        incus: Incus::new(&cfg.incus_socket),
        cfg,
        lock: Mutex::new(()),
        pending: Mutex::new(BTreeMap::new()),
        // Ids start at the time, so that they do not repeat after a
        // restart of the agent.
        next_id: AtomicU64::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() * 1000)
                .unwrap_or(1),
        ),
        events,
    });

    let b = agent.clone();
    tokio::spawn(async move {
        if let Err(e) = broker::serve(b).await {
            tracing::error!("the broker stopped: {e:#}");
            std::process::exit(1);
        }
    });

    let gc = agent.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = collect_garbage(&gc).await {
                tracing::warn!("farm garbage collection: {e:#}");
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });

    while let Some((peer, conn)) = link.accept().await {
        let agent = agent.clone();
        tokio::spawn(async move {
            tracing::info!(peer = %peer.name, "connected");
            serve_connection(agent, peer, conn).await;
        });
    }
    Ok(())
}

async fn collect_garbage(agent: &Agent) -> Result<()> {
    let in_use: BTreeSet<String> = {
        let _lock = agent.lock.lock().await;
        state::all(&agent.cfg)?
            .into_iter()
            .flat_map(|r| r.farms)
            .collect()
    };
    for key in farm::collect_garbage(&agent.cfg, &in_use).await? {
        tracing::info!("deleted the unused farm {key}");
    }
    Ok(())
}

async fn serve_connection(agent: Arc<Agent>, peer: Peer, conn: Connection) {
    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let agent = agent.clone();
        let peer = peer.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_stream(agent, peer, send, recv).await {
                tracing::warn!("stream failed: {e:#}");
            }
        });
    }
}

/// Sends progress lines to the client.
struct Progress<'a> {
    send: &'a mut SendStream,
}

impl Progress<'_> {
    async fn say(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::info!("{text}");
        let _ = frame::write(self.send, &Reply::Progress(text)).await;
    }
}

async fn serve_stream(
    agent: Arc<Agent>,
    peer: Peer,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    let Some(req) = frame::read::<_, Request>(&mut recv).await? else {
        return Ok(());
    };
    tracing::info!(peer = %peer.name, "request: {}", short(&req));
    let result = match req {
        Request::Push {
            vm,
            repo,
            commit,
            stash,
            branch,
        } => {
            let r = RepoRecord {
                name: repo,
                policy: String::new(),
                commit,
                stash,
                branch,
            };
            push(&agent, &peer, &vm, r, &mut recv)
                .await
                .map(|()| json!(null))
        }
        Request::Launch {
            vm,
            repos,
            git_user,
        } => {
            let r = launch(
                &agent,
                &peer,
                &vm,
                &repos,
                git_user.as_ref(),
                &mut Progress { send: &mut send },
            )
            .await;
            if r.is_err() {
                let _ = destroy(&agent, &peer, &vm).await;
            }
            r.map(|info| json!(info))
        }
        Request::Destroy { vm } => destroy(&agent, &peer, &vm).await.map(|()| json!(null)),
        Request::List => list(&agent, &peer).await.map(|l| json!(l)),
        Request::Attach { vm, cols, rows } => match owned(&agent, &peer, &vm).await {
            Ok(rec) => {
                frame::write(&mut send, &Reply::Ok(json!(null))).await?;
                return attach::run(&agent.cfg, &rec, cols, rows, recv, send).await;
            }
            Err(e) => Err(e),
        },
        Request::Fetch { vm, repo } => {
            let rec = owned(&agent, &peer, &vm).await;
            match rec.and_then(|r| r.repo(&repo).cloned().context("no such repo in the VM")) {
                Ok(_) => {
                    // The bundle goes to a buffer first, so that an error
                    // can still be a frame.
                    let mut buf = Vec::new();
                    match git::fetch(&agent.cfg, &vm, &repo, &mut buf).await {
                        Ok(head) => {
                            frame::write(&mut send, &Reply::Ok(json!(FetchInfo { head }))).await?;
                            send.write_all(&buf).await?;
                            send.finish()?;
                            send.stopped().await.ok();
                            return Ok(());
                        }
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }
        Request::Watch => return watch(&agent, &peer, send).await,
        Request::Decide { id, decision } => {
            decide(&agent, &peer, id, decision).await.map(|r| json!(r))
        }
    };
    let reply = match result {
        Ok(v) => Reply::Ok(v),
        Err(e) => {
            tracing::warn!("request failed: {e:#}");
            Reply::Error(format!("{e:#}"))
        }
    };
    frame::write(&mut send, &reply).await?;
    send.finish()?;
    send.stopped().await.ok();
    Ok(())
}

fn short(req: &Request) -> String {
    match req {
        Request::Push { vm, repo, .. } => format!("push {vm}/{repo}"),
        Request::Launch { vm, .. } => format!("launch {vm}"),
        Request::Destroy { vm } => format!("destroy {vm}"),
        Request::List => "list".into(),
        Request::Attach { vm, .. } => format!("attach {vm}"),
        Request::Fetch { vm, repo } => format!("fetch {vm}/{repo}"),
        Request::Watch => "watch".into(),
        Request::Decide { id, decision } => format!("decide {id} {decision:?}"),
    }
}

async fn owned(agent: &Agent, peer: &Peer, vm: &str) -> Result<VmRecord> {
    if !valid_vm_name(vm) {
        bail!("{vm:?} is not a valid VM name");
    }
    match state::load(&agent.cfg, vm)? {
        Some(r) if r.client == peer.name => Ok(r),
        Some(_) => bail!("VM {vm} belongs to another client"),
        None => bail!("no VM {vm} on this member"),
    }
}

async fn push(
    agent: &Agent,
    peer: &Peer,
    vm: &str,
    repo: RepoRecord,
    recv: &mut RecvStream,
) -> Result<()> {
    if !valid_vm_name(vm) {
        bail!("{vm:?} is not a valid VM name");
    }
    if !valid_name(&repo.name) {
        bail!("{:?} is not a valid repo name", repo.name);
    }
    if !git::valid_rev(&repo.commit) || repo.stash.as_deref().is_some_and(|s| !git::valid_rev(s)) {
        bail!("bad commit id");
    }
    if repo
        .branch
        .as_deref()
        .is_some_and(|b| !git::valid_branch(b))
    {
        bail!("bad branch name");
    }
    let _lock = agent.lock.lock().await;
    let mut rec = match state::load(&agent.cfg, vm)? {
        Some(r) if r.client != peer.name => bail!("VM {vm} belongs to another client"),
        Some(r) => r,
        None => {
            let records = state::all(&agent.cfg)?;
            VmRecord {
                name: vm.to_owned(),
                client: peer.name.clone(),
                slot: state::free_slot(&agent.cfg, &records)?,
                repos: vec![],
                launched: false,
                grants: vec![],
                farms: vec![],
            }
        }
    };
    state::save(&agent.cfg, &rec)?;
    git::receive(&agent.cfg, vm, &repo, recv).await?;
    let mut repo = repo;
    match rec.repos.iter_mut().find(|r| r.name == repo.name) {
        Some(old) => {
            repo.policy = old.policy.clone();
            *old = repo.clone();
        }
        None => rec.repos.push(repo.clone()),
    }
    state::save(&agent.cfg, &rec)?;
    if rec.launched {
        git::update(&agent.cfg, vm, &repo).await?;
    }
    Ok(())
}

async fn launch(
    agent: &Agent,
    peer: &Peer,
    vm: &str,
    repos: &[LaunchRepo],
    git_user: Option<&GitUser>,
    progress: &mut Progress<'_>,
) -> Result<VmInfo> {
    let cfg = &agent.cfg;
    if !valid_vm_name(vm) {
        bail!("{vm:?} is not a valid VM name");
    }
    if repos.is_empty() {
        bail!("a VM needs at least one repo");
    }
    if let Some(u) = git_user {
        u.validate()?;
    }
    let mut rec = {
        let _lock = agent.lock.lock().await;
        let mut rec = match state::load(cfg, vm)? {
            Some(r) if r.client != peer.name => bail!("VM {vm} belongs to another client"),
            Some(r) if r.launched => bail!("VM {vm} runs already"),
            Some(r) => r,
            None => VmRecord {
                name: vm.to_owned(),
                client: peer.name.clone(),
                slot: state::free_slot(cfg, &state::all(cfg)?)?,
                repos: vec![],
                launched: false,
                grants: vec![],
                farms: vec![],
            },
        };
        let mut out = Vec::new();
        for lr in repos {
            if !valid_name(&lr.name) || !valid_name(&lr.policy) {
                bail!("bad repo or policy name in {}", lr.name);
            }
            cfg.policy(&lr.policy)?;
            let r = match &lr.source {
                RepoSource::Pushed => {
                    let mut r = rec
                        .repo(&lr.name)
                        .cloned()
                        .with_context(|| format!("repo {} was not pushed", lr.name))?;
                    r.policy = lr.policy.clone();
                    r
                }
                RepoSource::Url { url, git_ref } => {
                    state::save(cfg, &rec)?;
                    progress.say(format!("cloning {url}")).await;
                    let commit = git::clone_url(cfg, vm, &lr.name, url, git_ref.as_deref()).await?;
                    RepoRecord {
                        name: lr.name.clone(),
                        policy: lr.policy.clone(),
                        commit,
                        stash: None,
                        branch: git_ref.clone().filter(|b| git::valid_branch(b)),
                    }
                }
            };
            out.push(r);
        }
        rec.repos = out;
        // Fail early on policies that do not merge.
        gatekeeper::effective(cfg, &rec)?;
        state::save(cfg, &rec)?;
        rec
    };

    for r in &rec.repos.clone() {
        progress
            .say(format!("building the devshell of {}", r.name))
            .await;
        let flake = farm::flake_ref(cfg, vm, &r.name, git::tree_rev(r));
        let key = farm::prepare(cfg, &rec, &r.name, &flake).await?;
        rec.farms.push(key);
    }
    state::save(cfg, &rec)?;

    progress.say("starting the gatekeeper").await;
    gatekeeper::prepare(cfg, &agent.incus, &rec).await?;
    gatekeeper::start(cfg, &rec).await?;

    progress.say("creating the VM").await;
    let address = cfg.vm_address(rec.slot)?;
    let mut devices = json!({
        "eth0": {
            "type": "nic",
            "name": "eth0",
            "network": cfg.bridge.name,
            "ipv4.address": address.to_string(),
            "security.ipv4_filtering": "true",
            "security.acls": gatekeeper::acl_name(vm),
            "security.acls.default.egress.action": "drop",
            "security.acls.default.ingress.action": "drop",
        },
        "root": { "type": "disk", "path": "/", "pool": cfg.storage_pool },
    });
    for r in &rec.repos {
        devices[format!("farm-{}", r.name)] = json!({
            "type": "disk",
            "source": farm::vm_store_dir(cfg, vm).join(&r.name).to_string_lossy(),
            "path": format!("/run/hvw/farms/{}", r.name),
            "readonly": "true",
        });
    }
    agent
        .incus
        .create_instance(json!({
            "name": vm,
            "type": "virtual-machine",
            "ephemeral": true,
            "source": { "type": "image", "alias": cfg.image },
            "profiles": [],
            "config": {
                "limits.cpu": cfg.vm.cpus.to_string(),
                "limits.memory": cfg.vm.memory,
                "security.secureboot": "false",
            },
            "devices": devices,
        }))
        .await?;
    agent.incus.set_state(vm, "start").await?;

    progress.say("waiting for the VM to boot").await;
    wait_for_vm(vm).await?;

    progress.say("setting up the VM").await;
    setup_vm(cfg, &rec).await?;
    if let Some(u) = git_user {
        git::set_identity(cfg, vm, u).await?;
    }
    for r in &rec.repos {
        progress.say(format!("checking out {}", r.name)).await;
        git::checkout(cfg, vm, r).await?;
    }
    rec.launched = true;
    {
        let _lock = agent.lock.lock().await;
        state::save(cfg, &rec)?;
    }
    progress.say(format!("VM {vm} is ready")).await;
    Ok(info(cfg, &rec, "Running", 0))
}

async fn wait_for_vm(vm: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    while tokio::time::Instant::now() < deadline {
        let probe = Exec::new(vm, &["true"]);
        if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(10), probe.run(b"")).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("the Incus agent in {vm} did not answer in 10 minutes")
}

/// The environment of the agent harness: proxy, CA and policy variables.
fn vm_env(cfg: &Config, rec: &VmRecord, eff: &Effective) -> BTreeMap<String, String> {
    let ports = cfg.ports(rec.slot);
    let proxy = format!("http://{}:{}", cfg.bridge.address, ports.tunnel);
    let bundle = "/run/hvw/ca-bundle.crt".to_owned();
    let mut env = eff.vm_env();
    for k in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        env.insert(k.to_owned(), proxy.clone());
    }
    for k in ["NO_PROXY", "no_proxy"] {
        env.insert(k.to_owned(), cfg.bridge.address.to_string());
    }
    for k in [
        "SSL_CERT_FILE",
        "NIX_SSL_CERT_FILE",
        "CURL_CA_BUNDLE",
        "GIT_SSL_CAINFO",
        "REQUESTS_CA_BUNDLE",
    ] {
        env.insert(k.to_owned(), bundle.clone());
    }
    env.insert(
        "NODE_EXTRA_CA_CERTS".to_owned(),
        "/run/hvw/ca.crt".to_owned(),
    );
    env.insert(
        "HVW_BROKER".to_owned(),
        format!("http://{}:{}", cfg.bridge.address, cfg.broker_port),
    );
    env.insert("HVW_VM".to_owned(), rec.name.clone());
    env
}

async fn setup_vm(cfg: &Config, rec: &VmRecord) -> Result<()> {
    let eff = gatekeeper::effective(cfg, rec)?;
    let ca = std::fs::read(gatekeeper::ca_cert(cfg, &rec.name))?;
    let env = env_file(&vm_env(cfg, rec, &eff));
    let vm = &rec.name;
    Exec::shell(vm, "mkdir -p /run/hvw && cat > /run/hvw/ca.crt")
        .run(&ca)
        .await?;
    Exec::shell(vm, "cat > /run/hvw/env")
        .run(env.as_bytes())
        .await?;
    let repos: Vec<&str> = rec.repos.iter().map(|r| r.name.as_str()).collect();
    let mut argv = vec!["hvw-vm-setup"];
    argv.extend(repos);
    Exec::new(vm, &argv).run(b"").await?;
    Ok(())
}

async fn destroy(agent: &Agent, peer: &Peer, vm: &str) -> Result<()> {
    let cfg = &agent.cfg;
    let rec = owned(agent, peer, vm).await?;
    let ids: Vec<u64> = agent
        .pending
        .lock()
        .await
        .iter()
        .filter(|(_, e)| e.info.vm == rec.name)
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        agent.withdraw(id).await;
    }
    let _lock = agent.lock.lock().await;
    agent.incus.delete_instance(vm).await?;
    gatekeeper::stop(vm).await?;
    agent.incus.delete_acl(&gatekeeper::acl_name(vm)).await?;
    farm::release(cfg, vm).await?;
    match std::fs::remove_dir_all(cfg.vm_dir(vm)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    Ok(())
}

fn info(cfg: &Config, rec: &VmRecord, status: &str, pending: usize) -> VmInfo {
    VmInfo {
        name: rec.name.clone(),
        client: rec.client.clone(),
        status: status.to_owned(),
        address: cfg
            .vm_address(rec.slot)
            .map(|a| a.to_string())
            .unwrap_or_default(),
        repos: rec.repos.iter().map(|r| r.name.clone()).collect(),
        policies: rec.policies(),
        pending,
    }
}

async fn list(agent: &Agent, peer: &Peer) -> Result<Vec<VmInfo>> {
    let statuses: BTreeMap<String, String> = agent.incus.statuses().await?.into_iter().collect();
    let pending = agent.pending.lock().await;
    Ok(state::all(&agent.cfg)?
        .iter()
        .filter(|r| r.client == peer.name)
        .map(|r| {
            let status = statuses.get(&r.name).map_or("Absent", String::as_str);
            let n = pending.values().filter(|e| e.info.vm == r.name).count();
            info(&agent.cfg, r, status, n)
        })
        .collect())
}

async fn watch(agent: &Agent, peer: &Peer, mut send: SendStream) -> Result<()> {
    let mut rx = agent.events.subscribe();
    frame::write(&mut send, &Reply::Ok(json!(null))).await?;
    // First the requests that wait already.
    let current: Vec<Pending> = agent
        .pending
        .lock()
        .await
        .values()
        .filter(|e| e.client == peer.name)
        .map(|e| e.info.clone())
        .collect();
    for p in current {
        frame::write(&mut send, &WatchEvent::Pending(Box::new(p))).await?;
    }
    loop {
        match rx.recv().await {
            Ok((client, ev)) if client == peer.name => frame::write(&mut send, &ev).await?,
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("a watcher lost {n} events");
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn decide(agent: &Agent, peer: &Peer, id: u64, decision: Decision) -> Result<DecideResult> {
    let info = {
        let pending = agent.pending.lock().await;
        match pending.get(&id) {
            Some(e) if e.client == peer.name => e.info.clone(),
            Some(_) => bail!("request {id} belongs to another client"),
            None => bail!("no request {id}: it was decided or withdrawn"),
        }
    };
    let nix = broker::apply(agent, &info, decision).await?;
    if let Some(e) = agent.pending.lock().await.remove(&id) {
        let _ = e.tx.send(decision);
        let _ = agent.events.send((
            e.client,
            WatchEvent::Resolved {
                id,
                vm: e.info.vm,
                decision,
            },
        ));
    }
    Ok(DecideResult { nix })
}
