//! The control plane of herdr-vm-worktree. Every link between a client
//! and a member is an iroh connection on one ALPN, [`ALPN`]. Each request
//! uses its own bidirectional stream: the client writes one [`Request`]
//! frame, then the stream continues as the request kind says.

pub mod config;
pub mod frame;
pub mod proto;

pub use config::{Config, Peer, Peers, Relay};
pub use proto::*;

use std::path::Path;

use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey, TransportAddr};

/// The ALPN of the protocol. A change that breaks old peers needs a new
/// ALPN.
pub const ALPN: &[u8] = b"hvw/1";

/// Reads an iroh secret key: 64 hex digits, with optional white space.
pub fn read_secret_key(path: &Path) -> Result<SecretKey> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the secret key {}", path.display()))?;
    text.trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("the secret key {} is not valid: {e}", path.display()))
}

/// Makes a new secret key. Returns the key file text and the endpoint id.
pub fn generate_secret_key() -> (String, String) {
    let key = SecretKey::generate();
    let text = data_encoding::HEXLOWER.encode(&key.to_bytes());
    (text, key.public().to_string())
}

/// An iroh endpoint that talks only to the peers in its configuration.
pub struct Link {
    endpoint: Endpoint,
    peers: Peers,
}

impl Link {
    pub async fn bind(cfg: &Config) -> Result<Self> {
        let peers = Peers::new(cfg.peers.clone())?;
        let key = read_secret_key(&cfg.secret_key_file)?;
        let mut builder = match &cfg.relay {
            // Without a relay there is no use for address lookup through
            // the n0 DNS servers either: peers have direct addresses.
            Relay::Disabled => Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled),
            Relay::Default => Endpoint::builder(presets::N0),
            Relay::Custom { urls } => {
                let urls = urls
                    .iter()
                    .map(|u| {
                        u.parse::<RelayUrl>()
                            .with_context(|| format!("bad relay URL {u}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Endpoint::builder(presets::N0).relay_mode(RelayMode::custom(urls))
            }
        };
        builder = builder.secret_key(key).alpns(vec![ALPN.to_vec()]);
        if let Some(addr) = cfg.bind {
            builder = builder
                .clear_ip_transports()
                .bind_addr(addr)
                .map_err(|e| anyhow::anyhow!("cannot bind {addr}: {e}"))?;
        }
        let endpoint = builder
            .bind()
            .await
            .context("cannot bind the iroh endpoint")?;
        Ok(Self { endpoint, peers })
    }

    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    pub fn peers(&self) -> &Peers {
        &self.peers
    }

    /// Waits for the next connection from a known peer. Connections from
    /// unknown peers are closed and logged. Returns `None` when the
    /// endpoint is closed.
    pub async fn accept(&self) -> Option<(Peer, Connection)> {
        loop {
            let incoming = self.endpoint.accept().await?;
            let accepting = match incoming.accept() {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("cannot accept a connection: {e}");
                    continue;
                }
            };
            let conn = match accepting.await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("handshake failed: {e}");
                    continue;
                }
            };
            let id = conn.remote_id();
            match self.peers.by_id(&id.to_string()) {
                Some(peer) => return Some((peer.clone(), conn)),
                None => {
                    tracing::warn!(peer = %id, "refused a connection from an unknown peer");
                    conn.close(1u8.into(), b"unknown peer");
                }
            }
        }
    }

    /// Connects to a peer by name.
    pub async fn connect(&self, name: &str) -> Result<Connection> {
        let Some(peer) = self.peers.by_name(name) else {
            bail!("no peer {name:?} in the configuration");
        };
        let id = peer
            .id
            .parse()
            .map_err(|e| anyhow::anyhow!("peer {name}: bad id: {e}"))?;
        let addr = EndpointAddr::from_parts(id, peer.addrs.iter().copied().map(TransportAddr::Ip));
        self.endpoint
            .connect(addr, ALPN)
            .await
            .with_context(|| format!("cannot connect to {name}"))
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

/// Reads replies until `Ok` or `Error`. Calls `progress` for each progress
/// line. An `Error` reply becomes an error.
pub async fn read_reply<R: tokio::io::AsyncRead + Unpin>(
    recv: &mut R,
    mut progress: impl FnMut(&str),
) -> Result<serde_json::Value> {
    loop {
        match frame::read::<_, Reply>(recv).await? {
            Some(Reply::Progress(p)) => progress(&p),
            Some(Reply::Ok(v)) => return Ok(v),
            Some(Reply::Error(e)) => bail!("{e}"),
            None => bail!("the peer closed the stream without an answer"),
        }
    }
}
