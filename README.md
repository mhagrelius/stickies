# Stickies

Sticky notes for the GNOME desktop, in Rust with GTK 4 and libadwaita.

Notes are small always-there windows you scatter across your screen. Close one
and it goes away; launch Stickies again and it comes back — same monitor, same
spot, same size.

<!-- Rendered with `cargo run --example preview` -->

## The Wayland problem, and what this does about it

**A Wayland client cannot position its own windows.** There is no `xdg-shell`
request for it, Mutter does not implement `wlr-layer-shell`, GTK 4 removed
`gtk_window_move`, and a client cannot even *read back* where the compositor put
its window. On GNOME 49 and later there is no X11 session to fall back to.

So the "come back to the same place" part cannot be done by the app alone. It
ships in two halves:

| Half | Where it runs | Job |
|---|---|---|
| `stickies` | Your session | The notes: content, colour, size, persistence |
| `stickies@hagreli.us` | Inside GNOME Shell | Moving windows and reporting where they are |

They talk over the session bus (`us.hagreli.Stickies.Shell`). The extension
identifies note windows by the D-Bus object path GTK publishes for each
application window — Mutter receives it over the `gtk_shell1` protocol — and
**refuses to touch any window whose path is not under Stickies' own prefix**.
That prefix is derived by GTK from the application ID, so no other application
can produce a matching path. The extension cannot move, resize or raise anyone
else's windows.

**Without the extension, Stickies still works.** Notes, colours, sizes and
content all persist; only placement is left to Mutter. Every D-Bus call
degrades to a no-op.

## Install

```sh
./install.sh              # app + shell extension, into ~/.local
./install.sh --app-only   # skip the extension
./uninstall.sh            # remove it again (--purge also deletes your notes)
```

Nothing goes outside `~/.local`, and nothing needs root.

The shell extension only takes effect once GNOME Shell loads it. On Wayland
that means **logging out and back in** — the compositor cannot be restarted in
place. Until then Stickies runs normally, it just will not reposition notes.

Verify both halves afterwards:

```sh
gnome-extensions info stickies@hagreli.us
busctl --user introspect us.hagreli.Stickies.Shell /us/hagreli/Stickies/Shell
```

The About dialog also reports whether the extension is connected.

### Requirements

GTK 4.16+, libadwaita 1.9+, GNOME Shell 48–50, Rust 1.80+. Built and tested on
Ubuntu 26.04 (GNOME 50.1, GTK 4.22.4, libadwaita 1.9.1, Wayland).

## Using it

| | |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>N</kbd> | New note |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Put the note away — kept, just off screen |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>A</kbd> | Show all notes |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | Keep on top |
| <kbd>Ctrl</kbd>+<kbd>Delete</kbd> | Delete note |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |
| <kbd>Super</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> | New note, from anywhere |

The global shortcut is registered by the shell extension — a Wayland client
cannot grab one itself. Change it with:

```sh
gsettings set org.gnome.shell.extensions.stickies new-note "['<Super>n']"
gsettings set org.gnome.shell.extensions.stickies new-note "[]"   # disable
```

### Markdown

Notes are Markdown. When you are not editing one it shows **rendered** — headings
large, `**bold**` bold, `> quotes` indented; when you click in, the markup
reappears so you can edit it.

That is one text view throughout, not two swapped views: the syntax characters
carry a tag whose visibility is toggled on focus. So the cursor lands exactly
where you clicked, and the scroll position never jumps.

Supported: `# headings` (1–6), `**bold**`, `*italic*`, `~~strikethrough~~`,
`` `code` ``, ``` fences ```, `> quotes`, `- ` and `1. ` lists, and
`[links](url)`. List bullets stay visible — hiding them would leave an
unexplained indent. Anything unrecognised, including half-typed markup, is left
as plain text.

**Enter carries a list on.** A new line inside a list starts with the same
indent and bullet, or the next number. Enter on an empty item ends the list
instead, and Backspace takes the bullet back off.

**The file stays plain Markdown.** Rendering is a view concern; `notes.json`
holds exactly what you typed.

New notes take the **least-used colour**, so each one is a fresh colour while
any remain unused. Colour only earns its place if it distinguishes notes.

### The status-bar icon

Stickies puts an icon in the top bar with New Note, Show/Hide All, a live note
count, and Quit. It needs a StatusNotifierItem host — on Ubuntu that is the
`ubuntu-appindicators` extension, which ships enabled.

**With the icon present, closing the last note no longer quits**: the icon and
the global shortcut are still there to reach the app. Quit is an explicit menu
item or <kbd>Ctrl</kbd>+<kbd>Q</kbd>.

