#!/usr/bin/env bash
set -euo pipefail

REPO="yijunyu/demo-precc"
PRECC_VERSION="0.1.0"
INSTALL_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"

ARCH="$(uname -m)"
OS="$(uname -s)"

case "$OS" in
    Linux)  OS_LABEL="unknown-linux-gnu" ;;
    Darwin) OS_LABEL="apple-darwin" ;;
    *)
        echo "Error: Unsupported OS: $OS" >&2
        echo "precc supports Linux and macOS." >&2
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64)        ARCH_LABEL="x86_64" ;;
    aarch64|arm64) ARCH_LABEL="aarch64" ;;
    *)
        echo "Error: Unsupported architecture: $ARCH" >&2
        echo "precc supports x86_64 and aarch64." >&2
        exit 1
        ;;
esac

TARGET="${ARCH_LABEL}-${OS_LABEL}"
ARCHIVE="precc-${TARGET}.tar.gz"

echo "precc installer"
echo "==============="
echo "Platform: ${TARGET}"
echo "Install:  ${INSTALL_DIR}"
echo ""

# Resolve latest release tag
if command -v gh &>/dev/null; then
    TAG="$(gh release view --repo "$REPO" --json tagName --jq '.tagName' 2>/dev/null || true)"
fi
if [[ -z "${TAG:-}" ]]; then
    RELEASE_JSON="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null || true)"
    TAG="$(echo "$RELEASE_JSON" | grep '"tag_name"' | head -1 | sed 's/.*: *"\(.*\)".*/\1/')"
fi
if [[ -z "${TAG:-}" ]]; then
    TAG="v$PRECC_VERSION"
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/download/$TAG/$ARCHIVE"
echo "Release: $TAG"
echo "Archive: $ARCHIVE"
echo ""

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading..."
if ! curl -fSL --progress-bar -o "$TMPDIR/$ARCHIVE" "$DOWNLOAD_URL" 2>/dev/null; then
    echo "" >&2
    echo "Error: no precompiled bundle available for $TARGET." >&2
    echo "Please file an issue at https://github.com/$REPO/issues" >&2
    exit 1
fi

echo "Extracting..."
tar -xzf "$TMPDIR/$ARCHIVE" -C "$TMPDIR"

mkdir -p "$INSTALL_DIR"
for bin in precc precc-sweep; do
    if [[ -f "$TMPDIR/$bin" ]]; then
        chmod +x "$TMPDIR/$bin"
        cp "$TMPDIR/$bin" "$INSTALL_DIR/$bin"
        echo "  Installed: $INSTALL_DIR/$bin"
    fi
done

echo ""
echo "Done! Run: precc --help"
