{
  description = "prov — a self-describing workspace metadata CLI and library";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # The shared dev shell: the pinned Rust toolchain and git-cliff. fig (7)
    # and twig (6) are pure Rust, so prov's build needs nothing else.
    diaryx-nix.url = "github:diaryx-org/nix";
    diaryx-nix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, diaryx-nix }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        # The workspace version (single source of truth in [workspace.package]).
        # Parse it so the flake reports the same number as `prov --version`.
        version =
          let m = builtins.match ".*\n[[:blank:]]*version = \"([^\"]+)\".*"
                    (builtins.readFile ./Cargo.toml);
          in if m == null
             then throw "prov flake: could not find workspace version in Cargo.toml"
             else builtins.head m;
      in {
        packages = rec {
          default = prov;

          prov = pkgs.rustPlatform.buildRustPackage {
            pname = "prov";
            inherit version;
            src = ./.;

            cargoLock.lockFile = ./Cargo.lock;

            # Build/test only the CLI crate; the library is a workspace member.
            cargoBuildFlags = [ "-p" "prov-cli" ];
            cargoTestFlags = [ "-p" "prov-cli" ];

            meta = {
              description = "Command-line companion for the prov self-describing workspace library";
              homepage = "https://github.com/diaryx-org/prov";
              license = with pkgs.lib.licenses; [ mit asl20 ];
              mainProgram = "prov";
              platforms = pkgs.lib.platforms.unix;
            };
          };
        };

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.prov}/bin/prov";
        };

        # The shared Rust shell. It carries the pinned Rust toolchain rather
        # than nixpkgs' `cargo`/`rustc`, and git-cliff for `ship changelog`.
        devShells.default = diaryx-nix.devShells.${system}.rust;
      });
}
