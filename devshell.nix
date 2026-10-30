{ pkgs, perSystem }:
pkgs.mkShell {
  packages = [
    pkgs.cargo
    pkgs.rustc
    pkgs.clippy
    pkgs.rustfmt
    pkgs.incus
    perSystem.self.formatter

    # Language servers and formatters. An editor such as Zed takes them from
    # PATH. If they are not on PATH, Zed downloads generic Linux binaries
    # that do not run on NixOS.
    pkgs.rust-analyzer
    pkgs.nixd
    pkgs.nil
    pkgs.nixfmt
    pkgs.taplo
    pkgs.package-version-server
    pkgs.basedpyright
    pkgs.ruff
  ];
  env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
}
