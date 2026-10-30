# The Rust workspace: hvw, hvw-agent, hvw-watch, hvw-review,
# egress-request and their libraries.
#
# The package does not run the tests: `passthru.tests` runs them as checks.
# Then a change of the code compiles the workspace one time for the
# package, and the VM tests do not wait for the cargo tests.
{
  pkgs,
  inputs,
  ...
}:
let
  inherit (import ./crane.nix { inherit pkgs inputs; }) craneLib common cargoArtifacts;
in
craneLib.buildPackage (
  common
  // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--locked --workspace";
    doCheck = false;

    passthru.tests = {
      test = craneLib.cargoTest (
        common
        // {
          inherit cargoArtifacts;
          cargoTestExtraArgs = "--workspace";
          # The git tests of the client and the member run real git.
          nativeBuildInputs = [ pkgs.git ];
        }
      );
      clippy = craneLib.cargoClippy (
        common
        // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
        }
      );
    };

    meta = {
      description = "herdr worktrees in short-lived Incus VMs behind an egress gatekeeper";
      license = pkgs.lib.licenses.mit;
      mainProgram = "hvw";
    };
  }
)
