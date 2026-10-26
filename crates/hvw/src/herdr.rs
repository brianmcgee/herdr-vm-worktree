//! Calls of the herdr CLI. `hvw-watch` and the plugin hooks run next to
//! the herdr server, so the CLI finds its socket. The herdr socket never
//! enters a VM.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

/// The source name of all reports of hvw.
pub const SOURCE: &str = "hvw";

/// The workspace token that counts pending egress requests. The sidebar
/// row format of herdr must name `$egress`.
pub const EGRESS_TOKEN: &str = "egress";

#[derive(Debug, Clone)]
pub struct Herdr {
    bin: OsString,
}

impl Herdr {
    /// The herdr binary: `$HERDR_BIN_PATH` inside herdr, else `herdr`
    /// from `PATH`.
    pub fn new() -> Self {
        Self {
            bin: std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()),
        }
    }

    async fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.bin)
            .args(args)
            .output()
            .await
            .context("cannot run herdr")?;
        if !out.status.success() {
            bail!(
                "herdr {} failed: {}",
                args.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Shows the number of pending egress requests on the workspace row.
    /// Zero clears the token.
    pub async fn set_pending(&self, workspace: &str, n: usize) -> Result<()> {
        let token;
        let args: Vec<&str> = if n == 0 {
            vec![
                "workspace",
                "report-metadata",
                workspace,
                "--source",
                SOURCE,
                "--clear-token",
                EGRESS_TOKEN,
            ]
        } else {
            token = format!("{EGRESS_TOKEN}={n}-pending");
            vec![
                "workspace",
                "report-metadata",
                workspace,
                "--source",
                SOURCE,
                "--token",
                &token,
            ]
        };
        self.run(&args).await.map(drop)
    }

    pub async fn notify(&self, title: &str, body: &str) -> Result<()> {
        self.run(&[
            "notification",
            "show",
            title,
            "--body",
            body,
            "--sound",
            "request",
        ])
        .await
        .map(drop)
    }

    /// Marks the agent in a pane as blocked on the user.
    pub async fn report_blocked(&self, pane: &str, message: &str) -> Result<()> {
        self.run(&[
            "pane",
            "report-agent",
            pane,
            "--source",
            SOURCE,
            "--agent",
            "hvw",
            "--state",
            "blocked",
            "--message",
            message,
        ])
        .await
        .map(drop)
    }

    /// Gives the state of the pane back to the detection of herdr.
    pub async fn release(&self, pane: &str) -> Result<()> {
        self.run(&["pane", "release-agent", pane, "--source", SOURCE])
            .await
            .map(drop)
    }

    /// Runs a command line in a pane.
    pub async fn pane_run(&self, pane: &str, command: &str) -> Result<()> {
        self.run(&["pane", "run", pane, command]).await.map(drop)
    }
}

impl Default for Herdr {
    fn default() -> Self {
        Self::new()
    }
}

/// The parts of a herdr worktree event that hvw uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEvent {
    pub created: bool,
    pub path: PathBuf,
    pub workspace: Option<String>,
}

#[derive(Deserialize)]
struct RawEvent {
    event: String,
    data: RawData,
}

#[derive(Deserialize)]
struct RawData {
    worktree: RawWorktree,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace: Option<RawWorkspace>,
}

#[derive(Deserialize)]
struct RawWorktree {
    path: PathBuf,
}

#[derive(Deserialize)]
struct RawWorkspace {
    workspace_id: String,
}

impl WorktreeEvent {
    /// Parses `HERDR_PLUGIN_EVENT_JSON` of `worktree.created` and
    /// `worktree.removed`.
    pub fn parse(json: &str) -> Result<Self> {
        let raw: RawEvent =
            serde_json::from_str(json).context("the herdr event is not valid JSON")?;
        let created = match raw.event.as_str() {
            "worktree_created" => true,
            "worktree_removed" => false,
            e => bail!("hvw does not handle the herdr event {e:?}"),
        };
        if !raw.data.worktree.path.is_absolute() {
            bail!("the worktree path of the herdr event is not absolute");
        }
        Ok(Self {
            created,
            path: raw.data.worktree.path,
            workspace: raw
                .data
                .workspace
                .map(|w| w.workspace_id)
                .or(raw.data.workspace_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Events as herdr 0.9.3 sends them.
    #[test]
    fn events() {
        let created = r#"{"event":"worktree_created","data":{"type":"worktree_created","workspace":{"workspace_id":"w2","number":2,"label":"feat-a","focused":false,"pane_count":1,"tab_count":1,"active_tab_id":"w2:t1","agent_status":"unknown","worktree":{"repo_key":"/tmp/hh/repo/.git","repo_name":"repo","repo_root":"/tmp/hh/repo","checkout_path":"/tmp/hh/.herdr/worktrees/repo/feat-a","is_linked_worktree":true}},"worktree":{"path":"/tmp/hh/.herdr/worktrees/repo/feat-a","branch":"feat-a","is_bare":false,"is_detached":false,"is_prunable":false,"is_linked_worktree":true,"open_workspace_id":"w2","label":"repo"}}}"#;
        assert_eq!(
            WorktreeEvent::parse(created).unwrap(),
            WorktreeEvent {
                created: true,
                path: "/tmp/hh/.herdr/worktrees/repo/feat-a".into(),
                workspace: Some("w2".into()),
            }
        );
        let removed = r#"{"event":"worktree_removed","data":{"type":"worktree_removed","workspace_id":"w2","workspace":{"workspace_id":"w2","number":2,"label":"feat-a","focused":false,"pane_count":1,"tab_count":1,"active_tab_id":"w2:t1","agent_status":"unknown","worktree":{"repo_key":"/tmp/hh/repo/.git","repo_name":"repo","repo_root":"/tmp/hh/repo","checkout_path":"/tmp/hh/.herdr/worktrees/repo/feat-a","is_linked_worktree":true}},"worktree":{"path":"/tmp/hh/.herdr/worktrees/repo/feat-a","branch":"feat-a","is_bare":false,"is_detached":false,"is_prunable":false,"is_linked_worktree":true,"label":"repo"},"forced":true}}"#;
        let e = WorktreeEvent::parse(removed).unwrap();
        assert!(!e.created);
        assert_eq!(e.workspace.as_deref(), Some("w2"));
        assert!(WorktreeEvent::parse(r#"{"event":"tab_created","data":{}}"#).is_err());
        assert!(
            WorktreeEvent::parse(
                r#"{"event":"worktree_created","data":{"worktree":{"path":"rel"}}}"#
            )
            .is_err()
        );
    }
}
