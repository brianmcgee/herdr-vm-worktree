//! The terminal UI of `hvw`, after k9s. It shows the VMs of this client,
//! the pending egress requests and the members. A `:` command changes the
//! view and `/` filters the rows. The keys of a view run actions on the
//! selected row.
//!
//! `hvw` opens it on the VMs and `hvw-review` (the herdr popup) opens it on
//! the pending requests.

mod draw;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use hvw_link::{DecideResult, Decision, Pending, Request};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::local::{self, decode};
use crate::state::Store;
use crate::view::{self, Snapshot, VmRow, printable};

/// The time between two automatic refreshes of the snapshot.
const REFRESH: Duration = Duration::from_secs(5);
/// The time that a flash message stays on the status line.
const FLASH: Duration = Duration::from_secs(5);
/// After the UI moves the selection to a different request, a decision key
/// does nothing for this time. The user must see the new request first.
const SETTLE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Vms,
    Pending,
    Members,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Normal,
    Command(String),
    Filter(String),
    /// The details of one VM.
    Describe(String),
    /// Destroy this VM after `y`.
    ConfirmDestroy(VmRow),
    Output {
        title: String,
        text: String,
    },
}

/// A message from a background task.
enum Msg {
    Snapshot(Result<Snapshot, String>),
    /// An `hvw` command finished.
    Done {
        title: String,
        ok: bool,
        text: String,
    },
    Decided {
        id: u64,
        host: String,
        decision: Decision,
        result: Result<DecideResult, String>,
    },
}

/// The work that a key asks for. The event loop does it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Quit,
    Attach(VmRow),
    Push(VmRow),
    Fetch(VmRow),
    Destroy(VmRow),
    Decide(Pending, Decision),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    View(View),
    Quit,
}

/// The command of the command bar, or `None` for an unknown command.
fn parse_command(s: &str) -> Option<Command> {
    Some(match s.trim().to_lowercase().as_str() {
        "vm" | "vms" => Command::View(View::Vms),
        "pending" | "pe" | "req" => Command::View(View::Pending),
        "members" | "mem" => Command::View(View::Members),
        "q" | "quit" => Command::Quit,
        _ => return None,
    })
}

/// The indices of the rows that have a cell that contains the filter. The
/// match ignores case.
fn matching<const N: usize>(rows: &[[String; N]], filter: &str) -> Vec<usize> {
    let filter = filter.to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, r)| r.iter().any(|c| c.to_lowercase().contains(&filter)))
        .map(|(i, _)| i)
        .collect()
}

struct Flash {
    text: String,
    at: Instant,
    error: bool,
}

/// The output of a background task, for the output overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Output {
    title: String,
    text: String,
    /// A Nix change. [`App::nix`] holds it too.
    nix: bool,
}

/// The identity of a pending request.
type RequestKey = (String, u64);

fn request_key(p: &Pending) -> RequestKey {
    (p.member.clone(), p.id)
}

struct App {
    store: Store,
    view: View,
    snapshot: Option<Snapshot>,
    /// Why hvw-watch does not answer.
    snapshot_error: Option<String>,
    refreshed: Option<Instant>,
    refreshing: bool,
    /// Sorted by the time of the request.
    pending: Vec<Pending>,
    /// Why the last read of the pending requests failed.
    pending_error: Option<String>,
    table: TableState,
    /// The selected request. The selection stays on it when the list of
    /// requests changes.
    selected_request: Option<RequestKey>,
    /// When the UI, not the user, moved the selection to a different
    /// request.
    auto_selected: Option<Instant>,
    filter: String,
    mode: Mode,
    flash: Option<Flash>,
    /// Outputs that wait until the user is back in the normal mode.
    outputs: VecDeque<Output>,
    /// Nix changes of policy grants, printed after the terminal UI closes.
    nix: Vec<String>,
    /// The background tasks that can still send a message.
    jobs: Vec<JoinHandle<()>>,
    tx: UnboundedSender<Msg>,
    refresh: Arc<Notify>,
}

