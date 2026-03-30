{
  description = "narwhal — TUI database client";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "rustfmt" "clippy" ];
        };

        # DuckDB's bundled C++ tree expects libstdc++ (the stdenv
        # default), not libc++. Earlier revisions added clang +
        # libcxx here and the build fell over with `'NAN' was not
        # declared in this scope` because GCC was still selected by
        # cc-rs but the -isystem libcxx headers shadowed cstdlib.
        # Stay on the plain stdenv toolchain; cmake is for the build,
        # libclang is for duckdb-rs's bindgen, dbus is for the keyring
        # crate (secret-service backend) on Linux.
        nativeBuildDeps = with pkgs; [ cmake pkg-config ];
        buildDeps = with pkgs; [ dbus ];
      in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "narwhal";
          version = "2.0.0";

          src = ./.;
          # crates.io now refuses requests with the default `curl`
          # User-Agent (returns 403). The per-crate fetcher used when
          # `cargoLock = { lockFile = ... }` is set still inherits that
          # default and breaks every transitive download, so use a
          # fixed-output `cargoHash` vendor instead: cargo runs inside
          # the sandbox, sends the right UA, and the vendored output
          # is content-addressed.
          cargoHash = "sha256-KgmSMQlZ/Uj9dZlmgp/Z3JvSCHSrlbL1SPSOpOXknVk=";

          nativeBuildInputs = nativeBuildDeps;
          buildInputs = buildDeps;

          # bindgen (used by duckdb-rs build.rs) needs libclang.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          meta = with pkgs.lib; {
            description = "A TUI database client — DataGrip in your terminal";
            homepage = "https://github.com/Nonanti/narwhal";
            license = with licenses; [ mit asl20 ];
            mainProgram = "narwhal";
          };
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rust
            pkg-config
            openssl
            sqlite
            postgresql.lib
            # For keyring on Linux:
            dbus
            # For duckdb-rs bundled build (C++ sources):
            cmake
            clang
            libcxx
          ];
          # Required by duckdb-rs build.rs (bindgen).
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          # tokio-postgres / openssl need this:
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
          RUST_BACKTRACE = "1";
        };
      });
}
