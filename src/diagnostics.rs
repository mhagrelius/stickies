//! Making failures findable.
//!
//! # The problem this solves
//!
//! Launched from the dock, the app is started by D-Bus activation and inherits
//! stdout/stderr from the bus daemon — sockets that produce no journal records.
//! `journalctl --user _COMM=stickies` returned "No entries" for every warning
//! the app had ever emitted. Combined with most failure paths logging at
//! *debug* level (off unless `G_MESSAGES_DEBUG` is set), a broken app was
//! effectively silent: placement failing, saves failing, and panics all looked
//! identical from outside, namely nothing.
//!
//! Three fixes live here:
//!
//! 1. [`install_log_writer`] sends records to the journal directly instead of
//!    relying on where stderr happens to point.
//! 2. [`install_panic_hook`] makes a Rust panic say so before the process dies.
//!    A panic inside a GTK or D-Bus callback cannot unwind — it aborts — so
//!    without this the only symptom is the window vanishing.
//! 3. [`Report`] backs `stickies --diagnose`, answering "is the other half
//!    working?" without needing a debugger or a rebuild.

use crate::model::store::{LoadOutcome, Store};
use gtk::glib;

/// Send log records to the journal, falling back to stderr.
///
/// `g_log_writer_journald` writes to the journal socket directly, so this works
/// regardless of what stdout and stderr are connected to. The fallback keeps
/// `cargo run` in a terminal readable.
///
/// Call once, before anything logs. Calling twice panics inside GLib, so this
/// is a no-op the second time.
pub fn install_log_writer() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    glib::log_set_writer_func(|level, fields| {
        match glib::log_writer_journald(level, fields) {
            glib::LogWriterOutput::Handled => glib::LogWriterOutput::Handled,
            // No journal (a container, a plain X session): fall back rather
            // than dropping the record entirely.
            _ => glib::log_writer_standard_streams(level, fields),
        }
    });
}

/// Log panics before the process dies.
///
/// Rust prints a panic to stderr, which — see the module docs — usually goes
/// nowhere. Worse, a panic raised inside a GTK signal handler or a D-Bus method
/// callback crosses `extern "C"` and aborts instead of unwinding, so there is
/// no chance to report it afterwards. This runs first and states plainly what
/// happened and where.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());

        // Not `payload_as_str()`: that is only stable since Rust 1.91, above
        // this crate's declared MSRV. Downcasting covers the same two payload
        // types (`panic!("literal")` and `panic!("{fmt}")`) on any version.
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic>");

        glib::g_critical!(
            "stickies",
            "PANIC at {location}: {message}\n\
             If this happened inside a signal or D-Bus callback the process is \
             about to abort rather than unwind. Re-run with \
             RUST_BACKTRACE=1 for frames."
        );

        // Chain up so the usual message and any backtrace still appear.
        previous(info);
    }));
}

/// A snapshot of everything worth knowing when something is not working.
///
/// Built as data rather than printed directly so it can be asserted on in
/// tests; [`Report::render`] does the formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub version: String,
    pub app_id: String,
    /// Session type as the environment reports it (`wayland`, `x11`, unknown).
    pub session_type: Option<String>,
    pub desktop: Option<String>,
    /// Protocol version the shell extension answers with, if it is running.
    pub extension_version: Option<u32>,
    /// Protocol version this build speaks.
    pub expected_extension_version: u32,
    /// Monitors the extension reports, as `(connector, WxH+X+Y)`.
    pub monitors: Vec<(String, String)>,
    /// Is a StatusNotifierWatcher hosting tray icons?
    pub tray_host: bool,
    pub store_path: String,
    pub store_outcome: String,
    pub note_count: usize,
    pub notes_with_geometry: usize,
    pub store_read_only: bool,
}

