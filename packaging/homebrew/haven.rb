# Homebrew formula for Haven — prebuilt binary, no Rust toolchain needed.
#
# This is the template/reference copy. The release workflow
# (.github/workflows/release.yml) renders a real `haven.rb` with the release
# version + per-arch sha256s on every tag, attaches it to the Release, prints it
# to the job summary, and pushes it to nibbletech-labs/homebrew-tap for stable
# releases.
class Haven < Formula
  desc "Local-first work-graph store for humans and AI agents"
  homepage "https://github.com/nibbletech-labs/haven"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.5/haven-0.1.5-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256"
    end
    on_intel do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.5/haven-0.1.5-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.5/haven-0.1.5-aarch64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_UNKNOWN_LINUX_MUSL_SHA256"
    end
    on_intel do
      url "https://github.com/nibbletech-labs/haven/releases/download/v0.1.5/haven-0.1.5-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_X86_64_UNKNOWN_LINUX_MUSL_SHA256"
    end
  end

  def install
    bin.install "haven"
  end

  def caveats
    <<~EOS
      Wire the MCP server + skills and create/select a first project:
        haven setup --project-key my-work --project-title "My Work" --prefix MW
      Then add an item and check the install:
        haven item add "First item"
        haven doctor
    EOS
  end

  test do
    assert_match "haven", shell_output("#{bin}/haven --version")
  end
end
