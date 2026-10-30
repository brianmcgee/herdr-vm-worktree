//! `hvw`: the client CLI of herdr-vm-worktree.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use hvw::herdr::{Herdr, WorktreeEvent};
use hvw::local::{self, decode};
use hvw::state::{Store, VmEntry};
use hvw::tui::View;
use hvw::view;
use hvw_core::manifest::{self, Manifest, Source};
use hvw_core::vm::vm_name;
use hvw_link::frame::{Chunk, read_chunk, write_chunk};
use hvw_link::{
    DecideResult, Decision, FetchInfo, LaunchRepo, RepoSource, Request, VmInfo, read_reply,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(
    version,
    about = "Run herdr worktrees in VMs on Incus members. Without a command, open the terminal UI."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(clap::Args, Clone)]
struct Target {
    /// The worktree with the workspace manifest `hvw.toml`. The default is
    /// the current directory.
    #[arg(long, short = 'w')]
    worktree: Option<PathBuf>,
}

#[derive(clap::Args, Clone)]
struct VmTarget {
    #[command(flatten)]
    target: Target,
    /// The VM. The default is the VM of the worktree.
    #[arg(long, conflicts_with = "worktree")]
    vm: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Make a new iroh secret key file and print its endpoint id.
    Keygen {
        /// The key file to make. It must not exist.
        #[arg(long)]
        secret_file: PathBuf,
    },
    /// Start the VM of a worktree if it does not run, then attach to it.
    Up {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        place: Place,
    },
    /// Start the VM of a worktree.
    Launch {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        place: Place,
    },
    /// Send the current commits and uncommitted changes of the local repos
    /// to the running VM, as `refs/hvw/head` and `refs/hvw/stash`.
    Push {
        #[command(flatten)]
        target: Target,
    },
    /// Get the branches of each local repo in the VM, as
    /// `refs/remotes/<vm>/<branch>`.
    Fetch {
        #[command(flatten)]
        target: Target,
    },
    /// Open a terminal on the agent harness in the VM.
    Attach {
        #[command(flatten)]
        target: VmTarget,
    },
    /// Stop and delete a VM.
    Destroy {
        #[command(flatten)]
        target: VmTarget,
    },
    /// List the VMs of this client on all members.
    ///
    /// Columns:
    ///   VM        the name of the VM
    ///   MEMBER    the member (Incus host) that runs the VM
    ///   STATUS    the Incus status, for example Running or Stopped
    ///   ADDRESS   the IPv4 address of the VM on the bridge of its member
    ///   REPOS     the repos in /work in the VM
    ///   POLICIES  the egress policies of the VM
    ///   PENDING   the number of egress requests that wait for a decision
    ///   WORKTREE  the worktree on this client, `-` if this client has no
    ///             entry for the VM
    #[command(
        verbatim_doc_comment,
        about = "List the VMs of this client on all members"
    )]
    List {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// List the egress requests that wait for a decision.
    ///
    /// Columns:
    ///   ID      the id of the request, for `hvw decide`
    ///   MEMBER  the member that runs the VM
    ///   VM      the VM of the agent that asks
    ///   METHOD  the method of the request that the gatekeeper refused
    ///   HOST    the host of the refused request
    ///   PATH    the path of the refused request, `-` for CONNECT
    ///   AGE     the time since the agent asked
    ///   REASON  the reason of the agent (untrusted text)
    #[command(
        verbatim_doc_comment,
        about = "List the egress requests that wait for a decision"
    )]
    Pending {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Decide on an egress request.
    Decide {
        /// The id of the request, from `hvw pending`.
        id: u64,
        decision: Scope,
        /// The member of the request. The default comes from `hvw pending`.
        #[arg(long)]
        member: Option<String>,
    },
    /// The herdr plugin hook for `worktree.created` and `worktree.removed`.
    #[command(hide = true)]
    HerdrHook,
}

