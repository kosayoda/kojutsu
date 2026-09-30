#!/usr/bin/env bash
# Update vendored jj-cli config files to match the jj-lib version Cargo.lock
# builds against.
#
# Usage:
#   ./scripts/update-jj-config.sh          # the version pinned in Cargo.lock
#   ./scripts/update-jj-config.sh 0.39.0   # explicit version

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

if [[ $# -ge 1 ]]; then
    VERSION="$1"
else
    # The version line following jj-lib's name in Cargo.lock.
    VERSION=$(grep -A1 '^name = "jj-lib"$' "$PROJECT_DIR/Cargo.lock" | grep -oP '^version = "\K[^"]+' || true)
    if [[ -z "$VERSION" ]]; then
        echo "error: could not find jj-lib in Cargo.lock" >&2
        exit 1
    fi
fi

echo "Fetching jj-cli config files for jj v${VERSION}..."

BASE_URL="https://raw.githubusercontent.com/jj-vcs/jj/v${VERSION}/cli/src/config"
DEST_DIR="$PROJECT_DIR/vendored"

mkdir -p "$DEST_DIR"

# Header we prepend to each vendored file
header() {
    cat <<EOF
# Vendored from jj-cli v${VERSION}
# Source: https://github.com/jj-vcs/jj/blob/v${VERSION}/cli/src/config/$1
# Update with: scripts/update-jj-config.sh
#
# jj-version: ${VERSION}

EOF
}

for file in revsets.toml; do
    echo "  -> ${file}"
    content=$(curl -sfL "${BASE_URL}/${file}")
    if [[ -z "$content" ]]; then
        echo "error: failed to fetch ${file}" >&2
        exit 1
    fi
    { header "$file"; echo "$content"; } > "${DEST_DIR}/${file}"
done

echo "Done. Vendored config updated to jj v${VERSION}."
