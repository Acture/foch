#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 4 ]; then
	echo "usage: $0 <repo> <version> <url> <sha256>" >&2
	exit 2
fi

repo="$1"
version="$2"
url="$3"
sha256="$4"

case "$url" in
	*/foch-"${version}"-source.tar.gz) ;;
	*) echo "expected a versioned Foch release source archive" >&2; exit 2 ;;
esac

cat <<EOF
class Foch < Formula
  desc "EU4 mod analysis, merging and language server toolkit"
  homepage "https://github.com/${repo}"
  url "${url}"
  sha256 "${sha256}"
  license all_of: ["AGPL-3.0-only", "GPL-3.0-only", "MIT"]

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "src/apps/foch-cli"), "--bin", "foch"
    pkgshare.install "NOTICE.md", "LICENSE-MERGIRAF.txt"
  end

  test do
    ENV["HOME"] = testpath
    ENV["XDG_CONFIG_HOME"] = testpath/".config"
    (testpath/"local_patch/descriptor.mod").write 'name="Homebrew test"'
    (testpath/"foch.toml").write <<~TOML
      [project]
      game = "eu4"

      [[project.mods]]
      id = "local_patch"
      path = "local_patch"
    TOML

    output = shell_output("#{bin}/foch input inspect #{testpath}/foch.toml")
    assert_match "game: eu4", output
    assert_match "local_patch", output
    assert_match (testpath/"local_patch").to_s, output
    assert_match "cwt-schema", shell_output("#{bin}/foch --version")
  end
end
EOF