#[derive(clap::Args, Clone, Default)]
struct Place {
    /// The member that runs the VM. The default is the reachable member
    /// with the fewest VMs of this client.
    #[arg(long)]
    member: Option<String>,
    /// The herdr workspace of the worktree.
    #[arg(long, hide = true)]
    workspace: Option<String>,
    /// The herdr pane that runs `hvw attach`.
    #[arg(long, hide = true)]
    pane: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    /// Allow for this VM only.
    Vm,
    /// Allow for this VM only, and print the Nix change that adds the grant
    /// to its policies. Commit and deploy it to allow new VMs too.
    Policy,
    Deny,
}

impl From<Scope> for Decision {
    fn from(s: Scope) -> Self {
        match s {
            Scope::Vm => Decision::Vm,
            Scope::Policy => Decision::Policy,
            Scope::Deny => Decision::Deny,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let r = match cli.command {
        Some(cmd) => run(cmd).await,
        None if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => tui().await,
        None => {
            Cli::command().print_help().ok();
            Ok(ExitCode::from(2))
        }
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("hvw: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the terminal UI. It prints the Nix changes of its policy grants.
async fn tui() -> Result<ExitCode> {
    hvw::tui::run(View::Vms).await?;
    Ok(ExitCode::SUCCESS)
}

async fn run(cmd: Cmd) -> Result<ExitCode> {
    let store = Store::open()?;
    match cmd {
        Cmd::Keygen { secret_file } => keygen(&secret_file)?,
        Cmd::Up { target, place } => {
            let worktree = worktree_dir(&target)?;
            let entry = match find_entry(&store, &worktree)? {
                Some(e) if status(&e).await.as_deref() == Some("Running") => e,
                Some(e) => {
                    eprintln!("hvw: the VM {} does not run: starting it again", e.vm);
                    destroy(&store, &e).await.ok();
                    launch(&store, &worktree, place).await?
                }
                None => launch(&store, &worktree, place).await?,
            };
            return attach(&entry.member, &entry.vm).await;
        }
        Cmd::Launch { target, place } => {
            let worktree = worktree_dir(&target)?;
            if let Some(e) = find_entry(&store, &worktree)? {
                bail!(
                    "the worktree has the VM {} on {} already: use `hvw up` or `hvw destroy`",
                    e.vm,
                    e.member
                );
            }
            launch(&store, &worktree, place).await?;
        }
        Cmd::Push { target } => {
            let entry = entry_of(&store, &worktree_dir(&target)?)?;
            push(&entry, &read_manifest(&entry.worktree)?).await?;
        }
        Cmd::Fetch { target } => {
            let entry = entry_of(&store, &worktree_dir(&target)?)?;
            fetch(&entry, &read_manifest(&entry.worktree)?).await?;
        }
        Cmd::Attach { target } => {
            let entry = vm_entry(&store, &target)?;
            return attach(&entry.member, &entry.vm).await;
        }
        Cmd::Destroy { target } => {
            let entry = vm_entry(&store, &target)?;
            destroy(&store, &entry).await?;
            println!("destroyed {}", entry.vm);
        }
        Cmd::List { json } => list(&store, json).await?,
        Cmd::Pending { json } => pending(&store, json)?,
        Cmd::Decide {
            id,
            decision,
            member,
        } => decide(&store, id, decision.into(), member).await?,
        Cmd::HerdrHook => herdr_hook(&store).await?,
    }
    Ok(ExitCode::SUCCESS)
}

fn keygen(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let (text, id) = hvw_link::generate_secret_key();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot make the key file {}", path.display()))?;
    writeln!(f, "{text}")?;
    println!("{id}");
    Ok(())
}

fn worktree_dir(t: &Target) -> Result<PathBuf> {
    let dir = match &t.worktree {
        Some(w) => std::path::absolute(w)?,
        None => std::env::current_dir()?,
    };
    Ok(dir)
}

fn read_manifest(worktree: &Path) -> Result<Manifest> {
    let path = worktree.join(manifest::FILE_NAME);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read the workspace manifest {}", path.display()))?;
    Ok(Manifest::parse(&text, worktree)?)
}

/// The entry of a worktree. herdr and the shell can name one worktree by
/// different paths (symlinks), so compare canonical paths too.
fn find_entry(store: &Store, worktree: &Path) -> Result<Option<VmEntry>> {
    if let Some(e) = store.load(&vm_name(&worktree.to_string_lossy()))? {
        return Ok(Some(e));
    }
    let Ok(canon) = std::fs::canonicalize(worktree) else {
        return Ok(None);
    };
    Ok(store
        .all()?
        .into_iter()
        .find(|e| std::fs::canonicalize(&e.worktree).is_ok_and(|c| c == canon)))
}

fn entry_of(store: &Store, worktree: &Path) -> Result<VmEntry> {
    find_entry(store, worktree)?
        .with_context(|| format!("the worktree {} has no VM", worktree.display()))
}

fn vm_entry(store: &Store, t: &VmTarget) -> Result<VmEntry> {
    match &t.vm {
        Some(vm) => store
            .load(vm)?
            .with_context(|| format!("this client has no VM {vm}")),
        None => entry_of(store, &worktree_dir(&t.target)?),
    }
}

fn say(line: &str) {
    eprintln!("hvw: {line}");
}

/// The Incus status of a VM, or `None` when its member does not answer.
async fn status(e: &VmEntry) -> Option<String> {
    let vms: Vec<VmInfo> = local::call(&e.member, &Request::List, None, |_| {})
        .await
        .and_then(decode)
        .ok()?;
    Some(
        vms.into_iter()
            .find(|v| v.name == e.vm)
            .map_or_else(|| "Absent".to_owned(), |v| v.status),
    )
}

async fn pick_member(store: &Store, place: &Place) -> Result<String> {
    if let Some(m) = &place.member {
        return Ok(m.clone());
    }
    let mut best: Option<(usize, String)> = None;
    for m in view::snapshot(store).await?.members {
        match m.error {
            None => {
                if best.as_ref().is_none_or(|(n, _)| m.vms < *n) {
                    best = Some((m.vms, m.name));
                }
            }
            Some(e) => say(&format!("skipping the member {}: {e}", m.name)),
        }
    }
    best.map(|(_, m)| m).context("no member answers")
}

async fn launch(store: &Store, worktree: &Path, place: Place) -> Result<VmEntry> {
    let manifest = read_manifest(worktree)?;
    let member = pick_member(store, &place).await?;
    let entry = VmEntry {
        vm: vm_name(&worktree.to_string_lossy()),
        member,
        worktree: worktree.to_owned(),
        workspace: place.workspace,
        pane: place.pane,
    };
    store.save(&entry)?;
    say(&format!("launching {} on {}", entry.vm, entry.member));
    let r = async {
        push(&entry, &manifest).await?;
        let repos = manifest
            .repos
            .iter()
            .map(|r| LaunchRepo {
                name: r.name.clone(),
                policy: r.policy.clone(),
                source: match &r.source {
                    Source::Path(_) => RepoSource::Pushed,
                    Source::Url { url, git_ref } => RepoSource::Url {
                        url: url.clone(),
                        git_ref: git_ref.clone(),
                    },
                },
            })
            .collect();
        let git_user = hvw::git::user(worktree).await;
        if git_user.is_none() {
            say("git has no user.name or user.email here: the agent cannot commit in the VM");
        }
        let req = Request::Launch {
            vm: entry.vm.clone(),
            repos,
            git_user,
        };
        local::call(&entry.member, &req, None, say).await
    }
    .await;
    if let Err(e) = r {
        // The member deletes a VM that failed to start, but not the repos
        // that it received before.
        destroy(store, &entry).await.ok();
        return Err(e);
    }
    Ok(entry)
}

async fn push(entry: &VmEntry, manifest: &Manifest) -> Result<()> {
    for repo in &manifest.repos {
        let Source::Path(path) = &repo.source else {
            continue;
        };
        let snap = hvw::git::snapshot(path).await?;
        let bundle = hvw::git::bundle(path, &entry.vm, &snap).await?;
        say(&format!(
            "sending {} at {}{} ({} KiB)",
            repo.name,
            &snap.commit[..12],
            if snap.stash.is_some() {
                " with uncommitted changes"
            } else {
                ""
            },
            bundle.len() / 1024
        ));
        let req = Request::Push {
            vm: entry.vm.clone(),
            repo: repo.name.clone(),
            commit: snap.commit,
            stash: snap.stash,
            branch: snap.branch,
        };
        local::call(&entry.member, &req, Some(&bundle), say).await?;
    }
    Ok(())
}

async fn fetch(entry: &VmEntry, manifest: &Manifest) -> Result<()> {
    for repo in &manifest.repos {
        let Source::Path(path) = &repo.source else {
            continue;
        };
        let req = Request::Fetch {
            vm: entry.vm.clone(),
            repo: repo.name.clone(),
        };
        let mut s = local::open(&entry.member, &req).await?;
        s.send.shutdown().await?;
        let info: FetchInfo = decode(read_reply(&mut s.recv, say).await?)?;
        let mut bundle = Vec::new();
        s.recv.read_to_end(&mut bundle).await?;
        hvw::git::fetch_bundle(path, &entry.vm, &bundle).await?;
        match info.head {
            Some(h) => println!(
                "{}: fetched the branches as refs/remotes/{}/*; the VM is on {h}",
                repo.name, entry.vm
            ),
            None => println!(
                "{}: fetched the branches as refs/remotes/{}/*",
                repo.name, entry.vm
            ),
        }
    }
    Ok(())
}

async fn destroy(store: &Store, entry: &VmEntry) -> Result<()> {
    let req = Request::Destroy {
        vm: entry.vm.clone(),
    };
    match local::call(&entry.member, &req, None, say).await {
        Ok(_) => {}
        // The member forgot the VM already.
        Err(e) if format!("{e:#}").contains("no VM") => {}
        Err(e) => return Err(e),
    }
    store.remove(&entry.vm)
}

async fn list(store: &Store, json: bool) -> Result<()> {
    let s = view::snapshot(store).await?;
    if json {
        println!("{}", view::json(&s.vms)?);
    } else if s.vms.is_empty() {
        say("no VMs");
    } else {
        let rows: Vec<_> = s.vms.iter().map(view::VmRow::cells).collect();
        print!("{}", view::table(view::VM_COLUMNS, &rows));
    }
    let mut failed = false;
    for m in &s.members {
        if let Some(e) = &m.error {
            failed = true;
            eprintln!(
                "hvw: the member {} does not answer: {}",
                m.name,
                view::one_line(e)
            );
        }
    }
    if failed {
        bail!("some members do not answer");
    }
    Ok(())
}

fn pending(store: &Store, json: bool) -> Result<()> {
    let mut p = store.pending()?;
    p.sort_by_key(|p| (p.created, p.id));
    if json {
        println!("{}", view::json(&p)?);
    } else if p.is_empty() {
        say("no pending requests");
    } else {
        let now = view::now();
        let rows: Vec<_> = p.iter().map(|p| view::pending_cells(p, now)).collect();
        print!("{}", view::table(view::PENDING_COLUMNS, &rows));
    }
    Ok(())
}

async fn decide(store: &Store, id: u64, decision: Decision, member: Option<String>) -> Result<()> {
    let member = match member {
        Some(m) => m,
        None => {
            let matching: Vec<String> = store
                .pending()?
                .into_iter()
                .filter(|p| p.id == id)
                .map(|p| p.member)
                .collect();
            match matching.as_slice() {
                [m] => m.clone(),
                [] => bail!("no pending request {id}: see `hvw pending`"),
                _ => bail!(
                    "members {} have a request {id}: give --member",
                    matching.join(", ")
                ),
            }
        }
    };
    let r: DecideResult =
        decode(local::call(&member, &Request::Decide { id, decision }, None, say).await?)?;
    if let Some(nix) = r.nix {
        println!("{nix}");
    }
    Ok(())
}

/// Restores the terminal mode when the attach ends.
struct RawMode;

impl RawMode {
    fn enable() -> Result<Option<Self>> {
        if !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        ratatui::crossterm::terminal::enable_raw_mode()?;
        Ok(Some(Self))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        ratatui::crossterm::terminal::disable_raw_mode().ok();
    }
}

async fn attach(member: &str, vm: &str) -> Result<ExitCode> {
    let (cols, rows) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let req = Request::Attach {
        vm: vm.to_owned(),
        cols,
        rows,
    };
    let mut s = local::open(member, &req).await?;
    read_reply(&mut s.recv, say).await?;
    let raw = RawMode::enable()?;

    // Separate tasks feed the stream, because a chunk read or a stdin read
    // must not be cancelled in the middle.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Chunk>(64);
    let stdin_tx = tx.clone();
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = vec![0u8; 8192];
        // At the end of stdin the session goes on: a closed send side
        // would stop the harness.
        while let Ok(n) = stdin.read(&mut buf).await {
            if n == 0 || stdin_tx.send(Chunk::Data(buf[..n].to_vec())).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let Ok(mut winch) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        else {
            return;
        };
        while winch.recv().await.is_some() {
            if let Ok((cols, rows)) = ratatui::crossterm::terminal::size()
                && tx.send(Chunk::Resize { cols, rows }).await.is_err()
            {
                break;
            }
        }
    });
    let mut send = s.send;
    tokio::spawn(async move {
        while let Some(c) = rx.recv().await {
            if write_chunk(&mut send, &c).await.is_err() {
                break;
            }
        }
    });

    let mut stdout = tokio::io::stdout();
    let code = loop {
        match read_chunk(&mut s.recv).await {
            Ok(Some(Chunk::Data(d))) => {
                stdout.write_all(&d).await?;
                stdout.flush().await?;
            }
            Ok(Some(Chunk::Exit(code))) => break code,
            Ok(Some(Chunk::Resize { .. })) => {}
            Ok(None) => {
                drop(raw);
                bail!("the member ended the session without an exit status");
            }
            Err(e) => {
                drop(raw);
                return Err(e.context("the session broke"));
            }
        }
    };
    drop(raw);
    // The stdin task can block in a read; do not wait for it.
    std::process::exit(code.clamp(0, 255));
}

/// Quotes a string for a POSIX shell.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

async fn herdr_hook(store: &Store) -> Result<()> {
    let json = std::env::var("HERDR_PLUGIN_EVENT_JSON")
        .context("HERDR_PLUGIN_EVENT_JSON is not set: herdr runs this command")?;
    let ev = WorktreeEvent::parse(&json)?;
    if ev.created {
        if !ev.path.join(manifest::FILE_NAME).exists() {
            say(&format!(
                "{} has no {}: no VM",
                ev.path.display(),
                manifest::FILE_NAME
            ));
            return Ok(());
        }
        let pane =
            std::env::var("HERDR_PANE_ID").context("herdr gave the hook no HERDR_PANE_ID")?;
        let exe = std::env::current_exe()?;
        let mut cmd = format!(
            "{} up --worktree {} --pane {}",
            shell_quote(&exe.to_string_lossy()),
            shell_quote(&ev.path.to_string_lossy()),
            shell_quote(&pane)
        );
        if let Some(ws) = &ev.workspace {
            cmd.push_str(&format!(" --workspace {}", shell_quote(ws)));
        }
        // The launch takes minutes: it runs in the pane, where the user
        // sees its progress, and not in the hook.
        Herdr::new().pane_run(&pane, &cmd).await
    } else {
        match find_entry(store, &ev.path)? {
            Some(e) => {
                destroy(store, &e).await?;
                say(&format!("destroyed {}", e.vm));
                Ok(())
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/a b/c"), "'/a b/c'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
