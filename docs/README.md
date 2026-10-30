# herdr-vm-worktree documentation

herdr-vm-worktree (hvw) runs each herdr worktree in its own short-lived
KVM VM on a remote Incus host. The VM holds no credential. A gatekeeper
on the host adds the credentials to allowed requests and refuses all
other hosts. When an agent needs a refused host, you approve or deny the
request in herdr.

The documentation follows the [Diátaxis](https://diataxis.fr) framework.

| You want to | Read |
|---|---|
| Learn hvw with a first complete run | [Tutorial](tutorials/first-worktree-vm.md) |
| Do a specific task | [How-to guides](#how-to-guides) |
| Look up a command, an option or a file | [Reference](#reference) |
| Understand the design and its limits | [Explanation](#explanation) |

## Tutorials

- [Your first worktree VM](tutorials/first-worktree-vm.md)

## How-to guides

- [Deploy hvw with clan](how-to/deploy-with-clan.md)
- [Give a VM a credential](how-to/give-a-vm-a-credential.md)
- [Add an agent harness to the image](how-to/add-an-agent-harness.md)
- [Work with a running VM](how-to/work-with-a-vm.md)
- [Develop and test this repo](how-to/develop-this-repo.md)

## Reference

- [Command line](reference/cli.md)
- [Workspace manifest (`hvw.toml`)](reference/workspace-manifest.md)
- [NixOS modules and clan roles](reference/nixos-options.md)
- [VM environment, files and ports](reference/vm-environment.md)

## Explanation

- [Architecture](explanation/architecture.md)
- [Egress escalation](explanation/egress-escalation.md)
- [Security model](explanation/security-model.md)
