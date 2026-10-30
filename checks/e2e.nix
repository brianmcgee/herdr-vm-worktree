# Milestones 2 to 6, end to end: a client launches a worktree VM on a
# member over the control plane, with a local repo that has uncommitted
# changes and a devshell. In the VM, the gatekeeper injects the credential
# for an allowed host and refuses other hosts. A refused request goes to
# the client for a decision, and the agent retries it. At the end the
# client fetches a commit of the VM and destroys the VM.
{ pkgs, flake, ... }:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;

  # Fixed test keys from `hvw keygen`. Test keys are not secret.
  memberId = "681d324cdda552a766ae273f2d58dc6b4d6c8fed97c9667c4f539dfafea96e08";
  clientKey = "271f2ea211344c09883aaa74694de248c2ff50e65e41ec321692a4a73737a03a";
  clientId = "1cd8975c2ceee1c6af2818c3f5c666b3fd1470f86646066254d42a999530a7bc";

  secret = "real-secret-0123456789";
  placeholder = "hvw-placeholder-web";

  # A test CA and a certificate for *.example.test on the web node.
  certs = pkgs.runCommand "hvw-test-certs" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out && cd $out
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
      -keyout ca.key -out ca.crt -days 3650 -subj /CN=hvw-test-ca
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
      -keyout web.key -out web.csr -subj /CN=example.test
    printf 'subjectAltName=DNS:*.example.test\n' > ext
    openssl x509 -req -in web.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
      -out web.crt -days 3650 -extfile ext
  '';

  # Answers each GET with the Host and Authorization headers it got.
  echoServer = pkgs.writers.writePython3 "echo-server" { } ''
    import http.server
    import json
    import ssl


    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = json.dumps({
                "host": self.headers.get("Host"),
                "auth": self.headers.get("Authorization"),
                "path": self.path,
            }).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)


    s = http.server.ThreadingHTTPServer(("", 443), H)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(
        "${certs}/web.crt",
        "${certs}/web.key",
    )
    s.socket = ctx.wrap_socket(s.socket, server_side=True)
    s.serve_forever()
  '';

  # The devshell of the test repo. The member evaluates the same
  # expression from the repo flake, so its inputs must be in its store.
  devShell = pkgs.mkShell { packages = [ pkgs.hello ]; };

  repoFlake = pkgs.writeText "flake.nix" ''
    {
      inputs.nixpkgs.url = "path:${pkgs.path}";
      outputs = { nixpkgs, ... }:
        let pkgs = nixpkgs.legacyPackages.${system};
        in { devShells.${system}.default = pkgs.mkShell { packages = [ pkgs.hello ]; }; };
    }
  '';
