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
