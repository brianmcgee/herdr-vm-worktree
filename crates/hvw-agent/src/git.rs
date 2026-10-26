//! Source transfer. The member keeps a bare mirror of each repo of a VM.
//! Git bundles carry the source from the client to the mirror, from the
//! mirror into the VM, and from the VM back to the client.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use hvw_link::GitUser;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;

use crate::config::Config;
use crate::incus::Exec;
use crate::state::RepoRecord;

/// The ref of the pushed commit in the mirror and in the VM.
pub const HEAD_REF: &str = "refs/hvw/head";
/// The ref of the stash commit (uncommitted changes).
pub const STASH_REF: &str = "refs/hvw/stash";

pub fn mirror(cfg: &Config, vm: &str, repo: &str) -> PathBuf {
    cfg.vm_dir(vm).join("git").join(format!("{repo}.git"))
}

pub fn valid_rev(rev: &str) -> bool {
    (rev.len() == 40 || rev.len() == 64) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A branch name that is safe to pass to git as an argument.
pub fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with('-')
        && !b.contains("..")
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_' | b'.'))
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .context("cannot run git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

async fn init_mirror(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        git(path, &["init", "--quiet", "--bare"]).await?;
    }
    Ok(())
}

/// Receives a bundle from the client into the mirror, then points
/// `refs/hvw/head` and `refs/hvw/stash` at the commits of the request.
/// The client may name the refs in the bundle as it likes: a worktree
/// shares its refs with the other worktrees of its repo.
pub async fn receive<R: AsyncRead + Unpin>(
    cfg: &Config,
    vm: &str,
    repo: &RepoRecord,
    bundle: &mut R,
) -> Result<()> {
    receive_into(&mirror(cfg, vm, &repo.name), repo, bundle).await
}

async fn receive_into<R: AsyncRead + Unpin>(
    m: &Path,
    repo: &RepoRecord,
    bundle: &mut R,
) -> Result<()> {
    init_mirror(m).await?;
    let file = m.join("incoming.bundle");
    {
        let mut f = tokio::fs::File::create(&file).await?;
        tokio::io::copy(bundle, &mut f)
            .await
            .context("cannot receive the bundle")?;
        f.flush().await?;
    }
    let file_s = file.to_string_lossy().into_owned();
    let r = git(
        m,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--prune",
            &file_s,
            "+refs/*:refs/incoming/*",
        ],
    )
    .await;
    std::fs::remove_file(&file)?;
    r?;
    set_ref(m, HEAD_REF, &repo.commit).await?;
    match &repo.stash {
        Some(stash) => set_ref(m, STASH_REF, stash).await,
        None => git(m, &["update-ref", "-d", STASH_REF]).await.map(drop),
    }
}

