//! The workspace manifest: the repos that one worktree VM receives.
//!
//! Format: TOML. Location: `hvw.toml` in the root of the herdr worktree.
//!
//! ```toml
//! [[repo]]
//! path = "."
//! policy = "github"
//!
//! [[repo]]
//! name = "nixpkgs"
//! url = "https://github.com/NixOS/nixpkgs"
//! ref = "nixos-unstable"
//! policy = "nix"
//! ```
//!
//! A repo has a local `path` (relative to the manifest) or a `url` with an
//! optional `ref`. Its `name` is the directory under `/work` in the VM. The
//! default name is the last component of the path or the URL.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The file name of the manifest in the root of a worktree.
pub const FILE_NAME: &str = "hvw.toml";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("manifest is not valid TOML: {0}")]
    Toml(String),
    #[error("manifest has no repo")]
    Empty,
    #[error("repo {0}: set exactly one of `path` and `url`")]
    Source(usize),
    #[error("repo {0}: `ref` needs `url`")]
    RefWithoutUrl(usize),
    #[error("repo {index}: cannot derive a name from {source_text:?}; set `name`")]
    NoName { index: usize, source_text: String },
    #[error(
        "repo name {0:?} is not valid: use letters, digits, `.`, `_`, `-`, and do not start with `.`"
    )]
    BadName(String),
    #[error("repo name {0:?} occurs more than once")]
    DuplicateName(String),
    #[error("repo {0}: `policy` is empty")]
    EmptyPolicy(String),
}

/// Where the source of a repo comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A local checkout. The path is absolute.
    Path(PathBuf),
    /// A remote repo, at a ref (`None`: the default branch).
    Url {
        url: String,
        git_ref: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub source: Source,
    pub policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub repos: Vec<Repo>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default)]
    repo: Vec<RawRepo>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    name: Option<String>,
    path: Option<PathBuf>,
    url: Option<String>,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    policy: String,
}

/// Checks a name for a repo, a VM or a policy. The name becomes a path
/// component and an Incus or systemd name, so the set of characters is
/// small.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn last_component(text: &str) -> Option<String> {
    let trimmed = text.trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let last = last.strip_suffix(".git").unwrap_or(last);
    (!last.is_empty() && last != "." && last != "..").then(|| last.to_owned())
}

impl Manifest {
    /// Parses a manifest. `base` is the directory of the manifest file. It
    /// makes relative paths absolute and names the repo of `path = "."`.
    pub fn parse(text: &str, base: &Path) -> Result<Self, Error> {
        let raw: RawManifest = toml::from_str(text).map_err(|e| Error::Toml(e.to_string()))?;
        if raw.repo.is_empty() {
            return Err(Error::Empty);
        }
        let mut names = BTreeSet::new();
        let mut repos = Vec::with_capacity(raw.repo.len());
        for (index, r) in raw.repo.into_iter().enumerate() {
            let source = match (r.path, r.url) {
                (Some(path), None) => {
                    if r.git_ref.is_some() {
                        return Err(Error::RefWithoutUrl(index));
                    }
                    Source::Path(normalize(&base.join(path)))
                }
                (None, Some(url)) => Source::Url {
                    url,
                    git_ref: r.git_ref,
                },
                _ => return Err(Error::Source(index)),
            };
            let name = match r.name {
                Some(name) => name,
                None => {
                    let source_text = match &source {
                        Source::Path(p) => p.to_string_lossy().into_owned(),
                        Source::Url { url, .. } => url.clone(),
                    };
                    last_component(&source_text).ok_or(Error::NoName { index, source_text })?
                }
            };
            if !valid_name(&name) {
                return Err(Error::BadName(name));
            }
            if !names.insert(name.clone()) {
                return Err(Error::DuplicateName(name));
            }
            if r.policy.is_empty() {
                return Err(Error::EmptyPolicy(name));
            }
            repos.push(Repo {
                name,
                source,
                policy: r.policy,
            });
        }
        Ok(Self { repos })
    }

    /// The policies of all repos, sorted, without duplicates.
    pub fn policies(&self) -> Vec<String> {
        let set: BTreeSet<&str> = self.repos.iter().map(|r| r.policy.as_str()).collect();
        set.into_iter().map(str::to_owned).collect()
    }
}

/// Removes `.` and `..` components without a look at the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "/home/user/src/com/github/brianmcgee/herdr-vm-worktree/.worktrees/feat-x";

    fn parse(text: &str) -> Result<Manifest, Error> {
        Manifest::parse(text, Path::new(BASE))
    }

    #[test]
    fn local_and_remote_repos() {
        let m = parse(
            r#"
            [[repo]]
            path = "."
            policy = "github"

            [[repo]]
            url = "https://github.com/NixOS/nixpkgs.git"
            ref = "nixos-unstable"
            policy = "nix"

            [[repo]]
            name = "dotfiles"
            path = "../../../../Mic92/dotfiles"
            policy = "github"
            "#,
        )
        .unwrap();
        assert_eq!(
            m.repos,
            vec![
                Repo {
                    name: "feat-x".into(),
                    source: Source::Path(BASE.into()),
                    policy: "github".into(),
                },
                Repo {
                    name: "nixpkgs".into(),
                    source: Source::Url {
                        url: "https://github.com/NixOS/nixpkgs.git".into(),
                        git_ref: Some("nixos-unstable".into()),
                    },
                    policy: "nix".into(),
                },
                Repo {
                    name: "dotfiles".into(),
                    source: Source::Path("/home/user/src/com/github/Mic92/dotfiles".into()),
                    policy: "github".into(),
                },
            ]
        );
        assert_eq!(m.policies(), vec!["github".to_owned(), "nix".to_owned()]);
    }

    #[test]
    fn scp_style_url_gives_name() {
        let m = parse("[[repo]]\nurl = \"git@github.com:numtide/blueprint\"\npolicy = \"p\"\n")
            .unwrap();
        assert_eq!(m.repos[0].name, "blueprint");
    }

    #[test]
    fn rejects_bad_manifests() {
        assert_eq!(parse(""), Err(Error::Empty));
        assert_eq!(
            parse("[[repo]]\npath = \".\"\nurl = \"https://x/y\"\npolicy = \"p\"\n"),
            Err(Error::Source(0))
        );
        assert_eq!(parse("[[repo]]\npolicy = \"p\"\n"), Err(Error::Source(0)));
        assert_eq!(
            parse("[[repo]]\npath = \".\"\nref = \"main\"\npolicy = \"p\"\n"),
            Err(Error::RefWithoutUrl(0))
        );
        assert_eq!(
            parse("[[repo]]\nname = \".git\"\npath = \".\"\npolicy = \"p\"\n"),
            Err(Error::BadName(".git".into()))
        );
        assert_eq!(
            parse("[[repo]]\nname = \"a/b\"\npath = \".\"\npolicy = \"p\"\n"),
            Err(Error::BadName("a/b".into()))
        );
        assert_eq!(
            parse(
                "[[repo]]\npath = \"a/x\"\npolicy = \"p\"\n[[repo]]\npath = \"b/x\"\npolicy = \"p\"\n"
            ),
            Err(Error::DuplicateName("x".into()))
        );
        assert_eq!(
            parse("[[repo]]\npath = \".\"\npolicy = \"\"\n"),
            Err(Error::EmptyPolicy("feat-x".into()))
        );
        assert!(matches!(
            parse("[[repo]]\npath = \".\"\npolicy = \"p\"\nbranch = \"x\"\n"),
            Err(Error::Toml(_))
        ));
    }
}
