# A member: it runs worktree VMs on its own Incus, their gatekeepers
# and hvw-agent.
{ flake, ... }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.hvw.member;
  inherit (lib) mkOption types;
  system = pkgs.stdenv.hostPlatform.system;
in
{
  options.services.hvw.member = {
    enable = lib.mkEnableOption "a herdr-vm-worktree member";

    image = mkOption {
      type = types.package;
      default = flake.packages.${system}.hvw-image;
      defaultText = lib.literalExpression "flake.packages.\${system}.hvw-image";
      description = "The base image of the worktree VMs: a directory with `metadata.tar.xz` and `disk.qcow2`.";
    };

    bridge = {
      name = mkOption {
        type = types.str;
        default = "hvwbr0";
        description = "The name of the Incus bridge of the worktree VMs.";
      };
      address = mkOption {
        type = types.str;
        default = "10.250.0.1";
        description = "The IPv4 address of the member on the bridge. The gatekeepers and the brokers listen on it.";
      };
      prefixLength = mkOption {
        type = types.ints.between 8 30;
        default = 24;
        description = "The prefix length of the bridge network.";
      };
    };

    storagePool = mkOption {
      type = types.str;
      default = "hvw";
      description = "The name of the Incus btrfs storage pool of the worktree VMs.";
    };

    package = mkOption {
      type = types.package;
      default = flake.packages.${system}.hvw;
      defaultText = lib.literalExpression "flake.packages.\${system}.hvw";
      description = "The package with `hvw-agent`.";
    };

    ironProxy = mkOption {
      type = types.package;
      default = flake.packages.${system}.iron-proxy;
      defaultText = lib.literalExpression "flake.packages.\${system}.iron-proxy";
      description = "The iron-proxy package of the gatekeepers.";
    };

    name = mkOption {
      type = types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = "The peer name of this member on the control plane.";
    };

    link = mkOption {
      type = types.submodule {
        freeformType = (pkgs.formats.json { }).type;
        options.secret_key_file = mkOption {
          type = types.str;
          description = "The file with the iroh secret key of this member.";
        };
        options.peers = mkOption {
          type = types.listOf (types.attrsOf types.anything);
          default = [ ];
          description = "The clients that may use this member: `{ name, id, addrs }`.";
        };
      };
      description = "The control plane configuration: the `hvw-link` JSON schema.";
    };

    brokerPort = mkOption {
      type = types.port;
      default = 7999;
      description = "The port of the egress broker on the bridge address.";
    };

    gatekeeperPortBase = mkOption {
      type = types.port;
      default = 20000;
      description = "The first port of the gatekeeper port ranges: five ranges of 1000 ports.";
    };

    farmRetentionDays = mkOption {
      type = types.ints.positive;
      default = 14;
      description = "Farms that no VM used for this many days are deleted.";
    };

    vm = {
      cpus = mkOption {
        type = types.ints.positive;
        default = 2;
        description = "The vCPUs of each worktree VM.";
      };
      memory = mkOption {
        type = types.str;
        default = "4GiB";
        description = "The memory of each worktree VM, in Incus syntax.";
      };
      harness = mkOption {
        type = types.listOf types.str;
        # The devshell puts a bash without readline first in PATH, so the
        # default names the interactive bash of the image.
        default = [
          "/run/current-system/sw/bin/bash"
          "--login"
        ];
        description = "The command of the agent harness in the VM. `hvw attach` runs it in the first repo, after the devshell is in PATH.";
      };
    };

    credentials = mkOption {
      type = types.attrsOf types.path;
      default = { };
      example = lib.literalExpression "{ github = config.clan.core.vars.generators.github.files.token.path; }";
      description = ''
        Credential files by name. Each gatekeeper loads them as systemd
        credentials. The `credential` field of a policy secret names one.
      '';
    };

    policies = mkOption {
      type = types.attrsOf (
        types.submodule {
          options = {
            domains = mkOption {
              type = types.listOf types.str;
              default = [ ];
              description = "Domain globs that the VM may reach.";
            };
            rules = mkOption {
              type = types.listOf (types.attrsOf types.anything);
              default = [ ];
              description = "Allowlist rules: `{ host, methods ? [], paths ? [] }`.";
            };
            secrets = mkOption {
              type = types.listOf (types.attrsOf types.anything);
              default = [ ];
              description = "Secret mappings: `{ credential, placeholder, hosts, headers ? [\"Authorization\"], env ? [] }`.";
            };
            env = mkOption {
              type = types.attrsOf types.str;
              default = { };
              description = "Plain environment variables of the VM. No secret goes here.";
            };
          };
        }
      );
      default = { };
      description = "The egress policies, by name. A repo in a workspace manifest names one.";
    };
  };

  config = lib.mkIf cfg.enable (
    let
      json = pkgs.formats.json { };
      stateDir = "/var/lib/hvw";
      # The alias holds the hash of the image, so a new image gets a new
      # alias and running VMs keep the old one.
      imageAlias = "hvw-" + builtins.substring 0 12 (baseNameOf cfg.image);
      policiesDir = pkgs.linkFarm "hvw-policies" (
        lib.mapAttrsToList (name: p: {
          name = "${name}.json";
          path = json.generate "hvw-policy-${name}.json" p;
        }) cfg.policies
      );
      linkConfig = json.generate "hvw-link.json" cfg.link;
      agentConfig = json.generate "hvw-agent.json" {
        inherit (cfg) name;
        link = linkConfig;
        state_dir = stateDir;
        policies_dir = policiesDir;
        incus_socket = "/var/lib/incus/unix.socket";
        image = imageAlias;
        storage_pool = cfg.storagePool;
        bridge = {
          inherit (cfg.bridge) name address;
          prefix_length = cfg.bridge.prefixLength;
        };
        broker_port = cfg.brokerPort;
        gatekeeper_port_base = cfg.gatekeeperPortBase;
        farm_retention_days = cfg.farmRetentionDays;
        vm = {
          inherit (cfg.vm) cpus memory harness;
          user = "agent";
          uid = 1000;
          gid = 100;
        };
      };
      tunnelPorts = {
        from = cfg.gatekeeperPortBase;
        to = cfg.gatekeeperPortBase + 999;
      };
    in
    {
      assertions = [
        {
          assertion = lib.all (p: lib.all (s: cfg.credentials ? ${s.credential}) p.secrets) (
            lib.attrValues cfg.policies
          );
          message = "services.hvw.member: a policy secret names a credential that services.hvw.member.credentials does not define";
        }
      ];

      networking.nftables.enable = true;

      # Incus makes a tap device for the NIC of each VM.
      boot.kernelModules = [ "tun" ];

      # The VMs reach DHCP, DNS, the gatekeeper tunnels and the broker on
      # the bridge. The network ACL of each VM narrows this to its own
      # gatekeeper.
      #
      # The ACL loads bridge conntrack. Then the DHCP broadcasts of a VM
      # come into the IP stack with the conntrack state `invalid`, and the
      # NixOS firewall drops all `invalid` packets before its port rules.
      # Thus the NixOS firewall trusts the bridge, and this table filters
      # it. A drop in any table is final.
      networking.firewall.trustedInterfaces = [ cfg.bridge.name ];
      networking.nftables.tables.hvw-bridge = {
        family = "inet";
        content = ''
          chain input {
            type filter hook input priority filter; policy accept;
            iifname != "${cfg.bridge.name}" accept
            udp sport 68 udp dport 67 accept
            ct state established,related accept
            meta l4proto { tcp, udp } th dport 53 accept
            tcp dport ${toString cfg.brokerPort} accept
            tcp dport ${toString tunnelPorts.from}-${toString tunnelPorts.to} accept
            drop
          }
        '';
      };

      # Clients reach a fixed control plane port directly. Without `bind`,
      # the link uses a random port and the relay.
      networking.firewall.allowedUDPPorts = lib.optional (cfg.link ? bind) (
        lib.toInt (lib.last (lib.splitString ":" cfg.link.bind))
      );

      virtualisation.incus = {
        enable = true;
        preseed = {
          storage_pools = [
            {
              name = cfg.storagePool;
              driver = "btrfs";
              config.source = "/var/lib/incus/storage-pools/${cfg.storagePool}";
            }
          ];
          networks = [
            {
              name = cfg.bridge.name;
              type = "bridge";
              config = {
                "ipv4.address" = "${cfg.bridge.address}/${toString cfg.bridge.prefixLength}";
                "ipv4.nat" = "false";
                "ipv6.address" = "none";
              };
            }
          ];
        };
      };

      users.users.hvw-gatekeeper = {
        isSystemUser = true;
        group = "hvw-gatekeeper";
      };
      users.groups.hvw-gatekeeper = { };

      systemd.tmpfiles.settings."10-hvw".${stateDir}.d.mode = "0700";

      environment.etc."hvw/agent.json".source = agentConfig;

      systemd.services.hvw-image-import = {
        description = "Import the base image of the worktree VMs into Incus";
        after = [ "incus-preseed.service" ];
        requires = [ "incus-preseed.service" ];
        wantedBy = [ "multi-user.target" ];
        path = [ config.virtualisation.incus.package.client ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
        };
        script = ''
          if ! incus image info ${imageAlias} > /dev/null 2>&1; then
            incus image import ${cfg.image}/metadata.tar.xz ${cfg.image}/disk.qcow2 --alias ${imageAlias}
          fi
        '';
      };

      systemd.services.hvw-agent = {
        description = "herdr-vm-worktree member agent";
        after = [
          "hvw-image-import.service"
          "network-online.target"
        ];
        wants = [ "network-online.target" ];
        requires = [ "hvw-image-import.service" ];
        wantedBy = [ "multi-user.target" ];
        restartTriggers = [ agentConfig ];
        path = [
          config.virtualisation.incus.package.client
          config.nix.package
          cfg.ironProxy
          pkgs.btrfs-progs
          pkgs.util-linux
          pkgs.coreutils
          pkgs.git
          pkgs.systemd
        ];
        environment.RUST_LOG = "info";
        serviceConfig = {
          ExecStart = "${cfg.package}/bin/hvw-agent --config /etc/hvw/agent.json";
          Restart = "on-failure";
          StateDirectory = "hvw";
          StateDirectoryMode = "0700";
        };
      };

      # One gatekeeper per VM. `hvw-agent` writes its files and starts it.
      systemd.services."iron-proxy@" = {
        description = "Egress gatekeeper of the worktree VM %i";
        serviceConfig = {
          User = "hvw-gatekeeper";
          Group = "hvw-gatekeeper";
          ExecStart = "${lib.getExe cfg.ironProxy} -config ${stateDir}/vms/%i/gatekeeper/config.json";
          EnvironmentFile = "${stateDir}/vms/%i/gatekeeper/env";
          LoadCredential = lib.mapAttrsToList (n: f: "${n}:${f}") cfg.credentials;
          Restart = "on-failure";
          # The gatekeeper reads its files in the state directory of
          # hvw-agent. It needs no other file and no privilege.
          BindReadOnlyPaths = [ "${stateDir}/vms/%i/gatekeeper" ];
          TemporaryFileSystem = "${stateDir}:ro";
          NoNewPrivileges = true;
          PrivateTmp = true;
          PrivateDevices = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          ProtectKernelTunables = true;
          ProtectKernelModules = true;
          ProtectControlGroups = true;
          RestrictAddressFamilies = [
            "AF_INET"
            "AF_INET6"
            "AF_UNIX"
          ];
          RestrictNamespaces = true;
          LockPersonality = true;
          MemoryDenyWriteExecute = true;
          SystemCallArchitectures = "native";
          CapabilityBoundingSet = "";
        };
      };
    }
  );
}
