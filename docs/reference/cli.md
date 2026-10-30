# Command line

## `hvw` (client)

`hvw` talks to the members through `hvw-watch`. The `hvw-watch` user
service must run.

| Command | Does |
|---|---|
| `hvw` | Opens the terminal UI. Without a terminal, prints the help. |
| `hvw up [-w DIR] [--member M]` | Starts the VM of the worktree if it does not run, then attaches to it. |
| `hvw launch [-w DIR] [--member M]` | Starts the VM of the worktree. Fails if the worktree has a VM already. |
| `hvw push [-w DIR]` | Sends the commits and the uncommitted changes of the local repos to the VM, as `refs/hvw/head` and `refs/hvw/stash`. |
| `hvw fetch [-w DIR]` | Gets the branches of each local repo in the VM, as `refs/remotes/<vm>/<branch>`. |
| `hvw attach [-w DIR \| --vm VM]` | Opens a terminal on the agent harness in the VM. |
| `hvw destroy [-w DIR \| --vm VM]` | Stops and deletes the VM. |
| `hvw list [--json]` | Lists the VMs of this client on all members. |
| `hvw pending [--json]` | Lists the egress requests that wait for a decision. |
| `hvw decide ID vm\|policy\|deny [--member M]` | Decides on an egress request. |
| `hvw keygen --secret-file FILE` | Makes an iroh secret key and prints its endpoint id. The clan service uses it. |

- `-w`, `--worktree`: the worktree with `hvw.toml`. Default: the current
  directory.
- `--member`: the member that runs the VM. Default: the reachable member
  with the fewest VMs of this client.
- `--vm`: a VM by name, instead of the VM of a worktree.
- `--json`: print JSON instead of a table. With no row, the table form
  prints nothing on stdout and a note on stderr.

`hvw herdr-hook` is hidden. The herdr plugin runs it on
`worktree.created` and `worktree.removed`.

### Columns of `hvw list`

| Column | Meaning |
|---|---|
| `VM` | The name of the VM. |
| `MEMBER` | The member (Incus host) that runs the VM. |
| `STATUS` | The Incus status, for example `Running` or `Stopped`. |
| `ADDRESS` | The IPv4 address of the VM on the bridge of its member. |
| `REPOS` | The repos in `/work` in the VM. |
| `POLICIES` | The egress policies of the VM. |
| `PENDING` | The number of egress requests that wait for a decision. |
| `WORKTREE` | The worktree on this client. `-` if this client has no entry for the VM. |

### Columns of `hvw pending`

| Column | Meaning |
|---|---|
| `ID` | The id of the request, for `hvw decide`. |
| `MEMBER` | The member that runs the VM. |
| `VM` | The VM of the agent that asks. |
| `METHOD`, `HOST`, `PATH` | The request that the gatekeeper refused. `PATH` is `-` for CONNECT. |
| `AGE` | The time since the agent asked. |
| `REASON` | The reason of the agent. Untrusted text, on one line. |

### Terminal UI

`hvw` without a command opens a terminal UI after k9s. It has three
views: the VMs, the pending requests and the members. The top lines show
the state of `hvw-watch`, the counts and the keys of the view. The UI
gets the VMs again every 5 seconds.

| Key | View | Does |
|---|---|---|
| `1`, `2`, `3` | all | Shows the VMs, the pending requests or the members. |
| `:` | all | Opens the command bar: `vm`, `pending` (`pe`, `req`), `members` (`mem`), `quit` (`q`). |
| `/` | all | Filters the rows. A row stays when a cell contains the text, in any case. `Esc` clears the filter. |
| `j`, `k`, arrows, `g`, `G` | all | Moves the selection. |
| `r` | all | Gets the VMs again now. |
| `q` | all | Quits. In the details of a VM and in an output, it closes the window. |
| `ctrl-c` | all | Quits. |
| `Enter`, `d` | VMs | Shows the details of the VM and its pending requests. |
| `a` | VMs | Runs `hvw attach` on the terminal. The UI comes back when the session ends. |
| `p`, `f` | VMs | Runs `hvw push` or `hvw fetch` for the worktree of the VM and shows the output. |
| `ctrl-d` | VMs | Destroys the VM, after `y`. |
| `v`, `p`, `d` | pending | Allows the request for the VM, allows it for the policies, or denies it. |

Attach, push, fetch and destroy need the local entry of the VM, as the
commands do. A VM of the same name on another member has no local entry.

The selection stays on its request when other requests come or go. When
the selected request goes away, the selection moves to another request,
and `v`, `p` and `d` do nothing for 1 second. A key that moves the
selection ends this wait.

The output of push, fetch and destroy opens when you are not in a filter,
the command bar or a confirmation. When you quit, the UI waits for the
commands that still run, and prints the outputs that you did not see.
After a policy grant, the UI shows the Nix change, and prints it again
when it closes.

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

The popup of the herdr plugin. It is the terminal UI of `hvw`, opened on
the pending requests. It shows each pending request with the exact host,
method and path that the gatekeeper refused, and the reason of the
agent. Open it with the plugin action `review`.

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
