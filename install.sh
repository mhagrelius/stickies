#!/usr/bin/env bash
#
# Install Stickies and its GNOME Shell extension into the user's home
# directory. No root, no system paths — everything lands under ~/.local.
#
#   ./install.sh              build and install both halves
#   ./install.sh --app-only   skip the shell extension
#
set -euo pipefail

APP_ID="us.hagreli.Stickies"
EXT_UUID="stickies@hagreli.us"

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
DATA_DIR="$PREFIX/share"
EXT_DIR="$DATA_DIR/gnome-shell/extensions/$EXT_UUID"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

app_only=false
[[ "${1:-}" == "--app-only" ]] && app_only=true

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warning:\033[0m %s\n' "$*" >&2; }

# ---- the application ------------------------------------------------------

say "Building (release)"
cargo build --release

say "Installing to $PREFIX"
install -Dm755 target/release/stickies "$BIN_DIR/stickies"
install -Dm644 "data/$APP_ID.desktop" "$DATA_DIR/applications/$APP_ID.desktop"
install -Dm644 "data/$APP_ID.metainfo.xml" "$DATA_DIR/metainfo/$APP_ID.metainfo.xml"
install -Dm644 "data/icons/hicolor/scalable/apps/$APP_ID.svg" \
  "$DATA_DIR/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "data/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg" \
  "$DATA_DIR/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"

# The desktop file declares DBusActivatable, so GNOME needs a matching D-Bus
# service file to launch the app on demand (e.g. from the dock's context menu).
install -Dm644 /dev/stdin "$DATA_DIR/dbus-1/services/$APP_ID.service" <<EOF
[D-BUS Service]
Name=$APP_ID
Exec=$BIN_DIR/stickies --gapplication-service
EOF

if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache -qtf "$DATA_DIR/icons/hicolor" 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null; then
  update-desktop-database -q "$DATA_DIR/applications" 2>/dev/null || true
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR is not on your PATH; add it to run 'stickies' from a terminal" ;;
esac

# ---- the shell extension --------------------------------------------------

if $app_only; then
  say "Skipping the shell extension (--app-only)"
  echo
  warn "Without it, notes will not be repositioned: Wayland gives applications"
  warn "no way to place their own windows."
  exit 0
fi

say "Installing the GNOME Shell extension"
mkdir -p "$EXT_DIR"
for f in extension.js geometry.js interface.js metadata.json; do
  install -Dm644 "extension/$f" "$EXT_DIR/$f"
done

# The global shortcut and the taskbar option live in GSettings, so the schema
# has to be compiled into the extension directory for the shell to read it.
install -Dm644 extension/schemas/org.gnome.shell.extensions.stickies.gschema.xml \
  "$EXT_DIR/schemas/org.gnome.shell.extensions.stickies.gschema.xml"
glib-compile-schemas "$EXT_DIR/schemas" \
  || warn "could not compile the extension schema; the global shortcut will not work"

if command -v gnome-extensions >/dev/null; then
  # enable is a no-op if it is already on.
  gnome-extensions enable "$EXT_UUID" 2>/dev/null \
    || warn "could not enable $EXT_UUID automatically; run: gnome-extensions enable $EXT_UUID"
fi

echo
say "Installed."
cat <<EOF

  Binary     $BIN_DIR/stickies
  Notes      \${XDG_DATA_HOME:-\$HOME/.local/share}/stickies/notes.json
  Extension  $EXT_DIR
  Shortcut   Super+Shift+N creates a note (once the extension loads)

The shell extension only takes effect after the shell reloads it. On Wayland
that means logging out and back in — GNOME cannot restart the compositor in
place. Until then Stickies runs fine, it just will not reposition notes.

Check it is live with:

  gnome-extensions info $EXT_UUID
  busctl --user introspect $APP_ID.Shell /us/hagreli/Stickies/Shell

EOF
