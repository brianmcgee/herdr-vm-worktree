//! The gatekeeper of a VM: an `iron-proxy@<vm>` unit with its own CA,
//! configuration and Incus network ACL.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hvw_core::policy::{DEFAULT_DENY_CIDRS, Effective, Gatekeeper, Policy};
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::process::Command;

use crate::config::Config;
use crate::incus::Incus;
use crate::state::{VmRecord, write_atomic};

/// The user of the `iron-proxy@` units.
pub const USER: &str = "hvw-gatekeeper";
const KEY_ENV: &str = "IRON_MANAGEMENT_API_KEY";
/// How long the CA of a VM is valid. A VM lives shorter than this.
const CA_HOURS: u32 = 24 * 14;

pub fn dir(cfg: &Config, vm: &str) -> PathBuf {
    cfg.vm_dir(vm).join("gatekeeper")
}

pub fn acl_name(vm: &str) -> String {
    format!("hvw-{vm}")
}

/// The policies of a VM, merged, with the grants of the VM. A grant of the
/// user lives as long as its VM. Only a policy in Nix lasts longer.
pub fn effective(cfg: &Config, rec: &VmRecord) -> Result<Effective> {
    let policies: Vec<Policy> = rec
        .policies()
        .iter()
        .map(|p| cfg.policy(p))
        .collect::<Result<_>>()?;
    let mut eff = Effective::merge(&policies)?;
    for g in &rec.grants {
        eff.grant(g);
    }
    Ok(eff)
}

fn spec(cfg: &Config, rec: &VmRecord) -> Gatekeeper {
    let d = dir(cfg, &rec.name);
    let ports = cfg.ports(rec.slot);
    let mut deny: Vec<String> = DEFAULT_DENY_CIDRS.iter().map(|s| (*s).to_owned()).collect();
    // The bridge network: the gatekeeper must not reach the member or
    // other VMs on behalf of a VM.
    deny.push(format!(
        "{}/{}",
        cfg.bridge.address, cfg.bridge.prefix_length
    ));
    Gatekeeper {
        bridge_address: cfg.bridge.address.to_string(),
        tunnel_port: ports.tunnel,
        http_port: ports.http,
        https_port: ports.https,
        metrics_port: ports.metrics,
        management_port: ports.management,
        management_key_env: KEY_ENV.to_owned(),
        ca_cert: d.join("ca/ca.crt").to_string_lossy().into_owned(),
        ca_key: d.join("ca/ca.key").to_string_lossy().into_owned(),
        credentials_dir: format!("/run/credentials/iron-proxy@{}.service", rec.name),
        deny_cidrs: deny,
    }
}

async fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().await.context("cannot run a command")?;
    if !out.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd.as_std(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The CA certificate of the gatekeeper of a VM.
pub fn ca_cert(cfg: &Config, vm: &str) -> PathBuf {
    dir(cfg, vm).join("ca/ca.crt")
}

/// Writes the CA, the management key and the configuration, and creates
/// the network ACL of the VM.
pub async fn prepare(cfg: &Config, incus: &Incus, rec: &VmRecord) -> Result<()> {
    let d = dir(cfg, &rec.name);
    std::fs::create_dir_all(&d)?;
    if !d.join("ca/ca.crt").exists() {
        let _ = std::fs::remove_dir_all(d.join("ca"));
        run(Command::new("iron-proxy").args([
            "generate-ca",
            "-outdir",
            &d.join("ca").to_string_lossy(),
            "-name",
            &format!("hvw gatekeeper {}", rec.name),
            "-expiry-hours",
            &CA_HOURS.to_string(),
            "-alg",
            "ed25519",
        ]))
        .await?;
    }
    if !d.join("env").exists() {
        let key: [u8; 32] = rand_bytes()?;
        write_atomic(
            &d.join("env"),
            format!("{KEY_ENV}={}\n", data_encoding::HEXLOWER.encode(&key)).as_bytes(),
        )?;
    }
    write_config(cfg, rec)?;
    run(Command::new("chown").args(["-R", &format!("{USER}:{USER}"), &d.to_string_lossy()]))
        .await?;
    run(Command::new("chmod").args(["-R", "go-rwx", &d.to_string_lossy()])).await?;

    let ports = cfg.ports(rec.slot);
    incus
        .put_acl(
            &acl_name(&rec.name),
            json!([{
                "action": "allow",
                "state": "enabled",
                "protocol": "tcp",
                "destination": format!("{}/32", cfg.bridge.address),
                "destination_port": format!("{},{}", ports.tunnel, cfg.broker_port),
                "description": "gatekeeper and broker",
            }]),
        )
        .await
}

fn rand_bytes() -> Result<[u8; 32]> {
    use std::io::Read;
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

fn write_config(cfg: &Config, rec: &VmRecord) -> Result<()> {
    let eff = effective(cfg, rec)?;
    let config = spec(cfg, rec).config(&eff);
    write_atomic(
        &dir(cfg, &rec.name).join("config.json"),
        &serde_json::to_vec_pretty(&config)?,
    )
}

/// Starts the gatekeeper and waits until its tunnel port accepts
/// connections.
pub async fn start(cfg: &Config, rec: &VmRecord) -> Result<()> {
    run(Command::new("systemctl").args(["start", &format!("iron-proxy@{}.service", rec.name)]))
        .await?;
    let addr = (cfg.bridge.address, cfg.ports(rec.slot).tunnel);
    for _ in 0..100 {
        if TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "the gatekeeper of {} does not listen on {}:{}",
        rec.name,
        addr.0,
        addr.1
    )
}

/// Writes the configuration again and makes the gatekeeper load it. Open
/// connections stay open.
pub async fn reload(cfg: &Config, rec: &VmRecord) -> Result<()> {
    write_config(cfg, rec)?;
    let d = dir(cfg, &rec.name);
    run(Command::new("chown")
        .arg(format!("{USER}:{USER}"))
        .arg(d.join("config.json")))
    .await?;
    let env = std::fs::read_to_string(d.join("env"))?;
    let key = env
        .trim()
        .strip_prefix(&format!("{KEY_ENV}="))
        .context("the management key file is not valid")?;
    let stream = TcpStream::connect(("127.0.0.1", cfg.ports(rec.slot).management)).await?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(conn);
    let req = Request::post("/v1/reload")
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {key}"))
        .body(Full::new(Bytes::new()))?;
    let res = sender.send_request(req).await?;
    let status = res.status();
    let body = res.into_body().collect().await?.to_bytes();
    if !status.is_success() {
        bail!(
            "the gatekeeper refused the reload: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(())
}

pub async fn stop(vm: &str) -> Result<()> {
    run(Command::new("systemctl").args(["stop", &format!("iron-proxy@{vm}.service")])).await
}

/// The log lines of the gatekeeper of the last `minutes` minutes.
pub async fn recent_log(vm: &str, minutes: u32) -> Result<Vec<String>> {
    let out = Command::new("journalctl")
        .args([
            "--unit",
            &format!("iron-proxy@{vm}.service"),
            "--output",
            "cat",
            "--no-pager",
            "--since",
            &format!("-{minutes}min"),
        ])
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "journalctl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RepoRecord;
    use hvw_core::policy::Grant;

    fn record(name: &str, grants: Vec<Grant>) -> VmRecord {
        VmRecord {
            name: name.into(),
            client: "laptop".into(),
            slot: 0,
            repos: vec![RepoRecord {
                name: "repo".into(),
                policy: "web".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                stash: None,
                branch: Some("main".into()),
            }],
            launched: true,
            grants,
            farms: vec![],
        }
    }

    #[test]
    fn a_grant_stays_in_its_vm() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::tests::cfg(24);
        cfg.policies_dir = dir.path().join("policies");
        cfg.state_dir = dir.path().join("state");
        std::fs::create_dir_all(&cfg.policies_dir).unwrap();
        std::fs::write(
            cfg.policies_dir.join("web.json"),
            r#"{"domains": ["github.com"]}"#,
        )
        .unwrap();
        // An older agent kept policy grants here, for all VMs of a policy.
        let old = cfg.state_dir.join("policy-grants");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("web.json"), r#"[{"host": "api.example.com"}]"#).unwrap();

        let grant = Grant {
            host: "api.example.com".into(),
            methods: vec![],
            paths: vec![],
        };
        let a = effective(&cfg, &record("hvw-repo-1a2b3c4d", vec![grant])).unwrap();
        let b = effective(&cfg, &record("hvw-repo-5e6f7a8b", vec![])).unwrap();
        assert!(a.domains.contains("api.example.com"));
        assert_eq!(b.domains, ["github.com".to_owned()].into());
    }
}
