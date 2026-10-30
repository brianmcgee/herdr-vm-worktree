# Deploy hvw with clan

## Before you start

- A member host with nested KVM. Hetzner Cloud servers do not have it.
  Dedicated servers do.
- On the member: `/nix` and `/var/lib/hvw` on one btrfs filesystem.
- On the client: the herdr server, version 0.9.3 or later.

## 1. Add the flake input

In the flake of your clan:

```nix
inputs.hvw.url = "github:brianmcgee/herdr-vm-worktree";
inputs.hvw.inputs.nixpkgs.follows = "nixpkgs";
inputs.hvw.inputs.clan-core.follows = "clan-core";
```

## 2. Add an instance

```nix
clan.inventory.instances.hvw = {
  module.input = "hvw";
  roles.member.machines.forge.settings = {
    bind = "0.0.0.0:7842";
    addrs = [ "203.0.113.7:7842" ];
    policies.default.domains = [ "cache.nixos.org" ];
  };
  roles.client.machines.laptop.settings.user = "alice";
};
```

`bind` and `addrs` are optional. Without them, the client reaches the
member through a relay.

## 3. Generate the vars and deploy

```sh
clan vars generate
clan machines update forge laptop
```

Each machine gets an iroh key. A new machine in the instance changes the
peers of the others, so deploy all of them.

## 4. Link the herdr plugin on the client

Run this once, as the herdr user:

```sh
herdr plugin link /run/current-system/sw/share/hvw/herdr-plugin
```

In the herdr configuration, add `$egress` to the sidebar row format to
show pending requests. Bind a key to the plugin action `review` to open
the popup.

## 5. Check

```sh
systemctl --user status hvw-watch    # on the client
systemctl status hvw-agent           # on the member
hvw list                             # on the client: no error
```

## Optional: a self-hosted relay

Add a machine with a public DNS name and open ports 80, 443 and
7842/udp:

```nix
roles.relay.machines.relay1.settings = {
  hostname = "relay.example.org";
  contact = "admin@example.org";
};
```

The members and clients of the instance use it instead of the n0 relays.

## Without clan

Import `nixosModules.member` and `nixosModules.client`. Set
`services.hvw.member.link` and `services.hvw.client.link` yourself: a key
file from `hvw keygen` and the peers of the other side. See the
[reference](../reference/nixos-options.md).
