import shlex


def require_nested_kvm(machine):
    """Fail with a clear message when the node cannot run KVM guests."""
    machine.wait_for_unit("multi-user.target")
    status, _ = machine.execute("test -c /dev/kvm")
    if status != 0:
        raise RuntimeError(
            "the test node has no /dev/kvm: worktree VMs need nested KVM "
            "(kvm_intel or kvm_amd with nested=1 on the build host)"
        )


def wait_for_vm(machine, name, timeout=600):
    """Wait until the Incus agent in the VM answers. Print the console log
    of the VM when it does not."""
    try:
        machine.wait_until_succeeds(
            f"timeout 10 incus exec -n {name} -- true", timeout=timeout
        )
    except Exception:
        print(machine.execute(f"incus console {name} --show-log")[1])
        print(machine.execute(f"incus info {name}")[1])
        raise


def vm_command(name, command, user=None):
    """The member command that runs a shell command in a worktree VM. stdin
    is /dev/null: `incus exec` would otherwise read the command channel of
    the test driver. `user` is a uid: `incus exec` takes no user names."""
    as_user = f"--user {user} --group 100 --env HOME=/home/agent " if user else ""
    return (
        f"incus exec -n {as_user}--env PATH=/run/wrappers/bin:/run/current-system/sw/bin "
        f"{name} -- sh -c {shlex.quote(command)}"
    )


def vm(machine, name, command, user=None):
    """Run a shell command in a worktree VM and return its output."""
    return machine.succeed(vm_command(name, command, user))


def wait_for_vm_ipv4(machine, name, prefix, timeout=120):
    """Wait until the VM has an IPv4 address that starts with `prefix`.
    Print the network state of the VM when it does not."""
    try:
        machine.wait_until_succeeds(
            vm_command(name, f"ip -4 -o addr | grep -qF 'inet {prefix}'"),
            timeout=timeout,
        )
    except Exception:
        for c in [
            "ip addr",
            "networkctl",
            "journalctl -b -u systemd-networkd --no-pager",
        ]:
            print(machine.execute(vm_command(name, c))[1])
        print(machine.execute("incus list; incus network show hvwbr0")[1])
        print(machine.execute(f"incus config show {name} --expanded")[1])
        print(machine.execute("nft list ruleset")[1])
        print(
            machine.execute(
                "timeout 30 tcpdump -n -e -i hvwbr0 -c 10 'port 67 or port 68' 2>&1"
            )[1]
        )
        raise
