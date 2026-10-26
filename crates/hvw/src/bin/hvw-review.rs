//! `hvw-review`: the popup of the herdr plugin. It shows the pending
//! egress requests with the exact host, method and path that the
//! gatekeeper refused, next to the reason of the agent. The user allows a
//! request for the VM or for its policies, or denies it.

use std::time::Duration;

use anyhow::Result;
use hvw::local::{self, decode};
use hvw::state::Store;
use hvw_link::{DecideResult, Decision, Pending, Request};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

struct App {
    store: Store,
    items: Vec<Pending>,
    list: ListState,
    /// The last result, for the status line.
    status: String,
    /// Nix changes of policy grants, printed after the popup closes.
    nix: Vec<String>,
}

impl App {
    fn reload(&mut self) {
        match self.store.pending() {
            Ok(mut p) => {
                p.sort_by_key(|p| (p.created, p.id));
                self.items = p;
            }
            Err(e) => self.status = format!("cannot read the pending requests: {e:#}"),
        }
        let n = self.items.len();
        match self.list.selected() {
            _ if n == 0 => self.list.select(None),
            Some(i) if i >= n => self.list.select(Some(n - 1)),
            None => self.list.select(Some(0)),
            Some(_) => {}
        }
    }

    fn selected(&self) -> Option<&Pending> {
        self.items.get(self.list.selected()?)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut app = App {
        store: Store::open()?,
        items: Vec::new(),
        list: ListState::default(),
        status: "v: allow for the VM  p: allow for the policies  d: deny  q: quit".into(),
        nix: Vec::new(),
    };
    app.reload();
    let mut terminal = ratatui::init();
    let r = run(&mut terminal, &mut app).await;
    ratatui::restore();
    for nix in &app.nix {
        println!("{nix}");
    }
    r
}

async fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        // Poll, so that requests from hvw-watch show up while the popup is
        // open.
        if !event::poll(Duration::from_millis(500))? {
            app.reload();
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let decision = match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => {
                app.list.select_next();
                continue;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.list.select_previous();
                continue;
            }
            KeyCode::Char('v') => Decision::Vm,
            KeyCode::Char('p') => Decision::Policy,
            KeyCode::Char('d') => Decision::Deny,
            _ => continue,
        };
        let Some(p) = app.selected().cloned() else {
            continue;
        };
        app.status = format!("sending the decision on {}…", p.id);
        terminal.draw(|f| draw(f, app))?;
        let req = Request::Decide { id: p.id, decision };
        match local::call(&p.member, &req, None, |_| {})
            .await
            .and_then(decode::<DecideResult>)
        {
            Ok(r) => {
                app.status = format!("{}: {decision:?} for {}", p.id, p.refusal.host);
                if let Some(nix) = r.nix {
                    app.status.push_str(": the Nix change is printed at exit");
                    app.nix.push(nix);
                }
            }
            Err(e) => app.status = format!("{}: {e:#}", p.id),
        }
        app.reload();
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [top, detail, status] = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let items: Vec<ListItem> = app
        .items
        .iter()
        .map(|p| {
            ListItem::new(format!(
                "{:>4}  {:<24} {} {}",
                p.id, p.vm, p.refusal.method, p.refusal.host
            ))
        })
        .collect();
    let title = format!(" egress requests ({}) ", app.items.len());
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, top, &mut app.list);

    let text = match app.selected() {
        None => vec![Line::from("No request waits.")],
        Some(p) => {
            let field = |k: &'static str, v: String| {
                Line::from(vec![
                    Span::styled(format!("{k:<9}"), Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(v),
                ])
            };
            vec![
                field("vm", format!("{} on {}", p.vm, p.member)),
                field("policies", p.policies.join(", ")),
                field("refused", p.refusal.time.clone()),
                field("method", p.refusal.method.clone()),
                field("host", p.refusal.host.clone()),
                field("path", p.refusal.path.clone()),
                Line::from(""),
                Line::from(Span::styled(
                    "reason of the agent (untrusted):",
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Line::from(p.reason.clone()),
            ]
        }
    };
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" request ")),
        detail,
    );
    f.render_widget(Paragraph::new(app.status.as_str()), status);
}
