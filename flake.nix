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
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      perSystem =
        { self', pkgs, ... }:
        let
          inherit (pkgs) lib;
          linuxOnly = lib.optionals pkgs.stdenv.isLinux;
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
            limen = pkgs.callPackage ./nix/pkgs/limen.nix { withGui = true; };
            headless = pkgs.callPackage ./nix/pkgs/limen.nix { withGui = false; };
            default = limen;
          };

          apps.default = {
            type = "app";
            program = "${self'.packages.default}/bin/limen";
          };

          devShells.default = pkgs.mkShell {
            inputsFrom = [ self'.packages.default ];
            packages =
              builtins.attrValues {
                inherit (pkgs)
                  cargo
                  rustc
                  rustfmt
                  clippy
                  rust-analyzer
                  cmake
                  pkg-config
                  gnumake
                  coreutils
                  pre-commit
                  ;
              }
              # gcc and the desktop stack are Linux-only: macOS builds against
              # the system SDK, and there is no D-Bus/Fcitx/PipeWire there.
              ++ linuxOnly (
                builtins.attrValues {
                  inherit (pkgs)
                    gcc
                    dbus
                    desktop-file-utils
                    fcitx5
                    pipewire
                    libnotify
                    # Desktop E2E: a headless X server for `cargo gui-e2e`.
                    xvfb
                    ;
                }
                # Desktop E2E: real `gsettings` over a keyfile backend.
                ++ [
                  pkgs.glib.bin
                  pkgs.glib.dev
                ]
              );
            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
          }
          // lib.optionalAttrs pkgs.stdenv.isLinux {
            # Desktop E2E renders with Mesa's software Vulkan driver, registers
            # with a private AT-SPI registry, and sees only the fonts in
            # nix/gui-e2e/fonts.conf; `makeFontsConf` would add the host's.
            LIMEN_E2E_REGISTRYD = "${pkgs.at-spi2-core}/libexec/at-spi2-registryd";
            LIMEN_E2E_VK_ICD = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.uname.processor}.json";
            LIMEN_E2E_FONTCONFIG = pkgs.replaceVars ./nix/gui-e2e/fonts.conf {
              ibmPlex = pkgs.ibm-plex;
              dejavu = pkgs.dejavu_fonts;
              wqyMicrohei = pkgs.wqy_microhei;
            };
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.wayland
              pkgs.vulkan-loader
              pkgs.libGL
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
