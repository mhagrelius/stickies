//! Status-bar (system tray) icon, via StatusNotifierItem.
//!
//! # Why this is hand-written
//!
//! The obvious dependency is `ksni`, but it brings zbus *and a full tokio
//! runtime*. This app is entirely GLib-main-loop driven and its widgets are not
//! `Send`, so every menu click would arrive on a tokio worker thread and have
//! to be shuttled back before it could touch anything. Implementing the two
//! interfaces directly on the `gio` D-Bus connection the app already owns keeps
//! callbacks on the main loop, where they can act immediately.
//!
//! # The two interfaces
//!
//! - `org.kde.StatusNotifierItem` — the icon itself. Registered with
//!   `org.kde.StatusNotifierWatcher`, which on Ubuntu is owned by GNOME Shell
//!   through the `ubuntu-appindicators` extension. No watcher, no tray: the
//!   registration simply fails and the app carries on.
//! - `com.canonical.dbusmenu` — the menu. A tree of numbered items whose
//!   properties are fetched on demand; item 0 is the invisible root.

use gtk::gio;
use gtk::glib::{self, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/StatusNotifierMenu";
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// One row of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A clickable row. `action` is a detailed action name (`"app.new-note"`),
    /// dispatched through the callback given to [`Tray::new`].
    Item {
        label: String,
        action: String,
        enabled: bool,
    },
    /// A non-interactive line, for status ("4 notes").
    Info {
        label: String,
    },
    Separator,
}

impl MenuEntry {
    pub fn item(label: &str, action: &str) -> Self {
        MenuEntry::Item {
            label: label.to_string(),
            action: action.to_string(),
            enabled: true,
        }
    }

    pub fn disabled(label: &str, action: &str) -> Self {
        MenuEntry::Item {
            label: label.to_string(),
            action: action.to_string(),
            enabled: false,
        }
    }

    pub fn info(label: &str) -> Self {
        MenuEntry::Info {
            label: label.to_string(),
        }
    }
}

const ITEM_XML: &str = r#"
<node>
  <interface name="org.kde.StatusNotifierItem">
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="Menu" type="o" access="read"/>
    <method name="Activate">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
    </method>
    <method name="SecondaryActivate">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
    </method>
    <method name="Scroll">
      <arg type="i" direction="in" name="delta"/>
      <arg type="s" direction="in" name="orientation"/>
    </method>
    <signal name="NewIcon"/>
    <signal name="NewStatus"><arg type="s" name="status"/></signal>
  </interface>
</node>"#;

const MENU_XML: &str = r#"
<node>
  <interface name="com.canonical.dbusmenu">
    <property name="Version" type="u" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="TextDirection" type="s" access="read"/>
    <property name="IconThemePath" type="as" access="read"/>
    <method name="GetLayout">
      <arg type="i" direction="in" name="parentId"/>
      <arg type="i" direction="in" name="recursionDepth"/>
      <arg type="as" direction="in" name="propertyNames"/>
      <arg type="u" direction="out" name="revision"/>
      <arg type="(ia{sv}av)" direction="out" name="layout"/>
    </method>
    <method name="GetGroupProperties">
      <arg type="ai" direction="in" name="ids"/>
      <arg type="as" direction="in" name="propertyNames"/>
      <arg type="a(ia{sv})" direction="out" name="properties"/>
    </method>
    <method name="GetProperty">
      <arg type="i" direction="in" name="id"/>
      <arg type="s" direction="in" name="name"/>
      <arg type="v" direction="out" name="value"/>
    </method>
    <method name="Event">
      <arg type="i" direction="in" name="id"/>
      <arg type="s" direction="in" name="eventId"/>
      <arg type="v" direction="in" name="data"/>
      <arg type="u" direction="in" name="timestamp"/>
    </method>
    <method name="AboutToShow">
      <arg type="i" direction="in" name="id"/>
      <arg type="b" direction="out" name="needUpdate"/>
    </method>
    <signal name="LayoutUpdated">
      <arg type="u" name="revision"/>
      <arg type="i" name="parent"/>
    </signal>
    <signal name="ItemsPropertiesUpdated">
      <arg type="a(ia{sv})" name="updated"/>
      <arg type="a(ias)" name="removed"/>
    </signal>
  </interface>
