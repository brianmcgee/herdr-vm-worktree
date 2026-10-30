# VM environment, files and ports

## Environment of the agent harness

`hvw-agent` writes `/run/hvw/env` into the VM at launch. A login shell
reads it.

| Variable | Value |
|---|---|
| `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` (and lower case) | `http://<bridge address>:<tunnel port>` |
| `NO_PROXY`, `no_proxy` | the bridge address |
| `SSL_CERT_FILE`, `NIX_SSL_CERT_FILE`, `CURL_CA_BUNDLE`, `GIT_SSL_CAINFO`, `REQUESTS_CA_BUNDLE` | `/run/hvw/ca-bundle.crt` |
| `NODE_EXTRA_CA_CERTS` | `/run/hvw/ca.crt` |
| `HVW_BROKER` | `http://<bridge address>:<broker port>` |
| `HVW_VM` | the VM name |
| the `env` names of each secret | the placeholder of the secret |
| the `env` of each policy | its value |

A login shell in `/work/<repo>` also sources the devshell environment of
that repo.

## Files in the VM

| Path | Content |
|---|---|
| `/work/<repo>` | The checkout of each repo, owned by `agent`. |
| `/run/hvw/env` | The environment above. |
| `/run/hvw/ca.crt` | The CA of the gatekeeper of this VM. |
| `/run/hvw/farms/<repo>` | Read-only farm snapshot of each repo (virtiofs). |
| `~/.claude/skills`, `~/.codex/skills`, `~/.agents/skills` | The `egress-request` skill. |

## Files on the member

| Path | Content |
|---|---|
| `/etc/hvw/agent.json` | Configuration of `hvw-agent`. |
| `/var/lib/hvw/vms/<vm>/vm.json` | The record of a VM: client, slot, repos, grants, farms. |
| `/var/lib/hvw/vms/<vm>/gatekeeper/` | iron-proxy configuration and CA of the VM. |
| `/var/lib/hvw/vms/<vm>/store/<repo>` | Farm snapshots of the VM. |
| `/var/lib/hvw/farms/` | The farms: one btrfs subvolume for each devshell closure. |

## Ports on the member

Each VM has a slot *n* from 0 to 999. With the default
`gatekeeperPortBase` of 20000:

| Port | Use | Reachable from the VM |
|---|---|---|
| 20000 + *n* | gatekeeper proxy (tunnel) | yes |
| 21000 + *n* | gatekeeper HTTP | no |
| 22000 + *n* | gatekeeper HTTPS | no |
| 23000 + *n* | gatekeeper metrics | no |
| 24000 + *n* | gatekeeper management (reload) | no |
| 7999 | egress broker (`brokerPort`) | yes |
| `bind` port, UDP | control plane | — (clients) |

## systemd units on the member

| Unit | Job |
|---|---|
| `hvw-agent.service` | The member daemon. |
| `hvw-image-import.service` | Imports the base image into Incus once. |
| `iron-proxy@<vm>.service` | The gatekeeper of one VM. Its journal is the audit log. |
