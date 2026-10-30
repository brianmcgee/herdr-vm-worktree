# The default package: the Rust workspace with the hvw CLI, so that
# `nix run` opens the terminal UI of hvw.
{ perSystem, ... }:
perSystem.self.hvw
