//! The local socket of `hvw-watch`. A client connects, writes one
//! [`LocalRequest`] frame and reads one [`Reply`]:
//!
//! - `Members`: the reply holds the peer names of the members.
//! - `Open`: after `Ok`, the socket is a stream to the member, as
//!   [`hvw_link`] describes. A half close of the socket finishes the send
//!   side of the stream.

use anyhow::{Context, Result};
use hvw_link::{Reply, Request, VmInfo, frame, read_reply};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalRequest {
    Members,
    Open { member: String },
}

/// A stream to a member, through `hvw-watch`.
pub struct Stream {
    pub recv: OwnedReadHalf,
    pub send: OwnedWriteHalf,
}

async fn connect(req: &LocalRequest) -> Result<UnixStream> {
    let path = crate::paths::socket()?;
    let mut sock = UnixStream::connect(&path).await.with_context(|| {
        format!(
            "cannot connect to hvw-watch at {}: start the hvw-watch user service",
            path.display()
        )
    })?;
    frame::write(&mut sock, req).await?;
    Ok(sock)
}

/// The peer names of the members.
pub async fn members() -> Result<Vec<String>> {
    let mut sock = connect(&LocalRequest::Members).await?;
    decode(read_reply(&mut sock, |_| {}).await?)
}

/// Opens a stream to a member and writes one request.
pub async fn open(member: &str, req: &Request) -> Result<Stream> {
    let mut sock = connect(&LocalRequest::Open {
        member: member.to_owned(),
    })
    .await?;
    read_reply(&mut sock, |_| {})
        .await
        .with_context(|| format!("cannot reach the member {member}"))?;
    frame::write(&mut sock, req).await?;
    let (recv, send) = sock.into_split();
    Ok(Stream { recv, send })
}

/// The VMs of this client on each member. The result of each member is
/// separate, so that one member that does not answer hides no other.
pub async fn list_all() -> Result<Vec<(String, Result<Vec<VmInfo>>)>> {
    let mut tasks = Vec::new();
    for m in members().await? {
        tasks.push(tokio::spawn(async move {
            let r = call(&m, &Request::List, None, |_| {})
                .await
                .and_then(decode::<Vec<VmInfo>>);
            (m, r)
        }));
    }
    let mut out = Vec::new();
    for t in tasks {
        out.push(t.await?);
    }
    Ok(out)
}

/// Sends a request with an optional body and reads the answer.
pub async fn call(
    member: &str,
    req: &Request,
    body: Option<&[u8]>,
    progress: impl FnMut(&str),
) -> Result<Value> {
    let mut s = open(member, req).await?;
    if let Some(b) = body {
        s.send.write_all(b).await?;
    }
    s.send.shutdown().await?;
    read_reply(&mut s.recv, progress).await
}

/// Writes the answer to a local request.
pub async fn answer<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, r: Result<Value>) -> Result<()> {
    let reply = match r {
        Ok(v) => Reply::Ok(v),
        Err(e) => Reply::Error(format!("{e:#}")),
    };
    frame::write(w, &reply).await
}

pub fn decode<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).context("the member sent an answer of the wrong form")
}
