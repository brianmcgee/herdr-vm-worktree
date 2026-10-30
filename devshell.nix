{ pkgs, perSystem }:
pkgs.mkShell {
  packages = [
    pkgs.cargo
    pkgs.rustc
    pkgs.clippy
    pkgs.rustfmt
    pkgs.rust-analyzer
    pkgs.incus
    perSystem.self.formatter
  ];
  env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
}
