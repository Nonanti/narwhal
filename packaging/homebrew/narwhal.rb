class Narwhal < Formula
  desc "TUI database client with a built-in MCP server"
  homepage "https://github.com/Nonanti/narwhal"
  url "https://github.com/Nonanti/narwhal/archive/refs/tags/v2.0.0.tar.gz"
  sha256 "56dcb3bbeec4aace943cd6f10a31dae7f6366593cd19a49397f11983cd800387"  # shasum -a 256 v2.0.0.tar.gz
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/Nonanti/narwhal.git", branch: "main"

  # Build-time only. The mysql / postgres drivers link statically through
  # their respective Rust crates (rusqlite / duckdb are bundled, mssql via
  # tiberius); the client libraries are not needed at runtime.
  depends_on "rust"  => :build
  depends_on "cmake" => :build      # DuckDB bundled C++ tree
  uses_from_macos "llvm" => :build  # libclang for bindgen (DuckDB)

  def install
    system "cargo", "install", *std_cargo_args(path: "narwhal")
  end

  test do
    assert_match "narwhal", shell_output("#{bin}/narwhal --version")
    # `narwhal exec` with --help is hermetic (no DB connection).
    assert_match "narwhal", shell_output("#{bin}/narwhal --help")
  end
end
