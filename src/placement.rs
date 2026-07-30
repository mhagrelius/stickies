//! D-Bus client for the companion GNOME Shell extension.
//!
//! # Why this exists
//!
//! On GNOME Wayland a client cannot position its own toplevel windows, and
//! cannot read back where the compositor put them. `xdg-shell` has no
//! positioning request, Mutter does not implement `wlr-layer-shell`, and GTK 4
//! removed `gtk_window_move` outright. Everything a sticky-note app needs here
//! is only reachable from inside the compositor.
//!
//! The extension in `extension/` runs there, owns `us.hagreli.Stickies.Shell`
//! on the session bus, and matches our windows by the D-Bus object path GTK
//! publishes for each `GtkApplicationWindow` — Mutter receives it over the
//! `gtk_shell1` Wayland protocol and exposes it as
//! `meta_window_get_gtk_window_object_path`.
//!
//! # Degradation
//!
//! Every call here is best-effort. If the extension is not installed, not
//! enabled, or the session bus is unavailable, calls return `None`/`false` and
//! the app carries on — notes keep their content and colour and simply open
//! wherever Mutter decides.

use crate::model::{Monitor, Placement};
use gtk::gio;
use gtk::glib::{self, prelude::ToVariant};

/// Well-known name, object path and interface the extension exports.
pub const BUS_NAME: &str = "us.hagreli.Stickies.Shell";
pub const OBJECT_PATH: &str = "/us/hagreli/Stickies/Shell";
pub const INTERFACE: &str = "us.hagreli.Stickies.Shell";

/// Protocol version this client speaks. The extension reports its own via the
/// `Version` property; a mismatch means the two halves were installed from
/// different releases and should not talk to each other.
pub const PROTOCOL_VERSION: u32 = 1;

/// D-Bus calls are answered from the compositor's main loop, which is never
/// slow but can be busy. Short timeout: a stalled placement call must not hold
/// up presenting the note.
const TIMEOUT_MS: i32 = 2000;

/// Client handle for the shell extension.
#[derive(Clone)]
pub struct Shell {
    connection: Option<gio::DBusConnection>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("connected", &self.connection.is_some())
            .finish()
    }
}

