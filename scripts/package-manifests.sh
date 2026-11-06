#!/usr/bin/env bash
# Write a Homebrew formula (fsls.rb) and a Scoop manifest (fsls.json) for a release, with
# the SHA-256 of each archive in DIST. Copy them into a tap / bucket repository to publish.
# Usage: scripts/package-manifests.sh 0.1.0 dist/
set -euo pipefail

VERSION=${1:?usage: $0 VERSION DIST_DIR}
DIST=${2:?usage: $0 VERSION DIST_DIR}
REPO=https://github.com/gyazbek/fsls
BASE="$REPO/releases/download/v$VERSION"
DESC="List mounted file systems, disks and RAID health on Windows, Linux and macOS"

sha() {
    local f="$DIST/fsls-v$VERSION-$1"
    [ -f "$f" ] || { echo "missing $f" >&2; exit 1; }
    sha256sum "$f" | cut -d' ' -f1
}

cat >"$DIST/fsls.rb" <<EOF
class Fsls < Formula
  desc "$DESC"
  homepage "$REPO"
  version "$VERSION"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "$BASE/fsls-v$VERSION-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin.tar.gz)"
    end
    on_intel do
      url "$BASE/fsls-v$VERSION-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin.tar.gz)"
    end
  end

  on_linux do
    on_arm do
      url "$BASE/fsls-v$VERSION-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-musl.tar.gz)"
    end
    on_intel do
      url "$BASE/fsls-v$VERSION-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-musl.tar.gz)"
    end
  end

  def install
    bin.install "fsls"
    man1.install "man/fsls.1"
    bash_completion.install "completions/fsls.bash" => "fsls"
    zsh_completion.install "completions/_fsls"
    fish_completion.install "completions/fsls.fish"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/fsls --version")
  end
end
EOF

cat >"$DIST/fsls.json" <<EOF
{
  "version": "$VERSION",
  "description": "$DESC",
  "homepage": "$REPO",
  "license": "MIT|Apache-2.0",
  "architecture": {
    "64bit": {
      "url": "$BASE/fsls-v$VERSION-x86_64-pc-windows-msvc.zip",
      "hash": "$(sha x86_64-pc-windows-msvc.zip)",
      "extract_dir": "fsls-v$VERSION-x86_64-pc-windows-msvc"
    },
    "arm64": {
      "url": "$BASE/fsls-v$VERSION-aarch64-pc-windows-msvc.zip",
      "hash": "$(sha aarch64-pc-windows-msvc.zip)",
      "extract_dir": "fsls-v$VERSION-aarch64-pc-windows-msvc"
    }
  },
  "bin": "fsls.exe",
  "checkver": "github",
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "$REPO/releases/download/v\$version/fsls-v\$version-x86_64-pc-windows-msvc.zip",
        "extract_dir": "fsls-v\$version-x86_64-pc-windows-msvc"
      },
      "arm64": {
        "url": "$REPO/releases/download/v\$version/fsls-v\$version-aarch64-pc-windows-msvc.zip",
        "extract_dir": "fsls-v\$version-aarch64-pc-windows-msvc"
      }
    }
  }
}
EOF

echo "wrote $DIST/fsls.rb and $DIST/fsls.json"
