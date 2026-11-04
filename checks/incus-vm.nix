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

    import time


    def qemu_rss_mib(machine, name):
        """The resident memory of the QEMU process of a VM, in MiB."""
        pid = machine.succeed(f"cat /run/incus/{name}/qemu.pid").strip()
        kb = machine.succeed(f"awk '/^VmRSS:/ {{print $2}}' /proc/{pid}/status").strip()
        return int(kb) // 1024


    def wait_for_rss_at_most(machine, name, mib, timeout):
        """Wait until the QEMU process of a VM holds at most `mib` MiB."""
        deadline = time.monotonic() + timeout
        while True:
            rss = qemu_rss_mib(machine, name)
            if rss <= mib:
                return
            if time.monotonic() > deadline:
                raise AssertionError(f"the QEMU of {name} holds {rss} MiB after {timeout} s, expected at most {mib} MiB")
            time.sleep(5)


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

    with subtest("DAMON_RECLAIM runs in the VM"):
        enabled = vm(member, "t1", "cat /sys/module/damon_reclaim/parameters/enabled").strip()
        assert enabled == "Y", f"DAMON_RECLAIM is not on: {enabled!r}"

    with subtest("freed guest memory goes back to the host"):
        base = qemu_rss_mib(member, "t1")
        vm(member, "t1", "dd if=/dev/zero of=/dev/shm/fill bs=1M count=400")
        full = qemu_rss_mib(member, "t1")
        assert full - base >= 300, f"the VM took too little memory: {base} -> {full} MiB"
        vm(member, "t1", "rm /dev/shm/fill")
        wait_for_rss_at_most(member, "t1", full - 250, timeout=60)

    with subtest("cold page cache goes back to the host"):
        vm(member, "t1", "dd if=/dev/urandom of=/root/cache bs=1M count=400 && sync")
        full = qemu_rss_mib(member, "t1")
        try:
            wait_for_rss_at_most(member, "t1", full - 250, timeout=300)
        except Exception:
            print(vm(member, "t1", "grep . /sys/module/damon_reclaim/parameters/*"))
            raise

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
