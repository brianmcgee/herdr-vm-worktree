# A NixOS test node whose root filesystem is btrfs and holds its own Nix
# store. The default test node mounts the host store over 9p, and a reflink
# from 9p to btrfs is not possible. Farms need `cp --reflink=always` from
# /nix/store to /var/lib/hvw, so both must be on one btrfs filesystem.
#
# The root image holds the closure of `virtualisation.additionalPaths`. The
# test runner puts a qcow2 overlay with free space on top of it.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:
let
  rootImage = pkgs.callPackage "${modulesPath}/../lib/make-btrfs-fs.nix" {
    storePaths = config.virtualisation.additionalPaths;
    volumeLabel = "hvwroot";
  };

  # The drive option `file` goes into the shell script of the runner, so a
  # command substitution there makes the overlay before qemu starts.
  overlay = pkgs.writeShellScript "hvw-root-overlay" ''
    set -eu
    img="$TMPDIR/hvwroot.qcow2"
    if [ ! -e "$img" ]; then
      ${pkgs.qemu-utils}/bin/qemu-img create -q -f qcow2 -F raw -b ${rootImage} "$img" ${toString config.virtualisation.diskSize}M >&2
    fi
    echo "$img"
  '';
in
{
  virtualisation = {
    diskImage = null;
    mountHostNixStore = false;
    useNixStoreImage = false;
    writableStore = false;
    fileSystems."/" = lib.mkForce {
      device = "/dev/disk/by-label/hvwroot";
      fsType = "btrfs";
    };
    qemu.drives = [
      {
        name = "hvwroot";
        file = ''"$(${overlay})"'';
        driveExtraOpts = {
          format = "qcow2";
          cache = "unsafe";
          werror = "report";
        };
      }
    ];
  };

  boot.initrd.supportedFilesystems.btrfs = true;

  # make-btrfs-fs writes the registration of the closure to the root.
  systemd.services.hvw-test-register-store = {
    unitConfig.DefaultDependencies = false;
    wantedBy = [ "sysinit.target" ];
    before = [
      "sysinit.target"
      "shutdown.target"
      "nix-daemon.socket"
      "nix-daemon.service"
    ];
    after = [ "local-fs.target" ];
    conflicts = [ "shutdown.target" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    script = ''
      if [ -e /nix-path-registration ]; then
        ${config.nix.package.out}/bin/nix-store --load-db < /nix-path-registration
        rm /nix-path-registration
      fi
      ${pkgs.btrfs-progs}/bin/btrfs filesystem resize max /
    '';
  };
}
