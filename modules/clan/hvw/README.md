Run each herdr worktree in its own short-lived Incus VM on a member. An
egress gatekeeper on the member injects credentials and enforces a domain
allowlist. The VM holds no credential.

## Roles

- `member`: Incus, `hvw-agent`, the `iron-proxy@` gatekeepers, the egress
  policies and their credentials. The member needs nested KVM and one
  btrfs filesystem for `/nix` and `/var/lib/hvw`.
- `client`: `hvw`, the `hvw-watch` user service and the herdr plugin, for
  the user that runs herdr.
- `relay`: an optional self-hosted iroh relay. It needs a public DNS name
  (`hostname`) and ports 80, 443 and 7842/udp. When the instance has a
  relay, the members and clients use it instead of the n0 relays.

Each machine gets an iroh key from the clan var `hvw-link`. A member
trusts the clients of the instance. A client trusts the members.

## Example

```nix
clan.inventory.instances.hvw = {
  module.input = "hvw";
  roles.member.machines.forge.settings = {
    credentials = [ "github" ];
    policies.github = {
      domains = [ "github.com" "api.github.com" "codeload.github.com" ];
      secrets = [
        {
          credential = "github";
          placeholder = "hvw-github-placeholder";
          hosts = [ "api.github.com" "github.com" ];
          env = [ "GH_TOKEN" ];
        }
      ];
    };
  };
  roles.client.machines.laptop.settings.user = "alice";
};
```

`clan vars generate` asks for the value of each credential once.

## The agent harness

The default image holds no agent harness. Build an image with
`hvw.image.packages` and give it to the members:

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

The harness runs in a login shell in the first repo of the worktree.
The image puts the `egress-request` skill in `~/.claude/skills`,
`~/.codex/skills` and `~/.agents/skills` of the harness user.
