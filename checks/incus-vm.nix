# Milestone 1: one member boots and starts a worktree VM from the base
# image. The VM runs at the third level of nesting.
{ pkgs, flake, ... }:
pkgs.testers.runNixOSTest {
  name = "hvw-incus-vm";

  nodes.member =
    { config, ... }:
    {
      imports = [
        flake.nixosModules.member
        ../tests/member-node.nix
      ];
      services.hvw.member.enable = true;
      virtualisation.additionalPaths = [ config.services.hvw.member.image ];
    };

  testScript = ''
    ${builtins.readFile ../tests/common.py}

    member.start()
    require_nested_kvm(member)
    member.wait_for_unit("hvw-image-import.service")

    alias = member.succeed("incus image list -f csv -c l").strip()
    assert alias.startswith("hvw-"), f"unexpected image aliases: {alias!r}"
    member.succeed(
        f"incus launch {alias} t1 --vm --network hvwbr0 --storage hvw"
        " -c security.secureboot=false -c limits.memory=1GiB"
    )

    with subtest("the incus agent in the VM answers"):
        wait_for_vm(member, "t1", timeout=300)

    with subtest("the VM has the harness user and no password login"):
        vm(member, "t1", "id agent")
        root_hash = vm(member, "t1", "awk -F: '$1 == \"root\" { print $2 }' /etc/shadow").strip()
        assert root_hash == "!", f"root has a password hash: {root_hash!r}"

    with subtest("the VM is on the member bridge"):
        wait_for_vm_ipv4(member, "t1", "10.250.0.")

    with subtest("hvw-agent runs and its broker listens on the bridge"):
        member.wait_for_unit("hvw-agent.service")
        member.wait_until_succeeds("journalctl -u hvw-agent | grep -q 'broker listens on 10.250.0.1:7999'", timeout=60)
  '';
}
