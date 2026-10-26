//! The records of the VMs of this member, in `<state>/vms/<vm>/vm.json`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use hvw_core::policy::Grant;
use serde::{Deserialize, Serialize};

use crate::config::{Config, MAX_SLOTS};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRecord {
    pub name: String,
    pub policy: String,
    pub commit: String,
    pub stash: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmRecord {
    pub name: String,
    /// The peer name of the client that owns the VM.
    pub client: String,
    pub slot: u16,
    pub repos: Vec<RepoRecord>,
    /// True once the VM runs and holds its repos.
    pub launched: bool,
    /// Grants of the user with scope VM.
    pub grants: Vec<Grant>,
    /// The farms whose snapshots the VM uses. The garbage collector keeps
    /// them.
    #[serde(default)]
    pub farms: Vec<String>,
}

impl VmRecord {
    pub fn policies(&self) -> Vec<String> {
        let mut p: Vec<String> = self.repos.iter().map(|r| r.policy.clone()).collect();
        p.sort();
        p.dedup();
        p
    }

    pub fn repo(&self, name: &str) -> Option<&RepoRecord> {
        self.repos.iter().find(|r| r.name == name)
    }
}

pub fn load(cfg: &Config, vm: &str) -> Result<Option<VmRecord>> {
    let path = cfg.vm_dir(vm).join("vm.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            Ok(Some(serde_json::from_str(&text).with_context(|| {
                format!("{} is not valid", path.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save(cfg: &Config, rec: &VmRecord) -> Result<()> {
    let dir = cfg.vm_dir(&rec.name);
    std::fs::create_dir_all(&dir)?;
    write_atomic(&dir.join("vm.json"), &serde_json::to_vec_pretty(rec)?)
}

pub fn all(cfg: &Config) -> Result<Vec<VmRecord>> {
    let mut out = Vec::new();
    let dir = cfg.vms_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for e in entries {
        let name = e?.file_name().to_string_lossy().into_owned();
        if let Some(rec) = load(cfg, &name)? {
            out.push(rec);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The lowest slot that no VM uses and that has an address.
pub fn free_slot(cfg: &Config, records: &[VmRecord]) -> Result<u16> {
    for slot in 0..MAX_SLOTS {
        if records.iter().all(|r| r.slot != slot) {
            if cfg.vm_address(slot).is_err() {
                break;
            }
            return Ok(slot);
        }
    }
    bail!("this member has no free VM slot")
}

pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
