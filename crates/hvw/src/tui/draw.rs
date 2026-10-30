//! The rendering of the terminal UI. It reads the state and changes only
//! the scroll state of the table.

use hvw_link::Pending;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, Wrap};

use super::{App, Mode, View};
use crate::view::{self, MEMBER_COLUMNS, PENDING_COLUMNS, VM_COLUMNS, VmRow};

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let [header, main, status] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_header(f, app, header);
    match &app.mode {
        Mode::Describe(vm) => {
            let vm = vm.clone();
            draw_describe(f, app, &vm, main);
        }
        _ => draw_main(f, app, main),
    }
    draw_status(f, app, status);
    match &app.mode {
        Mode::ConfirmDestroy(row) => {
            let text = format!("Destroy the VM {} on {}? y/n", row.info.name, row.member);
            overlay(f, " Destroy ", Paragraph::new(text).red(), 50, 20);
        }
        Mode::Output { title, text } => {
            let p = Paragraph::new(text.as_str()).wrap(Wrap { trim: false });
            overlay(f, &format!(" {title} "), p, 80, 60);
        }
        _ => {}
    }
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let [facts, hints, logo] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Fill(2),
        Constraint::Length(5),
    ])
    .areas(area);

    let watch = match &app.snapshot_error {
        None => Span::raw("ok").green(),
        Some(e) => Span::raw(format!("unreachable: {e}")).red(),
    };
    let (members, down, vms) = app.snapshot.as_ref().map_or((0, 0, 0), |s| {
        let down = s.members.iter().filter(|m| m.error.is_some()).count();
        (s.members.len(), down, s.vms.len())
    });
    let mut members = vec![Span::raw(format!("members: {members}"))];
    if down > 0 {
        members.push(Span::raw(format!(" ({down} unreachable)")).red());
    }
    let pending = app.pending.len();
    let pending = if pending > 0 {
        Span::raw(pending.to_string()).yellow().bold()
    } else {
        Span::raw("0")
    };
    let refreshed = if app.refreshing {
        "refreshing…".to_owned()
    } else {
        app.refreshed.map_or_else(String::new, |t| {
            format!("refreshed: {} ago", view::age(t.elapsed().as_secs()))
        })
    };
    let facts_text = vec![
        Line::from(vec![Span::raw("hvw-watch: "), watch]),
        Line::from(members),
        Line::from(vec![Span::raw(format!("VMs: {vms}  pending: ")), pending]),
        Line::from(refreshed).dim(),
    ];
    f.render_widget(Paragraph::new(facts_text), facts);

    let view_hints = match app.view {
        View::Vms => "<a> attach  <enter> describe  <p> push  <f> fetch  <ctrl-d> destroy",
        View::Pending => "<v> allow VM  <p> allow VM + Nix change  <d> deny",
        View::Members => "",
    };
    let hint_text = vec![
        Line::from(view_hints).cyan(),
        Line::from("<:> command  </> filter  <1-3> views  <r> refresh  <q> quit").dim(),
    ];
    f.render_widget(Paragraph::new(hint_text).wrap(Wrap { trim: false }), hints);
    f.render_widget(Paragraph::new("hvw").style(bold().fg(Color::Cyan)), logo);
}

/// A table with a header row. Each column but the last is as wide as its
/// widest cell.
fn table<'a, const N: usize>(
    headers: [&'a str; N],
    rows: Vec<[String; N]>,
    style: impl Fn(usize, &str) -> Style,
) -> Table<'a> {
    let mut widths = headers.map(|h| h.chars().count());
    for row in &rows {
        for (w, c) in widths.iter_mut().zip(row) {
            *w = (*w).max(c.chars().count());
        }
    }
    let constraints = widths.iter().enumerate().map(|(i, w)| {
        if i + 1 == N {
            Constraint::Fill(1)
        } else {
            Constraint::Length(u16::try_from(*w).unwrap_or(u16::MAX))
        }
    });
    let rows = rows.into_iter().map(|r| {
        Row::new(
            r.into_iter()
                .enumerate()
                .map(|(i, c)| Cell::from(Span::styled(c.clone(), style(i, &c)))),
        )
    });
    Table::new(rows, constraints)
        .header(Row::new(headers).style(bold()))
        .column_spacing(2)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
}

fn vm_style(col: usize, cell: &str) -> Style {
    match (col, cell) {
        (2, "Running") => Style::new().green(),
        (2, "Stopped") => Style::new().yellow(),
        (2, _) => Style::new().red(),
        (6, n) if n != "0" => Style::new().yellow(),
        _ => Style::new(),
    }
}

fn member_style(col: usize, cell: &str) -> Style {
    if col == 2 && cell.starts_with("unreachable") {
        Style::new().red()
    } else {
        Style::new()
    }
}

fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible();
    let (name, empty, table_area) = match app.view {
        View::Vms => ("VMs", "No VMs.", area),
        View::Members => ("Members", "No members.", area),
        View::Pending => {
            let [top, detail] =
                Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(area);
            let p = app.selected_pending();
            draw_request(f, p.as_ref(), detail);
            ("Pending", "No request waits.", top)
        }
    };
    let mut title = format!(" {name} [{}] ", visible.len());
    if !app.filter.is_empty() {
        title.push_str(&format!("/{} ", app.filter));
    }
    let block = Block::bordered().title(title.bold().cyan());

    let t = match app.view {
        View::Vms => table(VM_COLUMNS, pick(app.vm_cells(), &visible), vm_style),
        View::Pending => table(
            PENDING_COLUMNS,
            pick(app.pending_cells(), &visible),
            |_, _| Style::new(),
        ),
        View::Members => table(
            MEMBER_COLUMNS,
            pick(app.member_cells(), &visible),
            member_style,
        ),
    };
    let inner = block.inner(table_area);
    f.render_widget(block, table_area);
    f.render_stateful_widget(t, inner, &mut app.table);

    if visible.is_empty() {
        let line = match (&app.snapshot_error, app.view) {
            (Some(e), View::Vms | View::Members) => {
                Line::from(format!("hvw-watch does not answer: {e}")).red()
            }
            _ => Line::from(empty).dim(),
        };
        let [_, mid, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        f.render_widget(Paragraph::new(line).centered(), mid);
    }
}

/// The rows at the indices, in their order.
fn pick<T>(rows: Vec<T>, indices: &[usize]) -> Vec<T> {
    let mut rows: Vec<Option<T>> = rows.into_iter().map(Some).collect();
    indices.iter().filter_map(|&i| rows[i].take()).collect()
}

/// The detail of a request, with the reason of the agent.
fn draw_request(f: &mut Frame, p: Option<&Pending>, area: Rect) {
    let text = match p {
        None => vec![Line::from("No request waits.").dim()],
        Some(p) => {
            let field = |k: &'static str, v: String| {
                Line::from(vec![Span::styled(format!("{k:<9}"), bold()), Span::raw(v)])
            };
            let mut lines = vec![
                field("vm", format!("{} on {}", p.vm, p.member)),
                field("policies", p.policies.join(", ")),
                field("refused", p.refusal.time.clone()),
                field("method", p.refusal.method.clone()),
                field("host", p.refusal.host.clone()),
                field("path", p.refusal.path.clone()),
                Line::from(""),
                Line::from(Span::styled("reason of the agent (untrusted):", bold())),
            ];
            lines.extend(p.reason.lines().map(|l| Line::from(view::one_line(l))));
            lines
        }
    };
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" Request ")),
        area,
    );
}

fn draw_describe(f: &mut Frame, app: &App, vm: &str, area: Rect) {
    let block = Block::bordered().title(format!(" Describe {vm} ").bold().cyan());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(row) = app.vms().iter().find(|r| r.info.name == vm) else {
        f.render_widget(Paragraph::new(format!("the VM {vm} is gone")).red(), inner);
        return;
    };
    let lines = describe_lines(row);
    let n = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let [top, _, bottom] = Layout::vertical([
        Constraint::Length(n),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    f.render_widget(Paragraph::new(lines), top);
    let now = view::now();
    let rows = app
        .pending
        .iter()
        .filter(|p| p.vm == vm)
        .map(|p| view::pending_cells(p, now))
        .collect();
    f.render_widget(table(PENDING_COLUMNS, rows, |_, _| Style::new()), bottom);
}

fn describe_lines(row: &VmRow) -> Vec<Line<'static>> {
    let i = &row.info;
    let or_dash = |s: String| if s.is_empty() { "-".to_owned() } else { s };
    let field = |k: &'static str, v: String| {
        Line::from(vec![Span::styled(format!("{k:<18}"), bold()), Span::raw(v)])
    };
    vec![
        field("VM", i.name.clone()),
        field("Member", row.member.clone()),
        field("Status", i.status.clone()),
        field("Address (bridge)", or_dash(i.address.clone())),
        field("Repos", or_dash(i.repos.join(", "))),
        field("Policies", or_dash(i.policies.join(", "))),
        field("Pending requests", i.pending.to_string()),
        field(
            "Worktree",
            row.worktree
                .as_ref()
                .map_or_else(|| "-".to_owned(), |w| w.display().to_string()),
        ),
        field(
            "herdr workspace",
            row.workspace.clone().unwrap_or_else(|| "-".to_owned()),
        ),
    ]
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let (prefix, text) = match &app.mode {
        Mode::Command(s) => (":", s),
        Mode::Filter(s) => ("/", s),
        _ => {
            if let Some(fl) = &app.flash {
                let line = Line::from(fl.text.as_str());
                let line = if fl.error { line.red() } else { line };
                f.render_widget(Paragraph::new(line), area);
            }
            return;
        }
    };
    f.render_widget(Paragraph::new(format!("{prefix}{text}")).bold(), area);
    let x = area.x + u16::try_from(text.chars().count() + 1).unwrap_or(u16::MAX);
    f.set_cursor_position(Position::new(x.min(area.right()), area.y));
}

/// A centred rectangle of a part of the area.
fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Percentage(pct_y)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::horizontal([Constraint::Percentage(pct_x)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn overlay(f: &mut Frame, title: &str, p: Paragraph, pct_x: u16, pct_y: u16) {
    let area = centered(f.area(), pct_x, pct_y);
    f.render_widget(Clear, area);
    f.render_widget(
        p.block(Block::bordered().title(title.to_owned().bold())),
        area,
    );
}