impl App {
    fn new(store: Store, view: View, tx: UnboundedSender<Msg>, refresh: Arc<Notify>) -> Self {
        Self {
            store,
            view,
            snapshot: None,
            snapshot_error: None,
            refreshed: None,
            refreshing: true,
            pending: Vec::new(),
            pending_error: None,
            table: TableState::default(),
            selected_request: None,
            auto_selected: None,
            filter: String::new(),
            mode: Mode::Normal,
            flash: None,
            outputs: VecDeque::new(),
            nix: Vec::new(),
            jobs: Vec::new(),
            tx,
            refresh,
        }
    }

    fn vms(&self) -> &[VmRow] {
        self.snapshot.as_ref().map_or(&[], |s| &s.vms)
    }

    fn vm_cells(&self) -> Vec<[String; 8]> {
        self.vms().iter().map(VmRow::cells).collect()
    }

    fn pending_cells(&self) -> Vec<[String; 8]> {
        let now = view::now();
        self.pending
            .iter()
            .map(|p| view::pending_cells(p, now))
            .collect()
    }

    fn member_cells(&self) -> Vec<[String; 3]> {
        self.snapshot.as_ref().map_or_else(Vec::new, |s| {
            s.members.iter().map(view::MemberState::cells).collect()
        })
    }

    /// The indices of the visible rows of the current view.
    fn visible(&self) -> Vec<usize> {
        match self.view {
            View::Vms => matching(&self.vm_cells(), &self.filter),
            View::Pending => matching(&self.pending_cells(), &self.filter),
            View::Members => matching(&self.member_cells(), &self.filter),
        }
    }

    /// The index of the selected row in the data of the current view.
    fn selected(&self) -> Option<usize> {
        self.visible().get(self.table.selected()?).copied()
    }

    fn selected_vm(&self) -> Option<VmRow> {
        match self.view {
            View::Vms => self.vms().get(self.selected()?).cloned(),
            _ => None,
        }
    }

    fn selected_pending(&self) -> Option<Pending> {
        match self.view {
            View::Pending => self.pending.get(self.selected()?).cloned(),
            _ => None,
        }
    }

    /// Keeps the selection on a visible row. In the pending view, the
    /// selection stays on the same request.
    fn clamp(&mut self) {
        let visible = self.visible();
        if self.view == View::Pending
            && let Some(k) = &self.selected_request
            && let Some(row) = visible
                .iter()
                .position(|&i| request_key(&self.pending[i]) == *k)
        {
            self.table.select(Some(row));
        }
        let n = visible.len();
        match self.table.selected() {
            _ if n == 0 => self.table.select(None),
            Some(i) if i >= n => self.table.select(Some(n - 1)),
            None => self.table.select(Some(0)),
            Some(_) => {}
        }
        self.track_request(false);
    }

    /// Records the selected request. `user` tells if a key of the user
    /// moved the selection.
    fn track_request(&mut self, user: bool) {
        if self.view != View::Pending {
            return;
        }
        let key = self.selected_pending().as_ref().map(request_key);
        if user {
            self.auto_selected = None;
        } else if key.is_some() && key != self.selected_request {
            self.auto_selected = Some(Instant::now());
        }
        self.selected_request = key;
    }

    fn select(&mut self, i: usize) {
        self.table.select(Some(i));
        self.selected_request = None;
        self.clamp();
        self.track_request(true);
    }

