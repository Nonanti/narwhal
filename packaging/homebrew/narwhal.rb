class Narwhal < Formula
  desc "TUI database client — DataGrip in your terminal"
  homepage "https://github.com/nonantiy/narwhal"
  url "https://github.com/nonantiy/narwhal/archive/v1.0.0.tar.gz"
  sha256 "REPLACE_AT_RELEASE_TIME"  # filled by `shasum -a 256 <tarball>`
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/nonantiy/narwhal.git", branch: "main"

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
