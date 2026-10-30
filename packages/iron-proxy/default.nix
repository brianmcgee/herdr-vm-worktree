# The egress gatekeeper. A third-party Go program, not project code.
{ pkgs, ... }:
(pkgs.buildGoModule.override { go = pkgs.go_1_27; }) rec {
  pname = "iron-proxy";
  version = "0.54.0";

  src = pkgs.fetchFromGitHub {
    owner = "paradigmxyz";
    repo = "iron-proxy";
    tag = "v${version}";
    hash = "sha256-i3aWgF9WR2FxqQo5Fqfc/NMDwm5prlEtpwg6mwugJCg=";
  };

  vendorHash = "sha256-dNMIHyuflyQ+Pan1RrB+biqjQzPu4axtz7rQLkiAlyU=";

  subPackages = [ "cmd/iron-proxy" ];

  env.CGO_ENABLED = 0;

  # The upstream tests need the network.
  doCheck = false;

  meta = {
    description = "Egress proxy with allowlists and secret injection";
    homepage = "https://github.com/paradigmxyz/iron-proxy";
    license = pkgs.lib.licenses.asl20;
    mainProgram = "iron-proxy";
  };
}