    fn switch(&mut self, view: View) {
        self.view = view;
        self.filter.clear();
        self.select(0);
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            at: Instant::now(),
            error: false,
        });
    }

    fn flash_error(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            at: Instant::now(),
            error: true,
        });
    }

    fn expire_flash(&mut self) {
        if self.flash.as_ref().is_some_and(|f| f.at.elapsed() > FLASH) {
            self.flash = None;
        }
    }

    fn request_refresh(&mut self) {
        self.refreshing = true;
        self.refresh.notify_one();
    }

    fn reload_pending(&mut self) {
        match self.store.pending() {
            Ok(mut p) => {
                p.sort_by_key(|p| (p.created, p.id));
                self.pending = p;
                self.pending_error = None;
            }
            // Flash the error once, not at each tick.
            Err(e) => {
                let e = format!("cannot read the pending requests: {e:#}");
                if self.pending_error.as_ref() != Some(&e) {
                    self.flash_error(e.clone());
                    self.pending_error = Some(e);
                }
            }
        }
        self.clamp();
    }

    /// Opens the next output, but only in the normal mode: an overlay must
    /// not take the keys of a filter, a command or a confirmation.
    fn show_output(&mut self) {
        if self.mode == Mode::Normal
            && let Some(o) = self.outputs.pop_front()
        {
            self.mode = Mode::Output {
                title: o.title,
                text: o.text,
            };
        }
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Snapshot(r) => {
                self.refreshing = false;
                self.refreshed = Some(Instant::now());
                match r {
                    Ok(s) => {
                        self.snapshot = Some(s);
                        self.snapshot_error = None;
                    }
                    Err(e) => self.snapshot_error = Some(e),
                }
            }
            Msg::Done { title, ok, text } => {
                if ok {
                    self.flash(format!("{title}: done"));
                } else {
                    self.flash_error(format!("{title}: failed"));
                }
                self.outputs.push_back(Output {
                    title,
                    text,
                    nix: false,
                });
                self.request_refresh();
            }
            Msg::Decided {
                id,
                host,
                decision,
                result,
            } => match result {
                Ok(r) => {
                    self.flash(format!("{id}: {decision:?} for {host}"));
                    if let Some(nix) = r.nix {
                        self.nix.push(nix.clone());
                        self.outputs.push_back(Output {
                            title: "Nix change for the clan repo (printed at exit too)".into(),
                            text: nix,
                            nix: true,
                        });
                    }
                    self.request_refresh();
                }
                Err(e) => self.flash_error(format!("{id}: {e}")),
            },
        }
        self.clamp();
    }

    /// Handles one key. Does no I/O: the event loop does the action.
    fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => return self.on_normal_key(key.code, ctrl),
            Mode::Command(mut s) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => match parse_command(&s) {
                    Some(Command::View(v)) => self.switch(v),
                    Some(Command::Quit) => return Some(Action::Quit),
                    None => self.flash_error(format!("unknown command: {}", s.trim())),
                },
                KeyCode::Backspace => {
                    s.pop();
                    self.mode = Mode::Command(s);
                }
                KeyCode::Char(c) => {
                    s.push(c);
                    self.mode = Mode::Command(s);
                }
                _ => self.mode = Mode::Command(s),
            },
            Mode::Filter(mut s) => {
                match key.code {
                    KeyCode::Esc => self.filter.clear(),
                    KeyCode::Enter => self.filter = s,
                    KeyCode::Backspace => {
                        s.pop();
                        self.filter.clone_from(&s);
                        self.mode = Mode::Filter(s);
                    }
                    KeyCode::Char(c) => {
                        s.push(c);
                        self.filter.clone_from(&s);
                        self.mode = Mode::Filter(s);
                    }
                    _ => self.mode = Mode::Filter(s),
                }
                self.clamp();
            }
            Mode::Describe(vm) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('a') => {
                    let row = self.vms().iter().find(|r| r.info.name == vm).cloned();
                    self.mode = Mode::Describe(vm);
                    return self.vm_action(row?, Action::Attach);
                }
                _ => self.mode = Mode::Describe(vm),
            },
            Mode::ConfirmDestroy(row) => {
                if key.code == KeyCode::Char('y') {
                    return Some(Action::Destroy(row));
                }
                self.flash("cancelled");
            }
            Mode::Output { title, text } => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.mode = Mode::Output { title, text };
                }
            }
        }
        None
    }

    fn on_normal_key(&mut self, code: KeyCode, ctrl: bool) -> Option<Action> {
        if ctrl {
            if code == KeyCode::Char('d') && self.view == View::Vms {
                let row = self.selected_vm()?;
                if row.worktree.is_none() {
                    self.flash_error(format!("this client has no entry for {}", row.info.name));
                } else {
                    self.mode = Mode::ConfirmDestroy(row);
                }
            }
            return None;
        }
        let last = self.visible().len().saturating_sub(1);
        match code {
            KeyCode::Char('q') => return Some(Action::Quit),
            KeyCode::Char(':') => self.mode = Mode::Command(String::new()),
            KeyCode::Char('/') => self.mode = Mode::Filter(self.filter.clone()),
            KeyCode::Esc => {
                self.filter.clear();
                self.clamp();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let i = self.table.selected().map_or(0, |i| (i + 1).min(last));
                self.select(i);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let i = self.table.selected().map_or(0, |i| i.saturating_sub(1));
                self.select(i);
            }
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(last),
            KeyCode::Char('r') => self.request_refresh(),
            KeyCode::Char('1') => self.switch(View::Vms),
            KeyCode::Char('2') => self.switch(View::Pending),
            KeyCode::Char('3') => self.switch(View::Members),
            KeyCode::Enter | KeyCode::Char('d') if self.view == View::Vms => {
                self.mode = Mode::Describe(self.selected_vm()?.info.name);
            }
            KeyCode::Char('a') if self.view == View::Vms => {
                return self.vm_action(self.selected_vm()?, Action::Attach);
            }
            KeyCode::Char('p') if self.view == View::Vms => {
                return self.vm_action(self.selected_vm()?, Action::Push);
            }
            KeyCode::Char('f') if self.view == View::Vms => {
                return self.vm_action(self.selected_vm()?, Action::Fetch);
            }
            KeyCode::Char(c @ ('v' | 'p' | 'd')) if self.view == View::Pending => {
                let decision = match c {
                    'v' => Decision::Vm,
                    'p' => Decision::Policy,
                    _ => Decision::Deny,
                };
                let p = self.selected_pending()?;
                if self.auto_selected.is_some_and(|t| t.elapsed() < SETTLE) {
                    self.flash_error(format!(
                        "the selection moved to request {}: read it, then press {c} again",
                        p.id
                    ));
                    return None;
                }
                return Some(Action::Decide(p, decision));
            }
            _ => {}
        }
        None
    }

    /// An action on a VM. `hvw` finds the VM in the local entries, so the
    /// action needs one.
    fn vm_action(&mut self, row: VmRow, action: fn(VmRow) -> Action) -> Option<Action> {
        if row.worktree.is_none() {
            self.flash_error(format!("this client has no entry for {}", row.info.name));
            return None;
        }
        Some(action(row))
    }

    /// Does an action. Returns `true` when the user quits.
    fn act(&mut self, terminal: &mut DefaultTerminal, action: Action) -> Result<bool> {
        match action {
            Action::Quit => return Ok(true),
            Action::Attach(row) => self.attach(terminal, &row.info.name)?,
            Action::Push(row) => self.spawn_hvw("push", &row, vec!["push".into(), "-w".into()]),
            Action::Fetch(row) => self.spawn_hvw("fetch", &row, vec!["fetch".into(), "-w".into()]),
            Action::Destroy(row) => {
                let args = vec![
                    "destroy".into(),
                    "--vm".into(),
                    row.info.name.clone().into(),
                ];
                self.spawn_hvw("destroy", &row, args);
            }
            Action::Decide(p, decision) => self.decide(p, decision),
        }
        Ok(false)
    }

    /// Runs `hvw attach` on the terminal and waits for it. `hvw attach`
    /// ends the process when the session ends, so it must be a child.
    fn attach(&mut self, terminal: &mut DefaultTerminal, vm: &str) -> Result<()> {
        let exe = match hvw_exe() {
            Ok(e) => e,
            Err(e) => {
                self.flash_error(format!("{e:#}"));
                return Ok(());
            }
        };
        // `ratatui::restore` does not show the cursor again.
        terminal.show_cursor()?;
        ratatui::restore();
        let status = std::process::Command::new(exe)
            .args(["attach", "--vm", vm])
            .status();
        *terminal = ratatui::init();
        terminal.clear()?;
        match status {
            Ok(s) if s.success() => self.flash(format!("attach to {vm} ended: {s}")),
            Ok(s) => self.flash_error(format!("attach to {vm} ended: {s}")),
            Err(e) => self.flash_error(format!("cannot run hvw attach: {e}")),
        }
        self.request_refresh();
        Ok(())
    }

    /// Runs an `hvw` command in the background. The worktree of the VM
    /// comes after `args` for push and fetch.
    fn spawn_hvw(&mut self, cmd: &str, row: &VmRow, mut args: Vec<OsString>) {
        let vm = &row.info.name;
        if cmd != "destroy"
            && let Some(w) = &row.worktree
        {
            args.push(w.clone().into());
        }
        self.flash(format!("running hvw {cmd} for {vm}…"));
        let title = format!("hvw {cmd} {vm}");
        let tx = self.tx.clone();
        self.jobs.push(tokio::spawn(async move {
            let out = match hvw_exe() {
                Ok(exe) => tokio::process::Command::new(exe)
                    .args(&args)
                    .stdin(Stdio::null())
                    .output()
                    .await
                    .context("cannot run hvw"),
                Err(e) => Err(e),
            };
            let msg = match out {
                Ok(o) => Msg::Done {
                    title,
                    ok: o.status.success(),
                    text: format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ),
                },
                Err(e) => Msg::Done {
                    title,
                    ok: false,
                    text: format!("{e:#}"),
                },
            };
            tx.send(msg).ok();
        }));
    }

    fn decide(&mut self, p: Pending, decision: Decision) {
        self.flash(format!("sending the decision on {}…", p.id));
        let tx = self.tx.clone();
        self.jobs.push(tokio::spawn(async move {
            let req = Request::Decide { id: p.id, decision };
            let result = local::call(&p.member, &req, None, |_| {})
                .await
                .and_then(decode::<DecideResult>)
                .map_err(|e| format!("{e:#}"));
            tx.send(Msg::Decided {
                id: p.id,
                host: p.refusal.host,
                decision,
                result,
            })
            .ok();
        }));
    }
}

