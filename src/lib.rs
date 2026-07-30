//! Stickies — sticky notes for the GNOME desktop.
//!
//! The crate splits into two halves. Everything under [`model`] is plain Rust:
//! no GTK, no display, no main loop, and therefore unit-testable anywhere. The
//! `ui` half wraps that in GTK 4/libadwaita widgets.
//!
//! Window placement is the awkward part. Wayland gives a client no way to
//! position its own toplevels or even read where they landed, and GNOME does
//! not implement `wlr-layer-shell`, so restoring a note to the same spot on the
//! same monitor is impossible from inside the app. The companion GNOME Shell
//! extension in `extension/` does the moving on our behalf and answers geometry
//! queries over D-Bus; [`placement`] is the client for it. When the extension
//! is missing, every call degrades to a no-op and the app still runs — notes
//! simply land wherever Mutter puts them.

pub mod diagnostics;
pub mod model;
pub mod placement;
pub mod tray;
pub mod ui;

/// Reverse-domain application ID. Must match `data/us.hagreli.Stickies.desktop`
/// and the GSettings schema path.
pub const APP_ID: &str = "us.hagreli.Stickies";

/// D-Bus object path prefix GTK exports application windows under. Mutter reads
/// the per-window path out of the `gtk_shell1` Wayland protocol, which is how
/// the shell extension matches a `Meta.Window` back to one of our notes.
pub const APP_OBJECT_PATH: &str = "/us/hagreli/Stickies";
