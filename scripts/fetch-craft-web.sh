#!/usr/bin/env bash
set -euo pipefail

DEST="${1:-$(cd "$(dirname "$0")/.." && pwd)/web-apps}"
WORDCRAFT_VER="${WORDCRAFT_VER:-0.19.1}"
PDFCRAFT_VER="${PDFCRAFT_VER:-0.22.0}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch_and_unpack() {
    local repo="$1" tag="$2" zip="$3" out="$4"
    mkdir -p "$out"
    rm -rf "${out:?}"/*
    gh release download "$tag" -R "$repo" -p "$zip" -p "SHA256SUMS.txt" -D "$TMP" --clobber
    (cd "$TMP" && grep "$zip" SHA256SUMS.txt | sha256sum -c -)
    rm -f "$TMP/SHA256SUMS.txt"
    unzip -oq "$TMP/$zip" -d "$out"
    local nested="$out/${zip%.zip}"
    if [ -d "$nested" ]; then
        shopt -s dotglob nullglob
        mv "$nested"/* "$out"/
        rmdir "$nested"
    fi
}

fetch_and_unpack "storytold/wordcraft" "v${WORDCRAFT_VER}" "wordcraft-web-${WORDCRAFT_VER}.zip" "$DEST/wordcraft"
fetch_and_unpack "storytold/pdfcraft" "v${PDFCRAFT_VER}" "pdfcraft-web-${PDFCRAFT_VER}.zip" "$DEST/pdfcraft"

echo "Installed WordCraft v${WORDCRAFT_VER} and PdfCraft v${PDFCRAFT_VER} web bundles into $DEST"
