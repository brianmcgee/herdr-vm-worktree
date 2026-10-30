# NixOS modules and clan roles

## Flake outputs

| Output | Content |
|---|---|
| `clan.modules.hvw` | The clan service, with the roles `member`, `client` and `relay`. |
| `nixosModules.member` | `services.hvw.member` |
| `nixosModules.client` | `services.hvw.client` |
| `nixosModules.relay` | `services.hvw.relay` |
| `nixosModules.image` | `hvw.image`, the base image of the VMs |
| `packages.<system>.hvw` | `hvw`, `hvw-watch`, `hvw-review`, `hvw-agent`, `egress-request` |
| `packages.<system>.default` | The same as `hvw`. `nix run` opens the terminal UI. |
| `packages.<system>.hvw-image` | The default base image |
| `packages.<system>.herdr-plugin` | The herdr plugin |
| `packages.<system>.iron-proxy` | The gatekeeper |

The systems are `x86_64-linux` and `aarch64-linux`.

## Clan roles

The clan service makes the iroh key of each machine (var `hvw-link`) and
fills in the peers. Members trust the clients. Clients trust the members.

### `member`

| Setting | Default | Meaning |
|---|---|---|
| `addrs` | `[]` | Direct UDP addresses of the member, for example `203.0.113.7:7842`. Clients try them before the relay. |
| `bind` | `null` | The UDP address of the control plane. The firewall opens its port. `null`: any port. |
| `relay` | `null` | `{ mode = "default"; }`, `{ mode = "disabled"; }` or `{ mode = "custom"; urls = [...]; }`. `null`: the `relay` machines, else the n0 relays. |
| `policies` | `{}` | See `services.hvw.member.policies`. |
| `credentials` | `[]` | Credential names. Each becomes a shared, prompted clan var `hvw-credential-<name>`. |
| `extraConfig` | `{}` | More `services.hvw.member` settings. |

### `client`

| Setting | Default | Meaning |
|---|---|---|
| `user` | required | The user that runs herdr. It owns the key and runs `hvw-watch`. |
| `relay` | `null` | As for `member`. |
| `extraConfig` | `{}` | More `services.hvw.client` settings. |

### `relay`

| Setting | Meaning |
|---|---|
| `hostname` | The public DNS name. Needs TCP ports 80 and 443 and UDP port 7842. |
| `contact` | The Let's Encrypt contact address. |

Only the members and clients of the instance may use the relay.

## `services.hvw.member`

| Option | Default | Meaning |
|---|---|---|
| `enable` | `false` | Run a member. |
| `image` | `hvw-image` package | The base image: `metadata.tar.xz` and `disk.qcow2`. |
| `bridge.name` | `hvwbr0` | The Incus bridge of the VMs. |
| `bridge.address` | `10.250.0.1` | The member address on the bridge. VM slot *n* gets the address *n* + 1 after it. |
| `bridge.prefixLength` | `24` | The prefix length of the bridge network. |
| `storagePool` | `hvw` | The Incus btrfs storage pool. |
| `package` | `hvw` package | The package with `hvw-agent`. |
| `ironProxy` | `iron-proxy` package | The gatekeeper package. |
| `name` | `networking.hostName` | The peer name of the member on the control plane. |
| `link` | — | The `hvw-link` configuration: `secret_key_file`, `peers`, `bind`, `relay`. The clan service sets it. |
| `brokerPort` | `7999` | The egress broker port on the bridge address. |
| `gatekeeperPortBase` | `20000` | The first of five ranges of 1000 gatekeeper ports. |
| `farmRetentionDays` | `14` | `hvw-agent` deletes a farm that no VM used for this many days. |
| `vm.cpus` | `2` | vCPUs of each VM. |
| `vm.memory` | `4GiB` | Memory of each VM, in Incus syntax. |
| `vm.harness` | `[ "/run/current-system/sw/bin/bash" "--login" ]` | The agent harness command. `hvw attach` runs it in the first repo, after the devshell is in `PATH`. |
| `credentials` | `{}` | Credential files by name. Each gatekeeper loads them as systemd credentials. |
| `policies` | `{}` | Egress policies by name. See below. |

### A policy

```nix
services.hvw.member.policies.github = {
  domains = [ "github.com" "codeload.github.com" ];      # domain globs
  rules = [ { host = "api.github.com"; methods = [ "GET" ]; paths = [ "/repos/*" ]; } ];
  secrets = [
    {
      credential = "github";                # a name in `credentials`
      placeholder = "hvw-github-placeholder";
      hosts = [ "api.github.com" "github.com" ];
      headers = [ "Authorization" ];        # the default
      env = [ "GH_TOKEN" ];                 # VM variables that hold the placeholder
    }
  ];
  env = { GIT_TERMINAL_PROMPT = "0"; };     # plain variables, never a secret
};
```

The member needs nested KVM, and `/nix` and `/var/lib/hvw` on one btrfs
filesystem.

## `services.hvw.client`

| Option | Default | Meaning |
|---|---|---|
| `enable` | `false` | Run a client. |
| `user` | required | The user that runs herdr. Only this user gets `hvw-watch`. |
| `package` | `hvw` package | The package with `hvw`, `hvw-watch` and `hvw-review`. |
| `plugin` | `herdr-plugin` package | The herdr plugin, at `/run/current-system/sw/share/hvw/herdr-plugin`. |
| `link` | — | The `hvw-link` configuration. Its peers are the members. |

## `services.hvw.relay`

| Option | Default | Meaning |
|---|---|---|
| `enable` | `false` | Run an iroh relay. |
| `package` | `pkgs.iroh-relay` | The relay package. |
| `hostname` | required | The public DNS name. |
| `contact` | required | The Let's Encrypt contact address. |
| `allow` | required | The endpoint ids that may use the relay. |

## `hvw.image` (module `image`)

| Option | Default | Meaning |
|---|---|---|
| `user` | `agent` | The user that owns `/work` and runs the harness. |
| `uid` | `1000` | The uid of that user. |
| `packages` | `[]` | Extra packages, for example the agent harness. |
| `image` | read-only | The resulting Incus image. Give it to `services.hvw.member.image`. |

`hvw-agent` runs git and the harness as user `agent` with uid 1000.
Keep the defaults of `user` and `uid`.
