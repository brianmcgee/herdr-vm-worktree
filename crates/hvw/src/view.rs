//! The data that `hvw list`, `hvw pending` and the terminal UI show, and
//! the cells of their tables. The CLI and the terminal UI use the same
//! column labels.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use hvw_link::{Pending, VmInfo};
use serde::Serialize;

use crate::local;
use crate::state::{Store, VmEntry};

/// One VM of this client, with its local entry when the client has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VmRow {
    pub member: String,
    #[serde(flatten)]
    pub info: VmInfo,
    pub worktree: Option<PathBuf>,
    pub workspace: Option<String>,
}

/// One member, as the last refresh found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberState {
    pub name: String,
    pub vms: usize,
    /// Why the member does not answer.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub vms: Vec<VmRow>,
    pub members: Vec<MemberState>,
}

/// Asks each member for its VMs and joins them with the local entries.
/// Fails only when hvw-watch does not answer.
pub async fn snapshot(store: &Store) -> Result<Snapshot> {
    let mut s = Snapshot::default();
    for (m, r) in local::list_all().await? {
        match r {
            Ok(vms) => {
                s.members.push(MemberState {
                    name: m.clone(),
                    vms: vms.len(),
                    error: None,
                });
                for info in vms {
                    // A bad local entry must not hide the VM.
                    let entry = store.load(&info.name).ok().flatten();
                    s.vms.push(VmRow::new(&m, info, entry));
                }
            }
            Err(e) => s.members.push(MemberState {
                name: m,
                vms: 0,
                error: Some(format!("{e:#}")),
            }),
        }
    }
    s.vms
        .sort_by(|a, b| (&a.info.name, &a.member).cmp(&(&b.info.name, &b.member)));
    s.members.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(s)
}

pub const VM_COLUMNS: [&str; 8] = [
    "VM", "MEMBER", "STATUS", "ADDRESS", "REPOS", "POLICIES", "PENDING", "WORKTREE",
];

pub const PENDING_COLUMNS: [&str; 8] = [
    "ID", "MEMBER", "VM", "METHOD", "HOST", "PATH", "AGE", "REASON",
];

pub const MEMBER_COLUMNS: [&str; 3] = ["MEMBER", "VMS", "STATE"];

/// The text, or `-` when it is empty.
fn or_dash(s: String) -> String {
    if s.is_empty() { "-".to_owned() } else { s }
}

impl VmRow {
    /// A VM of `member`. The local entry belongs to the row only when it
    /// names the same member: a VM of the same name on another member is
    /// not the VM of the entry, and `hvw` commands must not act on it.
    pub fn new(member: &str, info: VmInfo, entry: Option<VmEntry>) -> Self {
        let entry = entry.filter(|e| e.member == member);
        Self {
            member: member.to_owned(),
            worktree: entry.as_ref().map(|e| e.worktree.clone()),
            workspace: entry.and_then(|e| e.workspace),
            info,
        }
    }

    pub fn cells(&self) -> [String; 8] {
        let i = &self.info;
        [
            i.name.clone(),
            self.member.clone(),
            i.status.clone(),
            or_dash(i.address.clone()),
            or_dash(i.repos.join(",")),
            or_dash(i.policies.join(",")),
            i.pending.to_string(),
            self.worktree
                .as_ref()
                .map_or_else(|| "-".to_owned(), |w| w.display().to_string()),
        ]
        .map(|c| one_line(&c))
    }
}

impl MemberState {
    pub fn cells(&self) -> [String; 3] {
        match &self.error {
            None => [self.name.clone(), self.vms.to_string(), "ok".to_owned()],
            Some(e) => [
                self.name.clone(),
                "-".to_owned(),
                format!("unreachable: {}", one_line(e)),
            ],
        }
    }
}

/// The cells of a pending request. `now` is in seconds since the Unix
/// epoch. The VM controls the reason and the path of its refused request.
pub fn pending_cells(p: &Pending, now: u64) -> [String; 8] {
    [
        p.id.to_string(),
        p.member.clone(),
        p.vm.clone(),
        p.refusal.method.clone(),
        p.refusal.host.clone(),
        or_dash(p.refusal.path.clone()),
        age(now.saturating_sub(p.created)),
        p.reason.clone(),
    ]
    .map(|c| one_line(&c))
}

/// A duration in its largest whole unit, for example `5m`.
pub fn age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

/// The text on one line, with no control characters. Text from a member
/// or a VM is untrusted: it must not break the rows of a table or send
/// escape sequences to the terminal.
pub fn one_line(s: &str) -> String {
    s.replace(char::is_control, " ")
}

/// The text with no control characters other than line feeds, for
/// untrusted text on more than one line.
pub fn printable(s: &str) -> String {
    s.replace(|c: char| c.is_control() && c != '\n', " ")
}

