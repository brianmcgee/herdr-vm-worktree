# Add an agent harness to the image

The default image runs its own interactive bash
(`/run/current-system/sw/bin/bash --login`) and holds no agent. Build
your own image with the harness and give it to the members.

```nix
roles.member.machines.forge.settings.extraConfig = {
  image =
    (inputs.hvw.inputs.nixpkgs.lib.nixosSystem {
      modules = [
        inputs.hvw.nixosModules.image
        ({ pkgs, ... }: {
          nixpkgs.hostPlatform = "x86_64-linux";
          hvw.image.packages = [ pkgs.claude-code ];
        })
      ];
    }).config.hvw.image.image;
  vm.harness = [ "claude" ];
};
```

- `hvw.image.packages` adds the harness to the image.
- `vm.harness` is the command that `hvw attach` runs in the first repo.
- The image puts the `egress-request` skill in `~/.claude/skills`,
  `~/.codex/skills` and `~/.agents/skills`.

The harness needs its own credential, for example an API key. Give it
through a policy, as in [Give a VM a credential](give-a-vm-a-credential.md).
Add the API host to `domains`.

A new image gets a new Incus alias. Running VMs keep the old image. New
VMs get the new one.
