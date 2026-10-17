{
  description = "louro: let any coding agent act as an IRC bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      crane,
      nixpkgs,
      flake-utils,
      git-hooks,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        toolchain = pkgs.rust-bin.stable."1.98.1".minimal.override {
          extensions = [
            "clippy"
            "rust-analyzer"
            "rust-src"
            "rustfmt"
          ];
        };
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
        craneArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
        };
        cargoArtifacts = craneLib.buildDepsOnly craneArgs;
        louro = craneLib.buildPackage (
          craneArgs
          // {
            inherit cargoArtifacts;
            doCheck = false;
            meta.mainProgram = "louro";
          }
        );
        cargoHook =
          {
            name,
            text,
            runtimeInputs ? [ ],
            files ? "(^|/)(Cargo\\.(toml|lock)|.*\\.rs)$",
            stages ? [ "pre-commit" ],
          }:
          {
            enable = true;
            entry = "${
              pkgs.writeShellApplication {
                inherit name text;
                runtimeInputs = [ toolchain ] ++ runtimeInputs;
              }
            }/bin/${name}";
            inherit files stages;
            pass_filenames = false;
          };
        offlineHooks = {
          nixfmt.enable = true;
          deadnix = {
            enable = true;
            settings = {
              noLambdaArg = true;
              noLambdaPatternNames = true;
            };
          };
          check-toml.enable = true;
          check-merge-conflicts.enable = true;
          end-of-file-fixer.enable = true;
          trim-trailing-whitespace.enable = true;
          shellcheck.enable = true;
          actionlint.enable = true;
          yamllint.enable = true;
        };
        cargoHooks = {
          rustfmt = cargoHook {
            name = "rustfmt-hook";
            text = "cargo fmt --all -- --check";
          };
          clippy = cargoHook {
            name = "clippy-hook";
            text = "cargo clippy --all-targets --locked -- -D warnings";
          };
          cargo-deny = cargoHook {
            name = "cargo-deny-hook";
            runtimeInputs = [ pkgs.cargo-deny ];
            files = "(^|/)(Cargo\\.(toml|lock)|deny\\.toml)$";
            text = "cargo deny check bans licenses sources";
          };
          tests = cargoHook {
            name = "tests-hook";
            runtimeInputs = [ pkgs.cargo-nextest ];
            text = "cargo nextest run --locked --no-tests=pass";
          };
        };
        gitHooks = git-hooks.lib.${system}.run {
          src = ./.;
          package = pkgs.prek;
          hooks = offlineHooks // cargoHooks;
        };
      in
      {
        packages = {
          default = louro;
          inherit louro;
        };

        checks = {
          inherit louro;
          pre-commit = git-hooks.lib.${system}.run {
            src = ./.;
            package = pkgs.prek;
            hooks = offlineHooks;
          };
          clippy = craneLib.cargoClippy (
            craneArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          fmt = craneLib.cargoFmt craneArgs;
          nextest = craneLib.cargoNextest (
            craneArgs
            // {
              inherit cargoArtifacts;
              cargoNextestPartitionsExtraArgs = "--no-tests=pass";
            }
          );
        };

        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            pkgs.cargo-nextest
            pkgs.cargo-deny
            pkgs.cargo-mutants
            pkgs.git-absorb
            pkgs.gnumake
          ]
          ++ gitHooks.enabledPackages;
          inherit (gitHooks) shellHook;
        };
      }
    )
    // {
      overlays.default = final: _: {
        louro = self.packages.${final.stdenv.hostPlatform.system}.louro;
      };
    };
}
