#!/bin/sh
# Installs OmaTuber for the current user: binary in ~/.cargo/bin, launcher
# and icon under ~/.local/share so it shows up in the Omarchy app menu.
set -eu
cd "$(dirname "$0")/.."
cargo install --locked --path .
data="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm644 packaging/omatuber.desktop "$data/applications/omatuber.desktop"
install -Dm644 packaging/omatuber.svg "$data/icons/hicolor/scalable/apps/omatuber.svg"
# The launcher runs `omatuber` from PATH; ~/.cargo/bin must be on it.
command -v omatuber >/dev/null || echo "warning: ~/.cargo/bin is not on PATH" >&2