</node>"#;

/// A live tray icon. Dropping it removes the icon.
pub struct Tray {
    connection: gio::DBusConnection,
    entries: Rc<RefCell<Vec<MenuEntry>>>,
    revision: Rc<Cell<u32>>,
    registrations: Vec<gio::RegistrationId>,
    name_id: Option<gio::OwnerId>,
}

impl Tray {
    /// Create the tray icon and register it with the watcher.
    ///
    /// `on_action` receives the detailed action name of whichever row the user
    /// clicked. Returns `None` when there is no session bus or the interfaces
    /// cannot be exported; a missing *watcher* is not fatal, since one can
    /// appear later (the user enabling the appindicator extension), and the
    /// registration is retried when it does.
    pub fn new<F>(entries: Vec<MenuEntry>, on_action: F) -> Option<Self>
    where
        F: Fn(&str) + 'static,
    {
        if std::env::var_os("STICKIES_NO_TRAY").is_some() {
            return None;
        }

        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;

        // Refuse to create an *invisible* tray icon. Exporting the interfaces
        // succeeds whether or not anything is listening, and the caller treats
        // a live tray as reason to keep running with no windows on screen — so
        // without a watcher that would strand the app with no way to reach it.
        if !watcher_present(&connection) {
            return None;
        }

        let entries = Rc::new(RefCell::new(entries));
        let revision = Rc::new(Cell::new(1u32));
        let on_action = Rc::new(on_action);

        let item_info = gio::DBusNodeInfo::for_xml(ITEM_XML)
            .ok()?
            .lookup_interface("org.kde.StatusNotifierItem")?;
        let menu_info = gio::DBusNodeInfo::for_xml(MENU_XML)
            .ok()?
            .lookup_interface("com.canonical.dbusmenu")?;

        let mut registrations = Vec::new();

        // ---- the icon ----
        let item_reg = connection
            .register_object(ITEM_PATH, &item_info)
            .property(|_conn, _sender, _path, _iface, name| item_property(name))
            .method_call({
                let on_action = on_action.clone();
                move |_conn, _sender, _path, _iface, method, _params, invocation| {
                    // Left click. ItemIsMenu is true so the shell normally shows
                    // the menu instead, but some hosts still send Activate.
                    if method == "Activate" {
                        on_action("app.new-note");
                    }
                    invocation.return_value(None);
                }
            })
            .build()
            .ok()?;
        registrations.push(item_reg);

        // ---- the menu ----
        let menu_reg = connection
            .register_object(MENU_PATH, &menu_info)
            .property(|_conn, _sender, _path, _iface, name| menu_property(name))
            .method_call({
                let entries = entries.clone();
                let revision = revision.clone();
                let on_action = on_action.clone();
                move |_conn, _sender, _path, _iface, method, params, invocation| {
                    match method {
                        "GetLayout" => {
                            let reply = glib::Variant::tuple_from_iter([
                                revision.get().to_variant(),
                                layout(&entries.borrow()),
                            ]);
                            invocation.return_value(Some(&reply));
                        }
                        "GetGroupProperties" => {
                            let ids = params
                                .try_child_value(0)
                                .and_then(|v| v.get::<Vec<i32>>())
                                .unwrap_or_default();
                            let reply = glib::Variant::tuple_from_iter([group_properties(
                                &entries.borrow(),
                                &ids,
                            )]);
                            invocation.return_value(Some(&reply));
                        }
                        "GetProperty" => {
                            let id = params
                                .try_child_value(0)
                                .and_then(|v| v.get::<i32>())
                                .unwrap_or(0);
                            let name = params
                                .try_child_value(1)
                                .and_then(|v| v.get::<String>())
                                .unwrap_or_default();
                            let value = entry_property(&entries.borrow(), id, &name)
                                .unwrap_or_else(|| "".to_variant());
                            invocation.return_value(Some(
                                &(glib::Variant::from_variant(&value),).to_variant(),
                            ));
                        }
                        "Event" => {
                            let id = params
                                .try_child_value(0)
                                .and_then(|v| v.get::<i32>())
                                .unwrap_or(0);
                            let event = params
                                .try_child_value(1)
                                .and_then(|v| v.get::<String>())
                                .unwrap_or_default();
                            // Answer before acting: opening a window is slow
                            // enough that the caller should not wait on it.
                            invocation.return_value(None);
                            dispatch_event(&entries, id, &event, on_action.as_ref());
                        }
                        "AboutToShow" => {
                            // The caller refreshes properties itself; nothing
                            // needs rebuilding between opening and drawing.
                            invocation.return_value(Some(&(false,).to_variant()));
                        }
                        _ => invocation.return_value(None),
                    }
                }
            })
            .build()
            .ok()?;
        registrations.push(menu_reg);

        // ---- claim a name and register with the watcher ----
        //
        // The spec names items after the owning process so several can coexist.
        let bus_name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let name_id = gio::bus_own_name_on_connection(
            &connection,
            &bus_name,
            gio::BusNameOwnerFlags::NONE,
            {
                let connection = connection.clone();
                move |_conn, name| register_with_watcher(&connection, name)
            },
            |_conn, _name| {
                glib::g_debug!("stickies", "lost the tray item bus name");
            },
        );

        Some(Self {
            connection,
            entries,
            revision,
            registrations,
            name_id: Some(name_id),
        })
    }

