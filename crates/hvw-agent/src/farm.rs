//! Store delivery: build the devshell of a repo on the member, copy its
//! closure into a farm with reflinks, and give the VM a read-only snapshot
//! of the farm.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use hvw_core::farm::{self, FarmUse};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::state::VmRecord;

/// Farm builds and garbage collection run one at a time.
static LOCK: Mutex<()> = Mutex::const_new(());

const COMPLETE: &str = "complete";
const LAST_USED: &str = "last-used";

async fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().await.context("cannot run a command")?;
    if !out.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd.as_std(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The directory of the read-only farm snapshots of a VM.
pub fn vm_store_dir(cfg: &Config, vm: &str) -> PathBuf {
    cfg.vm_dir(vm).join("store")
}

/// The flake reference of a pushed repo at a commit.
pub fn flake_ref(cfg: &Config, vm: &str, repo: &str, rev: &str) -> String {
    format!(
        "git+file://{}?rev={rev}",
        crate::git::mirror(cfg, vm, repo).display()
    )
}

/// Builds the farm of one repo and snapshots it for the VM. Returns the
/// farm key.
pub async fn prepare(cfg: &Config, rec: &VmRecord, repo: &str, flake: &str) -> Result<String> {
    let gcroots = cfg.state_dir.join("gcroots");
    std::fs::create_dir_all(&gcroots)?;
    // The profile is a GC root: the host GC keeps the closure while the
    // VM lives. The farm itself has its own inodes and needs no root.
    let profile = gcroots.join(format!("{}-{repo}", rec.name));
    // The rc script of the devshell. The login shell of the VM sources it.
    let rc = output(Command::new("nix").args([
        "--extra-experimental-features",
        "nix-command flakes",
        "print-dev-env",
        "--no-write-lock-file",
        "--profile",
        &profile.to_string_lossy(),
        flake,
    ]))
    .await
    .with_context(|| format!("cannot build the devshell of {repo}"))?;
    let env = std::fs::canonicalize(&profile)?;
    let archive = output(Command::new("nix").args([
        "--extra-experimental-features",
        "nix-command flakes",
        "flake",
        "archive",
        "--json",
        "--dry-run",
        "--no-write-lock-file",
        flake,
    ]))
    .await?;
    let mut roots = farm::archive_paths(&archive)?;
    roots.push(env.to_string_lossy().into_owned());
    let mut args = vec![
        "--extra-experimental-features".to_owned(),
        "nix-command".to_owned(),
        "path-info".to_owned(),
        "--recursive".to_owned(),
    ];
    args.extend(roots);
    let closure: BTreeSet<String> = output(Command::new("nix").args(&args))
        .await?
        .lines()
        .map(str::to_owned)
        .collect();
    for p in &closure {
        if !farm::is_store_path(p) {
            bail!("nix path-info returned {p:?}, not a store path");
        }
    }
    let key = farm::key(closure.iter().map(String::as_str));

    let _guard = LOCK.lock().await;
    let dir = cfg.farms_dir().join(&key);
    if !dir.join(COMPLETE).exists() {
        build(cfg, &dir, &closure, &rc).await?;
    }
    std::fs::write(dir.join(LAST_USED), now().to_string())?;

    let vm_store = vm_store_dir(cfg, &rec.name);
    std::fs::create_dir_all(&vm_store)?;
    let snap = vm_store.join(repo);
    if snap.exists() {
        delete_subvolume(&snap).await?;
    }
    output(Command::new("btrfs").args([
        "subvolume",
        "snapshot",
        "-r",
        &dir.to_string_lossy(),
        &snap.to_string_lossy(),
    ]))
    .await?;
    Ok(key)
}

async fn build(cfg: &Config, dir: &Path, closure: &BTreeSet<String>, rc: &str) -> Result<()> {
    std::fs::create_dir_all(cfg.farms_dir())?;
    let tmp = dir.with_extension("tmp");
    if tmp.exists() {
        delete_subvolume(&tmp).await?;
    }
    output(Command::new("btrfs").args(["subvolume", "create", &tmp.to_string_lossy()])).await?;
    let store = tmp.join("store");
    std::fs::create_dir(&store)?;
    // One cp for all paths: the paths are read-only, cp keeps modes.
    let mut args = vec!["-a".to_owned(), "--reflink=always".to_owned()];
    args.extend(closure.iter().cloned());
    args.push(store.to_string_lossy().into_owned());
    output(Command::new("cp").args(&args))
        .await
        .context("cannot copy the closure with reflinks")?;
    let mut dump = vec!["--dump-db".to_owned()];
    dump.extend(closure.iter().cloned());
    let reg = output(Command::new("nix-store").args(&dump)).await?;
    std::fs::write(tmp.join("registration"), reg)?;
    std::fs::write(tmp.join("devenv.sh"), rc)?;
    std::fs::write(tmp.join(COMPLETE), "")?;
    if dir.exists() {
        delete_subvolume(dir).await?;
    }
    std::fs::rename(&tmp, dir)?;
    Ok(())
}

async fn delete_subvolume(p: &Path) -> Result<()> {
    output(Command::new("btrfs").args(["subvolume", "delete", &p.to_string_lossy()])).await?;
    Ok(())
}

/// Deletes the farm snapshots and the GC roots of a VM.
pub async fn release(cfg: &Config, vm: &str) -> Result<()> {
    let dir = vm_store_dir(cfg, vm);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries {
            delete_subvolume(&e?.path()).await?;
        }
    }
    let gcroots = cfg.state_dir.join("gcroots");
    if let Ok(entries) = std::fs::read_dir(&gcroots) {
        for e in entries {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{vm}-")) {
                std::fs::remove_file(e.path())?;
            }
        }
    }
    Ok(())
}

/// Fails unless the store and the state directory are on one btrfs
/// filesystem: farms are reflink copies of store paths.
pub async fn check_layout(cfg: &Config) -> Result<()> {
    async fn fs(dir: &Path) -> Result<String> {
        output(
            Command::new("findmnt")
                .args(["--noheadings", "--output", "FSTYPE,UUID", "--target"])
                .arg(dir),
        )
        .await
    }
    let store = fs(Path::new("/nix/store")).await?;
    let state = fs(&cfg.state_dir).await?;
    if let Some(problem) = farm::layout_problem(&store, &state) {
        bail!("{problem}");
    }
    Ok(())
}

/// Deletes farms that no VM used for the retention period.
pub async fn collect_garbage(cfg: &Config, in_use: &BTreeSet<String>) -> Result<Vec<String>> {
    let _guard = LOCK.lock().await;
    let mut farms = Vec::new();
    let entries = match std::fs::read_dir(cfg.farms_dir()) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    for e in entries {
        let e = e?;
        let key = e.file_name().to_string_lossy().into_owned();
        let last_used = std::fs::read_to_string(e.path().join(LAST_USED))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        farms.push(FarmUse { key, last_used });
    }
    let retention = cfg.farm_retention_days * 86400;
    let mut deleted = Vec::new();
    for key in farm::expired(&farms, in_use, now(), retention) {
        delete_subvolume(&cfg.farms_dir().join(key)).await?;
        deleted.push(key.to_owned());
    }
    Ok(deleted)
}
