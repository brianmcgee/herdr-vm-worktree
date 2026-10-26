# The herdr plugin of herdr-vm-worktree. Link it with
# `herdr plugin link <path>`. The NixOS client module puts it at
# /run/current-system/sw/share/hvw/herdr-plugin.
{ pkgs, flake, ... }:
let
  hvw = flake.packages.${pkgs.stdenv.hostPlatform.system}.hvw;
  manifest = (pkgs.formats.toml { }).generate "herdr-plugin.toml" {
    id = "hvw";
    name = "herdr-vm-worktree";
    version = hvw.version;
    min_herdr_version = "0.9.3";
    description = "Runs each worktree in a VM on an Incus member, and reviews its egress requests";
    platforms = [ "linux" ];
    events = [
      {
        on = "worktree.created";
        command = [
          "${hvw}/bin/hvw"
          "herdr-hook"
        ];
      }
      {
        on = "worktree.removed";
        command = [
          "${hvw}/bin/hvw"
          "herdr-hook"
        ];
      }
    ];
    actions = [
      {
        id = "review";
        title = "Review egress requests";
        command = [
          "${pkgs.runtimeShell}"
          "-c"
          ''exec "$HERDR_BIN_PATH" plugin pane open --plugin "$HERDR_PLUGIN_ID" --entrypoint review''
        ];
      }
    ];
    panes = [
      {
        id = "review";
        title = "Egress requests";
        placement = "popup";
        command = [ "${hvw}/bin/hvw-review" ];
        width = "90%";
        height = "80%";
      }
    ];
  };
in
pkgs.runCommand "hvw-herdr-plugin" { } ''
  mkdir -p $out/share/hvw/herdr-plugin
  cp ${manifest} $out/share/hvw/herdr-plugin/herdr-plugin.toml
''
