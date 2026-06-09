# Homebrew formula for Haven (build from source).
#
# To serve `brew install nibbletech-labs/tap/haven`, copy this file into a tap
# repo named `homebrew-tap` (i.e. nibbletech-labs/homebrew-tap) as
# `Formula/haven.rb`. The `url`/`sha256` below track the v0.1.0 source tarball;
# bump both on each release (`brew fetch` prints the sha, or shasum the tarball).
class Haven < Formula
  desc "Local-first, cloud-synced store for a long-lived work-graph"
  homepage "https://github.com/nibbletech-labs/haven"
  url "https://github.com/nibbletech-labs/haven/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_TARBALL_SHA256"
  license "MIT"
  head "https://github.com/nibbletech-labs/haven.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/haven-cli")
  end

  def caveats
    <<~EOS
      Wire the MCP server + Claude skill (idempotent):
        haven setup
      Then check the install:
        haven doctor
    EOS
  end

  test do
    assert_match "haven #{version}", shell_output("#{bin}/haven --version")
  end
end
