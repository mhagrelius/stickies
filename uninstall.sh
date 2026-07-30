#!/usr/bin/env bash
#
# Remove everything install.sh put in place. Notes are left alone unless
# --purge is given.
#
set -euo pipefail

APP_ID="us.hagreli.Stickies"
EXT_UUID="stickies@hagreli.us"

PREFIX="${PREFIX:-$HOME/.local}"
DATA_DIR="$PREFIX/share"
NOTES_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/stickies"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

if command -v gnome-extensions >/dev/null; then
  gnome-extensions disable "$EXT_UUID" 2>/dev/null || true
fi

say "Removing files"
rm -f  "$PREFIX/bin/stickies"
rm -f  "$DATA_DIR/applications/$APP_ID.desktop"
rm -f  "$DATA_DIR/metainfo/$APP_ID.metainfo.xml"
rm -f  "$DATA_DIR/dbus-1/services/$APP_ID.service"
rm -f  "$DATA_DIR/icons/hicolor/scalable/apps/$APP_ID.svg"
rm -f  "$DATA_DIR/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
rm -rf "$DATA_DIR/gnome-shell/extensions/$EXT_UUID"

if [[ "${1:-}" == "--purge" ]]; then
  say "Removing notes in $NOTES_DIR"
  rm -rf "$NOTES_DIR"
else
  say "Notes kept in $NOTES_DIR (pass --purge to delete them)"
fi

if command -v update-desktop-database >/dev/null; then
  update-desktop-database -q "$DATA_DIR/applications" 2>/dev/null || true
fi

say "Done. Log out and back in to unload the shell extension."
