#!/bin/bash
set -e

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
    echo "Usage: $0 <version>"
    echo "Example: $0 0.1.0"
    exit 1
fi

REPO="maramizo/agent-of-empires-2"
BASE_URL="https://github.com/${REPO}/releases/download/v${VERSION}"

echo "Fetching sha256 hashes for v${VERSION}..."
echo ""

for ARTIFACT in aoe2-darwin-arm64 aoe2-darwin-amd64 aoe2-linux-arm64 aoe2-linux-amd64; do
    URL="${BASE_URL}/${ARTIFACT}.tar.gz"
    echo "Downloading ${ARTIFACT}..."
    SHA=$(curl -sL "${URL}" | shasum -a 256 | cut -d' ' -f1)
    echo "  ${ARTIFACT}: ${SHA}"

    eval "SHA_${ARTIFACT//-/_}=${SHA}"
done

echo ""
echo "=== Update Formula/aoe2.rb with these values ==="
echo ""
cat << EOF
  on_macos do
    on_arm do
      url "https://github.com/${REPO}/releases/download/v${VERSION}/aoe2-darwin-arm64.tar.gz"
      sha256 "${SHA_aoe2_darwin_arm64}"
    end
    on_intel do
      url "https://github.com/${REPO}/releases/download/v${VERSION}/aoe2-darwin-amd64.tar.gz"
      sha256 "${SHA_aoe2_darwin_amd64}"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/${REPO}/releases/download/v${VERSION}/aoe2-linux-arm64.tar.gz"
      sha256 "${SHA_aoe2_linux_arm64}"
    end
    on_intel do
      url "https://github.com/${REPO}/releases/download/v${VERSION}/aoe2-linux-amd64.tar.gz"
      sha256 "${SHA_aoe2_linux_amd64}"
    end
  end
EOF
