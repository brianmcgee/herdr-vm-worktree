//! The configuration of `hvw-agent`. The NixOS module writes it as JSON.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use hvw_core::policy::Policy;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The peer name of this member.
    pub name: String,
    pub link: PathBuf,
    pub state_dir: PathBuf,
    /// The policies, by name. JSON files written by Nix.
    pub policies_dir: PathBuf,
    pub incus_socket: PathBuf,
    /// The alias of the base image in Incus.
    pub image: String,
    pub storage_pool: String,
    pub bridge: Bridge,
    pub broker_port: u16,
    /// The first port of the five port ranges of the gatekeepers. Each
    /// range has 1000 ports, one for each VM slot.
    pub gatekeeper_port_base: u16,
    pub vm: VmConfig,
    /// Farms that no VM used for this many days are deleted.
    pub farm_retention_days: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    pub name: String,
    pub address: Ipv4Addr,
    pub prefix_length: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmConfig {
    pub cpus: u32,
    pub memory: String,
    pub user: String,
    pub uid: u32,
    pub gid: u32,
    /// The command of the agent harness.
    pub harness: Vec<String>,
}

/// The largest number of VMs on one member. The gatekeeper port ranges
/// and the addresses on the bridge limit it.
pub const MAX_SLOTS: u16 = 1000;

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid", path.display()))?;
        if !(8..=30).contains(&cfg.bridge.prefix_length) {
            bail!(
                "bridge prefix length {} is out of range",
                cfg.bridge.prefix_length
            );
        }
        if u32::from(cfg.gatekeeper_port_base) + 5 * u32::from(MAX_SLOTS) > 65535 {
            bail!(
                "gatekeeper_port_base {} is too high",
                cfg.gatekeeper_port_base
            );
        }
        Ok(cfg)
    }

    pub fn policy(&self, name: &str) -> Result<Policy> {
        if !hvw_core::manifest::valid_name(name) {
            bail!("policy name {name:?} is not valid");
        }
        let path = self.policies_dir.join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("no policy {name} on this member"))?;
        serde_json::from_str(&text).with_context(|| format!("policy {name} is not valid"))
    }

    /// The ports of the gatekeeper of a slot.
    pub fn ports(&self, slot: u16) -> Ports {
        let p = |k: u16| self.gatekeeper_port_base + k * MAX_SLOTS + slot;
        Ports {
            tunnel: p(0),
            http: p(1),
            https: p(2),
            metrics: p(3),
            management: p(4),
        }
    }

    /// The address of the VM of a slot. Slots start after the member
    /// address, so slot 0 gets the next address.
    pub fn vm_address(&self, slot: u16) -> Result<Ipv4Addr> {
        let bits = u32::from(self.bridge.address);
        let mask = u32::MAX << (32 - self.bridge.prefix_length);
        let host = bits + 1 + u32::from(slot);
        if host & mask != bits & mask || host & !mask == !mask {
            bail!("slot {slot} has no address in the bridge network");
        }
        Ok(Ipv4Addr::from(host))
    }

    pub fn vms_dir(&self) -> PathBuf {
        self.state_dir.join("vms")
    }

    pub fn vm_dir(&self, vm: &str) -> PathBuf {
        self.vms_dir().join(vm)
    }

    pub fn farms_dir(&self) -> PathBuf {
        self.state_dir.join("farms")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub tunnel: u16,
    pub http: u16,
    pub https: u16,
    pub metrics: u16,
    pub management: u16,
}

/// The environment file of a VM: `KEY=value` lines with shell quoting.
pub fn env_file(vars: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (k, v) in vars {
        out.push_str(k);
        out.push('=');
        out.push('\'');
        out.push_str(&v.replace('\'', "'\\''"));
        out.push_str("'\n");
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn cfg(prefix: u8) -> Config {
        Config {
            name: "m1".into(),
            link: "/l".into(),
            state_dir: "/var/lib/hvw".into(),
            policies_dir: "/etc/hvw/policies".into(),
            incus_socket: "/s".into(),
            image: "hvw".into(),
            storage_pool: "hvw".into(),
            bridge: Bridge {
                name: "hvwbr0".into(),
                address: "10.250.0.1".parse().unwrap(),
                prefix_length: prefix,
            },
            broker_port: 7300,
            gatekeeper_port_base: 20000,
            vm: VmConfig {
                cpus: 2,
                memory: "2GiB".into(),
                user: "agent".into(),
                uid: 1000,
                gid: 100,
                harness: vec!["bash".into()],
            },
            farm_retention_days: 14,
        }
    }

    #[test]
    fn ports_do_not_overlap() {
        let c = cfg(24);
        assert_eq!(
            c.ports(0),
            Ports {
                tunnel: 20000,
                http: 21000,
                https: 22000,
                metrics: 23000,
                management: 24000
            }
        );
        assert_eq!(c.ports(999).management, 24999);
    }

    #[test]
    fn addresses_stay_in_the_bridge_network() {
        let c = cfg(24);
        assert_eq!(c.vm_address(0).unwrap().to_string(), "10.250.0.2");
        assert_eq!(c.vm_address(252).unwrap().to_string(), "10.250.0.254");
        // .255 is the broadcast address.
        assert!(c.vm_address(253).is_err());
        assert_eq!(cfg(30).vm_address(0).unwrap().to_string(), "10.250.0.2");
        assert!(cfg(30).vm_address(1).is_err());
    }

    #[test]
    fn env_quoting() {
        let mut v = BTreeMap::new();
        v.insert("A".into(), "it's".into());
        v.insert("B".into(), "$x".into());
        assert_eq!(env_file(&v), "A='it'\\''s'\nB='$x'\n");
    }
}
