# Installation

narwhal ships a single static binary. Pick whichever channel fits
your platform.

## Prebuilt binaries

The fastest path. The installer detects your OS and architecture,
verifies the SHA-256 sum from the GitHub release, and drops the
binary in `~/.local/bin`.

```sh
curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | sh
```

Environment variables:

- `NARWHAL_VERSION=v2.2.0` — pin to a specific tag (default: latest)
- `NARWHAL_BIN_DIR=/usr/local/bin` — install location (default:
  `~/.local/bin`)
- `NARWHAL_FORCE=1` — overwrite an existing binary without prompting

Direct downloads live at
<https://github.com/Nonanti/narwhal/releases/latest>. Supported
targets:

| Target                       | File                                         |
|------------------------------|----------------------------------------------|
| `x86_64-unknown-linux-gnu`   | `narwhal-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |
| `x86_64-apple-darwin`        | `narwhal-X.Y.Z-x86_64-apple-darwin.tar.gz`   |
| `aarch64-apple-darwin`       | `narwhal-X.Y.Z-aarch64-apple-darwin.tar.gz`  |

Linux binaries link libdbus statically, so they work on Alpine,
minimal containers, and NixOS without a host `libdbus-1.so.3`.

## Cargo

```sh
cargo install narwhaldb
```

The crate is published as `narwhaldb`; the binary is still called
`narwhal`. The mismatch is because the `narwhal` slot on crates.io
was claimed in 2018 by an abandoned project.

For toolchain-less installs, `cargo-binstall` will grab the
prebuilt tarball:

```sh
cargo binstall narwhaldb
```

## Homebrew

```sh
brew tap Nonanti/tap
brew install narwhal
```

## Arch Linux (AUR)

```sh
yay -S narwhal
```

Or any AUR helper of your choice.

## Nix

```sh
nix run github:Nonanti/narwhal
```

A `flake.nix` is committed at the repository root.

## Building from source

See [`dev/build.md`](./dev/build.md).
