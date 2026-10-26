//! The state of the client, as JSON files in the state directory:
//!
//! - `vms/<vm>.json`: one [`VmEntry`] for each VM of this client.
//! - `pending.json`: the egress requests that wait for a decision.
//!   `hvw-watch` writes it, `hvw-review` reads it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hvw_core::vm::valid_vm_name;
use hvw_link::Pending;
use serde::{Deserialize, Serialize};

/// A VM of this client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmEntry {
    pub vm: String,
    /// The peer name of the member that runs the VM.
    pub member: String,
    /// The worktree on the client that holds the workspace manifest.
    pub worktree: PathBuf,
    /// The herdr workspace of the worktree, when herdr made it.
    #[serde(default)]
    pub workspace: Option<String>,
    /// The herdr pane that runs `hvw attach`.
    #[serde(default)]
    pub pane: Option<String>,
}

/// The files of the client state, below one directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The store in the XDG state directory.
    pub fn open() -> Result<Self> {
        Ok(Self::new(crate::paths::state_dir()?))
    }

    fn vm_file(&self, vm: &str) -> Result<PathBuf> {
        anyhow::ensure!(valid_vm_name(vm), "{vm:?} is not a valid VM name");
        Ok(self.dir.join("vms").join(format!("{vm}.json")))
    }

    pub fn pending_file(&self) -> PathBuf {
        self.dir.join("pending.json")
    }

    pub fn load(&self, vm: &str) -> Result<Option<VmEntry>> {
        read_json(&self.vm_file(vm)?)
    }

    pub fn save(&self, entry: &VmEntry) -> Result<()> {
        write_json(&self.vm_file(&entry.vm)?, entry)
    }

    pub fn remove(&self, vm: &str) -> Result<()> {
        match std::fs::remove_file(self.vm_file(vm)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// All VMs of this client, sorted by name.
    pub fn all(&self) -> Result<Vec<VmEntry>> {
        let dir = self.dir.join("vms");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for e in entries {
            let path = e?.path();
            if path.extension().is_some_and(|x| x == "json")
                && let Some(entry) = read_json::<VmEntry>(&path)?
            {
                out.push(entry);
            }
        }
        out.sort_by(|a, b| a.vm.cmp(&b.vm));
        Ok(out)
    }

    pub fn pending(&self) -> Result<Vec<Pending>> {
        Ok(read_json(&self.pending_file())?.unwrap_or_default())
    }

    pub fn save_pending(&self, pending: &[Pending]) -> Result<()> {
        write_json(&self.pending_file(), &pending)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(b) => {
            Ok(Some(serde_json::from_slice(&b).with_context(|| {
                format!("{} is not valid", path.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Writes a file through a temporary file and a rename, so that a reader
/// never sees half a file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path.parent().context("a state file has no directory")?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path().to_owned());
        assert!(store.all().unwrap().is_empty());
        assert_eq!(store.load("hvw-a-1").unwrap(), None);
        let a = VmEntry {
            vm: "hvw-b-1".into(),
            member: "m1".into(),
            worktree: "/w/b".into(),
            workspace: Some("w2".into()),
            pane: Some("w2:p1".into()),
        };
        let b = VmEntry {
            vm: "hvw-a-1".into(),
            workspace: None,
            pane: None,
            ..a.clone()
        };
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        assert_eq!(store.all().unwrap(), vec![b.clone(), a.clone()]);
        store.remove("hvw-a-1").unwrap();
        store.remove("hvw-a-1").unwrap();
        assert_eq!(store.all().unwrap(), vec![a]);
        assert!(store.load("../x").is_err());
    }
}
