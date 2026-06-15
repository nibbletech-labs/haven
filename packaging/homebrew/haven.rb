# Homebrew formula for Haven — prebuilt binary, no Rust toolchain needed.
#
# This is the template/reference copy. The release workflow
# (.github/workflows/release.yml) renders a ready-to-paste `haven.rb` with the
# real version + per-arch sha256s on every tag and attaches it to the Release
# (and prints it to the job summary). To serve `brew install
# nibbletech-labs/tap/haven`, copy that generated file into the tap repo
# (nibbletech-labs/homebrew-tap) as `Formula/haven.rb` on each release.
class Haven < Formula
  desc "Local-first, cloud-synced store for a long-lived work-graph"
  homepage "https://github.com/nibbletech-labs/haven"
  version "0.1.1"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.1/haven-0.1.1-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256"
    end
    on_intel do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.1/haven-0.1.1-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.1/haven-0.1.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_UNKNOWN_LINUX_MUSL_SHA256"
    end
    on_intel do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.1/haven-0.1.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_X86_64_UNKNOWN_LINUX_MUSL_SHA256"
    end
  end

  def install
    bin.install "haven"
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
    assert_match "haven", shell_output("#{bin}/haven --version")
  end
end
