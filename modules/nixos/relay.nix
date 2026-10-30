# A self-hosted iroh relay for the control plane. Only the endpoints of
# the clan service may use it. It gets its certificate from Let's Encrypt,
# so it needs a public DNS name and ports 80 and 443.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.hvw.relay;
  inherit (lib) mkOption types;
  stateDir = "/var/lib/hvw-relay";
  relayConfig = (pkgs.formats.toml { }).generate "iroh-relay.toml" {
    enable_relay = true;
    http_bind_addr = "[::]:80";
    enable_quic_addr_discovery = true;
    tls = {
      hostname = cfg.hostname;
      cert_mode = "LetsEncrypt";
      cert_dir = "${stateDir}/certs";
      contact = cfg.contact;
    };
    access.allowlist = cfg.allow;
  };
in
{
  options.services.hvw.relay = {
    enable = lib.mkEnableOption "a self-hosted iroh relay for herdr-vm-worktree";
    package = mkOption {
      type = types.package;
      default = pkgs.iroh-relay;
      defaultText = lib.literalExpression "pkgs.iroh-relay";
      description = "The iroh-relay package.";
    };
    hostname = mkOption {
      type = types.str;
      example = "relay.example.org";
      description = "The public DNS name of the relay. The peers use `https://<hostname>`.";
    };
    contact = mkOption {
      type = types.str;
      example = "admin@example.org";
      description = "The contact email address for Let's Encrypt.";
    };
    allow = mkOption {
      type = types.listOf types.str;
      description = "The endpoint ids (64 hex digits) that may use the relay.";
    };
  };

  config = lib.mkIf cfg.enable {
    # 80: HTTP and the ACME challenge. 443: the relay. 7842/udp: QUIC
    # address discovery.
    networking.firewall.allowedTCPPorts = [
      80
      443
    ];
    networking.firewall.allowedUDPPorts = [ 7842 ];

    systemd.services.hvw-relay = {
      description = "iroh relay of herdr-vm-worktree";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      restartTriggers = [ relayConfig ];
      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "iroh-relay"} --config-path ${relayConfig}";
        Restart = "always";
        DynamicUser = true;
        StateDirectory = "hvw-relay";
        WorkingDirectory = stateDir;
        AmbientCapabilities = [ "CAP_NET_BIND_SERVICE" ];
        CapabilityBoundingSet = [ "CAP_NET_BIND_SERVICE" ];
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
      };
    };
  };
}