/// Pretty JSON that is safe on a terminal. `serde_json` escapes only the
/// C0 controls in strings, so this escapes DEL and the C1 controls too.
/// Outside strings, the output has no control characters other than line
/// feeds.
pub fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let s = serde_json::to_string_pretty(value)?;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() && c != '\n' {
            out.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// An aligned table with a header row. Two spaces separate the columns.
/// The last column has no padding.
pub fn table<const N: usize>(headers: [&str; N], rows: &[[String; N]]) -> String {
    let mut widths = headers.map(|h| h.chars().count());
    for row in rows {
        for (w, c) in widths.iter_mut().zip(row) {
            *w = (*w).max(c.chars().count());
        }
    }
    let mut out = String::new();
    let mut line = |cells: [&str; N]| {
        for (i, c) in cells.iter().enumerate() {
            out.push_str(c);
            if i + 1 < N {
                let pad = widths[i] - c.chars().count() + 2;
                out.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push('\n');
    };
    line(headers);
    for row in rows {
        line(row.each_ref().map(String::as_str));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hvw_core::refusal::{EgressAsk, Refusal};

    fn info(name: &str) -> VmInfo {
        VmInfo {
            name: name.into(),
            client: "laptop".into(),
            status: "Running".into(),
            address: "10.250.0.2".into(),
            repos: vec!["repo".into()],
            policies: vec!["default".into()],
            pending: 0,
        }
    }

    #[test]
    fn row_takes_the_entry_of_its_member_only() {
        let entry = VmEntry {
            vm: "hvw-repo-1a2b3c4d".into(),
            member: "m2".into(),
            worktree: "/home/lemmy/work/repo".into(),
            workspace: Some("w3".into()),
            pane: None,
        };
        let live = VmRow::new("m2", info("hvw-repo-1a2b3c4d"), Some(entry.clone()));
        assert_eq!(live.worktree, Some("/home/lemmy/work/repo".into()));
        assert_eq!(live.workspace.as_deref(), Some("w3"));
        // A stale VM of the same name on another member.
        let stale = VmRow::new("m1", info("hvw-repo-1a2b3c4d"), Some(entry));
        assert_eq!((stale.worktree, stale.workspace), (None, None));
    }

    #[test]
    fn table_aligns_columns() {
        let rows = [
            ["a".to_owned(), "wide-cell".to_owned(), "x".to_owned()],
            ["bb".to_owned(), "c".to_owned(), "long last".to_owned()],
        ];
        assert_eq!(
            table(["A", "B", "C"], &rows),
            "A   B          C\n\
             a   wide-cell  x\n\
             bb  c          long last\n"
        );
    }

    fn pending(path: &str, reason: &str) -> Pending {
        Pending {
            id: 7,
            vm: "hvw-repo-1a2b3c4d".into(),
            member: "m1".into(),
            ask: EgressAsk {
                host: "example.com".into(),
                method: None,
                path: None,
            },
            reason: reason.into(),
            refusal: Refusal {
                time: "2025-01-01T00:00:00Z".into(),
                host: "example.com".into(),
                method: "CONNECT".into(),
                path: path.into(),
            },
            policies: vec!["default".into()],
            created: 1000,
        }
    }

    #[test]
    fn pending_cells_flatten_untrusted_text() {
        let c = pending_cells(
            &pending(
                "/\u{9b}2K\x1b]52;c;eA==\x07",
                "line one\nline\ttwo\r\x1b[2J",
            ),
            1090,
        );
        assert_eq!(
            c,
            [
                "7",
                "m1",
                "hvw-repo-1a2b3c4d",
                "CONNECT",
                "example.com",
                "/ 2K ]52;c;eA== ",
                "1m",
                "line one line two  [2J"
            ]
        );
        assert_eq!(pending_cells(&pending("", "r"), 900)[5..7], ["-", "0s"]);
    }

    #[test]
    fn json_escapes_all_control_characters() {
        let p = pending("/a\u{7f}b\u{9b}c", "x\x1by\nz");
        let s = json(&p).unwrap();
        assert!(!s.chars().any(|c| c.is_control() && c != '\n'), "{s}");
        assert!(s.contains(r#""path": "/a\u007fb\u009bc""#), "{s}");
        assert!(s.contains(r#""reason": "x\u001by\nz""#), "{s}");
        let back: Pending = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn age_boundaries() {
        assert_eq!(age(59), "59s");
        assert_eq!(age(60), "1m");
        assert_eq!(age(3599), "59m");
        assert_eq!(age(3600), "1h");
        assert_eq!(age(86399), "23h");
        assert_eq!(age(86400), "1d");
    }
}
