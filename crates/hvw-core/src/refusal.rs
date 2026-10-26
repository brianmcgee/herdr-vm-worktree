//! Refusals in the audit log of iron-proxy, and how an egress request of
//! an agent matches one. The broker accepts a request only when the
//! gatekeeper of the VM refused a matching request recently, so the user
//! sees the real refused request next to the reason of the agent.

use serde::{Deserialize, Serialize};

/// One request that the gatekeeper refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// RFC 3339 time stamp of iron-proxy.
    pub time: String,
    /// The host, without a port.
    pub host: String,
    pub method: String,
    /// Empty for CONNECT.
    pub path: String,
}

/// What the agent asks for. `None` means "any".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressAsk {
    pub host: String,
    pub method: Option<String>,
    /// A path pattern: a trailing `/*` matches a prefix, `*` matches one
    /// path segment.
    pub path: Option<String>,
}

#[derive(Deserialize)]
struct Line {
    time: String,
    msg: String,
    audit: Option<Audit>,
    rejected_by: Option<String>,
}

#[derive(Deserialize)]
struct Audit {
    host: String,
    method: String,
    #[serde(default)]
    path: String,
    action: String,
}

/// Parses one log line of iron-proxy. Returns a refusal of the allowlist,
/// or `None` for any other line.
pub fn parse(line: &str) -> Option<Refusal> {
    let l: Line = serde_json::from_str(line).ok()?;
    let a = l.audit?;
    if l.msg != "request" || a.action != "reject" || l.rejected_by.as_deref() != Some("allowlist") {
        return None;
    }
    Some(Refusal {
        time: l.time,
        host: strip_port(&a.host).to_ascii_lowercase(),
        method: a.method,
        path: a.path,
    })
}

fn strip_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match host.rsplit_once(':') {
        Some((h, port)) if !h.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    }
}

/// Matches a path against a pattern with the semantics of iron-proxy: a
/// trailing `/*` matches the prefix and everything below it; otherwise `*`
/// matches within one segment.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    glob(pattern.as_bytes(), path.as_bytes())
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob(&p[1..], s) || (s.first().is_some_and(|&c| c != b'/') && glob(p, &s[1..]))
        }
        (Some(&pc), Some(&sc)) => pc == sc && glob(&p[1..], &s[1..]),
        _ => false,
    }
}

impl EgressAsk {
    /// Checks the request for sense before it reaches a human.
    pub fn validate(&self) -> Result<(), String> {
        let host_ok = !self.host.is_empty()
            && self.host.len() <= 253
            && self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'*'));
        if !host_ok {
            return Err(format!("not a host name: {:?}", self.host));
        }
        if let Some(m) = &self.method
            && (m.is_empty() || !m.bytes().all(|b| b.is_ascii_alphabetic()))
        {
            return Err(format!("not an HTTP method: {m:?}"));
        }
        if let Some(p) = &self.path
            && (!p.starts_with('/') || p.chars().any(char::is_control))
        {
            return Err(format!("not an absolute path: {p:?}"));
        }
        Ok(())
    }

    /// True if the refusal is one that this request would allow. A refused
    /// CONNECT matches any method and path: the gatekeeper refuses the
    /// tunnel before it sees the request in it.
    pub fn matches(&self, r: &Refusal) -> bool {
        if !self.host.eq_ignore_ascii_case(&r.host) {
            return false;
        }
        if r.method == "CONNECT" {
            return true;
        }
        let method_ok = self
            .method
            .as_ref()
            .is_none_or(|m| m.eq_ignore_ascii_case(&r.method));
        let path_ok = self.path.as_ref().is_none_or(|p| path_matches(p, &r.path));
        method_ok && path_ok
    }
}

