# Workspace manifest (`hvw.toml`)

The file `hvw.toml` in the root of a worktree lists the repos that its VM
receives. Without this file, the herdr plugin starts no VM.

```toml
[[repo]]
path = "."
policy = "github"

[[repo]]
name = "nixpkgs"
url = "https://github.com/NixOS/nixpkgs"
ref = "nixos-unstable"
policy = "nix"
```

## `[[repo]]` fields

| Field | Required | Meaning |
|---|---|---|
| `path` | one of `path`, `url` | A local checkout, relative to `hvw.toml`. `hvw` pushes its commits and uncommitted changes. |
| `url` | one of `path`, `url` | A remote repo. The member clones it through its own network, not through the gatekeeper. |
| `ref` | no | The branch or tag of `url`. Only with `url`. Default: the default branch. |
| `name` | no | The directory `/work/<name>` in the VM. Default: the last component of the path or the URL, without `.git`. |
| `policy` | yes | The name of an egress policy on the member. |

## Rules

- The manifest has at least one repo.
- A name has 1 to 63 characters: letters, digits, `.`, `_` and `-`. It
  does not start with `.`. Names are unique.
- Each repo is a flake with a default devshell. The member builds it
  with `nix print-dev-env`.
- The VM gets the union of the policies of all repos. Two policies that
  map one credential in different ways cannot go in one VM.
- The agent harness starts in the first repo.
- Unknown fields are an error.
