# The Rust workspace: hvw, hvw-agent, hvw-watch, hvw-review,
# egress-request and their libraries.
{ pkgs, flake, ... }:
let
  inherit (pkgs) lib;
  root = ../..;
in
pkgs.rustPlatform.buildRustPackage {
  pname = "hvw";
  version = (lib.importTOML (root + "/Cargo.toml")).workspace.package.version;

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      (root + "/crates")
    ];
  };

  cargoLock.lockFile = root + "/Cargo.lock";

  cargoTestFlags = [ "--workspace" ];

  passthru.tests.clippy = pkgs.rustPlatform.buildRustPackage {
    pname = "hvw-clippy";
    version = "0";
    inherit (flake.packages.${pkgs.stdenv.hostPlatform.system}.hvw) src cargoDeps;
    nativeBuildInputs = [ pkgs.clippy ];
    buildPhase = ''
      runHook preBuild
      cargo clippy --workspace --all-targets --offline -- --deny warnings
      runHook postBuild
    '';
    doCheck = false;
    installPhase = "touch $out";
  };

  meta = {
    description = "herdr worktrees in short-lived Incus VMs behind an egress gatekeeper";
    license = lib.licenses.mit;
    mainProgram = "hvw";
  };
}