impl Report {
    /// Everything that can be gathered without a display or a main loop.
    ///
    /// The shell-extension parts are passed in by the caller, which needs an
    /// async context to fetch them.
    pub fn gather(
        extension_version: Option<u32>,
        monitors: Vec<(String, String)>,
        tray_host: bool,
    ) -> Self {
        let path = Store::default_path();
        let (store, outcome) = Store::open(&path);

        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            app_id: crate::APP_ID.to_string(),
            session_type: std::env::var("XDG_SESSION_TYPE").ok(),
            desktop: std::env::var("XDG_CURRENT_DESKTOP").ok(),
            extension_version,
            expected_extension_version: crate::placement::PROTOCOL_VERSION,
            monitors,
            tray_host,
            store_path: path.display().to_string(),
            store_outcome: match &outcome {
                LoadOutcome::Loaded => "loaded".to_string(),
                LoadOutcome::Fresh => "no file yet (first run)".to_string(),
                LoadOutcome::Recovered { backup, reason } => {
                    format!(
                        "RECOVERED: {reason} (previous file at {})",
                        backup.display()
                    )
                }
            },
            note_count: store.len(),
            notes_with_geometry: store
                .notes()
                .iter()
                .filter(|n| n.geometry.monitor.is_some())
                .count(),
            store_read_only: store.is_read_only(),
        }
    }

    /// Does the shell extension look usable?
    pub fn placement_working(&self) -> bool {
        self.extension_version == Some(self.expected_extension_version) && !self.monitors.is_empty()
    }

    /// Human-readable report, with the diagnosis spelled out rather than left
    /// for the reader to infer from the fields.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };

        line(format!("Stickies {} ({})", self.version, self.app_id));
        line(format!(
            "  session:            {} / {}",
            self.session_type.as_deref().unwrap_or("unknown"),
            self.desktop.as_deref().unwrap_or("unknown")
        ));
        line(String::new());

        line("Shell extension (window placement, global shortcut)".to_string());
        match self.extension_version {
            Some(v) if v == self.expected_extension_version => {
                line(format!("  status:             connected (protocol {v})"))
            }
            Some(v) => line(format!(
                "  status:             VERSION MISMATCH — extension speaks {v}, \
                 this build speaks {}. The two halves are from different \
                 releases; reinstall both.",
                self.expected_extension_version
            )),
            None => line(
                "  status:             NOT RUNNING — notes will not be repositioned.\n\
                 \x20                   Check: gnome-extensions info stickies@hagreli.us\n\
                 \x20                   A newly installed extension needs a logout."
                    .to_string(),
            ),
        }

        if self.monitors.is_empty() {
            line("  monitors:           none reported".to_string());
        } else {
            for (index, (connector, geometry)) in self.monitors.iter().enumerate() {
                let label = if index == 0 {
                    "  monitors:         "
                } else {
                    "                    "
                };
                line(format!("{label}  {connector}  {geometry}"));
            }
        }
        line(String::new());

        line("Status icon".to_string());
        line(format!(
            "  host:               {}",
            if self.tray_host {
                "present"
            } else {
                "none — the app quits with its last note instead of staying resident"
            }
        ));
        line(String::new());

        line("Notes".to_string());
        line(format!("  file:               {}", self.store_path));
        line(format!("  state:              {}", self.store_outcome));
        line(format!("  count:              {}", self.note_count));
        line(format!(
            "  with a position:    {} of {}",
            self.notes_with_geometry, self.note_count
        ));
        if self.store_read_only {
            line(
                "  WRITES DISABLED:    the file is newer than this build understands; \
                  changes are not being saved."
                    .to_string(),
            );
        }
        line(String::new());

        line(format!(
            "Placement: {}",
            if self.placement_working() {
                "working"
            } else {
                "unavailable — content, colour and size still persist"
            }
        ));

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            version: "0.1.0".into(),
            app_id: "us.hagreli.Stickies".into(),
            session_type: Some("wayland".into()),
            desktop: Some("ubuntu:GNOME".into()),
            extension_version: Some(1),
            expected_extension_version: 1,
            monitors: vec![("DP-1".into(), "5048x1403+72+37".into())],
            tray_host: true,
            store_path: "/home/u/.local/share/stickies/notes.json".into(),
            store_outcome: "loaded".into(),
            note_count: 4,
            notes_with_geometry: 4,
            store_read_only: false,
        }
    }

    #[test]
    fn a_healthy_system_reads_as_working() {
        let r = report();
        assert!(r.placement_working());
        let text = r.render();
        assert!(text.contains("connected (protocol 1)"));
        assert!(text.contains("Placement: working"));
        assert!(text.contains("DP-1"));
    }

    #[test]
    fn a_missing_extension_says_what_to_do_about_it() {
        let mut r = report();
        r.extension_version = None;
        r.monitors.clear();

        assert!(!r.placement_working());
        let text = r.render();
        assert!(text.contains("NOT RUNNING"));
        // The two things that actually resolve it.
        assert!(text.contains("gnome-extensions info"));
        assert!(text.contains("logout"));
        assert!(
            text.contains("content, colour and size still persist"),
            "must say what still works, so it does not read as total failure"
        );
    }

    #[test]
    fn a_version_mismatch_is_called_out_separately_from_absence() {
        // Two halves from different releases fail differently from a missing
        // extension, and the fix is different too.
        let mut r = report();
        r.extension_version = Some(99);

        assert!(!r.placement_working());
        let text = r.render();
        assert!(text.contains("VERSION MISMATCH"));
        assert!(text.contains("extension speaks 99"));
        assert!(text.contains("reinstall both"));
        assert!(!text.contains("NOT RUNNING"));
    }

    #[test]
    fn an_extension_reporting_no_monitors_is_not_working() {
        // The exact failure hit in practice: the extension was ACTIVE and
        // answered Version, but ListMonitors returned nothing usable.
        let mut r = report();
        r.monitors.clear();
        assert!(!r.placement_working(), "answering Version is not enough");
        assert!(r.render().contains("none reported"));
    }

    #[test]
    fn a_corrupt_store_is_reported_with_the_backup_path() {
        let mut r = report();
        r.store_outcome =
            "RECOVERED: expected value at line 1 (previous file at /tmp/n.corrupt-1)".into();
        let text = r.render();
        assert!(text.contains("RECOVERED"));
        assert!(
            text.contains("/tmp/n.corrupt-1"),
            "the rescue path must be printed"
        );
    }

    #[test]
    fn a_read_only_store_is_flagged_loudly() {
        let mut r = report();
        r.store_read_only = true;
        assert!(r.render().contains("WRITES DISABLED"));
    }

    #[test]
    fn a_missing_tray_host_explains_the_lifecycle_difference() {
        let mut r = report();
        r.tray_host = false;
        assert!(r.render().contains("quits with its last note"));
    }

    #[test]
    fn gather_reads_the_real_store_without_needing_a_display() {
        // Also the guard that --diagnose works with no session bus at all.
        let temp = tempfile::tempdir().unwrap();
        temp_env_scope(temp.path(), || {
            let r = Report::gather(None, Vec::new(), false);
            assert_eq!(r.note_count, 0);
            assert!(r.store_path.ends_with("stickies/notes.json"));
            assert!(r.render().contains("no file yet (first run)"));
        });
    }

    /// Run `f` with `XDG_DATA_HOME` pointed at `dir`.
    fn temp_env_scope(dir: &std::path::Path, f: impl FnOnce()) {
        let previous = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", dir) };
        f();
        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_DATA_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
    }

    #[test]
    fn the_panic_hook_can_be_installed() {
        // Installing twice would replace the hook rather than chain, so this
        // just proves the call is well-formed; behaviour is covered by the
        // process surviving the rest of the suite.
        install_panic_hook();
    }
}