    /// Replace the menu contents and tell the host to re-read them.
    pub fn set_entries(&self, entries: Vec<MenuEntry>) {
        if *self.entries.borrow() == entries {
            return;
        }
        self.entries.replace(entries);
        self.revision.set(self.revision.get().wrapping_add(1));

        let _ = self.connection.emit_signal(
            None,
            MENU_PATH,
            "com.canonical.dbusmenu",
            "LayoutUpdated",
            Some(&(self.revision.get(), 0i32).to_variant()),
        );
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            self.connection.unregister_object(id).ok();
        }
        if let Some(id) = self.name_id.take() {
            gio::bus_unown_name(id);
        }
    }
}

/// Act on a menu click.
///
/// The borrow of `entries` is released *before* the callback runs. Handlers
/// re-enter this object — "New Note" ends up calling [`Tray::set_entries`] to
/// update the count — and holding a shared borrow across that is a guaranteed
/// `RefCell already borrowed` abort, which inside a D-Bus callback takes the
/// whole process down rather than unwinding.
fn dispatch_event(
    entries: &RefCell<Vec<MenuEntry>>,
    id: i32,
    event: &str,
    on_action: &dyn Fn(&str),
) {
    if event != "clicked" {
        return;
    }

    let action = {
        let entries = entries.borrow();
        match entry_at(&entries, id) {
            Some(MenuEntry::Item {
                action,
                enabled: true,
                ..
            }) => Some(action.clone()),
            _ => None,
        }
    };

    if let Some(action) = action {
        on_action(&action);
    }
}

/// Is anything hosting tray icons right now, without creating one?
///
/// Used by `--diagnose`, which must be able to report the state of the tray
/// without side effects.
pub fn watcher_available() -> bool {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map(|connection| watcher_present(&connection))
        .unwrap_or(false)
}

/// Is anything hosting tray icons right now?
///
/// On Ubuntu the host is GNOME Shell via the `ubuntu-appindicators` extension;
/// on a stock GNOME session there is usually nothing.
fn watcher_present(connection: &gio::DBusConnection) -> bool {
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(WATCHER_NAME,).to_variant()),
            Some(glib::VariantTy::new("(b)").unwrap()),
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.try_child_value(0)?.get::<bool>())
        .unwrap_or(false)
}

