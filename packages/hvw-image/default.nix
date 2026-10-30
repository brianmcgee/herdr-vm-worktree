# The default base image of a worktree VM, for `incus image import`.
{
  pkgs,
  flake,
  inputs,
  ...
}:
(inputs.nixpkgs.lib.nixosSystem {
  modules = [
    flake.nixosModules.image
    { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
  ];
}).config.hvw.image.image