impl Shell {
    /// Connect to the session bus. A missing bus is not an error — it yields a
    /// client whose every call is a no-op.
    pub fn session() -> Self {
        let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(connection) => Some(connection),
            Err(err) => {
                glib::g_warning!("stickies", "no session bus, placement disabled: {err}");
                None
            }
        };
        Self { connection }
    }

    /// A client that never talks to anything, for tests and headless runs.
    pub fn disconnected() -> Self {
        Self { connection: None }
    }

    /// Whether the extension is reachable and speaks a compatible protocol.
    pub async fn is_available(&self) -> bool {
        self.version().await == Some(PROTOCOL_VERSION)
    }

    /// The extension's protocol version, or `None` if it is not running.
    pub async fn version(&self) -> Option<u32> {
        let reply = self
            .call(
                "org.freedesktop.DBus.Properties",
                "Get",
                Some(&(INTERFACE, "Version").to_variant()),
                Some(glib::VariantTy::new("(v)").unwrap()),
            )
            .await
            .ok()??;
        reply
            .try_child_value(0)
            .and_then(|v| v.as_variant())
            .and_then(|v| v.get::<u32>())
    }

    /// Every attached monitor, with work areas in absolute coordinates.
    ///
    /// Returns an empty vector when the extension is unavailable, which
    /// [`crate::model::geometry::resolve`] reads as "cannot place".
    pub async fn list_monitors(&self) -> Vec<Monitor> {
        match self
            .call(
                INTERFACE,
                "ListMonitors",
                None,
                Some(glib::VariantTy::new("(a(ssbiiii))").unwrap()),
            )
            .await
        {
            Ok(Some(variant)) => parse_monitors(&variant),
            Ok(None) => Vec::new(),
            Err(err) => {
                log_call_failure("ListMonitors", &err);
                Vec::new()
            }
        }
    }

    /// Move and resize the window with the given GTK object path.
    ///
    /// `placement` carries monitor-relative coordinates; the extension converts
    /// them against the live work area, so a stale monitor layout on our side
    /// cannot fling a note off-screen.
    pub async fn place(&self, object_path: &str, placement: &Placement) -> bool {
        let args = (
            object_path,
            placement.connector.as_str(),
            placement.x,
            placement.y,
            placement.width,
            placement.height,
        )
            .to_variant();
        self.call_bool("Place", Some(&args)).await
    }

    /// Read back where the compositor currently has the window, in
    /// monitor-relative coordinates. `None` when the window is not mapped yet
    /// or the extension is unavailable.
    pub async fn query(&self, object_path: &str) -> Option<Placement> {
        let reply = self
            .call(
                INTERFACE,
                "Query",
                Some(&(object_path,).to_variant()),
                Some(glib::VariantTy::new("(bsiiii)").unwrap()),
            )
            .await
            .ok()??;
        parse_placement(&reply)
    }

    /// Geometry for every mapped note window in one round trip, keyed by
    /// object path. Preferred over per-window [`Shell::query`] for the periodic
    /// sync: the cost is one D-Bus call regardless of how many notes are open.
    pub async fn query_all(&self) -> Vec<(String, Placement)> {
        match self
            .call(
                INTERFACE,
                "QueryAll",
                None,
                Some(glib::VariantTy::new("(a(ssiiii))").unwrap()),
            )
            .await
        {
            Ok(Some(variant)) => parse_query_all(&variant),
            Ok(None) => Vec::new(),
            Err(err) => {
                log_call_failure("QueryAll", &err);
                Vec::new()
            }
        }
    }

    /// Subscribe to the extension's `MonitorsChanged` signal, so notes can be
    /// re-resolved when a display is plugged, unplugged or rearranged.
    ///
    /// The returned subscription must be kept alive: dropping it
    /// unsubscribes. `None` when there is no session bus.
    #[must_use]
    pub fn connect_monitors_changed<F>(&self, callback: F) -> Option<gio::SignalSubscription>
    where
        F: Fn() + 'static,
    {
        let connection = self.connection.as_ref()?;
        Some(connection.subscribe_to_signal(
            Some(BUS_NAME),
            Some(INTERFACE),
            Some("MonitorsChanged"),
            Some(OBJECT_PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |_signal| callback(),
        ))
    }

    /// Keep the window above others, or stop doing so.
    pub async fn set_pinned(&self, object_path: &str, pinned: bool) -> bool {
        self.call_bool("SetPinned", Some(&(object_path, pinned).to_variant()))
            .await
    }

    async fn call_bool(&self, method: &str, args: Option<&glib::Variant>) -> bool {
        match self
            .call(
                INTERFACE,
                method,
                args,
                Some(glib::VariantTy::new("(b)").unwrap()),
            )
            .await
        {
            Ok(Some(reply)) => reply
                .try_child_value(0)
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false),
            Ok(None) => false,
            Err(err) => {
                log_call_failure(method, &err);
                false
            }
        }
    }

    /// `Ok(None)` means "no session bus"; `Err` means the call was made and
    /// failed (extension absent, method unknown, timeout).
    async fn call(
        &self,
        interface: &str,
        method: &str,
        args: Option<&glib::Variant>,
        reply_type: Option<&glib::VariantTy>,
    ) -> Result<Option<glib::Variant>, glib::Error> {
        let Some(connection) = self.connection.as_ref() else {
            return Ok(None);
        };
        connection
            .call_future(
                Some(BUS_NAME),
                OBJECT_PATH,
                interface,
                method,
                args,
                reply_type,
                gio::DBusCallFlags::NONE,
                TIMEOUT_MS,
            )
            .await
            .map(Some)
    }
}

/// Log a failed call at the right volume.
///
/// "The extension is not installed" is an expected, documented state and must
/// not warn on every tick — it would be pure noise for anyone who chose not to
/// install it. Anything else means the extension *is* there and misbehaving,
/// which is exactly what someone debugging needs to see without having to know
/// to set `G_MESSAGES_DEBUG` first.
fn log_call_failure(method: &str, err: &glib::Error) {
    if is_extension_absent(err) {
        glib::g_debug!("stickies", "{method}: shell extension not running");
        return;
    }

    // Deduplicated per method. The sync tick calls QueryAll every two seconds,
    // so a persistently broken extension would otherwise write the same line
    // to the journal ~1800 times an hour and bury everything else. The first
    // occurrence and any *change* are what carry information.
    let message = err.to_string();
    if !first_time_seeing(method, &message) {
        return;
    }
    glib::g_warning!(
        "stickies",
        "shell extension call {method} failed: {message}"
    );
}

/// Has this exact failure for this method already been reported?
///
/// Single-threaded by construction: every caller is on the GLib main loop.
fn first_time_seeing(method: &str, message: &str) -> bool {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static SEEN: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }
    SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        match seen.get(method) {
            Some(previous) if previous == message => false,
            _ => {
                seen.insert(method.to_string(), message.to_string());
                true
            }
        }
    })
}

