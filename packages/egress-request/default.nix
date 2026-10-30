# `egress-request` alone, for the VM image. Its source holds only the code
# of `egress-request` and dummy code for the other crates of the workspace.
# Thus a change of the other crates does not build the VM image again.
{
  pkgs,
  inputs,
  ...
}:
let
  inherit (pkgs) lib;
  inherit (import ../hvw/crane.nix { inherit pkgs inputs; }) craneLib common cargoArtifacts;
  crate = ../../crates/egress-request;
in
craneLib.buildPackage {
  pname = "egress-request";
  inherit (common) version strictDeps cargoVendorDir;
  inherit cargoArtifacts;
  src = craneLib.mkDummySrc {
    inherit (common) src;
    extraDummyScript = ''
      rm -r $out/crates/egress-request
      cp -r --no-preserve=mode ${
        lib.fileset.toSource {
          root = crate;
          fileset = crate;
        }
      } $out/crates/egress-request
      # The cleaned workspace manifest has no `workspace.lints`, but the
      # crate inherits them.
      rm $out/Cargo.toml
      cp --no-preserve=mode ${../../Cargo.toml} $out/Cargo.toml
    '';
  };
  cargoExtraArgs = "--locked -p egress-request";
  doCheck = false;

  meta = {
    description = "Asks the user of a herdr-vm-worktree VM to allow a refused egress request";
    license = lib.licenses.mit;
    mainProgram = "egress-request";
  };
}
