# Install

## One-line (Linux / macOS)

```sh
curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | sh
```

Detects your OS/arch, downloads the matching prebuilt binary from the
latest release, verifies its SHA-256, and drops it into `~/.local/bin`.

Supported targets today: `x86_64-unknown-linux-gnu`,
`aarch64-apple-darwin`. The script falls back to a friendly error
pointing at `cargo install` / `brew` for other targets.

### Environment knobs

```sh
NARWHAL_VERSION=v2.0.0          # pin to a specific tag
NARWHAL_BIN_DIR=/usr/local/bin  # custom install dir (default: ~/.local/bin)
NARWHAL_FORCE=1                 # overwrite existing binary without warning
NARWHAL_NO_MODIFY_PATH=1        # suppress the PATH advisory
NO_COLOR=1                      # plain output
```

Env vars must precede `sh` (not after the pipe), otherwise the shell
parses them as separate commands:

```sh
# correct
curl -fsSL .../install.sh | NARWHAL_FORCE=1 sh

# also correct (preferred when piping is awkward)
NARWHAL_FORCE=1 sh -c "$(curl -fsSL .../install.sh)"
```

## Cargo

```sh
cargo install narwhaldb
```

The crate name is `narwhaldb` (the bare `narwhal` slot belongs to an
unrelated 2018 Docker library); the installed binary is `narwhal`.

For users without a Rust toolchain:

```sh
cargo binstall narwhaldb
```

## Package managers

```sh
brew install Nonanti/tap/narwhal       # macOS / Linux
yay -S narwhal                          # Arch (AUR)
nix run github:Nonanti/narwhal          # Nix
```

## Pre-built binaries

Download from the [latest release](https://github.com/Nonanti/narwhal/releases):

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin` (Apple Silicon)

Each tarball ships with a `.sha256` sibling.

## Build from source

See [`docs/dev/build.md`](./dev/build.md).
