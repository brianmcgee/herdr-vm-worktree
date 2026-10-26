# The common settings of a member node in the VM tests.
{ pkgs, ... }:
{
  imports = [ ./btrfs-root.nix ];

  # For network diagnostics when a VM test fails.
  environment.systemPackages = [ pkgs.tcpdump ];

  # A fixed test key. Test keys are not secret.
  environment.etc."hvw-test/member.key".text =
    "454ba1c22a7835031e1f641e99453f7d3511f5d7a0f8e9954cf14d3d7b6c890b";
  services.hvw.member.link = {
    secret_key_file = "/etc/hvw-test/member.key";
    relay.mode = "disabled";
  };

  virtualisation = {
    cores = 4;
    memorySize = 6144;
    diskSize = 16 * 1024;
    # Worktree VMs need KVM in the test node: the third level of nesting.
    qemu.options = [ "-cpu host" ];
  };
}
