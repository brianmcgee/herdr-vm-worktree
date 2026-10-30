//! The messages of the protocol.
//!
//! A client opens a bidirectional stream and writes one [`Request`]. The
//! member answers with [`Reply`] frames: zero or more `Progress`, then one
//! `Ok` or `Error`. Some requests carry raw bytes after the frames:
//!
//! - `Push`: after the request, the client writes a git bundle and
//!   finishes its side. Then the member replies.
//! - `Fetch`: after `Ok`, the member writes a git bundle and finishes.
//! - `Attach`: after `Ok`, both sides exchange [`crate::frame::Chunk`]s.
//! - `Watch`: after `Ok`, the member writes [`WatchEvent`] frames until
//!   the client goes away.

use hvw_core::refusal::{EgressAsk, Refusal};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Sends the source of one repo to the member, as a git bundle that
    /// holds `commit`. `stash` is a commit made by `git stash create`: the
    /// uncommitted changes on top of `commit`. If the VM runs, the member
    /// also updates `/work/<repo>` in the VM.
    Push {
        vm: String,
        repo: String,
        commit: String,
        stash: Option<String>,
        branch: Option<String>,
    },
    /// Builds the farms, creates the VM and its gatekeeper, and gives the
    /// pushed repos to the VM. `git_user` is the git identity of the user
    /// on the client. The agent commits with it in the VM.
    Launch {
        vm: String,
        repos: Vec<LaunchRepo>,
        git_user: Option<GitUser>,
    },
    Destroy {
        vm: String,
    },
    List,
    /// A terminal on the agent harness in the VM.
    Attach {
        vm: String,
        cols: u16,
        rows: u16,
    },
    /// Gets the commits of one repo in the VM as a git bundle.
    Fetch {
        vm: String,
        repo: String,
    },
    /// Streams the egress requests of the VMs of this client.
    Watch,
    Decide {
        id: u64,
        decision: Decision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRepo {
    pub name: String,
    pub policy: String,
    pub source: RepoSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RepoSource {
    /// The client sent the repo with `Push` before the launch.
    Pushed,
    /// The member clones the repo.
    Url {
        url: String,
        git_ref: Option<String>,
    },
}

/// The `user.name` and `user.email` of git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitUser {
    pub name: String,
    pub email: String,
}

impl GitUser {
    /// The longest value that the member accepts.
    pub const MAX_LEN: usize = 256;

    /// Fails when a value is empty, too long, or has a control character.
    /// A control character could change the meaning of the git config.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (key, v) in [("name", &self.name), ("email", &self.email)] {
            if v.is_empty() || v.len() > Self::MAX_LEN || v.chars().any(char::is_control) {
                anyhow::bail!("the git user {key} {v:?} is not valid");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Progress(String),
    Ok(Value),
    Error(String),
}

/// One worktree VM, as `List` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmInfo {
    pub name: String,
    /// The peer name of the client that launched the VM.
    pub client: String,
    /// The Incus status, for example `Running`.
    pub status: String,
    pub address: String,
    pub repos: Vec<String>,
    pub policies: Vec<String>,
    pub pending: usize,
}

/// The answer to `Fetch`, before the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchInfo {
    /// The branch that is checked out in the VM.
    pub head: Option<String>,
}

/// The scope of a decision on an egress request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Allow for this VM only. The grant goes away with the VM.
    Vm,
    /// Allow for this VM, as `Vm` does. The member also returns the Nix
    /// change that adds the grant to the policies of the VM, for the clan
    /// repo. Other VMs get the grant only after a deploy of that change.
    Policy,
    Deny,
}

/// The answer to `Decide`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DecideResult {
    /// For a policy grant: the Nix change that makes it permanent.
    pub nix: Option<String>,
}

/// An egress request of an agent, that waits for a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub id: u64,
    pub vm: String,
    /// The peer name of the member.
    pub member: String,
    pub ask: EgressAsk,
    /// The reason of the agent. Untrusted text.
    pub reason: String,
    /// The request that the gatekeeper refused.
    pub refusal: Refusal,
    pub policies: Vec<String>,
    /// Seconds since the Unix epoch.
    pub created: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WatchEvent {
    Pending(Box<Pending>),
    Resolved {
        id: u64,
        vm: String,
        decision: Decision,
    },
    /// A request that the agent gave up on, or whose VM went away.
    Withdrawn {
        id: u64,
        vm: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format() {
        let req = Request::Launch {
            vm: "hvw-x".into(),
            repos: vec![LaunchRepo {
                name: "app".into(),
                policy: "github".into(),
                source: RepoSource::Url {
                    url: "https://github.com/o/app".into(),
                    git_ref: None,
                },
            }],
            git_user: Some(GitUser {
                name: "Alice Liddell".into(),
                email: "alice@example.org".into(),
            }),
        };
        let text = serde_json::to_string(&req).unwrap();
        assert_eq!(
            text,
            r#"{"op":"launch","vm":"hvw-x","repos":[{"name":"app","policy":"github","source":{"kind":"url","url":"https://github.com/o/app","git_ref":null}}],"git_user":{"name":"Alice Liddell","email":"alice@example.org"}}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), req);
        assert_eq!(
            serde_json::to_string(&Reply::Error("x".into())).unwrap(),
            r#"{"error":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Decide {
                id: 3,
                decision: Decision::Policy
            })
            .unwrap(),
            r#"{"op":"decide","id":3,"decision":"policy"}"#
        );
    }

    #[test]
    fn git_user_values() {
        let user = |name: &str, email: &str| GitUser {
            name: name.into(),
            email: email.into(),
        };
        assert!(user("Zoë O'Neil", "zoe@example.org").validate().is_ok());
        assert!(user("", "zoe@example.org").validate().is_err());
        assert!(user("Zoë", "zoe@example.org\n[core]").validate().is_err());
        assert!(user("Zoë\0", "zoe@example.org").validate().is_err());
        let long = "a".repeat(GitUser::MAX_LEN + 1);
        assert!(user("Zoë", &long).validate().is_err());
    }
}
