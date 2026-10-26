//! The link configuration: the key file, the relay mode and the peer
//! allowlist. The clan service writes it as JSON.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum Relay {
    /// The public n0 relays and address lookup through n0 DNS.
    #[default]
    Default,
    /// No relay and no address lookup: peers need direct addresses.
    Disabled,
    /// Self-hosted relays.
    Custom { urls: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    /// The machine name, for example `saturn`.
    pub name: String,
    /// The endpoint id: 64 hex digits.
    pub id: String,
    /// Direct addresses. Empty: find the peer through the relay.
    #[serde(default)]
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub secret_key_file: PathBuf,
    /// The UDP address to bind. Unset: any port on all addresses.
    #[serde(default)]
    pub bind: Option<SocketAddr>,
    #[serde(default)]
    pub relay: Relay,
    pub peers: Vec<Peer>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read the link configuration {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("the link configuration {} is not valid", path.display()))
    }
}

/// The peer allowlist, indexed by id and by name.
#[derive(Debug, Clone, Default)]
pub struct Peers {
    by_id: BTreeMap<String, Peer>,
    names: BTreeMap<String, String>,
}

fn is_hex_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Peers {
    pub fn new(peers: Vec<Peer>) -> Result<Self> {
        let mut out = Self::default();
        for mut p in peers {
            p.id = p.id.to_ascii_lowercase();
            if !is_hex_id(&p.id) {
                bail!("peer {}: the id {:?} is not 64 hex digits", p.name, p.id);
            }
            if p.name.is_empty() {
                bail!("peer {}: the name is empty", p.id);
            }
            if out.names.insert(p.name.clone(), p.id.clone()).is_some() {
                bail!("peer name {} occurs more than once", p.name);
            }
            if let Some(old) = out.by_id.insert(p.id.clone(), p.clone()) {
                bail!("peers {} and {} have the same id", old.name, p.name);
            }
        }
        Ok(out)
    }

    pub fn by_id(&self, id: &str) -> Option<&Peer> {
        self.by_id.get(&id.to_ascii_lowercase())
    }

    pub fn by_name(&self, name: &str) -> Option<&Peer> {
        self.by_id.get(self.names.get(name)?)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "8f0b1fb1e7b41d1d0ac6a9a5f7c1d8c4f5d4e2b8a0c1b2d3e4f5a6b7c8d9e0f1";
    const B: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    fn peer(name: &str, id: &str) -> Peer {
        Peer {
            name: name.into(),
            id: id.into(),
            addrs: vec![],
        }
    }

    #[test]
    fn lookup() {
        let peers = Peers::new(vec![peer("saturn", &A.to_uppercase()), peer("m1", B)]).unwrap();
        assert_eq!(peers.by_id(A).unwrap().name, "saturn");
        assert_eq!(peers.by_name("m1").unwrap().id, B);
        assert!(peers.by_id("ff").is_none());
        assert!(peers.by_name("pandora").is_none());
        assert_eq!(peers.names().collect::<Vec<_>>(), ["m1", "saturn"]);
    }

    #[test]
    fn rejects_bad_allowlists() {
        assert!(Peers::new(vec![peer("a", "xyz")]).is_err());
        assert!(Peers::new(vec![peer("", A)]).is_err());
        assert!(Peers::new(vec![peer("a", A), peer("a", B)]).is_err());
        assert!(Peers::new(vec![peer("a", A), peer("b", A)]).is_err());
    }

    #[test]
    fn parses_config() {
        let cfg: Config = serde_json::from_str(&format!(
            r#"{{"secret_key_file": "/run/k", "bind": "0.0.0.0:7345",
                "relay": {{"mode": "disabled"}},
                "peers": [{{"name": "m1", "id": "{A}", "addrs": ["192.168.1.2:7345"]}}]}}"#
        ))
        .unwrap();
        assert_eq!(cfg.relay, Relay::Disabled);
        assert_eq!(cfg.peers[0].addrs[0].port(), 7345);
        let default: Config =
            serde_json::from_str(r#"{"secret_key_file": "/k", "peers": []}"#).unwrap();
        assert_eq!(default.relay, Relay::Default);
        assert!(
            serde_json::from_str::<Config>(r#"{"secret_key_file": "/k", "peers": [], "x": 1}"#)
                .is_err()
        );
    }
}
