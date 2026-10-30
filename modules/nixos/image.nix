# The base image of a worktree VM. One generic NixOS image for all
# worktrees. It holds no repo devshell and no credential.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:
let
  cfg = config.hvw.image;

  # `hvw-agent` runs this once after the boot, with the repo names as
  # arguments. It needs /run/hvw/ca.crt and the farm disks of the repos.
  vmSetup = pkgs.writeShellApplication {
    name = "hvw-vm-setup";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.util-linux
      config.nix.package
    ];
    text = ''
      cat /etc/ssl/certs/ca-certificates.crt /run/hvw/ca.crt > /run/hvw/ca-bundle.crt

      # The farms are the lower layers of /nix/store, above the store of
      # the image. Store paths are unique, so the layers do not conflict.
      lower=""
      for repo in "$@"; do
        farm="/run/hvw/farms/$repo"
        if [ ! -d "$farm/store" ]; then
          echo "hvw-vm-setup: the farm of $repo is not mounted at $farm" >&2
          exit 1
        fi
        lower="$lower$farm/store:"
      done
      if [ -n "$lower" ] && [ ! -e /run/hvw/store-overlay ]; then
        mkdir -p /nix/.rw-store/upper /nix/.rw-store/work
        # The upper layer is on the root disk: the store of the image is a
        # read-only bind mount.
        mount -t overlay overlay \
          -o "lowerdir=''${lower}/nix/store,upperdir=/nix/.rw-store/upper,workdir=/nix/.rw-store/work" \
          /nix/store
        touch /run/hvw/store-overlay
      fi
      for repo in "$@"; do
        nix-store --load-db < "/run/hvw/farms/$repo/registration"
      done
    '';
  };
in
{
  imports = [ "${modulesPath}/virtualisation/incus-virtual-machine.nix" ];

  options.hvw.image = {
    user = lib.mkOption {
      type = lib.types.str;
      default = "agent";
      description = "The user that owns /work and runs the agent harness.";
    };

    uid = lib.mkOption {
      type = lib.types.int;
      default = 1000;
      description = "The uid of the harness user. `hvw-agent` runs git and the harness with this uid.";
    };

    image = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = "The Incus image: `metadata.tar.xz` and `disk.qcow2`.";
    };
  };

  config = {
    system.stateVersion = lib.trivial.release;
    networking.hostName = lib.mkDefault "hvw";

    # The VM has no login: Incus exec is the only way in.
    users.users.root.hashedPassword = lib.mkForce "!";
    users.users.root.initialHashedPassword = lib.mkForce null;
    services.openssh.enable = lib.mkForce false;
    services.getty.autologinUser = lib.mkForce null;
    users.mutableUsers = false;
    users.allowNoPasswordLogin = true;

    users.users.${cfg.user} = {
      isNormalUser = true;
      inherit (cfg) uid;
      home = "/home/${cfg.user}";
      hashedPassword = "!";
    };

    systemd.tmpfiles.settings."10-hvw-work"."/work".d = {
      user = cfg.user;
      group = "users";
      mode = "0755";
    };

    networking = {
      useDHCP = false;
      dhcpcd.enable = false;
      useHostResolvConf = false;
      firewall.enable = false;
    };
    systemd.network = {
      enable = true;
      networks."50-ether" = {
        matchConfig.Type = "ether";
        networkConfig.DHCP = "ipv4";
        linkConfig.RequiredForOnline = "routable";
      };
    };

    documentation.enable = false;
    documentation.nixos.enable = false;

    environment.systemPackages = [
      pkgs.git
      pkgs.curl
      vmSetup
    ];

    # A login shell gets the environment of the VM, and in a repo of /work
    # also the devshell of that repo.
    environment.loginShellInit = ''
      if [ -r /run/hvw/env ]; then
        set -a
        . /run/hvw/env
        set +a
      fi
      case "$PWD" in
        /work/*)
          hvw_repo=''${PWD#/work/}
          hvw_repo=''${hvw_repo%%/*}
          if [ -r "/run/hvw/farms/$hvw_repo/devenv.sh" ]; then
            . "/run/hvw/farms/$hvw_repo/devenv.sh"
          fi
          unset hvw_repo
          ;;
      esac
    '';

    nix.settings.experimental-features = [
      "nix-command"
      "flakes"
    ];

    hvw.image.image = pkgs.runCommand "hvw-image" { } ''
      mkdir $out
      ln -s ${config.system.build.metadata}/tarball/*.tar.xz $out/metadata.tar.xz
      ln -s ${config.system.build.repartImage}/${config.image.baseName}.qcow2 $out/disk.qcow2
    '';
  };
}