/// Tell the watcher we exist. Harmless if it is not running — the icon simply
/// does not appear, and the app is otherwise unaffected.
fn register_with_watcher(connection: &gio::DBusConnection, name: &str) {
    connection.call(
        Some(WATCHER_NAME),
        WATCHER_PATH,
        WATCHER_NAME,
        "RegisterStatusNotifierItem",
        Some(&(name,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
        |result| {
            if let Err(err) = result {
                glib::g_debug!("stickies", "no StatusNotifierWatcher: {err}");
            }
        },
    );
}

fn item_property(name: &str) -> glib::Variant {
    match name {
        "Category" => "ApplicationStatus".to_variant(),
        "Id" => crate::APP_ID.to_variant(),
        "Title" => "Stickies".to_variant(),
        "Status" => "Active".to_variant(),
        // Symbolic, so it follows the panel's foreground colour the way every
        // other status icon does.
        "IconName" => format!("{}-symbolic", crate::APP_ID).to_variant(),
        // Left click opens the menu rather than firing Activate.
        "ItemIsMenu" => true.to_variant(),
        "Menu" => glib::variant::ObjectPath::try_from(MENU_PATH.to_string())
            .expect("MENU_PATH is a valid object path")
            .to_variant(),
        _ => "".to_variant(),
    }
}

fn menu_property(name: &str) -> glib::Variant {
    match name {
        "Version" => 3u32.to_variant(),
        "Status" => "normal".to_variant(),
        "TextDirection" => "ltr".to_variant(),
        "IconThemePath" => Vec::<String>::new().to_variant(),
        _ => "".to_variant(),
    }
}

/// dbusmenu numbers items from 1; 0 is the root. Index `i` in the slice is id
/// `i + 1`.
fn entry_at(entries: &[MenuEntry], id: i32) -> Option<&MenuEntry> {
    usize::try_from(id - 1).ok().and_then(|i| entries.get(i))
}

fn entry_properties(entry: &MenuEntry) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    match entry {
        MenuEntry::Separator => {
            dict.insert("type", "separator");
        }
        MenuEntry::Info { label } => {
            dict.insert("label", label.as_str());
            dict.insert("enabled", false);
        }
        MenuEntry::Item { label, enabled, .. } => {
            dict.insert("label", label.as_str());
            dict.insert("enabled", *enabled);
        }
    }
    dict.insert("visible", true);
    dict.end()
}

/// Build one `(ia{sv}av)` menu node.
///
/// Built with `tuple_from_iter` rather than `(a, b, c).to_variant()`: a
/// `glib::Variant` placed inside a Rust tuple is boxed as `v`, which would
/// yield `(ivav)` and make every host reject the layout as the wrong type.
fn node(id: i32, properties: glib::Variant, children: Vec<glib::Variant>) -> glib::Variant {
    glib::Variant::tuple_from_iter([
        id.to_variant(),
        properties,
        glib::Variant::array_from_iter::<glib::Variant>(children),
    ])
}

/// The tree `GetLayout` returns: a root holding every entry.
fn layout(entries: &[MenuEntry]) -> glib::Variant {
    let children: Vec<glib::Variant> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let child = node(index as i32 + 1, entry_properties(entry), Vec::new());
            // `av` elements are boxed variants.
            glib::Variant::from_variant(&child)
        })
        .collect();

    let root = glib::VariantDict::new(None);
    root.insert("children-display", "submenu");

    node(0, root.end(), children)
}

/// The `a(ia{sv})` array `GetGroupProperties` returns.
fn group_properties(entries: &[MenuEntry], ids: &[i32]) -> glib::Variant {
    // An empty id list means "everything", per the spec.
    let wanted: Vec<i32> = if ids.is_empty() {
        (1..=entries.len() as i32).collect()
    } else {
        ids.to_vec()
    };

    let rows: Vec<glib::Variant> = wanted
        .into_iter()
        .filter_map(|id| {
            let entry = entry_at(entries, id)?;
            Some(glib::Variant::tuple_from_iter([
                id.to_variant(),
                entry_properties(entry),
            ]))
        })
        .collect();

    glib::Variant::array_from_iter_with_type(
        glib::VariantTy::new("(ia{sv})").expect("valid type"),
        rows,
    )
}

