//! Framing on a stream. A frame is a 4-byte big-endian length and a JSON
//! value. A terminal session uses binary chunks instead (see [`Chunk`]).

use anyhow::{Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest frame that a peer accepts.
pub const MAX_FRAME: u32 = 16 << 20;

pub async fn write<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, value: &T) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let Ok(len) = u32::try_from(body.len()) else {
        bail!("frame of {} bytes is too large", body.len());
    };
    if len > MAX_FRAME {
        bail!("frame of {len} bytes is too large");
    }
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one frame. Returns `None` at a clean end of the stream.
pub async fn read<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME {
        bail!("peer sent a frame of {len} bytes");
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

/// One unit of a terminal session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    Data(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// The exit status of the remote command. The last chunk.
    Exit(i32),
}

const DATA: u8 = 0;
const RESIZE: u8 = 1;
const EXIT: u8 = 2;

pub async fn write_chunk<W: AsyncWrite + Unpin>(w: &mut W, chunk: &Chunk) -> Result<()> {
    match chunk {
        Chunk::Data(d) => {
            let Ok(len) = u32::try_from(d.len()) else {
                bail!("chunk too large");
            };
            w.write_u8(DATA).await?;
            w.write_u32(len).await?;
            w.write_all(d).await?;
        }
        Chunk::Resize { cols, rows } => {
            w.write_u8(RESIZE).await?;
            w.write_u16(*cols).await?;
            w.write_u16(*rows).await?;
        }
        Chunk::Exit(code) => {
            w.write_u8(EXIT).await?;
            w.write_i32(*code).await?;
        }
    }
    w.flush().await?;
    Ok(())
}

/// Reads one chunk. Returns `None` at a clean end of the stream.
pub async fn read_chunk<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Chunk>> {
    let tag = match r.read_u8().await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(match tag {
        DATA => {
            let len = r.read_u32().await?;
            if len > MAX_FRAME {
                bail!("peer sent a chunk of {len} bytes");
            }
            let mut d = vec![0u8; len as usize];
            r.read_exact(&mut d).await?;
            Chunk::Data(d)
        }
        RESIZE => Chunk::Resize {
            cols: r.read_u16().await?,
            rows: r.read_u16().await?,
        },
        EXIT => Chunk::Exit(r.read_i32().await?),
        t => bail!("unknown chunk type {t}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{Reply, Request};

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let req = Request::Destroy { vm: "t1".into() };
        write(&mut a, &req).await.unwrap();
        write(&mut a, &Reply::Progress("x".into())).await.unwrap();
        drop(a);
        assert_eq!(read::<_, Request>(&mut b).await.unwrap(), Some(req));
        assert_eq!(
            read::<_, Reply>(&mut b).await.unwrap(),
            Some(Reply::Progress("x".into()))
        );
        assert_eq!(read::<_, Reply>(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_huge_frames() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_FRAME + 1).to_be_bytes()).await.unwrap();
        assert!(read::<_, Reply>(&mut b).await.is_err());
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&10u32.to_be_bytes()).await.unwrap();
        a.write_all(b"{\"").await.unwrap();
        drop(a);
        assert!(read::<_, Reply>(&mut b).await.is_err());
    }

    #[tokio::test]
    async fn chunks_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let chunks = [
            Chunk::Data(b"ls\r".to_vec()),
            Chunk::Resize {
                cols: 120,
                rows: 40,
            },
            Chunk::Data(vec![]),
            Chunk::Exit(-1),
        ];
        for c in &chunks {
            write_chunk(&mut a, c).await.unwrap();
        }
        drop(a);
        for c in &chunks {
            assert_eq!(read_chunk(&mut b).await.unwrap().as_ref(), Some(c));
        }
        assert_eq!(read_chunk(&mut b).await.unwrap(), None);
    }
}
