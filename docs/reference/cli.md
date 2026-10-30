# Command line

## `hvw` (client)

`hvw` talks to the members through `hvw-watch`. The `hvw-watch` user
service must run.

| Command | Does |
|---|---|
| `hvw up [-w DIR] [--member M]` | Starts the VM of the worktree if it does not run, then attaches to it. |
| `hvw launch [-w DIR] [--member M]` | Starts the VM of the worktree. Fails if the worktree has a VM already. |
| `hvw push [-w DIR]` | Sends the commits and the uncommitted changes of the local repos to the VM, as `refs/hvw/head` and `refs/hvw/stash`. |
| `hvw fetch [-w DIR]` | Gets the branches of each local repo in the VM, as `refs/remotes/<vm>/<branch>`. |
| `hvw attach [-w DIR \| --vm VM]` | Opens a terminal on the agent harness in the VM. |
| `hvw destroy [-w DIR \| --vm VM]` | Stops and deletes the VM. |
| `hvw list` | Lists the VMs of this client on all members. |
| `hvw pending` | Lists the egress requests that wait for a decision. |
| `hvw decide ID vm\|policy\|deny [--member M]` | Decides on an egress request. |
| `hvw keygen --secret-file FILE` | Makes an iroh secret key and prints its endpoint id. The clan service uses it. |

- `-w`, `--worktree`: the worktree with `hvw.toml`. Default: the current
  directory.
- `--member`: the member that runs the VM. Default: the reachable member
  with the fewest VMs of this client.
- `--vm`: a VM by name, instead of the VM of a worktree.

`hvw herdr-hook` is hidden. The herdr plugin runs it on
`worktree.created` and `worktree.removed`.

### Environment and files

| Name | Default | Use |
|---|---|---|
| `HVW_LINK` | `/etc/hvw/link.json` | Control plane configuration. The NixOS module writes it. |
| `HVW_SOCKET` | `$XDG_RUNTIME_DIR/hvw/watch.sock` | Socket of `hvw-watch`. |
| state directory | `$XDG_STATE_HOME/hvw` (`~/.local/state/hvw`) | `vms/<vm>.json` for each VM, `pending.json` for egress requests. |

## `hvw-watch` (client)

A systemd user service, for the user in `services.hvw.client.user`. It
holds the iroh endpoint of the client and serves the socket for `hvw`
and `hvw-review`. For each egress request, it sets a workspace token in
herdr, raises a toast and reports the agent as blocked.

## `hvw-review` (client)

The popup of the herdr plugin. It shows each pending request with the
exact host, method and path that the gatekeeper refused, and the reason
of the agent. Open it with the plugin action `review`.

## `egress-request` (VM)

```sh
egress-request HOST [--method METHOD] [--path PATH] --reason TEXT
```

Asks the user to allow a request that the gatekeeper refused. Make the
request first. The command blocks until the user decides, for at most 30
minutes.

| Argument | Meaning |
|---|---|
| `HOST` | The host of the refused request. |
| `--method` | The HTTP method. Without it: all methods. |
| `--path` | The path. A `*` at the end matches all paths below it. Without it: all paths. |
| `--reason` | Why the task needs the host. The user reads it. |
| `--broker` | The broker URL. Default: `$HVW_BROKER`, which the VM sets. |

| Exit status | Meaning |
|---|---|
| 0 | Allowed. Make the request again. |
| 1 | Denied. Do not make the request again. |
| 2 | Other error, for example no matching refusal. |

## `hvw-agent` (member)

```sh
hvw-agent --config /etc/hvw/agent.json
```

The NixOS module `member` writes the configuration and runs the service
`hvw-agent.service`.
