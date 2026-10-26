//! Files of the client, after the XDG base directory specification.

use std::path::PathBuf;

use anyhow::{Result, bail};

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `$XDG_STATE_HOME/hvw`, by default `~/.local/state/hvw`. It holds the
/// VMs of the client and the pending egress requests.
pub fn state_dir() -> Result<PathBuf> {
    if let Some(d) = env_dir("XDG_STATE_HOME") {
        return Ok(d.join("hvw"));
    }
    match env_dir("HOME") {
        Some(h) => Ok(h.join(".local/state/hvw")),
        None => bail!("set XDG_STATE_HOME or HOME"),
    }
}

/// The Unix socket of `hvw-watch`: `$HVW_SOCKET`, else
/// `$XDG_RUNTIME_DIR/hvw/watch.sock`.
pub fn socket() -> Result<PathBuf> {
    if let Some(s) = env_dir("HVW_SOCKET") {
        return Ok(s);
    }
    match env_dir("XDG_RUNTIME_DIR") {
        Some(d) => Ok(d.join("hvw/watch.sock")),
        None => bail!("set XDG_RUNTIME_DIR or HVW_SOCKET"),
    }
}

/// The link configuration of the client: `$HVW_LINK`, else
/// `/etc/hvw/link.json`, which the NixOS module writes.
pub fn link_config() -> PathBuf {
    env_dir("HVW_LINK").unwrap_or_else(|| PathBuf::from("/etc/hvw/link.json"))
}
