# The clan service of herdr-vm-worktree. A `member` runs worktree VMs. A
# `client` runs the herdr server, `hvw` and `hvw-watch`. An optional
# `relay` runs a self-hosted iroh relay. Each machine gets an iroh key
# from a clan var. Each machine trusts the endpoint ids of the machines in
# the other role.
{ flake, ... }:
{ clanLib, directory, ... }:
let
  relayOption =
    lib:
    lib.mkOption {
      type = lib.types.nullOr (lib.types.attrsOf lib.types.anything);
      default = null;
      example = {
        mode = "custom";
        urls = [ "https://relay.example.org" ];
      };
      description = ''
        The relay mode of the control plane: `default` (n0 relays),
        `disabled`, or `custom` with `urls`. Null: the machines of the
        `relay` role if there are any, else the n0 relays.
      '';
    };

  # The relay mode of a machine, after `relayOption`.
  relayOf =
    lib: settings: roles:
    let
      hostnames = lib.mapAttrsToList (_: m: m.settings.hostname) (roles.relay.machines or { });
    in
    if settings.relay != null then
      settings.relay
    else if hostnames == [ ] then
      { mode = "default"; }
    else
      {
        mode = "custom";
        urls = map (h: "https://${h}") hostnames;
      };

  # The key generator of a machine. `hvw keygen` writes the secret key and
  # prints the endpoint id.
  keyGenerator = pkgs: {
    files.key = { };
    files.id.secret = false;
    runtimeInputs = [ flake.packages.${pkgs.stdenv.hostPlatform.system}.hvw ];
    script = ''
      hvw keygen --secret-file "$out/key" > "$out/id"
    '';
  };

  # The endpoint id of a machine. Empty before its key is generated. The
  # var file ends with the newline of `hvw keygen`.
  idOf =
    lib: name:
    lib.trim (
      clanLib.getPublicValue {
        flake = directory;
        machine = name;
        generator = "hvw-link";
        file = "id";
        default = "";
      }
    );

  # The peers of a machine: the machines of the other role. A machine
  # without a generated key yet is left out.
  peersOf =
    lib: machines:
    lib.filter (p: p.id != "") (
      lib.mapAttrsToList (name: m: {
        inherit name;
        id = idOf lib name;
        addrs = m.settings.addrs or [ ];
      }) machines
    );
in
{
  _class = "clan.service";
  manifest = {
    name = "hvw";
    description = "herdr worktrees in short-lived Incus VMs behind an egress gatekeeper";
    categories = [ "Development" ];
    readme = builtins.readFile ./README.md;
  };

  roles.member = {
    description = "Runs worktree VMs, their gatekeepers and hvw-agent";
    interface =
      { lib, ... }:
      {
        options = {
          addrs = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [ ];
            example = [ "203.0.113.7:7842" ];
            description = "Direct UDP addresses of the member. The clients use them before the relay.";
          };
          bind = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "0.0.0.0:7842";
            description = "The UDP address of the control plane. Null: any port.";
          };
          relay = relayOption lib;
          policies = lib.mkOption {
            type = lib.types.attrsOf lib.types.anything;
            default = { };
            description = "The egress policies, by name. See `services.hvw.member.policies`.";
          };
          credentials = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [ ];
            example = [ "github" ];
            description = ''
              The names of the credentials that the policies use. Each
              becomes a shared clan var `hvw-credential-<name>` with a
              prompt.
            '';
          };
          extraConfig = lib.mkOption {
            type = lib.types.attrsOf lib.types.anything;
            default = { };
            description = "More settings of `services.hvw.member`.";
          };
        };
      };
    perInstance =
      { settings, roles, ... }:
      {
        nixosModule =
          {
            config,
            lib,
            pkgs,
            ...
          }:
          let
            gens = config.clan.core.vars.generators;
          in
          {
            imports = [ flake.nixosModules.member ];

            clan.core.vars.generators = {
              hvw-link = keyGenerator pkgs;
            }
            // lib.genAttrs' settings.credentials (
              name:
              lib.nameValuePair "hvw-credential-${name}" {
                share = true;
                files.secret = { };
                prompts.secret = {
                  type = "hidden";
                  persist = true;
                  description = "The credential ${name} of the hvw gatekeepers";
                };
                script = ''cp "$prompts/secret" "$out/secret"'';
              }
            );

            services.hvw.member = lib.mkMerge [
              {
                enable = true;
                link = {
                  secret_key_file = gens.hvw-link.files.key.path;
                  relay = relayOf lib settings roles;
                  peers = peersOf lib (roles.client.machines or { });
                }
                // lib.optionalAttrs (settings.bind != null) { inherit (settings) bind; };
                inherit (settings) policies;
                credentials = lib.genAttrs settings.credentials (
                  name: gens."hvw-credential-${name}".files.secret.path
                );
              }
              settings.extraConfig
            ];
          };
      };
  };

  roles.client = {
    description = "Runs hvw, hvw-watch and the herdr plugin next to the herdr server";
    interface =
      { lib, ... }:
      {
        options = {
          relay = relayOption lib;
          user = lib.mkOption {
            type = lib.types.str;
            description = "The user that runs herdr. It gets the key and the hvw-watch user service.";
          };
          extraConfig = lib.mkOption {
            type = lib.types.attrsOf lib.types.anything;
            default = { };
            description = "More settings of `services.hvw.client`.";
          };
        };
      };
    perInstance =
      { settings, roles, ... }:
      {
        nixosModule =
          {
            config,
            lib,
            pkgs,
            ...
          }:
          let
            gens = config.clan.core.vars.generators;
          in
          {
            imports = [ flake.nixosModules.client ];

            clan.core.vars.generators.hvw-link = lib.recursiveUpdate (keyGenerator pkgs) {
              files.key.owner = settings.user;
            };

            services.hvw.client = lib.mkMerge [
              {
                enable = true;
                inherit (settings) user;
                link = {
                  secret_key_file = gens.hvw-link.files.key.path;
                  relay = relayOf lib settings roles;
                  peers = peersOf lib (roles.member.machines or { });
                };
              }
              settings.extraConfig
            ];
          };
      };
  };

  roles.relay = {
    description = "Runs a self-hosted iroh relay that only the machines of this service may use";
    interface =
      { lib, ... }:
      {
        options = {
          hostname = lib.mkOption {
            type = lib.types.str;
            example = "relay.example.org";
            description = "The public DNS name of the relay. It needs ports 80 and 443 for Let's Encrypt.";
          };
          contact = lib.mkOption {
            type = lib.types.str;
            example = "admin@example.org";
            description = "The contact email address for Let's Encrypt.";
          };
        };
      };
    perInstance =
      { settings, roles, ... }:
      {
        nixosModule =
          { lib, ... }:
          {
            imports = [ flake.nixosModules.relay ];
            services.hvw.relay = {
              enable = true;
              inherit (settings) hostname contact;
              allow = lib.filter (id: id != "") (
                map (idOf lib) (
                  lib.attrNames (roles.member.machines or { }) ++ lib.attrNames (roles.client.machines or { })
                )
              );
            };
          };
      };
  };
}
