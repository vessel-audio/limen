{
  description = "System-wide whole-take voice input for Wayland";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      self,
      flake-parts,
      treefmt-nix,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      perSystem =
        { self', pkgs, ... }:
        let
          treefmtEval = treefmt-nix.lib.evalModule pkgs {
            projectRootFile = "Cargo.toml";
            programs = {
              nixfmt.enable = true;
              statix.enable = true;
              deadnix.enable = true;
              rustfmt.enable = true;
              yamlfmt.enable = true;
              taplo.enable = true;
            };
          };
        in
        {
          packages = rec {
            voice-input = pkgs.callPackage ./nix/pkgs/voice-input.nix { withGui = true; };
            headless = pkgs.callPackage ./nix/pkgs/voice-input.nix { withGui = false; };
            default = voice-input;
          };

          apps.default = {
            type = "app";
            program = "${self'.packages.default}/bin/voice-input";
          };

          devShells.default = pkgs.mkShell {
            inputsFrom = [ self'.packages.default ];
            packages = builtins.attrValues {
              inherit (pkgs)
                cargo
                rustc
                rustfmt
                clippy
                rust-analyzer
                cmake
                pkg-config
                gcc
                gnumake
                coreutils
                dbus
                desktop-file-utils
                fcitx5
                pipewire
                libnotify
                pre-commit
                ;
            };
            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.wayland
              pkgs.vulkan-loader
            ];
          };

          formatter = treefmtEval.config.build.wrapper;
          checks = {
            package = self'.packages.default;
            package-headless = self'.packages.headless;
            formatting = treefmtEval.config.build.check self;
          };
        };
    };
}
