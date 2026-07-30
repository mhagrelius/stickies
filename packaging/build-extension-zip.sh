#!/usr/bin/env bash
#
# Build the extensions.gnome.org submission bundle, and check it against the
# things EGO reviewers actually reject for.
#
#   packaging/build-extension-zip.sh
#
# Produces dist/stickies@hagreli.us.shell-extension.zip, ready to upload at
# https://extensions.gnome.org/upload/
#
set -euo pipefail

EXT_UUID="stickies@hagreli.us"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here"

DIST="$here/dist"
mkdir -p "$DIST"

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
pass() { printf '  \033[32mok\033[0m    %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; failures=$((failures + 1)); }

failures=0

# ---- review checks --------------------------------------------------------
#
# EGO review is done by humans reading the source. These cover the mechanical
# reasons submissions bounce, so they are caught here rather than a week later.

say "Review checks"

# Reviewers reject anything that can execute arbitrary strings.
if grep -nE '\beval\s*\(|new Function\s*\(|Function\s*\(\s*["'"'"']' extension/*.js \
     | grep -v '^extension/test.js:'; then
  fail "uses eval / Function() — an automatic rejection"
else
  pass "no eval or dynamic code construction"
fi

# Leaking anything past disable() is the single most common rejection.
if grep -q 'disable()' extension/extension.js \
   && grep -q '_service?.destroy()' extension/extension.js; then
  pass "disable() tears the service down"
else
  fail "disable() must destroy everything enable() created"
fi

for pattern in 'Gio.bus_unown_name' 'disconnect' 'unexport'; do
  if grep -q "$pattern" extension/extension.js; then
    pass "cleans up: $pattern"
  else
    fail "no $pattern — enable/disable cycles will leak"
  fi
done

# Extensions must not touch the session after being disabled.
if grep -qE 'setTimeout|setInterval' extension/extension.js; then
  fail "raw timers survive disable(); use GLib source ids and remove them"
else
  pass "no untracked timers"
fi

# Legacy imports were removed in GNOME 45.
if grep -qE '^\s*const .*= imports\.' extension/extension.js; then
  fail "uses the pre-45 imports.* system; must be ESM"
else
  pass "ESM imports only"
fi

# Metadata EGO validates on upload.
python3 - <<'PY' || failures=$((failures + 1))
import json, sys

meta = json.load(open('extension/metadata.json'))
ok = True

def need(key):
    global ok
    if key not in meta or meta[key] in ('', [], None):
        print(f"  \033[31mFAIL\033[0m  metadata.json missing {key}")
        ok = False
    else:
        print(f"  \033[32mok\033[0m    metadata.json has {key}")

for key in ('uuid', 'name', 'description', 'shell-version', 'url'):
    need(key)

# The uuid must match the directory name the shell loads it from.
if meta.get('uuid') != 'stickies@hagreli.us':
    print("  \033[31mFAIL\033[0m  uuid does not match the install path")
    ok = False

# EGO assigns the integer version itself; a string here is rejected.
if 'version' in meta and not isinstance(meta['version'], int):
    print("  \033[31mFAIL\033[0m  version must be an integer (EGO overwrites it anyway)")
    ok = False

# Listing an unreleased shell version gets the submission bounced.
versions = [str(v) for v in meta.get('shell-version', [])]
if any('.' in v for v in versions):
    print("  \033[33mnote\033[0m  point releases in shell-version are unnecessary; majors suffice")
print(f"  \033[32mok\033[0m    targets GNOME Shell {', '.join(versions)}")

sys.exit(0 if ok else 1)
PY

echo
if [[ $failures -gt 0 ]]; then
  echo "$failures check(s) failed — fix before uploading to extensions.gnome.org" >&2
  exit 1
fi

# ---- pack -----------------------------------------------------------------

say "Packing"
# gnome-extensions pack lays the zip out the way the shell and EGO expect:
# extension.js and metadata.json at the archive root, never in a subdirectory.
# Every additional module has to be named explicitly.
gnome-extensions pack extension \
  --extra-source=geometry.js \
  --extra-source=interface.js \
  --schema=schemas/org.gnome.shell.extensions.stickies.gschema.xml \
  --force \
  -o "$DIST"

ZIP="$DIST/$EXT_UUID.shell-extension.zip"

echo
say "Built $ZIP"
unzip -l "$ZIP" | sed 's/^/  /'

# The one layout mistake that makes the shell silently ignore the bundle.
if unzip -l "$ZIP" | grep -qE '  [^ ]+/(extension|metadata)\.'; then
  echo "the archive nests its files in a directory; the shell will not load it" >&2
  exit 1
fi

cat <<EOF

Upload at https://extensions.gnome.org/upload/

Reviewers read every line, so point them at the security boundary in your
submission notes: every D-Bus method resolves windows through _findWindow(),
which refuses any object path not under WINDOW_PATH_PREFIX. GTK derives that
prefix from the application ID, so the extension cannot act on windows
belonging to any other application.
EOF