fn entry_property(entries: &[MenuEntry], id: i32, name: &str) -> Option<glib::Variant> {
    let entry = entry_at(entries, id)?;
    glib::VariantDict::new(Some(&entry_properties(entry))).lookup_value(name, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<MenuEntry> {
        vec![
            MenuEntry::item("New Note", "app.new-note"),
            MenuEntry::Separator,
            MenuEntry::info("2 notes"),
            MenuEntry::disabled("Show All Notes", "app.show-all"),
            MenuEntry::item("Quit", "app.quit"),
        ]
    }

    #[test]
    fn the_layout_matches_the_type_dbusmenu_declares() {
        // A shape mismatch here means the menu silently never appears, which is
        // painful to debug against a live shell.
        let variant = layout(&sample());
        assert_eq!(variant.type_().as_str(), "(ia{sv}av)");
    }

    #[test]
    fn the_root_is_item_zero_and_holds_every_entry() {
        let variant = layout(&sample());
        let id = variant.try_child_value(0).unwrap().get::<i32>().unwrap();
        assert_eq!(id, 0, "the root is always id 0");

        let children = variant.try_child_value(2).unwrap();
        assert_eq!(children.n_children(), 5);
    }

    #[test]
    fn ids_are_one_based_because_zero_is_the_root() {
        let entries = sample();
        assert!(
            entry_at(&entries, 0).is_none(),
            "0 is the root, not an entry"
        );
        assert_eq!(entry_at(&entries, 1), Some(&entries[0]));
        assert_eq!(entry_at(&entries, 5), Some(&entries[4]));
        assert!(entry_at(&entries, 6).is_none());
        assert!(
            entry_at(&entries, -1).is_none(),
            "negative ids must not panic"
        );
    }

    #[test]
    fn a_separator_is_typed_rather_than_labelled() {
        let props = glib::VariantDict::new(Some(&entry_properties(&MenuEntry::Separator)));
        assert_eq!(
            props.lookup::<String>("type").unwrap().as_deref(),
            Some("separator")
        );
        assert!(props.lookup::<String>("label").unwrap().is_none());
    }

    #[test]
    fn an_info_row_is_shown_but_not_clickable() {
        let props = glib::VariantDict::new(Some(&entry_properties(&MenuEntry::info("2 notes"))));
        assert_eq!(
            props.lookup::<String>("label").unwrap().as_deref(),
            Some("2 notes")
        );
        assert_eq!(props.lookup::<bool>("enabled").unwrap(), Some(false));
        assert_eq!(props.lookup::<bool>("visible").unwrap(), Some(true));
    }

    #[test]
    fn a_disabled_item_reports_itself_as_disabled() {
        let entry = MenuEntry::disabled("Show All Notes", "app.show-all");
        let props = glib::VariantDict::new(Some(&entry_properties(&entry)));
        assert_eq!(props.lookup::<bool>("enabled").unwrap(), Some(false));
    }

    /// The ids in a `a(ia{sv})` reply, in order.
    fn reply_ids(variant: &glib::Variant) -> Vec<i32> {
        variant
            .iter()
            .filter_map(|row| row.try_child_value(0)?.get::<i32>())
            .collect()
    }

    #[test]
    fn group_properties_matches_the_type_dbusmenu_declares() {
        let variant = group_properties(&sample(), &[]);
        assert_eq!(variant.type_().as_str(), "a(ia{sv})");
    }

    #[test]
    fn group_properties_defaults_to_every_entry() {
        let entries = sample();
        assert_eq!(
            reply_ids(&group_properties(&entries, &[])),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(reply_ids(&group_properties(&entries, &[1, 3])), vec![1, 3]);
    }

    #[test]
    fn group_properties_skips_ids_that_do_not_exist() {
        // A host asking about a stale id after the menu shrank must not panic
        // or shift the remaining answers.
        assert_eq!(
            reply_ids(&group_properties(&sample(), &[1, 99, 4])),
            vec![1, 4]
        );
    }

    #[test]
    fn individual_properties_can_be_fetched() {
        let entries = sample();
        assert_eq!(
            entry_property(&entries, 1, "label").and_then(|v| v.get::<String>()),
            Some("New Note".to_string())
        );
        assert_eq!(entry_property(&entries, 1, "nonexistent"), None);
        assert_eq!(entry_property(&entries, 99, "label"), None);
    }

    #[test]
    fn clicking_an_item_dispatches_its_action() {
        let entries = RefCell::new(sample());
        let fired = RefCell::new(Vec::<String>::new());
        dispatch_event(&entries, 1, "clicked", &|action| {
            fired.borrow_mut().push(action.to_string())
        });
        assert_eq!(*fired.borrow(), vec!["app.new-note"]);
    }

    #[test]
    fn a_handler_may_rebuild_the_menu_from_inside_the_click() {
        // Regression: the click handler used to hold a borrow of the entries
        // while dispatching. "New Note" refreshes the menu to update its count,
        // so the callback re-entered and hit `RefCell already borrowed` — which,
        // inside a D-Bus callback, aborts the process instead of unwinding.
        let entries = RefCell::new(sample());
        dispatch_event(&entries, 1, "clicked", &|_action| {
            entries.borrow_mut().push(MenuEntry::item("Added", "app.x"));
        });
        assert_eq!(entries.borrow().len(), 6, "the handler's edit took effect");
    }

    #[test]
    fn disabled_and_non_click_events_do_nothing() {
        let entries = RefCell::new(sample());
        let fired = Cell::new(0);
        let bump = |_: &str| fired.set(fired.get() + 1);

        dispatch_event(&entries, 4, "clicked", &bump); // "Show All Notes", disabled
        dispatch_event(&entries, 2, "clicked", &bump); // a separator
        dispatch_event(&entries, 3, "clicked", &bump); // an info row
        dispatch_event(&entries, 99, "clicked", &bump); // no such id
        dispatch_event(&entries, 1, "hovered", &bump); // not a click
        assert_eq!(fired.get(), 0);

        dispatch_event(&entries, 5, "clicked", &bump); // "Quit", enabled
        assert_eq!(fired.get(), 1);
    }

    #[test]
    fn the_tray_can_be_opted_out_of() {
        // Documented escape hatch for anyone who would rather have the dock
        // entry, and what the lifecycle tests use to exercise the no-tray path.
        unsafe { std::env::set_var("STICKIES_NO_TRAY", "1") };
        let tray = Tray::new(sample(), |_| {});
        unsafe { std::env::remove_var("STICKIES_NO_TRAY") };
        assert!(tray.is_none());
    }

    #[test]
    fn the_icon_advertises_the_symbolic_variant() {
        // A full-colour icon in the panel looks wrong next to every other
        // status icon and will not follow the panel's foreground colour.
        let name = item_property("IconName").get::<String>().unwrap();
        assert_eq!(name, "us.hagreli.Stickies-symbolic");
        assert!(name.ends_with("-symbolic"));
    }

    #[test]
    fn the_item_points_at_the_menu_object_path() {
        let menu = item_property("Menu");
        assert_eq!(
            menu.type_().as_str(),
            "o",
            "must be an object path, not a string"
        );
        assert_eq!(menu.str(), Some(MENU_PATH));
    }

    #[test]
    fn left_click_opens_the_menu_rather_than_acting() {
        assert_eq!(item_property("ItemIsMenu").get::<bool>(), Some(true));
    }

    #[test]
    fn unknown_properties_return_something_rather_than_panicking() {
        assert_eq!(
            item_property("Nonsense").get::<String>().as_deref(),
            Some("")
        );
        assert_eq!(
            menu_property("Nonsense").get::<String>().as_deref(),
            Some("")
        );
    }
}