/// The `hvw` binary next to the current one. `hvw-review` runs this UI
/// too, and it must call `hvw`, not itself.
fn hvw_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot find the current executable")?;
    let hvw = exe.with_file_name("hvw");
    if !hvw.exists() {
        anyhow::bail!("cannot find hvw next to {}", exe.display());
    }
    Ok(hvw)
}

/// Gets a new snapshot every [`REFRESH`], or when the UI asks for one.
async fn refresh_loop(store: Store, tx: UnboundedSender<Msg>, refresh: Arc<Notify>) {
    loop {
        let s = view::snapshot(&store).await.map_err(|e| format!("{e:#}"));
        if tx.send(Msg::Snapshot(s)).is_err() {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(REFRESH) => {}
            () = refresh.notified() => {}
        }
    }
}

/// Runs the TUI until the user quits. After the terminal is restored, it
/// waits for the running `hvw` commands, prints the outputs that the user
/// did not see, and prints the Nix changes of the policy grants.
pub async fn run(start: View) -> Result<()> {
    let store = Store::open()?;
    let (tx, mut rx) = unbounded_channel();
    let refresh = Arc::new(Notify::new());
    let mut app = App::new(store.clone(), start, tx.clone(), refresh.clone());
    let task = tokio::spawn(refresh_loop(store, tx, refresh));
    let mut terminal = ratatui::init();
    let r = event_loop(&mut terminal, &mut app, &mut rx);
    ratatui::restore();
    task.abort();
    app.jobs.retain(|j| !j.is_finished());
    if !app.jobs.is_empty() {
        eprintln!("hvw: waiting for {} running commands", app.jobs.len());
    }
    for j in std::mem::take(&mut app.jobs) {
        j.await.ok();
    }
    while let Ok(msg) = rx.try_recv() {
        app.on_msg(msg);
    }
    for o in app.outputs.iter().filter(|o| !o.nix) {
        eprintln!("hvw: {}\n{}", o.title, printable(&o.text));
    }
    for nix in &app.nix {
        println!("{}", printable(nix));
    }
    r
}

