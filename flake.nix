{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
      in
      {
        devShells.default =
          with pkgs;
          mkShell {
            buildInputs = [
              # Rust
              (rust-bin.stable.latest.default.override {
                extensions = [ "rust-src" ];
                targets = [ "wasm32-unknown-unknown" ];
              })

              clippy
              lld
              rustfmt
              wasm-pack
              chromium
              chromedriver
              firefox
              geckodriver
              wasm-bindgen-cli_0_2_121
            ];

            RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";

          };
      }
    );
}
