# The crane setup that the packages `hvw` and `egress-request` share.
{ pkgs, inputs }:
let
  inherit (pkgs) lib;
  root = ../..;
  craneLib = inputs.crane.mkLib pkgs;
  common = {
    pname = "hvw";
    version = (lib.importTOML (root + "/Cargo.toml")).workspace.package.version;
    strictDeps = true;
    # Read Cargo.lock from the source tree, not from the dummy source of
    # a derivation. That would need import from derivation.
    cargoVendorDir = craneLib.vendorCargoDeps { cargoLock = root + "/Cargo.lock"; };
    src = lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions [
        (root + "/Cargo.toml")
        (root + "/Cargo.lock")
        (root + "/crates")
      ];
    };
  };
in
{
  inherit craneLib common;
  # The dependencies only. This derivation changes only when Cargo.toml
  # or Cargo.lock changes.
  cargoArtifacts = craneLib.buildDepsOnly common;
}
