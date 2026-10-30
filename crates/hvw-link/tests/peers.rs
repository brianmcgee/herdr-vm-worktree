//! Real iroh endpoints on the loopback interface, with the relay off: a
//! member accepts the client in its allowlist and refuses a stranger.

use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::time::Duration;

use hvw_link::{Config, Link, Peer, Relay, generate_secret_key};

struct Node {
    name: &'static str,
    id: String,
    addr: SocketAddr,
    key_file: std::path::PathBuf,
}

fn node(dir: &Path, name: &'static str) -> Node {
    let (key, id) = generate_secret_key();
    let key_file = dir.join(name);
    std::fs::write(&key_file, key).unwrap();
    // A free port: bind it, then let it go.
    let addr = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    Node {
        name,
        id,
        addr,
        key_file,
    }
}

fn config(me: &Node, peers: &[&Node]) -> Config {
    Config {
        secret_key_file: me.key_file.clone(),
        bind: Some(me.addr),
        relay: Relay::Disabled,
        peers: peers
            .iter()
            .map(|p| Peer {
                name: p.name.to_owned(),
                id: p.id.clone(),
                addrs: vec![p.addr],
            })
            .collect(),
    }
}

async fn ping(link: &Link, to: &str) -> anyhow::Result<Vec<u8>> {
    let conn = link.connect(to).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(b"ping").await?;
    send.finish()?;
    Ok(recv.read_to_end(64).await?)
}

#[tokio::test]
async fn member_refuses_unknown_peer() {
    let tmp = tempfile::tempdir().unwrap();
    let member = node(tmp.path(), "member");
    let client = node(tmp.path(), "client");
    let stranger = node(tmp.path(), "stranger");

    let m = Link::bind(&config(&member, &[&client])).await.unwrap();
    let c = Link::bind(&config(&client, &[&member])).await.unwrap();
    let s = Link::bind(&config(&stranger, &[&member])).await.unwrap();
    assert_eq!(m.id(), member.id);

    let server = tokio::spawn(async move {
        let mut seen = vec![];
        while let Some((peer, conn)) = m.accept().await {
            seen.push(peer.name.clone());
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let msg = recv.read_to_end(64).await.unwrap();
            assert_eq!(msg, b"ping");
            send.write_all(b"pong").await.unwrap();
            send.finish().unwrap();
            send.stopped().await.ok();
            if seen.len() == 1 {
                break;
            }
        }
        seen
    });

    let refused = tokio::time::timeout(Duration::from_secs(20), ping(&s, "member"))
        .await
        .expect("the stranger hangs instead of being refused");
    assert!(refused.is_err(), "the member answered a stranger");

    let answer = tokio::time::timeout(Duration::from_secs(20), ping(&c, "member"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer, b"pong");

    // Only the client got through `accept`.
    assert_eq!(server.await.unwrap(), vec!["client".to_owned()]);
}
