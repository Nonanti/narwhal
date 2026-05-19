# Plan 07-12 — Distribution path (crates.io + AUR + Homebrew)

## Why

`cargo install --git ...` works today but isn't discoverable;
there's no `narwhal` on crates.io and no packaging for any
distro. v1.0 ships with a clear path: `cargo install narwhal-cli`
on any stable rustc, plus templated `PKGBUILD` / Homebrew formula
that a downstream packager can submit.

## Scope

1. **Crates.io publication** — every workspace crate gets the
   Cargo.toml metadata required for publication (license,
   description, repository, keywords, categories). The
   publication itself is a one-time manual step; this plan
   prepares the metadata and documents the procedure.

2. **`cargo install` verification** — on a fresh shell *without*
   the nix flake, `cargo install --path narwhal` (the binary
   crate) must succeed against stable rustc with no missing
   system deps beyond what postgres / mysql clients require.

3. **AUR `PKGBUILD` template** under `packaging/aur/`. Not
   submitted; documents the recipe a packager would use.

4. **Homebrew formula template** under `packaging/homebrew/`.
   Same — documents the recipe.

5. **Release procedure doc** at `docs/RELEASING.md` — step-by-
   step for tagging v1.0.0, building artifacts, publishing.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code (no
  code is touched here anyway).
- Conventional commit, long-form.
- Cargo.toml metadata must be consistent across all crates
  (same license, same repository URL, version-locked to the
  workspace).
- The binary crate name is `narwhal` (already); the cargo
  install target is `cargo install narwhal-cli` if we rename the
  binary crate to avoid a name clash on crates.io, or `cargo
  install narwhal` if `narwhal` is available.

## Concrete steps

### Step 1: check crates.io name availability

```sh
curl -s https://crates.io/api/v1/crates/narwhal | jq .
```

If `narwhal` is taken, fall back to `narwhal-cli` for the
binary crate. Document the chosen name in `docs/RELEASING.md`.

### Step 2: Cargo.toml metadata

For each workspace crate (`narwhal-core`, `narwhal-config`,
`narwhal-sql`, `narwhal-pool`, `narwhal-history`,
`narwhal-driver-*`, `narwhal-vim`, `narwhal-tui`, `narwhal-app`,
`narwhal-plugin`, `narwhal-plugin-lua`, `narwhal` binary):

```toml
[package]
name = "narwhal-core"
version = "1.0.0"
edition = "2024"
license = "MIT OR Apache-2.0"
description = "Core types and traits for narwhal, a TUI database client."
repository = "https://github.com/berkant/narwhal"
keywords = ["database", "tui", "sql", "client"]
categories = ["command-line-utilities", "database"]
```

Workspace-level `[workspace.package]` factors the shared fields
so the per-crate Cargo.toml only override `name` and
`description`.

### Step 3: AUR PKGBUILD

`packaging/aur/PKGBUILD`:

```bash
# Maintainer: <packager-name> <packager-email>
pkgname=narwhal
pkgver=1.0.0
pkgrel=1
pkgdesc="A TUI database client"
arch=('x86_64' 'aarch64')
url="https://github.com/berkant/narwhal"
license=('MIT' 'Apache')
depends=('postgresql-libs' 'mariadb-libs' 'gcc-libs')
makedepends=('cargo' 'pkgconf')
source=("${pkgname}-${pkgver}.tar.gz::${url}/archive/v${pkgver}.tar.gz")
sha256sums=('SKIP')  # filled at release time

build() {
    cd "$pkgname-$pkgver"
    cargo build --release --bin narwhal
}

package() {
    cd "$pkgname-$pkgver"
    install -Dm755 "target/release/narwhal" "$pkgdir/usr/bin/narwhal"
    install -Dm644 "LICENSE-MIT" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
```

### Step 4: Homebrew formula

`packaging/homebrew/narwhal.rb`:

```ruby
class Narwhal < Formula
  desc "A TUI database client"
  homepage "https://github.com/berkant/narwhal"
  url "https://github.com/berkant/narwhal/archive/v1.0.0.tar.gz"
  sha256 "..."  # filled at release time
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/berkant/narwhal.git", branch: "main"

  depends_on "rust" => :build
  depends_on "postgresql"
  depends_on "mysql-client"

  def install
    system "cargo", "install", "--locked", "--root", prefix, "--path", "narwhal"
  end

  test do
    assert_match "narwhal", shell_output("#{bin}/narwhal --version")
  end
end
```

### Step 5: docs/RELEASING.md

The release procedure:

```markdown
# Releasing narwhal

1. Cut the version
   - Bump workspace.package.version in Cargo.toml.
   - Update CHANGELOG.md.
2. Commit and tag
   - git commit -am "chore: release v1.0.0"
   - git tag -s v1.0.0 -m "v1.0.0"
3. Publish to crates.io (in dependency order)
   - cargo publish -p narwhal-core
   - cargo publish -p narwhal-config
   - cargo publish -p narwhal-sql
   - cargo publish -p narwhal-pool
   - cargo publish -p narwhal-history
   - cargo publish -p narwhal-driver-postgres
   - cargo publish -p narwhal-driver-mysql
   - cargo publish -p narwhal-driver-sqlite
   - cargo publish -p narwhal-driver-duckdb
   - cargo publish -p narwhal-driver-clickhouse
   - cargo publish -p narwhal-vim
   - cargo publish -p narwhal-tui
   - cargo publish -p narwhal-plugin
   - cargo publish -p narwhal-plugin-lua
   - cargo publish -p narwhal-app
   - cargo publish -p narwhal (binary)
4. Build release artifacts
   - cargo build --release --bin narwhal
   - tar artifacts per platform
   - Sign with cosign or GPG.
5. Push the tag
   - git push origin v1.0.0
6. Update packaging templates
   - Bump pkgver in packaging/aur/PKGBUILD.
   - Bump url + sha256 in packaging/homebrew/narwhal.rb.
   - Open PRs / AUR submissions.
```

### Step 6: verify cargo install on a fresh shell

```sh
# Outside the nix flake, with rustup-installed stable rustc:
cd /tmp && git clone <repo> narwhal-install-check
cd narwhal-install-check
cargo install --path narwhal
narwhal --version
```

Fails reveal missing system deps; document them under "Install"
in the README (07-11) and in PKGBUILD `depends`.

## Files

- `Cargo.toml` (workspace.package shared metadata)
- All `crates/*/Cargo.toml` (publication metadata)
- `narwhal/Cargo.toml` (binary crate metadata)
- `packaging/aur/PKGBUILD` (new)
- `packaging/homebrew/narwhal.rb` (new)
- `docs/RELEASING.md` (new)
- `CHANGELOG.md` (new — first entry: v1.0.0)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports same count
  (no test delta).
- `cargo publish --dry-run -p narwhal-core` succeeds (verifies
  metadata).
- Fresh-shell `cargo install --path narwhal` works on Linux.

## Commit message template

```
chore(release): crates.io metadata + AUR / Homebrew templates + docs

cargo install --git works today but isn't discoverable; there's
no narwhal on crates.io and no packaging for any distro.  v1.0
needs a clear install path that doesn't require either git or
the nix flake.

Workspace.package factors the shared publication metadata —
license (MIT OR Apache-2.0), repository, keywords, categories —
so every crate inherits it and the per-crate Cargo.toml only
overrides name and description.  The publish order is
documented in docs/RELEASING.md: narwhal-core first, then the
helper crates, then drivers, then app, then the binary crate
last.

PKGBUILD template under packaging/aur/ and Homebrew formula
under packaging/homebrew/ document the recipe a packager would
use without committing to maintain those channels ourselves; the
release procedure doc notes that bumps land in the same PR as
the version tag.

CHANGELOG.md starts with the v1.0.0 entry summarising plans 04
through 07.

No code changes — this is metadata, templates, and docs.
```
