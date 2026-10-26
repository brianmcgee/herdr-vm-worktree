# A client of herdr-vm-worktree: the machine that runs the herdr server.
# It gets `hvw`, the herdr plugin and the `hvw-watch` user service, which
# holds the iroh endpoint of the client.
{ flake, ... }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.hvw.client;
  inherit (lib) mkOption types;
  system = pkgs.stdenv.hostPlatform.system;
  linkConfig = (pkgs.formats.json { }).generate "hvw-link.json" cfg.link;
in
{
  options.services.hvw.client = {
    enable = lib.mkEnableOption "a herdr-vm-worktree client";
    user = mkOption {
      type = types.str;
      description = "The user that runs herdr. Only this user gets the `hvw-watch` service.";
    };
    package = mkOption {
      type = types.package;
      default = flake.packages.${system}.hvw;
      defaultText = lib.literalExpression "flake.packages.\${system}.hvw";
      description = "The package with `hvw`, `hvw-watch` and `hvw-review`.";
    };
    plugin = mkOption {
      type = types.package;
      default = flake.packages.${system}.herdr-plugin;
      defaultText = lib.literalExpression "flake.packages.\${system}.herdr-plugin";
      description = "The herdr plugin. Link it once with `herdr plugin link /run/current-system/sw/share/hvw/herdr-plugin`.";
    };
    link = mkOption {
      type = types.attrsOf types.anything;
      description = "The control plane configuration: the `hvw-link` JSON schema. Its peers are the members.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [
      cfg.package
      cfg.plugin
    ];
    environment.pathsToLink = [ "/share/hvw" ];
    environment.etc."hvw/link.json".source = linkConfig;

    systemd.user.services.hvw-watch = {
      description = "herdr-vm-worktree: control plane and egress requests of the client";
      wantedBy = [ "default.target" ];
      unitConfig.ConditionUser = cfg.user;
      # `hvw-watch` calls herdr for tokens, toasts and agent states.
      path = [ pkgs.herdr ];
      restartTriggers = [ linkConfig ];
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/hvw-watch";
        Restart = "always";
        RestartSec = 5;
      };
    };
  };
}