/// Does this error mean "nobody owns that bus name"?
fn is_extension_absent(err: &glib::Error) -> bool {
    err.matches(gio::DBusError::ServiceUnknown) || err.matches(gio::DBusError::NameHasNoOwner)
}

/// Decode the `(a(ssbiiii))` reply of `ListMonitors`.
///
/// Malformed entries are skipped rather than failing the whole call: a monitor
/// we cannot parse should cost us that monitor, not every monitor.
pub fn parse_monitors(reply: &glib::Variant) -> Vec<Monitor> {
    let Some(array) = reply.try_child_value(0) else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| {
            let (connector, display_name, primary, x, y, width, height) =
                entry.get::<(String, String, bool, i32, i32, i32, i32)>()?;
            // A zero-sized monitor would make every later clamp degenerate.
            if width <= 0 || height <= 0 {
                return None;
            }
            Some(Monitor {
                connector,
                display_name,
                primary,
                x,
                y,
                width,
                height,
            })
        })
        .collect()
}

/// Decode the `(bsiiii)` reply of `Query`. The leading boolean is "found".
pub fn parse_placement(reply: &glib::Variant) -> Option<Placement> {
    let (found, connector, x, y, width, height) =
        reply.get::<(bool, String, i32, i32, i32, i32)>()?;
    if !found || width <= 0 || height <= 0 {
        return None;
    }
    Some(Placement {
        connector,
        x,
        y,
        width,
        height,
    })
}

/// Decode the `(a(ssiiii))` reply of `QueryAll` into `(object path, placement)`
/// pairs. Unparseable or degenerate entries are skipped, as in
/// [`parse_monitors`].
pub fn parse_query_all(reply: &glib::Variant) -> Vec<(String, Placement)> {
    let Some(array) = reply.try_child_value(0) else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| {
            let (path, connector, x, y, width, height) =
                entry.get::<(String, String, i32, i32, i32, i32)>()?;
            if width <= 0 || height <= 0 {
                return None;
            }
            Some((
                path,
                Placement {
                    connector,
                    x,
                    y,
                    width,
                    height,
                },
            ))
        })
        .collect()
}

