#!/usr/bin/env bash
#
# Run the whole suite the way CI does.
#
#   ./test.sh            use the current session's display
#   ./test.sh --headless run under Xvfb and a private D-Bus session
#
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# GTK_A11Y=none skips the accessibility bus, a common source of CI hangs.
# GSETTINGS_BACKEND=memory keeps tests from touching real user state.
export GTK_A11Y=none
export GSETTINGS_BACKEND=memory

# Widget tests need a display and a session bus (GtkApplication registers on
# it to assign window IDs, which is what the shell extension keys on).
run=(cargo test --all-targets)
if [[ "${1:-}" == "--headless" ]]; then
  command -v xvfb-run >/dev/null || { echo "install xvfb first" >&2; exit 1; }

  # The private bus activates its own xdg-document-portal, which mounts a FUSE
  # fs at $XDG_RUNTIME_DIR/doc. Inheriting the login session's runtime dir means
  # that mount lands on /run/user/$UID/doc, on top of the real portal's; the real
  # one exits 21 and every flatpak launch fails until it is restarted. Hand the
  # session a throwaway runtime dir so its portals stay inside it.
  runtime_dir="$(mktemp -d)"
  chmod 700 "$runtime_dir"
  trap 'rc=$?; fusermount3 -u "$runtime_dir/doc" 2>/dev/null || :; rm -rf "$runtime_dir"; exit $rc' EXIT
  export XDG_RUNTIME_DIR="$runtime_dir"

  run=(xvfb-run -a dbus-run-session -- cargo test --all-targets)
fi

echo "==> cargo fmt --check"
cargo fmt --check

echo "==> cargo clippy"
cargo clippy --all-targets -- -D warnings

echo "==> ${run[*]}"
"${run[@]}"

# The shell extension is JavaScript and never sees cargo. Its arithmetic and
# its D-Bus contract live in shell-free modules precisely so they can be tested
# here, outside a live gnome-shell.
if command -v gjs >/dev/null; then
  echo "==> gjs (extension)"
  (cd extension && gjs -m test.js)
else
  echo "==> skipping extension tests (gjs not installed)"
fi

echo "==> extension metadata"
python3 -c "
import json
meta = json.load(open('extension/metadata.json'))
for key in ('uuid', 'name', 'description', 'shell-version', 'version'):
    assert key in meta, f'metadata.json is missing {key}'
assert meta['uuid'] == 'stickies@hagreli.us', 'uuid must match install.sh'
print('metadata.json ok, supports shell', ', '.join(meta['shell-version']))
"

echo
echo "All checks passed."