/// Points a ref at a commit. Fails if the mirror does not hold the commit.
async fn set_ref(m: &Path, name: &str, commit: &str) -> Result<()> {
    git(m, &["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .await
        .with_context(|| format!("the bundle does not hold the commit {commit}"))?;
    git(m, &["update-ref", name, commit]).await.map(drop)
}

/// Clones a remote repo into the mirror. Returns the commit.
pub async fn clone_url(
    cfg: &Config,
    vm: &str,
    repo: &str,
    url: &str,
    git_ref: Option<&str>,
) -> Result<String> {
    let m = mirror(cfg, vm, repo);
    init_mirror(&m).await?;
    let src = git_ref.unwrap_or("HEAD");
    if src.starts_with('-') {
        bail!("bad ref {src:?}");
    }
    git(
        &m,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--",
            url,
            &format!("+{src}:{HEAD_REF}"),
        ],
    )
    .await?;
    git(&m, &["rev-parse", HEAD_REF]).await
}

/// The rev that the devshell of a repo is built from: the working tree
/// state of the client.
pub fn tree_rev(repo: &RepoRecord) -> &str {
    repo.stash.as_deref().unwrap_or(&repo.commit)
}

async fn mirror_bundle(m: &Path, refs: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(m)
        .args(["bundle", "create", "--quiet", "-"])
        .args(refs)
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "git bundle failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Checks out a repo in `/work/<repo>` of the VM, with the uncommitted
/// changes of the client. A login shell in that directory gets the
/// devshell of the repo (see the image module).
pub async fn checkout(cfg: &Config, vm: &str, repo: &RepoRecord) -> Result<()> {
    let m = mirror(cfg, vm, &repo.name);
    let mut refs = vec![HEAD_REF];
    if repo.stash.is_some() {
        refs.push(STASH_REF);
    }
    let bundle = mirror_bundle(&m, &refs).await?;
    let branch = repo.branch.clone().unwrap_or_else(|| "hvw".to_owned());
    if !valid_branch(&branch) || !valid_rev(&repo.commit) {
        bail!("bad branch or commit");
    }
    let apply = if repo.stash.is_some() {
        format!("git stash apply --quiet {STASH_REF}\n")
    } else {
        String::new()
    };
    let script = format!(
        r#"
dir=/work/{name}
mkdir -p "$dir"
cd "$dir"
b=$(mktemp)
cat > "$b"
git init --quiet
git fetch --quiet --no-tags "$b" '+refs/hvw/*:refs/hvw/*'
rm -f "$b"
git checkout --quiet -B '{branch}' {commit}
{apply}"#,
        name = repo.name,
        commit = repo.commit,
    );
    user_exec(cfg, vm, &script)
        .cwd("/work")
        .run(&bundle)
        .await?;
    Ok(())
}

/// Sets the git identity of the harness user in the VM. The values go in
/// through the environment, so the shell never parses them.
pub async fn set_identity(cfg: &Config, vm: &str, user: &GitUser) -> Result<()> {
    user.validate()?;
    user_exec(
        cfg,
        vm,
        r#"git config --global user.name "$HVW_GIT_NAME"
git config --global user.email "$HVW_GIT_EMAIL"
"#,
    )
    .env("HVW_GIT_NAME", &user.name)
    .env("HVW_GIT_EMAIL", &user.email)
    .run(b"")
    .await?;
    Ok(())
}

/// Adds new commits from the mirror to a running VM, as refs under
/// `refs/hvw/`. The working tree in the VM stays as it is.
pub async fn update(cfg: &Config, vm: &str, repo: &RepoRecord) -> Result<()> {
    let m = mirror(cfg, vm, &repo.name);
    let mut refs = vec![HEAD_REF];
    if repo.stash.is_some() {
        refs.push(STASH_REF);
    }
    let bundle = mirror_bundle(&m, &refs).await?;
    let script = format!(
        r#"cd /work/{name}
b=$(mktemp)
cat > "$b"
git fetch --quiet --no-tags "$b" '+refs/hvw/*:refs/hvw/*'
rm -f "$b"
"#,
        name = repo.name
    );
    user_exec(cfg, vm, &script).run(&bundle).await?;
    Ok(())
}

/// Writes a bundle of the branches of a repo in the VM. Returns the branch
/// that is checked out.
pub async fn fetch<W: AsyncWrite + Unpin>(
    cfg: &Config,
    vm: &str,
    repo: &str,
    out: &mut W,
) -> Result<Option<String>> {
    let head = user_exec(
        cfg,
        vm,
        &format!("cd /work/{repo} && git symbolic-ref --quiet --short HEAD || true"),
    )
    .run(b"")
    .await?;
    let head = String::from_utf8_lossy(&head).trim().to_owned();
    let bundle = user_exec(
        cfg,
        vm,
        &format!("cd /work/{repo} && git bundle create --quiet - --branches"),
    )
    .run(b"")
    .await?;
    out.write_all(&bundle).await?;
    Ok((!head.is_empty()).then_some(head))
}

fn user_exec(cfg: &Config, vm: &str, script: &str) -> Exec {
    Exec::shell(vm, script).user(cfg.vm.uid, cfg.vm.gid, &format!("/home/{}", cfg.vm.user))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_branch("feat/x-1.2"));
        assert!(!valid_branch("-x"));
        assert!(!valid_branch("a..b"));
        assert!(!valid_branch("a b"));
        assert!(!valid_branch("a'b"));
        assert!(valid_rev(&"a".repeat(40)));
        assert!(!valid_rev("abc"));
    }

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

    fn record(commit: &str, stash: Option<&str>) -> RepoRecord {
        RepoRecord {
            name: "app".into(),
            policy: String::new(),
            commit: commit.into(),
            stash: stash.map(str::to_owned),
            branch: None,
        }
    }

    #[tokio::test]
    async fn receive_sets_the_refs_of_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let m = tmp.path().join("mirror.git");
        let out = sh(
            &src,
            "git init -q; echo a > f; git add f; git commit -qm a
             echo b > f; s=$(git stash create)
             git update-ref refs/hvw/vm1/head HEAD; git update-ref refs/hvw/vm1/stash $s
             git bundle create -q ../b refs/hvw/vm1/head refs/hvw/vm1/stash
             echo $(git rev-parse HEAD) $s",
        );
        let (head, stash) = out.split_once(' ').unwrap();
        let mut bundle = tokio::fs::File::open(tmp.path().join("b")).await.unwrap();
        receive_into(&m, &record(head, Some(stash)), &mut bundle)
            .await
            .unwrap();
        assert_eq!(git(&m, &["rev-parse", HEAD_REF]).await.unwrap(), head);
        assert_eq!(git(&m, &["rev-parse", STASH_REF]).await.unwrap(), stash);

        // A second push without changes drops the stash ref.
        sh(&src, "git bundle create -q ../b refs/hvw/vm1/head");
        let mut bundle = tokio::fs::File::open(tmp.path().join("b")).await.unwrap();
        receive_into(&m, &record(head, None), &mut bundle)
            .await
            .unwrap();
        assert!(
            git(&m, &["rev-parse", "--verify", "--quiet", STASH_REF])
                .await
                .is_err()
        );

        // A request for a commit that the bundle does not hold fails.
        let mut bundle = tokio::fs::File::open(tmp.path().join("b")).await.unwrap();
        let err = receive_into(&m, &record(&"1".repeat(40), None), &mut bundle)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("does not hold"), "{err:#}");
    }
}
