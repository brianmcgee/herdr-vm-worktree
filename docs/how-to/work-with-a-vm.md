# Work with a running VM

Run these commands in the worktree, on the client.

## Start and attach

```sh
hvw up                    # start if needed, then attach
hvw up --member forge     # on a specific member
```

In herdr, a new worktree with `hvw.toml` does this in its root pane.

## Send new local changes to the VM

```sh
hvw push
```

The VM receives the commits as `refs/hvw/head` and the uncommitted
changes as `refs/hvw/stash`. The working tree in the VM stays as it is.
Merge or apply them in the VM.

## Get the work of the agent

```sh
hvw fetch
git log <vm>/main            # refs/remotes/<vm>/<branch>
```

Use `git merge` or `git cherry-pick` as with any remote.

## Decide on an egress request

In herdr: the workspace row shows the token, and a toast appears. Open
the popup with the plugin action `review`.

On the command line:

```sh
hvw pending
hvw decide <id> vm        # allow for this VM
hvw decide <id> policy    # allow for new VMs too, until the next deploy
hvw decide <id> deny
```

After `policy`, `hvw` prints a Nix change. Commit it to the clan repo and
deploy to keep the grant.

## Read the audit log

On the member:

```sh
journalctl -u iron-proxy@<vm>
```

## Stop the VM

```sh
hvw destroy
```

In herdr, removing the worktree does this. The VM, its gatekeeper, its
CA and its grants go away. The farms stay for the next VM.

## List all VMs

```sh
hvw list
```
