{
  description = "prov — a self-describing workspace metadata CLI and library";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # prov's `fig` and `twig-doc` dependencies are Zig-backed (their build.rs
    # scripts run `zig build`), so the Rust build needs the same Zig toolchain
    # those crates are built with. That is the whole reason the version is not
    # written here: it is one number that fig, twig, and prov have to agree on,
    # and it lives in diaryx-org/nix so that agreeing is not a thing anyone has
    # to remember.
    diaryx-nix.url = "github:diaryx-org/nix";
    diaryx-nix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, diaryx-nix }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        zig = diaryx-nix.lib.${system}.zig;

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

            # zig for the fig/twig-doc build.rs steps. On Apple targets those
            # build scripts also repack Zig's static archive with `libtool`
            # (ld64 rejects Zig's alignment) — cctools provides that `libtool`,
            # which isn't otherwise on the sandbox PATH.
            nativeBuildInputs = [ zig ]
              ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.cctools ];

            # Those Zig builds want a writable HOME + cache dir, which the
            # read-only Nix store won't provide.
            preBuild = ''
              export HOME="$TMPDIR"
              export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
              export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
            '';

            # The fig/twig-doc build scripts repack their Zig archives with
            # `libtool`/`ar`, which leaves an unreadable `__.SYMDEF` in each
            # build-script `out/repack` dir. buildRustPackage's install hook then
            # does a bulk `cp -r` of the release dir and fails on it. This runs
            # before that hook (the postBuild attr precedes postBuildHooks), so
            # make the tree readable first.
            postBuild = ''
              chmod -R u+rwX target
            '';

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

        # The shared rust+zig shell. It carries the pinned Rust toolchain rather
        # than nixpkgs' `cargo`/`rustc`, and git-cliff for `dx changelog`.
        devShells.default = diaryx-nix.devShells.${system}.rust-zig;
      });
}
