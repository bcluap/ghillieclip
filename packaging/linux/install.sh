#!/usr/bin/env bash
# Installs GhillieClip for the current user (no root needed).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm755 "$here/ghillieclip" "$bin/ghillieclip"
install -Dm644 "$here/ghillieclip.png" "$data/icons/hicolor/256x256/apps/ghillieclip.png"
install -Dm644 "$here/ghillieclip.desktop" "$data/applications/ghillieclip.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
echo "Installed to $bin/ghillieclip. Make sure $bin is on your PATH."
command -v ffmpeg >/dev/null || echo "Note: ffmpeg was not found; install it with your package manager."