in
pkgs.testers.runNixOSTest {
  name = "hvw-e2e";

  nodes.web = {
    networking.firewall.allowedTCPPorts = [ 443 ];
    systemd.services.echo = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = echoServer;
    };
  };

  nodes.member =
    { config, nodes, ... }:
    {
      imports = [
        flake.nixosModules.member
        ../tests/member-node.nix
      ];
      virtualisation.additionalPaths = [
        config.services.hvw.member.image
        devShell.inputDerivation
        "${pkgs.path}"
      ];
      # The test has no binary cache.
      nix.settings.substituters = lib.mkForce [ ];
      # Nix 2.35 copies the source of a git flake into the store only when
      # the evaluation needs it. The devshell of the test repo does not
      # need it, so the member must copy it itself.
      nix.package = pkgs.nixVersions.latest;
      security.pki.certificateFiles = [ "${certs}/ca.crt" ];
      networking.extraHosts = ''
        ${nodes.web.networking.primaryIPAddress} api.example.test other.example.test denied.example.test
      '';
      services.hvw.member = {
        enable = true;
        link = {
          bind = "0.0.0.0:7842";
          peers = [
            {
              name = "client";
              id = clientId;
            }
          ];
        };
        credentials.web = pkgs.writeText "web-credential" secret;
        policies.web = {
          domains = [ "api.example.test" ];
          secrets = [
            {
              credential = "web";
              inherit placeholder;
              hosts = [ "api.example.test" ];
              env = [ "WEB_TOKEN" ];
            }
          ];
        };
      };
    };

  nodes.client =
    { nodes, ... }:
    {
      imports = [ flake.nixosModules.client ];
      environment.etc."hvw-test/client.key" = {
        text = clientKey;
        user = "alice";
        mode = "0400";
      };
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        linger = true;
      };
      environment.systemPackages = [ pkgs.git ];
      services.hvw.client = {
        enable = true;
        user = "alice";
        link = {
          secret_key_file = "/etc/hvw-test/client.key";
          relay.mode = "disabled";
          peers = [
            {
              name = "member";
              id = memberId;
              addrs = [ "${nodes.member.networking.primaryIPAddress}:7842" ];
            }
          ];
        };
      };
    };

  testScript =
    { nodes, ... }:
    ''
      import json

      ${builtins.readFile ../tests/common.py}

      def alice(command, timeout=900):
          """Runs a command as the herdr user, in the worktree."""
          script = f"cd /home/alice/work/repo && {command}"
          return client.succeed(
              "sudo -u alice env XDG_RUNTIME_DIR=/run/user/1000 HOME=/home/alice"
              f" sh -c {shlex.quote(script)}",
              timeout=timeout,
          )

      def agent(name, command):
          """Runs a command in a login shell of the harness user in /work/repo."""
          return vm(member, name, f"cd /work/repo && bash -l -c {shlex.quote(command)}", user=1000)

      start_all()
      require_nested_kvm(member)
      member.wait_for_unit("hvw-agent.service")
      web.wait_for_unit("echo.service")
      client.wait_for_unit("user@1000.service")

      with subtest("the client reaches the member over the control plane"):
          client.wait_until_succeeds(
              "sudo -u alice env XDG_RUNTIME_DIR=/run/user/1000 hvw list", timeout=120
          )

      with subtest("the member refuses a client that is not in its allowlist"):
          stranger = {
              "secret_key_file": "/tmp/stranger.key",
              "relay": {"mode": "disabled"},
              "peers": [
                  {
                      "name": "member",
                      "id": "${memberId}",
                      "addrs": ["${nodes.member.networking.primaryIPAddress}:7842"],
                  }
              ],
          }
          client.succeed(
              "hvw keygen --secret-file /tmp/stranger.key"
              f" && echo {shlex.quote(json.dumps(stranger))} > /tmp/stranger.json"
          )
          client.succeed(
              "systemd-run --unit=stranger -E HVW_LINK=/tmp/stranger.json"
              " -E HVW_SOCKET=/tmp/stranger.sock -E HOME=/tmp hvw-watch"
          )
          client.wait_for_file("/tmp/stranger.sock")
          out = client.fail("HVW_SOCKET=/tmp/stranger.sock hvw list 2>&1")
          assert "does not answer" in out, out
          member.wait_until_succeeds(
              "journalctl -u hvw-agent | grep -q 'refused a connection from an unknown peer'",
              timeout=60,
          )
          client.succeed("systemctl stop stranger")

      client.succeed(
          "sudo -u alice sh -c '"
          "mkdir -p /home/alice/work/repo && cd /home/alice/work/repo"
          " && git init -q -b main"
          " && git config user.email a@b && git config user.name alice"
          " && cp ${repoFlake} flake.nix && chmod u+w flake.nix"
          " && printf \"[[repo]]\\npath = \\\".\\\"\\npolicy = \\\"web\\\"\\n\" > hvw.toml"
          " && git add -A && git commit -q -m init"
          " && echo uncommitted > notes.txt && git add notes.txt'"
      )

      with subtest("hvw launch starts a VM with the repo"):
          alice("hvw launch", timeout=1800)
          name = alice("hvw list").split()[0]
          assert name.startswith("hvw-repo-"), name

      with subtest("the VM has the uncommitted change"):
          assert agent(name, "cat notes.txt").strip() == "uncommitted"
          assert agent(name, "git log --format=%s -1").strip() == "init"

      with subtest("the devshell works in the VM, and only its closure is there"):
          assert "Hello, world!" in agent(name, "hello")
          vm(member, name, "test ! -e ${flake.packages.${system}.iron-proxy}")

      with subtest("the VM holds the placeholder, the gatekeeper injects the secret"):
          env = agent(name, "env")
          assert "WEB_TOKEN=${placeholder}" in env, env
          assert "${secret}" not in env
          wait_for_vm_ipv4(member, name, "10.250.0.")
          out = agent(name, "curl -sf -H \"Authorization: Bearer $WEB_TOKEN\" https://api.example.test/x")
          assert json.loads(out)["auth"] == "Bearer ${secret}", out

      with subtest("a member process outside hvw-agent cannot use a gatekeeper"):
          gk = json.loads(member.succeed(f"cat /var/lib/hvw/vms/{name}/gatekeeper/config.json"))
          tunnel = gk["proxy"]["tunnel_listen"]
          https_port = gk["proxy"]["https_listen"].rsplit(":", 1)[1]
          # nixbld1 stands for the fetcher of a fixed-output build on the member.
          as_builder = "setpriv --reuid=nixbld1 --regid=nixbld --clear-groups"
          for target in [
              f"--proxy http://{tunnel} https://api.example.test/x",
              f"--connect-to api.example.test:443:127.0.0.1:{https_port} https://api.example.test/x",
          ]:
              member.succeed(
                  f"rc=0; {as_builder} ${pkgs.curl}/bin/curl -sk -m 5 -o /dev/null {target} || rc=$?; test $rc -eq 7"
              )

      with subtest("the gatekeeper refuses other hosts, and the VM has no other way out"):
          # Through a proxy, the refusal is the answer to CONNECT, and curl
          # fails.
          code = agent(
              name,
              "curl -s -o /dev/null -w %{http_connect} https://other.example.test/y || true",
          )
          assert code.strip() == "403", code
          vm(member, name, "! curl -sk -m 5 --noproxy '*' https://${nodes.web.networking.primaryIPAddress}/")

      with subtest("an approved egress request opens the host for the VM"):
          vm(
              member,
              name,
              "systemd-run --unit=er --uid=1000 --working-directory=/work/repo"
              " bash -l -c 'egress-request other.example.test --method GET --path /y"
              " --reason \"the test needs it\"; echo $? > /tmp/er.rc'",
          )
          client.wait_until_succeeds(
              "sudo -u alice env XDG_RUNTIME_DIR=/run/user/1000 hvw pending | grep -q other.example.test",
              timeout=120,
          )
          pending = alice("hvw pending")
          assert "the test needs it" in pending, pending
          assert agent(name, "head -3 ~/.claude/skills/egress-request/SKILL.md").startswith("---"), (
              "the agent has no egress-request skill"
          )
          rid = pending.split()[0]
          alice(f"hvw decide {rid} vm")
          member.wait_until_succeeds(vm_command(name, "test -e /tmp/er.rc"), timeout=120)
          assert vm(member, name, "cat /tmp/er.rc").strip() == "0"
          out = agent(name, "curl -sf https://other.example.test/y")
          assert json.loads(out)["host"] == "other.example.test", out

      with subtest("a denied egress request fails"):
          agent(name, "curl -s -o /dev/null https://denied.example.test/ || true")
          vm(
              member,
              name,
              "systemd-run --unit=er2 --uid=1000 --working-directory=/work/repo"
              " bash -l -c 'egress-request denied.example.test --reason no; echo $? > /tmp/er2.rc'",
          )
          client.wait_until_succeeds(
              "sudo -u alice env XDG_RUNTIME_DIR=/run/user/1000 hvw pending | grep -q denied.example.test",
              timeout=120,
          )
          rid = alice("hvw pending").split()[0]
          alice(f"hvw decide {rid} deny")
          member.wait_until_succeeds(vm_command(name, "test -e /tmp/er2.rc"), timeout=120)
          assert vm(member, name, "cat /tmp/er2.rc").strip() == "1"

      with subtest("hvw attach runs the harness in the repo"):
          out = alice(
              "printf 'echo marker-$((6*7)) $PWD\\nshopt -oq emacs && echo editing-$((2*2))\\nexit 3\\n'"
              " | hvw attach; echo rc=$?"
          )
          assert "marker-42 /work/repo" in out, out
          # The devshell puts a bash without readline first in PATH. The
          # harness shell must still edit lines and expand the prompt.
          assert "editing-4" in out, out
          assert "rc=3" in out, out

      with subtest("hvw fetch gets the commits of the VM"):
          agent(name, "git checkout -q -b feat && echo work > work.txt && git add -A && git commit -q -m vm-work")
          alice("hvw fetch")
          log = alice(f"git log --format='%s %an <%ae>' -1 {name}/feat").strip()
          assert log == "vm-work alice <a@b>", log

      with subtest("hvw destroy deletes the VM"):
          alice("hvw destroy")
          member.wait_until_succeeds(f"! incus info {name}", timeout=120)
          assert alice("hvw list").strip() == ""
    '';
}
