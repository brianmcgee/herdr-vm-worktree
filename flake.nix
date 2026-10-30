{
  description = "herdr worktrees in short-lived Incus VMs behind an egress gatekeeper";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs?ref=nixos-unstable";
    blueprint.url = "github:numtide/blueprint";
    blueprint.inputs.nixpkgs.follows = "nixpkgs";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";
    # Only the checks use clan-core: they evaluate the clan service in a
    # small clan. A clan that imports this flake should make it follow its
    # own clan-core.
    clan-core.url = "github:clan-lol/clan-core";
    clan-core.inputs.nixpkgs.follows = "nixpkgs";
    clan-core.inputs.treefmt-nix.follows = "treefmt-nix";
  };

  outputs =
    inputs:
    let
      outputs = inputs.blueprint {
        inherit inputs;
        # Members, clients and worktree VMs are NixOS machines.
        systems = [
          "x86_64-linux"
          "aarch64-linux"
        ];
      };
    in
    # clan-core looks for clan services in the `clan.modules` output.
    outputs // { clan.modules = outputs.modules.clan; };
}
