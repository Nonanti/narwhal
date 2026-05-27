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

        # DuckDB bundled C++ build requires cmake, clang, and libcxx.
        nativeBuildDeps = with pkgs; [ cmake clang pkg-config ];
        buildDeps = with pkgs; [ libcxx ];
      in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "narwhal";
          version = "1.0.0";

          src = ./.;
          cargoLock = { lockFile = ./Cargo.lock; };

          nativeBuildInputs = nativeBuildDeps;
          buildInputs = buildDeps;

          # bindgen (used by duckdb-rs build.rs) needs libclang.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          # cmake needs to find the C++ standard library from libcxx.
          CXXFLAGS = "-isystem ${pkgs.libcxx.dev}/include/c++/v1";

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
