#!/usr/bin/env bash
# Update vendored jj-cli config files to match the jj-lib version in Cargo.toml.
#
# Usage:
#   ./scripts/update-jj-config.sh          # auto-detect version from Cargo.toml
#   ./scripts/update-jj-config.sh 0.39.0   # explicit version

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

if [[ $# -ge 1 ]]; then
    VERSION="$1"
else
    # Extract jj-lib version from Cargo.toml (e.g. "0.39" -> "0.39.0")
    RAW=$(grep 'jj-lib' "$PROJECT_DIR/Cargo.toml" | grep -oP '"\K[0-9]+\.[0-9]+' | head -1)
    if [[ -z "$RAW" ]]; then
        echo "error: could not detect jj-lib version from Cargo.toml" >&2
        exit 1
    fi
    VERSION="${RAW}.0"
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