Where nothing hosts tray icons, Stickies refuses to create an invisible one and
falls back to quitting with the last note, so it can never strand itself with no
way back in. To opt out on a system that *does* have a host:

```sh
STICKIES_NO_TRAY=1 stickies
```

Right-clicking the dock icon also gives you **New Note** and **Show All Notes**.

Launching again restores the notes that were visible; if you had closed them
all, it brings all of them back rather than opening to nothing.

**× deletes the note**, the way taking a sticky off the wall does. A blank note
goes straight in the bin; one with writing on it asks first, so a misclick
cannot cost you anything. To keep a note but clear it off the screen, use **Put
Away** in the menu or <kbd>Ctrl</kbd>+<kbd>W</kbd>.

Launching with every note put away opens the most recently edited one — not all
of them. **Show All Notes** is in the menu when you want the rest.

## How it works

```
src/
  model/          no GTK, no display — unit-testable anywhere
    note.rs         a note: id, body, palette, geometry, visible, pinned
    geometry.rs     monitor-relative placement arithmetic
    markdown.rs     the live-preview parser: styles *and* hideable syntax
    store.rs        the notes.json file: atomic writes, corruption recovery
  placement.rs    D-Bus client for the shell extension
  tray.rs         StatusNotifierItem + dbusmenu, on plain gio
  ui/
    application.rs  owns the store, the windows, and the 2s sync tick
    note_window.rs  one window per note; renders and reports, never persists
    style.css       the seven palettes, light and dark
extension/
  extension.js    shell glue (Meta calls — only runs inside gnome-shell)
  geometry.js     the same clamping arithmetic, testable under plain gjs
  interface.js    the D-Bus contract, shared by extension.js and test.js
  schemas/        GSettings: the global shortcut and the taskbar option
```

**The store is canonical.** Windows emit signals describing what the user did;
`StickiesApplication` is the only thing that mutates a note or writes to disk.
One place to lose data means one place to get it right.

**Positions are monitor-relative.** A note remembers a connector name (`DP-1`)
plus coordinates relative to that monitor's *work area* — panels and docks
already subtracted. Rearranging your displays moves the work area and the notes
follow it, instead of being stranded in dead space. If the monitor is gone the
note lands on the primary one, clamped to fit; plug it back in and the note
returns to where it was, because resolution never overwrites what is stored.

**Both sides clamp.** The app resolves against a monitor list that may be a few
seconds old; the extension re-clamps against the compositor's live view before
moving anything. Neither trusts the other's arithmetic.

**The tray is hand-written on `gio`.** `ksni` would have been the obvious
dependency, but it brings zbus *and a full tokio runtime*. Everything here runs
on the GLib main loop and GTK widgets are not `Send`, so every menu click would
arrive on a tokio worker and need shuttling back before it could touch anything.
Implementing `org.kde.StatusNotifierItem` and `com.canonical.dbusmenu` on the
D-Bus connection the app already has keeps callbacks on the main loop.

**Saving is coalesced.** A 2-second tick reads geometry back from the compositor
(one `QueryAll` round trip regardless of note count) and flushes the store if
anything changed, so typing never blocks on I/O. A hard crash costs at most a
couple of seconds. Writes go to a temp file and are `fsync`ed before an atomic
rename, so an interrupted write cannot destroy the previous notes. A file that
fails to parse is moved to `notes.json.corrupt-<timestamp>` and the app starts
empty rather than refusing to launch; a file from a *newer* schema version is
opened read-only and never overwritten.

Notes live in `~/.local/share/stickies/notes.json`. It is plain JSON — readable,
greppable, editable, syncable.

## Packaging and sharing

```sh
packaging/build-deb.sh              # dist/stickies_0.1.0_amd64.deb
packaging/build-extension-zip.sh    # dist/stickies@hagreli.us.shell-extension.zip
packaging/build-flatpak.sh          # the app half, installed --user
```

**The one thing that shapes all of this: no sandboxed format can ship the shell
extension.** GNOME Shell loads extension JavaScript into the compositor process
from two fixed, unsandboxed directories. Flatpak and Snap both lose the
extension half; Snap additionally cannot write to `~/.local` at all under strict
confinement (the `home` interface excludes dot-directories), so it would need a
`personal-files` plug or classic confinement, both of which require Snap Store
manual review.

| Format | App | Extension | Notes |
|---|---|---|---|
| **`.deb`** | ✅ `/usr/bin` | ✅ `/usr/share/gnome-shell/extensions` | The only single artifact that covers both. Needs root. |
| `install.sh` | ✅ `~/.local/bin` | ✅ `~/.local/share/...` | No root, one command, no packaging tooling. |
| Flatpak | ✅ sandboxed | ❌ | Extension installed separately. Needs `--talk-name` to reach it. |
| Snap | ✅ sandboxed | ❌ | Plus confinement and store-review problems. Not recommended. |