/// The most recent refusal that the request matches. `lines` are log lines
/// in time order.
pub fn find<'a>(ask: &EgressAsk, lines: impl IntoIterator<Item = &'a str>) -> Option<Refusal> {
    lines
        .into_iter()
        .filter_map(parse)
        .filter(|r| ask.matches(r))
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real lines of iron-proxy 0.54.0.
    const GET: &str = r#"{"time":"2026-10-26T00:28:47.460008819Z","level":"WARN","msg":"request","audit":{"host":"denied.test","method":"GET","path":"/foo/bar","remote_addr":"127.0.0.1:38806","sni":"","mode":"mitm","action":"reject","status_code":403,"duration_ms":0.009},"rejected_by":"allowlist","request_transforms":[{"name":"allowlist","action":"reject","duration_ms":0.002}]}"#;
    const CONNECT: &str = r#"{"time":"2026-10-26T00:28:47.487495384Z","level":"WARN","msg":"request","audit":{"host":"Other.test:443","method":"CONNECT","path":"","remote_addr":"127.0.0.1:38808","sni":"denied.test","mode":"mitm","action":"reject","status_code":403,"duration_ms":0.058},"rejected_by":"allowlist","request_transforms":[{"name":"allowlist","action":"reject","duration_ms":0.031}]}"#;
    const NOISE: &[&str] = &[
        r#"{"time":"2026-10-26T00:28:47.487465884Z","level":"INFO","msg":"tunnel short-circuited by transform","target":"denied.test:443","status":403}"#,
        r#"{"time":"2026-10-26T00:28:46.45874743Z","level":"INFO","msg":"metrics server starting","addr":"127.0.0.1:18083"}"#,
        r#"{"time":"2026-10-26T00:29:00Z","level":"INFO","msg":"request","audit":{"host":"denied.test","method":"GET","path":"/ok","remote_addr":"x","sni":"","mode":"mitm","action":"allow","status_code":200,"duration_ms":1},"request_transforms":[]}"#,
        "not json",
    ];

    fn ask(host: &str, method: Option<&str>, path: Option<&str>) -> EgressAsk {
        EgressAsk {
            host: host.into(),
            method: method.map(Into::into),
            path: path.map(Into::into),
        }
    }

    #[test]
    fn parses_refusals_only() {
        assert_eq!(
            parse(GET),
            Some(Refusal {
                time: "2026-10-26T00:28:47.460008819Z".into(),
                host: "denied.test".into(),
                method: "GET".into(),
                path: "/foo/bar".into(),
            })
        );
        assert_eq!(parse(CONNECT).unwrap().host, "other.test");
        for line in NOISE {
            assert_eq!(parse(line), None, "{line}");
        }
    }

    #[test]
    fn strips_ports() {
        assert_eq!(strip_port("a.b:443"), "a.b");
        assert_eq!(strip_port("a.b"), "a.b");
        assert_eq!(strip_port("[::1]:443"), "::1");
        assert_eq!(strip_port("::1"), "::1");
    }

    #[test]
    fn matching() {
        let lines: Vec<&str> = NOISE.iter().copied().chain([GET, CONNECT]).collect();
        assert!(find(&ask("denied.test", None, None), lines.iter().copied()).is_some());
        assert!(
            find(
                &ask("DENIED.test", Some("get"), Some("/foo/*")),
                lines.iter().copied()
            )
            .is_some()
        );
        assert!(
            find(
                &ask("denied.test", Some("GET"), Some("/foo/bar")),
                lines.iter().copied()
            )
            .is_some()
        );
        assert!(
            find(
                &ask("denied.test", Some("POST"), None),
                lines.iter().copied()
            )
            .is_none()
        );
        assert!(
            find(
                &ask("denied.test", None, Some("/other")),
                lines.iter().copied()
            )
            .is_none()
        );
        assert!(find(&ask("exfil.test", None, None), lines.iter().copied()).is_none());
        // A refused tunnel matches any method and path on that host.
        let r = find(
            &ask("other.test", Some("POST"), Some("/upload")),
            lines.iter().copied(),
        )
        .unwrap();
        assert_eq!(r.method, "CONNECT");
    }

    #[test]
    fn latest_refusal_wins() {
        let later = GET.replace("00:28:47.460008819Z", "00:30:00Z");
        let r = find(&ask("denied.test", None, None), [GET, later.as_str()]).unwrap();
        assert_eq!(r.time, "2026-10-26T00:30:00Z");
    }

    #[test]
    fn paths() {
        assert!(path_matches("/a/*", "/a"));
        assert!(path_matches("/a/*", "/a/b/c"));
        assert!(!path_matches("/a/*", "/ab"));
        assert!(path_matches("/a/*/c", "/a/b/c"));
        assert!(!path_matches("/a/*/c", "/a/b/x/c"));
        assert!(path_matches("/a", "/a"));
        assert!(!path_matches("/a", "/a/"));
    }

    #[test]
    fn validation() {
        assert!(
            ask("pypi.org", Some("GET"), Some("/simple/*"))
                .validate()
                .is_ok()
        );
        assert!(ask("", None, None).validate().is_err());
        assert!(ask("a b", None, None).validate().is_err());
        assert!(ask("a.b", Some("G ET"), None).validate().is_err());
        assert!(ask("a.b", None, Some("relative")).validate().is_err());
        assert!(ask("a.b", None, Some("/x\n/y")).validate().is_err());
    }
}