/// The D-Bus object path GTK exports for a window, which is how the extension
/// identifies it. GTK derives it from the application's object path and the
/// window's sequential ID.
pub fn window_object_path(window_id: u32) -> String {
    format!("{}/window/{}", crate::APP_OBJECT_PATH, window_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    // glib::Variant needs no display and no gtk::init(), so the whole
    // encode/decode layer is testable headless.

    fn monitors_reply(entries: Vec<(&str, &str, bool, i32, i32, i32, i32)>) -> glib::Variant {
        let owned: Vec<(String, String, bool, i32, i32, i32, i32)> = entries
            .into_iter()
            .map(|(c, d, p, x, y, w, h)| (c.to_string(), d.to_string(), p, x, y, w, h))
            .collect();
        (owned,).to_variant()
    }

    #[test]
    fn window_object_paths_match_gtks_scheme() {
        assert_eq!(window_object_path(1), "/us/hagreli/Stickies/window/1");
        assert_eq!(window_object_path(42), "/us/hagreli/Stickies/window/42");
    }

    #[test]
    fn monitors_decode_from_the_wire_format() {
        let reply = monitors_reply(vec![
            ("DP-1", "Dell U4924DW", true, 72, 37, 5048, 1403),
            ("eDP-1", "Built-in display", false, 0, 1440, 1920, 1043),
        ]);
        let monitors = parse_monitors(&reply);
        assert_eq!(monitors.len(), 2);
        assert_eq!(
            monitors[0],
            Monitor {
                connector: "DP-1".into(),
                display_name: "Dell U4924DW".into(),
                primary: true,
                x: 72,
                y: 37,
                width: 5048,
                height: 1403,
            }
        );
        assert!(!monitors[1].primary);
    }

    #[test]
    fn an_empty_monitor_list_decodes_to_nothing() {
        assert!(parse_monitors(&monitors_reply(vec![])).is_empty());
    }

    #[test]
    fn degenerate_monitors_are_dropped_not_fatal() {
        let reply = monitors_reply(vec![
            ("DP-1", "Good", true, 0, 0, 2560, 1440),
            ("DP-9", "Zero width", false, 0, 0, 0, 1440),
            ("DP-8", "Negative height", false, 0, 0, 800, -1),
        ]);
        let monitors = parse_monitors(&reply);
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].connector, "DP-1");
    }

    #[test]
    fn a_reply_of_the_wrong_shape_yields_no_monitors() {
        assert!(parse_monitors(&"not a monitor list".to_variant()).is_empty());
    }

    #[test]
    fn placement_decodes_when_the_window_was_found() {
        let reply = (true, "DP-1".to_string(), 400, 300, 320, 340).to_variant();
        assert_eq!(
            parse_placement(&reply),
            Some(Placement {
                connector: "DP-1".into(),
                x: 400,
                y: 300,
                width: 320,
                height: 340,
            })
        );
    }

    #[test]
    fn placement_is_none_when_the_window_was_not_found() {
        let reply = (false, String::new(), 0, 0, 0, 0).to_variant();
        assert_eq!(parse_placement(&reply), None);
    }

    #[test]
    fn placement_rejects_a_zero_sized_result() {
        // An unmapped window can report a 0x0 frame; treating that as real
        // would shrink the note to nothing on the next save.
        let reply = (true, "DP-1".to_string(), 10, 10, 0, 0).to_variant();
        assert_eq!(parse_placement(&reply), None);
    }

    #[test]
    fn placement_rejects_a_malformed_reply() {
        assert_eq!(parse_placement(&42i32.to_variant()), None);
    }

    #[test]
    fn query_all_decodes_every_window() {
        let entries: Vec<(String, String, i32, i32, i32, i32)> = vec![
            (
                "/us/hagreli/Stickies/window/1".into(),
                "DP-1".into(),
                10,
                20,
                300,
                320,
            ),
            (
                "/us/hagreli/Stickies/window/2".into(),
                "eDP-1".into(),
                40,
                50,
                280,
                300,
            ),
        ];
        let decoded = parse_query_all(&(entries,).to_variant());
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].0, "/us/hagreli/Stickies/window/1");
        assert_eq!(decoded[0].1.connector, "DP-1");
        assert_eq!(decoded[1].1.x, 40);
    }

    #[test]
    fn query_all_skips_unmapped_windows() {
        let entries: Vec<(String, String, i32, i32, i32, i32)> = vec![
            (
                "/us/hagreli/Stickies/window/1".into(),
                "DP-1".into(),
                0,
                0,
                0,
                0,
            ),
            (
                "/us/hagreli/Stickies/window/2".into(),
                "DP-1".into(),
                5,
                5,
                300,
                320,
            ),
        ];
        let decoded = parse_query_all(&(entries,).to_variant());
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, "/us/hagreli/Stickies/window/2");
    }

    #[test]
    fn query_all_tolerates_a_malformed_reply() {
        assert!(parse_query_all(&"nope".to_variant()).is_empty());
        assert!(parse_query_all(
            &(Vec::<(String, String, i32, i32, i32, i32)>::new(),).to_variant()
        )
        .is_empty());
    }

    #[test]
    fn repeated_identical_failures_are_reported_once() {
        // The sync tick runs every two seconds; without this a broken
        // extension floods the journal and hides everything else.
        assert!(
            first_time_seeing("QueryAll", "boom"),
            "first occurrence reports"
        );
        assert!(!first_time_seeing("QueryAll", "boom"), "repeat stays quiet");
        assert!(
            first_time_seeing("QueryAll", "different"),
            "a changed error is new information"
        );
        assert!(
            first_time_seeing("ListMonitors", "boom"),
            "methods are tracked independently"
        );
        assert!(
            first_time_seeing("QueryAll", "boom"),
            "and it reports again once the error changes back"
        );
    }

    #[test]
    fn a_disconnected_client_degrades_to_no_ops() {
        let shell = Shell::disconnected();
        glib::MainContext::new().block_on(async {
            assert!(!shell.is_available().await);
            assert_eq!(shell.version().await, None);
            assert!(shell.list_monitors().await.is_empty());
            assert!(shell.query_all().await.is_empty());
            assert_eq!(shell.query("/us/hagreli/Stickies/window/1").await, None);
            assert!(
                !shell
                    .place(
                        "/us/hagreli/Stickies/window/1",
                        &Placement {
                            connector: "DP-1".into(),
                            x: 0,
                            y: 0,
                            width: 300,
                            height: 320,
                        },
                    )
                    .await
            );
            assert!(
                !shell
                    .set_pinned("/us/hagreli/Stickies/window/1", true)
                    .await
            );
        });
    }
}
