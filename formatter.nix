{
  flake,
  inputs,
  pkgs,
  ...
}:
let
  treefmtEval = inputs.treefmt-nix.lib.evalModule pkgs {
    projectRootFile = "flake.nix";

    programs.nixfmt.enable = true;
    programs.deadnix.enable = true;
    programs.rustfmt.enable = true;
    programs.rustfmt.edition = "2024";
  };
  formatter = treefmtEval.config.build.wrapper;
in
formatter
// {
  passthru = formatter.passthru // {
    tests.check = treefmtEval.config.build.check flake;
  };
}
