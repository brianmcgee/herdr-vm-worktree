# The clan service in a small clan: one member, one client, one relay.
# The check evaluates the three machines and the wiring between them. The
# fixture holds the public `id` vars, as `clan vars generate` writes them.
{
  pkgs,
  inputs,
  flake,
  ...
}:
let
  inherit (pkgs) lib;

  clan = inputs.clan-core.lib.clan {
    self = flake;
    directory = ../tests/clan-fixture;
    modules.hvw = flake.modules.clan.hvw;
    inventory = {
      meta.name = "hvw-test";
      meta.domain = "hvw-test";
      machines = {
        m1 = { };
        laptop = { };
        relay1 = { };
      };
      instances.hvw = {
        module.name = "hvw";
        module.input = "self";
        roles.member.machines.m1.settings = {
          bind = "0.0.0.0:7842";
          addrs = [ "203.0.113.7:7842" ];
          credentials = [ "github" ];
          policies.github = {
            domains = [ "github.com" ];
            secrets = [
              {
                credential = "github";
                placeholder = "hvw-github-placeholder";
                hosts = [ "github.com" ];
                env = [ "GH_TOKEN" ];
              }
            ];
          };
        };
        roles.client.machines.laptop.settings.user = "alice";
        roles.relay.machines.relay1.settings = {
          hostname = "relay.example.org";
          contact = "admin@example.org";
        };
      };
    };
    machines =
      lib.genAttrs
        [
          "m1"
          "laptop"
          "relay1"
        ]
        (_: {
          nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
          fileSystems."/" = {
            device = "/dev/vda";
            fsType = "ext4";
          };
          boot.loader.grub.devices = [ "/dev/vda" ];
          users.users.alice.isNormalUser = true;
          system.stateVersion = lib.trivial.release;
        });
  };

  machines = clan.config.nixosConfigurations;
  cfgOf = name: machines.${name}.config;

  id = c: lib.strings.replicate 64 c;
  relay = {
    mode = "custom";
    urls = [ "https://relay.example.org" ];
  };

  expect =
    what: actual: expected:
    lib.assertMsg (
      actual == expected
    ) "${what}: expected ${builtins.toJSON expected}, got ${builtins.toJSON actual}";

  member = (cfgOf "m1").services.hvw.member;
  client = (cfgOf "laptop").services.hvw.client;
in
assert expect "the peers of the member" member.link.peers [
  {
    name = "laptop";
    id = id "b";
    addrs = [ ];
  }
];
assert expect "the peers of the client" client.link.peers [
  {
    name = "m1";
    id = id "a";
    addrs = [ "203.0.113.7:7842" ];
  }
];
assert expect "the relay of the member" member.link.relay relay;
assert expect "the relay of the client" client.link.relay relay;
assert expect "the bind address of the member" member.link.bind "0.0.0.0:7842";
assert expect "the allowlist of the relay" (cfgOf "relay1").services.hvw.relay.allow [
  (id "a")
  (id "b")
];
assert expect "the credentials of the member" (builtins.attrNames member.credentials) [ "github" ];
# The paths of the system derivations force a full evaluation. Without
# their context, the check does not build the systems.
pkgs.writeText "hvw-clan-eval" (
  builtins.unsafeDiscardStringContext (
    lib.concatMapStringsSep "\n" (name: machines.${name}.config.system.build.toplevel.drvPath) [
      "m1"
      "laptop"
      "relay1"
    ]
  )
)
