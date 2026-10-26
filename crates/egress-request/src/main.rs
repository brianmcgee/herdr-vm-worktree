//! `egress-request`: asks the user to allow a request that the gatekeeper
//! of this VM refused. The command blocks until the user decides.
//!
//! Exit status: 0 when the user allows the request, 1 when the user
//! denies it, 2 for all other errors.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde_json::{Value, json};

/// Ask the user to allow a request that the gatekeeper refused.
///
/// First make the request. When the gatekeeper refuses it (status 403),
/// run this command with the same host, method and path. The command
/// waits for the decision of the user. When the user allows the request,
/// make the request again.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The host of the refused request, for example `api.example.org`.
    host: String,
    /// The HTTP method of the refused request. Without it, the request
    /// asks for all methods.
    #[arg(long)]
    method: Option<String>,
    /// The path of the refused request. A `*` at the end matches all
    /// paths below it. Without it, the request asks for all paths.
    #[arg(long)]
    path: Option<String>,
    /// Why the task needs this host. The user reads it.
    #[arg(long)]
    reason: String,
    /// The broker URL. The VM sets it.
    #[arg(long, env = "HVW_BROKER", hide_env_values = true)]
    broker: String,
}

/// The wait for the user is 30 minutes on the broker. Wait a bit longer,
/// so that the broker answers first.
const READ_TIMEOUT: Duration = Duration::from_secs(31 * 60);

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("egress-request: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> Result<bool> {
    let body = json!({
        "host": args.host,
        "method": args.method,
        "path": args.path,
        "reason": args.reason,
    });
    eprintln!(
        "egress-request: waiting for the decision of the user about {}",
        args.host
    );
    let (status, reply) = post(&args.broker, "/v1/egress", &body)?;
    let error = reply.get("error").and_then(Value::as_str).unwrap_or("");
    match status {
        200 => {
            let scope = reply.get("decision").and_then(Value::as_str).unwrap_or("");
            println!("allowed ({scope}): make the request again");
            Ok(true)
        }
        403 if reply.get("decision").is_some() => {
            println!("denied: do not make the request again");
            Ok(false)
        }
        _ => bail!("the broker returned status {status}: {error}"),
    }
}

/// Splits `http://host:port` into `host:port`. The broker is on the bridge
/// of the member, so plain HTTP is sufficient.
fn authority(url: &str) -> Result<&str> {
    let Some(rest) = url.strip_prefix("http://") else {
        bail!("the broker URL {url:?} does not start with http://");
    };
    let auth = rest.trim_end_matches('/');
    if auth.is_empty() || auth.contains('/') {
        bail!("the broker URL {url:?} is not http://host:port");
    }
    Ok(auth)
}

/// Sends one POST request with a JSON body, directly and not through the
/// proxy of the VM. Returns the status and the JSON body of the answer.
fn post(url: &str, path: &str, body: &Value) -> Result<(u16, Value)> {
    let auth = authority(url)?;
    let body = serde_json::to_vec(body)?;
    let mut stream =
        TcpStream::connect(auth).with_context(|| format!("cannot connect to the broker {auth}"))?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {auth}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&body)?;
    let mut answer = Vec::new();
    stream
        .read_to_end(&mut answer)
        .context("no answer from the broker")?;
    parse_response(&answer)
}

fn parse_response(answer: &[u8]) -> Result<(u16, Value)> {
    let text = String::from_utf8_lossy(answer);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        bail!("the answer of the broker is not HTTP");
    };
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .context("the answer of the broker has no status")?;
    if head
        .lines()
        .any(|l| l.eq_ignore_ascii_case("transfer-encoding: chunked"))
    {
        bail!("the answer of the broker is chunked");
    }
    let value = serde_json::from_str(body).unwrap_or(Value::Null);
    Ok((status, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_urls() {
        assert_eq!(
            authority("http://10.250.0.1:7000").unwrap(),
            "10.250.0.1:7000"
        );
        assert_eq!(
            authority("http://10.250.0.1:7000/").unwrap(),
            "10.250.0.1:7000"
        );
        assert!(authority("https://x:1").is_err());
        assert!(authority("http://x:1/v1").is_err());
    }

    #[test]
    fn answers() {
        let a = b"HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\ncontent-length: 29\r\n\r\n{\"id\":3,\"decision\":\"deny\"}";
        let (status, v) = parse_response(a).unwrap();
        assert_eq!(status, 403);
        assert_eq!(v["decision"], "deny");
        assert!(parse_response(b"garbage").is_err());
    }

    /// The whole exchange against a real HTTP server: the broker of the
    /// test answers like axum.
    #[test]
    fn exchange() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 4096];
            let mut req = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some((_, body)) = text.split_once("\r\n\r\n")
                    && body.ends_with('}')
                {
                    break;
                }
            }
            let body = r#"{"id":1,"decision":"vm"}"#;
            write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            String::from_utf8(req).unwrap()
        });
        let (status, v) = post(
            &format!("http://{addr}"),
            "/v1/egress",
            &json!({"host": "a.b", "reason": "r"}),
        )
        .unwrap();
        assert_eq!((status, v["decision"].as_str()), (200, Some("vm")));
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /v1/egress HTTP/1.1\r\n"), "{req}");
        assert!(req.ends_with(r#"{"host":"a.b","reason":"r"}"#), "{req}");
    }
}
