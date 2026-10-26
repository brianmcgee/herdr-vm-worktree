//! Policies: allowlists, secret mappings and environment, defined in Nix.
//! A worktree VM gets the union of the policies of its repos, plus the
//! egress grants of the user.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One allowlist rule of iron-proxy. Empty `methods` or `paths` match all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub host: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// A secret mapping: the gatekeeper replaces `placeholder` with the
/// contents of the systemd credential `credential`, in requests to `hosts`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub credential: String,
    pub placeholder: String,
    pub hosts: Vec<String>,
    #[serde(default = "default_headers")]
    pub headers: Vec<String>,
    /// Environment variables in the VM that hold the placeholder.
    #[serde(default)]
    pub env: Vec<String>,
}

fn default_headers() -> Vec<String> {
    vec!["Authorization".to_owned()]
}

/// A policy as Nix writes it to `/etc/hvw/policies/<name>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub secrets: Vec<Secret>,
    /// Plain environment variables for the VM. No secret goes here.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// An egress grant of the user. Empty `methods` and `paths` allow the
/// whole host.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Grant {
    pub host: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("policies set the environment variable {name} to two values: {a:?} and {b:?}")]
    EnvConflict { name: String, a: String, b: String },
    #[error("the credential {0} has two different mappings")]
    SecretConflict(String),
    #[error("the environment variable {0} is set by a policy and by a secret")]
    EnvSecretConflict(String),
}

/// The union of policies and grants that one gatekeeper enforces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effective {
    pub domains: BTreeSet<String>,
    pub rules: BTreeSet<Rule>,
    pub secrets: BTreeMap<String, Secret>,
    pub env: BTreeMap<String, String>,
}

