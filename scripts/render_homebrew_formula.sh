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

cat <<EOF
class Foch < Formula
  desc "Paradox mod static analysis toolkit"
  homepage "https://github.com/${repo}"
  url "${url}"
  sha256 "${sha256}"
  version "${version}"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "."), "--bins"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/foch --version")
  end
end
EOF
