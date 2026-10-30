//! Git on the client: the snapshot of a checkout that goes to a VM, and
//! the bundle of the VM that comes back.
//!
//! The worktrees of one repo share their refs. Thus all refs of a VM are
//! below `refs/hvw/<vm>/` (temporary, for the bundle) and
//! `refs/remotes/<vm>/` (the branches of the VM).

use std::path::Path;

use anyhow::{Context, Result, bail};
use hvw_link::GitUser;
use tokio::process::Command;

/// The state of a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The commit of `HEAD`.
    pub commit: String,
    /// A commit from `git stash create` with the uncommitted changes to
    /// tracked files. Untracked files do not go to the VM.
    pub stash: Option<String>,
    /// The branch that is checked out. `None` for a detached `HEAD`.
    pub branch: Option<String>,
}

async fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .context("cannot run git")?;
    if !out.status.success() {
        bail!(
            "git {} in {} failed: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

async fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git(dir, args).await?)
        .trim()
        .to_owned())
}

pub async fn snapshot(repo: &Path) -> Result<Snapshot> {
    let commit = git_text(repo, &["rev-parse", "--verify", "HEAD^{commit}"])
        .await
        .context("the repo has no commit")?;
    let stash = git_text(repo, &["stash", "create"]).await?;
    let branch = git_text(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok()
        .filter(|b| !b.is_empty());
    Ok(Snapshot {
        commit,
        stash: (!stash.is_empty()).then_some(stash),
        branch,
    })
}

/// The git identity that applies in `dir`. `None` when git has no
/// `user.name` or no `user.email` there.
pub async fn user(dir: &Path) -> Option<GitUser> {
    let name = git_text(dir, &["config", "--get", "user.name"])
        .await
        .ok()?;
    let email = git_text(dir, &["config", "--get", "user.email"])
        .await
        .ok()?;
    Some(GitUser { name, email })
}

/// A git bundle that holds the commit and the stash of a snapshot.
pub async fn bundle(repo: &Path, vm: &str, snap: &Snapshot) -> Result<Vec<u8>> {
    let head = format!("refs/hvw/{vm}/head");
    let stash = format!("refs/hvw/{vm}/stash");
    let mut refs = vec![(head.as_str(), snap.commit.as_str())];
    if let Some(s) = &snap.stash {
        refs.push((stash.as_str(), s.as_str()));
    }
    for (name, commit) in &refs {
        git(repo, &["update-ref", name, commit]).await?;
    }
    let mut args = vec!["bundle", "create", "--quiet", "-"];
    args.extend(refs.iter().map(|(name, _)| *name));
    let out = git(repo, &args).await;
    for (name, _) in &refs {
        git(repo, &["update-ref", "-d", name]).await?;
    }
    out
}

/// Fetches a bundle of the branches of a VM into `refs/remotes/<vm>/`.
/// Branches that the VM deleted go away.
pub async fn fetch_bundle(repo: &Path, vm: &str, bundle: &[u8]) -> Result<()> {
    let dir = tempdir_in(repo).await?;
    let file = dir.join("vm.bundle");
    tokio::fs::write(&file, bundle).await?;
    let file_s = file.to_string_lossy().into_owned();
    let r = git(
        repo,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--prune",
            &file_s,
            &format!("+refs/heads/*:refs/remotes/{vm}/*"),
        ],
    )
    .await;
    tokio::fs::remove_dir_all(&dir).await.ok();
    r.map(drop)
}

/// A new directory in the git directory of the repo, for a bundle.
async fn tempdir_in(repo: &Path) -> Result<std::path::PathBuf> {
    let gitdir = git_text(repo, &["rev-parse", "--absolute-git-dir"]).await?;
    let dir = Path::new(&gitdir).join(format!("hvw-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, script: &str) -> String {
        let out = std::process::Command::new("sh")
            .args(["-ec", script])
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[tokio::test]
    async fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let vm = tmp.path().join("vm");
        std::fs::create_dir_all(&repo).unwrap();
        sh(
            &repo,
            "git init -q -b main; echo a > f; git add f; git commit -qm a",
        );

        let clean = snapshot(&repo).await.unwrap();
        assert_eq!(clean.stash, None);
        assert_eq!(clean.branch.as_deref(), Some("main"));

        sh(&repo, "echo b > f; echo u > untracked");
        let snap = snapshot(&repo).await.unwrap();
        assert_eq!(snap.commit, clean.commit);
        let stash = snap.stash.clone().unwrap();
        // The working tree stays as it is.
        assert_eq!(std::fs::read_to_string(repo.join("f")).unwrap(), "b\n");

        let b = bundle(&repo, "hvw-x-1", &snap).await.unwrap();
        std::fs::write(tmp.path().join("b"), &b).unwrap();
        // The temporary refs are gone again.
        assert_eq!(sh(&repo, "git for-each-ref refs/hvw"), "");

        // The member side: the bundle holds both commits.
        let out = sh(
            tmp.path(),
            "git init -q vm; cd vm
             git fetch -q ../b '+refs/hvw/hvw-x-1/*:refs/hvw/*'
             git rev-parse refs/hvw/head refs/hvw/stash",
        );
        assert_eq!(out, format!("{}\n{stash}", snap.commit));

        // Work in the VM comes back as a remote branch.
        sh(
            &vm,
            "git checkout -q -b feat refs/hvw/head; echo c > f; git commit -qam c
             git bundle create -q ../vm.bundle --branches",
        );
        let back = std::fs::read(tmp.path().join("vm.bundle")).unwrap();
        fetch_bundle(&repo, "hvw-x-1", &back).await.unwrap();
        assert_eq!(
            sh(&repo, "git show refs/remotes/hvw-x-1/feat:f"),
            "c",
            "the commit of the VM is on the client"
        );
        assert_eq!(sh(&repo, "ls .git | grep hvw- || true"), "");
    }

    #[tokio::test]
    async fn empty_repo() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), "git init -q");
        let err = snapshot(tmp.path()).await.unwrap_err();
        assert!(format!("{err:#}").contains("no commit"), "{err:#}");
    }

    #[tokio::test]
    async fn repo_identity() {
        let tmp = tempfile::tempdir().unwrap();
        sh(
            tmp.path(),
            "git init -q; git config user.name 'Zoë O'\\''Neil'; git config user.email zoe@example.org",
        );
        assert_eq!(
            user(tmp.path()).await,
            Some(GitUser {
                name: "Zoë O'Neil".into(),
                email: "zoe@example.org".into(),
            })
        );
    }
}