The conventional split for a two-part GNOME app is **extension →
[extensions.gnome.org](https://extensions.gnome.org)** (one-click install,
per-shell-version compatibility handled for you) and **app → deb/PPA or
Flatpak**. `build-extension-zip.sh` checks the submission against what EGO
reviewers reject for — `eval`, incomplete `disable()`, untracked timers, legacy
`imports.*`, bad metadata, nested archive layout — before packing.

The deb raises the library floors that `dpkg-shlibdeps` cannot see: the
stylesheet needs GTK 4.16 for CSS custom properties, which is a runtime
requirement with no corresponding ELF symbol.

### Flatpak prerequisites

```sh
sudo apt install flatpak-builder
```

`build-flatpak.sh` handles the rest. It installs the runtimes if missing, and
runs upstream's `flatpak-cargo-generator.py` — which needs `aiohttp` and
`tomlkit` — through `uv run --with`, so those land in a throwaway environment
rather than on the system interpreter. Without `uv` it falls back to
`/usr/bin/python3` and tells you the `apt install` line.

The rust SDK extension is versioned by the *freedesktop* base the GNOME SDK is
built on, not by the GNOME version (GNOME 50 sits on freedesktop 25.08). The
script reads that from the SDK metadata rather than hardcoding it.

Note the Flatpak keeps notes in `~/.var/app/us.hagreli.Stickies/data/stickies/`,
**not** `~/.local/share/stickies/` — a native install and a Flatpak install do
not share notes.

## Development

```sh
cargo run                              # against your real notes
XDG_DATA_HOME=/tmp/scratch cargo run   # against a throwaway store

./test.sh              # fmt, clippy, cargo test, gjs extension tests
./test.sh --headless   # the same under Xvfb and a private D-Bus session

cargo run --example preview -- /tmp/preview light
cargo run --example preview -- /tmp/preview dark
```

`preview` renders every palette to PNG. Screenshotting a live GNOME Wayland
session needs interactive consent, which makes "does this look right?" hard to
answer while iterating; this renders the real widget tree instead.

### Tests

| Where | Covers |
|---|---|
| `src/**` unit tests | Notes, palettes, geometry, Markdown, the store, D-Bus encode/decode |
| `tests/session.rs` | Whole scenarios: relaunch, unplug a monitor, corrupt the file |
| `tests/widgets.rs` | The real `NoteWindow`, headless |
| `tests/lifecycle.rs` | The real application: first launch, close, relaunch |
| `extension/test.js` | The extension's arithmetic and its D-Bus contract |

`extension/test.js` asserts that every method signature matches the
`VariantTy` strings in `src/placement.rs`, so a change to one side that is not
mirrored on the other fails a test rather than silently doing nothing at
runtime.

`tests/lifecycle.rs` runs the real `StickiesApplication` against a redirected
`XDG_DATA_HOME`, under a test-only application ID — sharing the real one would
make the test process a *remote* for a running Stickies, and it would silently
drive the live app instead of itself. Each run is bounded by a timeout, because
"the app never quits" is one of the failure modes being tested.

`tests/widgets.rs` is one `#[test]` on purpose: GTK may be initialised from
exactly one thread and every widget call must come from it, but Rust's test
harness spawns a thread per `#[test]` and `--test-threads=1` only serialises
them. The runner inside it names each case and continues after failures.

The Meta calls in `extension.js` are the one thing no test covers — they exist
only inside a running gnome-shell. That file is kept as thin a wrapper over the
tested modules as possible for exactly that reason.

## Known limitations

- **Placement needs the extension.** Covered above. There is no way around it on
  GNOME Wayland.
- **The extension needs a logout to load.** GNOME Shell picks up newly installed
  extensions at startup, and Wayland has no shell restart.
- **Position is polled, not pushed.** The compositor does not tell the app when
  the user drags a note, so geometry is read back every 2 seconds and on close
  and quit. Dragging a note and pulling the power cord within the same 2 seconds
  loses that move.
- **Keep on top needs the extension.** Wayland has no client-side always-on-top.
  Without the extension the pin button is disabled and says why, rather than
  latching while nothing happens.
- **Hiding notes from the dock is best-effort.** Mutter exposes `skip-taskbar`
  as read-only; the extension sets it anyway and it may or may not take. Turn it
  off with `gsettings set org.gnome.shell.extensions.stickies
  hide-from-taskbar false`.
- **No sync, no search, no rich text.** Notes are plain text in one JSON file.

## Licence

GPL-3.0-or-later.