/// The loop of the UI. It blocks in `event::poll`: the background tasks run
/// on the worker threads of the runtime.
fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    rx: &mut UnboundedReceiver<Msg>,
) -> Result<()> {
    loop {
        while let Ok(msg) = rx.try_recv() {
            app.on_msg(msg);
        }
        app.jobs.retain(|j| !j.is_finished());
        app.reload_pending();
        app.show_output();
        app.expire_flash();
        terminal.draw(|f| draw::draw(f, app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        if let Event::Key(key) = event::read()?
            && let Some(action) = app.on_key(key)
            && app.act(terminal, action)?
        {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hvw_core::refusal::{EgressAsk, Refusal};
    use hvw_link::VmInfo;
    use view::MemberState;

    fn vm(name: &str, worktree: Option<&str>) -> VmRow {
        VmRow {
            member: "m1".into(),
            info: VmInfo {
                name: name.into(),
                client: "laptop".into(),
                status: "Running".into(),
                address: "10.250.0.2".into(),
                repos: vec!["repo".into()],
                policies: vec!["default".into()],
                pending: 1,
            },
            worktree: worktree.map(PathBuf::from),
            workspace: None,
        }
    }

    fn request() -> Pending {
        Pending {
            id: 3,
            vm: "hvw-repo-1a2b3c4d".into(),
            member: "m1".into(),
            ask: EgressAsk {
                host: "api.example.com".into(),
                method: None,
                path: None,
            },
            reason: "the tests need the API".into(),
            refusal: Refusal {
                time: "2025-01-01T00:00:00Z".into(),
                host: "api.example.com".into(),
                method: "GET".into(),
                path: "/v1/x".into(),
            },
            policies: vec!["default".into()],
            created: 1_700_000_000,
        }
    }

    /// An app with two VMs, one without a local entry, and one request.
    fn app(dir: &tempfile::TempDir) -> App {
        let (tx, _rx) = unbounded_channel();
        let store = Store::new(dir.path().to_owned());
        store.save_pending(&[request()]).unwrap();
        let mut app = App::new(store, View::Vms, tx, Arc::new(Notify::new()));
        app.on_msg(Msg::Snapshot(Ok(Snapshot {
            vms: vec![
                vm("hvw-repo-1a2b3c4d", Some("/home/lemmy/work/repo")),
                vm("hvw-other-5e6f7a8b", None),
            ],
            members: vec![MemberState {
                name: "m1".into(),
                vms: 2,
                error: None,
            }],
        })));
        app.reload_pending();
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn keys(app: &mut App, s: &str) -> Option<Action> {
        s.chars()
            .map(|c| app.on_key(key(KeyCode::Char(c))))
            .last()?
    }

    #[test]
    fn commands() {
        assert_eq!(parse_command(" VMs "), Some(Command::View(View::Vms)));
        assert_eq!(parse_command("pe"), Some(Command::View(View::Pending)));
        assert_eq!(parse_command("req"), Some(Command::View(View::Pending)));
        assert_eq!(parse_command("mem"), Some(Command::View(View::Members)));
        assert_eq!(parse_command("quit"), Some(Command::Quit));
        assert_eq!(parse_command("pods"), None);
        assert_eq!(parse_command(""), None);
    }

    #[test]
    fn filter_matches_any_column_without_case() {
        let rows = [
            ["hvw-a".to_owned(), "Running".to_owned()],
            ["hvw-b".to_owned(), "Stopped".to_owned()],
        ];
        assert_eq!(matching(&rows, "STOP"), [1]);
        assert_eq!(matching(&rows, "hvw"), [0, 1]);
        assert_eq!(matching(&rows, ""), [0, 1]);
        assert!(matching(&rows, "zzz").is_empty());
    }

    #[test]
    fn command_bar_switches_view() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        assert_eq!(keys(&mut app, ":pe"), None);
        assert_eq!(app.mode, Mode::Command("pe".into()));
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert_eq!(app.view, View::Pending);
        assert_eq!(app.mode, Mode::Normal);

        keys(&mut app, ":pods");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.view, View::Pending);
        assert!(app.flash.as_ref().is_some_and(|f| f.error));
    }

    #[test]
    fn filter_hides_rows_and_esc_restores_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        keys(&mut app, "/OTHER");
        assert_eq!(app.visible(), [1]);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.selected_vm().unwrap().info.name, "hvw-other-5e6f7a8b");
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.visible(), [0, 1]);
    }

    #[test]
    fn destroy_needs_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(app.on_key(ctrl_d), None);
        assert!(matches!(app.mode, Mode::ConfirmDestroy(_)));
        assert_eq!(keys(&mut app, "n"), None);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.flash.as_ref().unwrap().text, "cancelled");

        app.on_key(ctrl_d);
        let row = vm("hvw-repo-1a2b3c4d", Some("/home/lemmy/work/repo"));
        assert_eq!(keys(&mut app, "y"), Some(Action::Destroy(row)));
    }

    #[test]
    fn push_needs_a_local_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        assert!(matches!(keys(&mut app, "p"), Some(Action::Push(_))));
        app.on_key(key(KeyCode::Down));
        assert_eq!(keys(&mut app, "p"), None);
        let flash = app.flash.as_ref().unwrap();
        assert!(flash.error);
        assert_eq!(
            flash.text,
            "this client has no entry for hvw-other-5e6f7a8b"
        );
    }

    #[test]
    fn pending_keys_decide() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        keys(&mut app, "2");
        assert_eq!(
            keys(&mut app, "v"),
            Some(Action::Decide(request(), Decision::Vm))
        );
        assert_eq!(
            keys(&mut app, "d"),
            Some(Action::Decide(request(), Decision::Deny))
        );
    }

    /// A second request, from another VM, that asks for another host.
    fn other_request(created: u64) -> Pending {
        let mut p = request();
        p.id = 4;
        p.vm = "hvw-other-5e6f7a8b".into();
        p.ask.host = "upload.example.net".into();
        p.refusal.host = "upload.example.net".into();
        p.created = created;
        p
    }

    #[test]
    fn selection_stays_on_its_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        keys(&mut app, "2");
        // An older request comes in above the selected one.
        let b = other_request(request().created - 10);
        app.store.save_pending(&[request(), b.clone()]).unwrap();
        app.reload_pending();
        assert_eq!(app.table.selected(), Some(1));
        assert_eq!(
            keys(&mut app, "v"),
            Some(Action::Decide(request(), Decision::Vm))
        );

        // The selected request goes away, and the selection moves to B.
        // A decision key must not act on B before the user sees it.
        app.store.save_pending(std::slice::from_ref(&b)).unwrap();
        app.reload_pending();
        assert_eq!(app.selected_pending(), Some(b.clone()));
        assert_eq!(keys(&mut app, "v"), None);
        assert!(app.flash.as_ref().is_some_and(|f| f.error));

        // A move of the user makes the selection hers again.
        keys(&mut app, "g");
        assert_eq!(keys(&mut app, "v"), Some(Action::Decide(b, Decision::Vm)));
    }

    #[test]
    fn output_waits_for_the_normal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        keys(&mut app, "2/qu");
        app.on_msg(Msg::Done {
            title: "hvw push hvw-repo-1a2b3c4d".into(),
            ok: true,
            text: "pushed".into(),
        });
        app.show_output();
        assert_eq!(app.mode, Mode::Filter("qu".into()));
        // The rest of the filter goes to the filter, not to the overlay or
        // to the normal mode.
        assert_eq!(keys(&mut app, "iv"), None);
        assert_eq!(app.filter, "quiv");
        app.on_key(key(KeyCode::Enter));
        app.show_output();
        assert!(matches!(app.mode, Mode::Output { .. }));
        assert!(app.outputs.is_empty());
    }

    #[test]
    fn a_bad_pending_file_flashes_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app(&dir);
        std::fs::write(app.store.pending_file(), "{ not json").unwrap();
        app.reload_pending();
        assert!(app.flash.as_ref().is_some_and(|f| f.error));
        app.flash("cancelled");
        app.reload_pending();
        assert_eq!(app.flash.as_ref().unwrap().text, "cancelled");
        // After a good read, a new failure flashes again.
        app.store.save_pending(&[request()]).unwrap();
        app.reload_pending();
        std::fs::write(app.store.pending_file(), "[").unwrap();
        app.reload_pending();
        assert!(app.flash.as_ref().unwrap().text.starts_with("cannot read"));
    }
}
