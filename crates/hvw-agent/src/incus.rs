//! A small client of the Incus REST API on the local Unix socket.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::process::Command;

/// The PATH of commands in a VM. Incus does not use the PATH of NixOS.
pub const VM_PATH: &str = "/run/wrappers/bin:/run/current-system/sw/bin";

#[derive(Clone)]
pub struct Incus {
    socket: PathBuf,
}

impl Incus {
    pub fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_owned(),
        }
    }

    /// Sends one request and returns the `metadata` of the answer. Waits for
    /// the operation when the answer is asynchronous.
    pub async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let v = self.raw(method.clone(), path, body).await?;
        if v["type"] == "async" {
            let op = v["operation"]
                .as_str()
                .context("asynchronous answer without operation")?
                .to_owned();
            let done = self
                .raw(Method::GET, &format!("{op}/wait?timeout=-1"), None)
                .await?;
            let meta = &done["metadata"];
            if meta["status"] != "Success" {
                bail!(
                    "incus {method} {path}: {}",
                    meta["err"]
                        .as_str()
                        .filter(|e| !e.is_empty())
                        .unwrap_or("operation failed")
                );
            }
            return Ok(meta.clone());
        }
        Ok(v["metadata"].clone())
    }

    async fn raw(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("cannot connect to {}", self.socket.display()))?;
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("incus connection: {e}");
            }
        });
        let body = match body {
            Some(b) => Full::new(Bytes::from(serde_json::to_vec(&b)?)),
            None => Full::new(Bytes::new()),
        };
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header("host", "incus")
            .header("content-type", "application/json")
            .body(body)?;
        let res = sender.send_request(req).await?;
        let status = res.status();
        let bytes = res.into_body().collect().await?.to_bytes();
        let v: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("incus {method} {path}: answer is not JSON"))?;
        if !status.is_success() || v["type"] == "error" {
            bail!(
                "incus {method} {path}: {}",
                v["error"].as_str().unwrap_or(status.as_str())
            );
        }
        Ok(v)
    }

    pub async fn exists(&self, path: &str) -> Result<bool> {
        match self.raw(Method::GET, path, None).await {
            Ok(_) => Ok(true),
            Err(e) if e.to_string().contains("not found") => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub async fn create_instance(&self, spec: Value) -> Result<()> {
        self.call(Method::POST, "/1.0/instances", Some(spec))
            .await?;
        Ok(())
    }

    pub async fn set_state(&self, name: &str, action: &str) -> Result<()> {
        self.call(
            Method::PUT,
            &format!("/1.0/instances/{name}/state"),
            Some(json!({ "action": action, "force": true, "timeout": 30 })),
        )
        .await?;
        Ok(())
    }

    pub async fn delete_instance(&self, name: &str) -> Result<()> {
        let path = format!("/1.0/instances/{name}");
        if !self.exists(&path).await? {
            return Ok(());
        }
        let state = self
            .call(Method::GET, &format!("{path}/state"), None)
            .await?;
        if state["status"] != "Stopped" {
            // An ephemeral VM deletes itself when it stops.
            self.set_state(name, "stop").await?;
        }
        if self.exists(&path).await? {
            self.call(Method::DELETE, &path, None).await?;
        }
        Ok(())
    }

    /// The status of each instance, by name.
    pub async fn statuses(&self) -> Result<Vec<(String, String)>> {
        let list = self
            .call(Method::GET, "/1.0/instances?recursion=1", None)
            .await?;
        Ok(list
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| {
                        (
                            i["name"].as_str().unwrap_or_default().to_owned(),
                            i["status"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn put_acl(&self, name: &str, egress: Value) -> Result<()> {
        let path = format!("/1.0/network-acls/{name}");
        if self.exists(&path).await? {
            self.call(
                Method::PUT,
                &path,
                Some(json!({ "egress": egress, "ingress": [] })),
            )
            .await?;
        } else {
            self.call(
                Method::POST,
                "/1.0/network-acls",
                Some(json!({ "name": name, "egress": egress, "ingress": [] })),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn delete_acl(&self, name: &str) -> Result<()> {
        let path = format!("/1.0/network-acls/{name}");
        if self.exists(&path).await? {
            self.call(Method::DELETE, &path, None).await?;
        }
        Ok(())
    }
}

/// A command in a VM, run with the `incus` CLI.
pub struct Exec {
    vm: String,
    user: Option<(u32, u32)>,
    home: Option<String>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    tty: bool,
    argv: Vec<String>,
}

impl Exec {
    pub fn new(vm: &str, argv: &[&str]) -> Self {
        Self {
            vm: vm.to_owned(),
            user: None,
            home: None,
            cwd: None,
            env: vec![],
            tty: false,
            argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    pub fn shell(vm: &str, script: &str) -> Self {
        Self::new(vm, &["sh", "-euc", script])
    }

    pub fn user(mut self, uid: u32, gid: u32, home: &str) -> Self {
        self.user = Some((uid, gid));
        self.home = Some(home.to_owned());
        self
    }

    pub fn cwd(mut self, dir: &str) -> Self {
        self.cwd = Some(dir.to_owned());
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.to_owned(), v.to_owned()));
        self
    }

    pub fn tty(mut self) -> Self {
        self.tty = true;
        self
    }

    /// The arguments of `incus`.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec![
            "exec".to_owned(),
            if self.tty { "-t" } else { "-T" }.to_owned(),
        ];
        if let Some((uid, gid)) = self.user {
            a.extend([
                "--user".to_owned(),
                uid.to_string(),
                "--group".to_owned(),
                gid.to_string(),
            ]);
        }
        if let Some(cwd) = &self.cwd {
            a.extend(["--cwd".to_owned(), cwd.clone()]);
        }
        a.extend(["--env".to_owned(), format!("PATH={VM_PATH}")]);
        if let Some(home) = &self.home {
            a.extend(["--env".to_owned(), format!("HOME={home}")]);
        }
        for (k, v) in &self.env {
            a.extend(["--env".to_owned(), format!("{k}={v}")]);
        }
        a.push(self.vm.clone());
        a.push("--".to_owned());
        a.extend(self.argv.iter().cloned());
        a
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new("incus");
        c.args(self.args());
        c.kill_on_drop(true);
        c
    }

    /// Runs the command with `input` on stdin and returns its stdout.
    pub async fn run(&self, input: &[u8]) -> Result<Vec<u8>> {
        use tokio::io::AsyncWriteExt;
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run incus")?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        let input = input.to_vec();
        let writer = tokio::spawn(async move {
            let r = stdin.write_all(&input).await;
            drop(stdin);
            r
        });
        let out = child.wait_with_output().await?;
        writer.await??;
        if !out.status.success() {
            bail!(
                "{} in {} failed ({}): {}",
                self.argv.join(" "),
                self.vm,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }
}
