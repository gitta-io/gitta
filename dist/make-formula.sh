#!/usr/bin/env bash
# Homebrew formula for a release: make-formula.sh VERSION RELEASES_URL DIR_WITH_SHA256_FILES
set -euo pipefail
v="$1"; url="$2"; dir="$3"
sha() { cut -d' ' -f1 < "$dir/gitta-$v-$1.tar.gz.sha256"; }
cat <<RB
class Gitta < Formula
  desc "Version control for code written with AI: keeps parallel agents' work in sync"
  homepage "https://gitta.io"
  version "$v"
  on_macos do
    on_arm do
      url "$url/gitta-$v-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$url/gitta-$v-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end
  on_linux do
    on_arm do
      url "$url/gitta-$v-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-musl)"
    end
    on_intel do
      url "$url/gitta-$v-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-musl)"
    end
  end
  depends_on "git"

  def install
    bin.install Dir["*"]
  end

  test do
    system "git", "init", "-q", testpath/"r"
    assert_match version.to_s, shell_output("#{bin}/gitta --version")
  end
end
RB
