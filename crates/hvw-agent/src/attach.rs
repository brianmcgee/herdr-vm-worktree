//! A terminal on the agent harness in a VM: `incus exec -t` on a pty of
//! the member, carried over the control plane in chunks.

use anyhow::{Context, Result};
use hvw_link::frame::{Chunk, read_chunk, write_chunk};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::Config;
use crate::incus::Exec;
use crate::state::VmRecord;

pub async fn run<R, W>(
    cfg: &Config,
    rec: &VmRecord,
    cols: u16,
    rows: u16,
    mut recv: R,
    mut send: W,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let cwd = rec
        .repos
        .first()
        .map_or_else(|| "/work".to_owned(), |r| format!("/work/{}", r.name));
    // A login shell reads the environment of the VM (/run/hvw/env), then
    // runs the harness.
    let mut argv = vec!["bash", "-lc", "exec \"$@\"", "hvw-harness"];
    argv.extend(cfg.vm.harness.iter().map(String::as_str));
    let exec = Exec::new(&rec.name, &argv)
        .user(cfg.vm.uid, cfg.vm.gid, &format!("/home/{}", cfg.vm.user))
        .cwd(&cwd)
        .env("TERM", "xterm-256color")
        .tty();

    let (pty, pts) = pty_process::open().context("cannot open a pty")?;
    pty.resize(pty_process::Size::new(rows, cols))?;
    let mut child = pty_process::Command::new("incus")
        .args(exec.args())
        .kill_on_drop(true)
        .spawn(pts)
        .context("cannot run incus exec")?;
    let (mut pty_r, mut pty_w) = pty.into_split();

    // Client to pty. `gone` fires when the client stream ends.
    let (gone_tx, mut gone) = tokio::sync::oneshot::channel::<()>();
    let input = tokio::spawn(async move {
        while let Ok(Some(chunk)) = read_chunk(&mut recv).await {
            match chunk {
                Chunk::Data(d) => {
                    if pty_w.write_all(&d).await.is_err() {
                        break;
                    }
                }
                Chunk::Resize { cols, rows } => {
                    let _ = pty_w.resize(pty_process::Size::new(rows, cols));
                }
                Chunk::Exit(_) => break,
            }
        }
        let _ = gone_tx.send(());
    });

    // Pty to client, until the pty closes (the command exited) or the
    // client went away.
    let mut buf = vec![0u8; 16 * 1024];
    let mut client_gone = false;
    loop {
        tokio::select! {
            n = pty_r.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if write_chunk(&mut send, &Chunk::Data(buf[..n].to_vec())).await.is_err() {
                        client_gone = true;
                        break;
                    }
                }
            },
            _ = &mut gone => {
                client_gone = true;
                break;
            }
        }
    }
    if client_gone {
        child.kill().await.ok();
        return Ok(());
    }
    input.abort();
    let status = child.wait().await?;
    write_chunk(&mut send, &Chunk::Exit(status.code().unwrap_or(-1)))
        .await
        .ok();
    send.shutdown().await.ok();
    Ok(())
}
