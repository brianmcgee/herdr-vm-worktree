# herdr-vm-worktree

Run each [herdr](https://herdr.dev) worktree in its
own short-lived KVM VM on a remote Incus host. The VM holds no
credential. A per-VM egress gatekeeper adds the credentials and enforces
a domain allowlist. When an agent needs a refused host, you approve or
deny the request in herdr.

```mermaid
flowchart LR
  subgraph Client
    herdr[herdr + plugin] --- hvw[hvw / hvw-watch]
  end
  subgraph Member["Member (Incus host)"]
    agent[hvw-agent] --> vm[Worktree VM]
    vm -- HTTP_PROXY --> gk[gatekeeper]
  end
  hvw <-- iroh --> agent
  gk --> net((allowed hosts))
```

Start with the [documentation](docs/README.md).

```sh
nix develop          # Rust, Nix and Python tools, language servers
cargo test           # unit tests
nix fmt              # treefmt: rustfmt and nixfmt
```