impl Effective {
    pub fn merge<'a>(policies: impl IntoIterator<Item = &'a Policy>) -> Result<Self, Error> {
        let mut out = Self::default();
        for p in policies {
            out.domains.extend(p.domains.iter().cloned());
            out.rules.extend(p.rules.iter().cloned());
            for s in &p.secrets {
                match out.secrets.get(&s.credential) {
                    Some(old) if old != s => {
                        return Err(Error::SecretConflict(s.credential.clone()));
                    }
                    _ => {
                        out.secrets.insert(s.credential.clone(), s.clone());
                    }
                }
            }
            for (k, v) in &p.env {
                match out.env.get(k) {
                    Some(old) if old != v => {
                        return Err(Error::EnvConflict {
                            name: k.clone(),
                            a: old.clone(),
                            b: v.clone(),
                        });
                    }
                    _ => {
                        out.env.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        for s in out.secrets.values() {
            for name in &s.env {
                if out.env.contains_key(name) {
                    return Err(Error::EnvSecretConflict(name.clone()));
                }
            }
        }
        Ok(out)
    }

    /// Adds a grant. A grant with a method or a path also allows the
    /// CONNECT request that opens the tunnel to the host: iron-proxy checks
    /// CONNECT with an empty path.
    pub fn grant(&mut self, g: &Grant) {
        let host = g.host.to_ascii_lowercase();
        if g.methods.is_empty() && g.paths.is_empty() {
            self.domains.insert(host);
            return;
        }
        self.rules.insert(Rule {
            host: host.clone(),
            methods: vec!["CONNECT".to_owned()],
            paths: vec![],
        });
        self.rules.insert(Rule {
            host,
            methods: g.methods.iter().map(|m| m.to_ascii_uppercase()).collect(),
            paths: g.paths.clone(),
        });
    }

    /// The environment of the VM: plain variables and placeholders.
    pub fn vm_env(&self) -> BTreeMap<String, String> {
        let mut env = self.env.clone();
        for s in self.secrets.values() {
            for name in &s.env {
                env.insert(name.clone(), s.placeholder.clone());
            }
        }
        env
    }
}

/// The listeners and files of one gatekeeper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gatekeeper {
    /// The address of the member on the bridge.
    pub bridge_address: String,
    pub tunnel_port: u16,
    pub http_port: u16,
    pub https_port: u16,
    pub metrics_port: u16,
    pub management_port: u16,
    /// The environment variable that holds the key of the management API.
    pub management_key_env: String,
    pub ca_cert: String,
    pub ca_key: String,
    /// The directory of the systemd credentials of the gatekeeper unit.
    pub credentials_dir: String,
    /// Upstream addresses that the gatekeeper never dials.
    pub deny_cidrs: Vec<String>,
}

/// The upstream deny list of iron-proxy, without RFC 1918 networks. The
/// member adds its bridge network.
pub const DEFAULT_DENY_CIDRS: &[&str] = &[
    "0.0.0.0/8",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "::1/128",
    "fe80::/10",
    "fd00:ec2::254/128",
    "fd20:ce::254/128",
];

impl Gatekeeper {
    /// The iron-proxy configuration. JSON is valid YAML, so iron-proxy
    /// reads it as it is.
    pub fn config(&self, policy: &Effective) -> Value {
        let localhost = |port: u16| format!("127.0.0.1:{port}");
        let secrets: Vec<Value> = policy
            .secrets
            .values()
            .map(|s| {
                json!({
                    "source": {
                        "type": "file",
                        "path": format!("{}/{}", self.credentials_dir, s.credential),
                    },
                    "replace": {
                        "proxy_value": s.placeholder,
                        "match_headers": s.headers,
                        "require": false,
                    },
                    "rules": s.hosts.iter().map(|h| json!({ "host": h })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let mut transforms = vec![json!({
            "name": "allowlist",
            "config": {
                "domains": policy.domains,
                "rules": policy.rules,
            },
        })];
        if !secrets.is_empty() {
            transforms.push(json!({ "name": "secrets", "config": { "secrets": secrets } }));
        }
        json!({
            "log": { "level": "info" },
            "dns": { "enabled": false },
            "proxy": {
                "tunnel_listen": format!("{}:{}", self.bridge_address, self.tunnel_port),
                "http_listen": localhost(self.http_port),
                "https_listen": localhost(self.https_port),
                "upstream_deny_cidrs": self.deny_cidrs,
            },
            "metrics": { "listen": localhost(self.metrics_port) },
            "management": {
                "listen": localhost(self.management_port),
                "api_key_env": self.management_key_env,
            },
            "tls": {
                "mode": "mitm",
                "ca_cert": self.ca_cert,
                "ca_key": self.ca_key,
            },
            "transforms": transforms,
        })
    }
}

/// Renders policy grants as a Nix attribute set for the clan repo.
pub fn grants_as_nix(policy: &str, grants: &[Grant]) -> String {
    let mut domains = Vec::new();
    let mut rules = Vec::new();
    for g in grants {
        if g.methods.is_empty() && g.paths.is_empty() {
            domains.push(format!("    {}", nix_string(&g.host)));
            continue;
        }
        rules.push(format!(
            "    {{ host = {}; methods = [ \"CONNECT\" ]; }}",
            nix_string(&g.host)
        ));
        let mut fields = vec![format!("host = {};", nix_string(&g.host))];
        if !g.methods.is_empty() {
            fields.push(format!("methods = {};", nix_list(&g.methods)));
        }
        if !g.paths.is_empty() {
            fields.push(format!("paths = {};", nix_list(&g.paths)));
        }
        rules.push(format!("    {{ {} }}", fields.join(" ")));
    }
    let mut out = String::new();
    let attr = format!("policies.{}", nix_attr(policy));
    if !domains.is_empty() {
        out.push_str(&format!("{attr}.domains = [\n{}\n];\n", domains.join("\n")));
    }
    if !rules.is_empty() {
        out.push_str(&format!("{attr}.rules = [\n{}\n];\n", rules.join("\n")));
    }
    out
}

fn nix_string(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${");
    format!("\"{escaped}\"")
}

fn nix_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| nix_string(s)).collect();
    format!("[ {} ]", inner.join(" "))
}

fn nix_attr(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '\''));
    if plain {
        name.to_owned()
    } else {
        nix_string(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> Policy {
        serde_json::from_value(json!({
            "domains": ["github.com", "api.github.com", "cache.nixos.org"],
            "rules": [{ "host": "objects.githubusercontent.com", "methods": ["GET"] }],
            "secrets": [{
                "credential": "github-token",
                "placeholder": "hvw-github-placeholder",
                "hosts": ["api.github.com", "github.com"],
                "env": ["GH_TOKEN", "GITHUB_TOKEN"],
            }],
            "env": { "NIX_CONFIG": "accept-flake-config = false" },
        }))
        .unwrap()
    }

    fn nix() -> Policy {
        serde_json::from_value(json!({
            "domains": ["cache.nixos.org", "channels.nixos.org"],
        }))
        .unwrap()
    }

    #[test]
    fn merge_is_a_union() {
        let e = Effective::merge([&github(), &nix()]).unwrap();
        assert_eq!(
            e.domains.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "api.github.com",
                "cache.nixos.org",
                "channels.nixos.org",
                "github.com"
            ]
        );
        assert_eq!(e.rules.len(), 1);
        assert_eq!(
            e.vm_env(),
            BTreeMap::from([
                ("GH_TOKEN".to_owned(), "hvw-github-placeholder".to_owned()),
                (
                    "GITHUB_TOKEN".to_owned(),
                    "hvw-github-placeholder".to_owned()
                ),
                (
                    "NIX_CONFIG".to_owned(),
                    "accept-flake-config = false".to_owned()
                ),
            ])
        );
    }

    #[test]
    fn merge_rejects_conflicts() {
        let mut other = github();
        other.secrets[0].placeholder = "other".into();
        assert_eq!(
            Effective::merge([&github(), &other]),
            Err(Error::SecretConflict("github-token".into()))
        );

        let mut env = nix();
        env.env.insert("NIX_CONFIG".into(), "x".into());
        assert!(matches!(
            Effective::merge([&github(), &env]),
            Err(Error::EnvConflict { .. })
        ));

        let mut shadow = nix();
        shadow.env.insert("GH_TOKEN".into(), "real-token".into());
        assert_eq!(
            Effective::merge([&github(), &shadow]),
            Err(Error::EnvSecretConflict("GH_TOKEN".into()))
        );
    }

    #[test]
    fn grants() {
        let mut e = Effective::merge([&nix()]).unwrap();
        e.grant(&Grant {
            host: "Example.ORG".into(),
            methods: vec![],
            paths: vec![],
        });
        e.grant(&Grant {
            host: "pypi.org".into(),
            methods: vec!["get".into()],
            paths: vec!["/simple/*".into()],
        });
        assert!(e.domains.contains("example.org"));
        assert_eq!(
            e.rules.iter().cloned().collect::<Vec<_>>(),
            vec![
                Rule {
                    host: "pypi.org".into(),
                    methods: vec!["CONNECT".into()],
                    paths: vec![],
                },
                Rule {
                    host: "pypi.org".into(),
                    methods: vec!["GET".into()],
                    paths: vec!["/simple/*".into()],
                },
            ]
        );
    }

    #[test]
    fn gatekeeper_config() {
        let e = Effective::merge([&github()]).unwrap();
        let g = Gatekeeper {
            bridge_address: "10.250.0.1".into(),
            tunnel_port: 20000,
            http_port: 40000,
            https_port: 40001,
            metrics_port: 40002,
            management_port: 40003,
            management_key_env: "IRON_MANAGEMENT_API_KEY".into(),
            ca_cert: "/var/lib/hvw/gatekeeper/w1/ca.crt".into(),
            ca_key: "/run/credentials/iron-proxy@w1.service/ca.key".into(),
            credentials_dir: "/run/credentials/iron-proxy@w1.service".into(),
            deny_cidrs: vec!["127.0.0.0/8".into(), "10.250.0.0/24".into()],
        };
        let c = g.config(&e);
        assert_eq!(c["proxy"]["tunnel_listen"], "10.250.0.1:20000");
        assert_eq!(c["proxy"]["http_listen"], "127.0.0.1:40000");
        assert_eq!(c["management"]["listen"], "127.0.0.1:40003");
        assert_eq!(c["proxy"]["upstream_deny_cidrs"][1], "10.250.0.0/24");
        assert_eq!(c["transforms"][0]["name"], "allowlist");
        assert_eq!(c["transforms"][0]["config"]["domains"][0], "api.github.com");
        assert_eq!(
            c["transforms"][0]["config"]["rules"][0],
            json!({ "host": "objects.githubusercontent.com", "methods": ["GET"] })
        );
        let secret = &c["transforms"][1]["config"]["secrets"][0];
        assert_eq!(
            secret["source"]["path"],
            "/run/credentials/iron-proxy@w1.service/github-token"
        );
        assert_eq!(secret["replace"]["proxy_value"], "hvw-github-placeholder");
        assert_eq!(secret["replace"]["match_headers"], json!(["Authorization"]));
        assert_eq!(
            secret["rules"],
            json!([{ "host": "api.github.com" }, { "host": "github.com" }])
        );
    }

    #[test]
    fn no_secrets_transform_without_secrets() {
        let e = Effective::merge([&nix()]).unwrap();
        let g = Gatekeeper {
            bridge_address: "10.250.0.1".into(),
            tunnel_port: 1,
            http_port: 2,
            https_port: 3,
            metrics_port: 4,
            management_port: 5,
            management_key_env: "K".into(),
            ca_cert: "c".into(),
            ca_key: "k".into(),
            credentials_dir: "/run/credentials/x".into(),
            deny_cidrs: vec![],
        };
        assert_eq!(g.config(&e)["transforms"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn nix_snippet() {
        let text = grants_as_nix(
            "github",
            &[
                Grant {
                    host: "example.org".into(),
                    methods: vec![],
                    paths: vec![],
                },
                Grant {
                    host: "pypi.org".into(),
                    methods: vec!["GET".into()],
                    paths: vec!["/simple/${x}\"".into()],
                },
            ],
        );
        assert_eq!(
            text,
            "policies.github.domains = [\n    \"example.org\"\n];\n\
             policies.github.rules = [\n    { host = \"pypi.org\"; methods = [ \"CONNECT\" ]; }\n    \
             { host = \"pypi.org\"; methods = [ \"GET\" ]; paths = [ \"/simple/\\${x}\\\"\" ]; }\n];\n"
        );
        assert!(grants_as_nix("my.policy", &[]).is_empty());
        assert_eq!(
            grants_as_nix(
                "my.policy",
                &[Grant {
                    host: "a".into(),
                    methods: vec![],
                    paths: vec![]
                }]
            ),
            "policies.\"my.policy\".domains = [\n    \"a\"\n];\n"
        );
    }
}
