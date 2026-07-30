//! The application: owns the store, the open windows, and the sync loop.
//!
//! Everything that mutates a note funnels through here. Windows report user
//! intent by signal; this object applies it to the [`Store`], marks the store
//! dirty, and lets the periodic tick write it out. Nothing else calls `save`,
//! so there is exactly one place where a note can be lost.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gio;
use gtk::gio::prelude::ApplicationCommandLineExt;
use gtk::glib::{self, clone};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Duration;

use crate::model::geometry;
use crate::model::note::{Note, Palette};
use crate::model::store::{LoadOutcome, Store};
use crate::model::Monitor;
use crate::placement::Shell;
use crate::tray::{MenuEntry, Tray};
use crate::ui::NoteWindow;

/// How often geometry is read back from the compositor and a dirty store is
/// written out. Long enough to be free, short enough that a hard crash loses
/// at most a couple of seconds of typing.
const TICK: Duration = Duration::from_secs(2);

/// A freshly presented window is not known to the compositor until it maps, so
/// the first placement attempt usually misses. Retry briefly rather than
/// guessing a single delay that is wrong on slow and fast machines alike.
const PLACE_ATTEMPTS: u32 = 12;
const PLACE_RETRY: Duration = Duration::from_millis(70);

mod imp {
    use super::*;

    pub struct StickiesApplication {
        pub store: RefCell<Store>,
        pub shell: RefCell<Shell>,
        /// Open windows by note ID. Hidden windows stay here — closing a note
        /// puts it away without tearing the window down.
        pub windows: RefCell<HashMap<String, NoteWindow>>,
        pub monitors: RefCell<Vec<Monitor>>,
        /// Present only when a StatusNotifierWatcher accepted the icon. Its
        /// presence is what keeps the app alive with no notes on screen.
        pub tray: RefCell<Option<Tray>>,
        pub dirty: Cell<bool>,
        /// Whether the shell extension is answering. Drives the pin button.
        pub placement_available: Cell<bool>,
        /// Last save failure, if saving is currently broken. Drives the banner.
        pub save_error: RefCell<Option<String>>,
        pub tick: RefCell<Option<glib::SourceId>>,
        /// Kept alive for the process lifetime; dropping it unsubscribes.
        pub monitors_subscription: RefCell<Option<gio::SignalSubscription>>,
        /// Suppresses the auto-quit-on-last-close rule while quitting, so the
        /// shutdown path does not re-enter itself.
        pub quitting: Cell<bool>,
    }

    impl Default for StickiesApplication {
        fn default() -> Self {
            Self {
                store: RefCell::new(Store::new(Store::default_path())),
                shell: RefCell::new(Shell::disconnected()),
                windows: RefCell::new(HashMap::new()),
                monitors: RefCell::new(Vec::new()),
                tray: RefCell::new(None),
                dirty: Cell::new(false),
                placement_available: Cell::new(false),
                save_error: RefCell::new(None),
                tick: RefCell::new(None),
                monitors_subscription: RefCell::new(None),
                quitting: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StickiesApplication {
        const NAME: &'static str = "StickiesApplication";
        type Type = super::StickiesApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for StickiesApplication {}

    impl ApplicationImpl for StickiesApplication {
        fn startup(&self) {
            // Chain up first: the toolkit initialises in the parent handler and
            // anything touching GTK before that is undefined.
            self.parent_startup();

            let obj = self.obj();
            if let Some(display) = gtk::gdk::Display::default() {
                crate::ui::load_stylesheet(&display);
            }
            self.shell.replace(Shell::session());
            obj.install_actions();
            obj.load_store();
            obj.start_tick();
            obj.watch_monitors();
            obj.setup_tray();
            obj.check_placement_available();
        }

        fn activate(&self) {
            self.parent_activate();
            self.obj().restore_session();
        }

        /// Entry point for the desktop-file actions ("New Note", "Show All
        /// Notes" on the dock's right-click menu) and for a second launch of an
        /// already-running instance.
        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            let options = command_line.options_dict();
            let obj = self.obj();
            let mut handled = false;

            if options.contains("diagnose") {
                // Printed through the command line object so the text reaches
                // the terminal that asked, even when a primary instance
                // elsewhere is what actually answers.
                let report = obj.diagnose();
                command_line.print_literal(&report);
                return glib::ExitCode::SUCCESS;
            }
            if options.contains("new-note") {
                obj.activate_action("new-note", None);
                handled = true;
            }
            if options.contains("show-all") {
                obj.activate_action("show-all", None);
                handled = true;
            }
            if !handled {
                obj.activate();
            }
            glib::ExitCode::SUCCESS
        }

        fn shutdown(&self) {
            self.quitting.set(true);
            let obj = self.obj();
            // Last chance to capture where the user left everything. The
            // compositor is still up at this point, so the query succeeds.
            obj.sync_geometry_blocking();
            obj.save_now();

            if let Some(id) = self.tick.borrow_mut().take() {
                id.remove();
            }
            self.parent_shutdown();
        }
    }

    impl GtkApplicationImpl for StickiesApplication {}
    impl AdwApplicationImpl for StickiesApplication {}
}

glib::wrapper! {
    pub struct StickiesApplication(ObjectSubclass<imp::StickiesApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for StickiesApplication {
    fn default() -> Self {
        Self::new()
    }
}

impl StickiesApplication {
    pub fn new() -> Self {
        Self::with_application_id(crate::APP_ID)
    }

    /// Construct under a different application ID.
    ///
    /// For tests. A `GApplication` that shares an ID with a running instance
    /// becomes a *remote* for it: `run` forwards the command line and returns
    /// immediately, without ever emitting `startup` or `activate`. Tests that
    /// used the real ID would therefore silently drive the user's live app
    /// instead of themselves.
    pub fn with_application_id(application_id: &str) -> Self {
        let app: Self = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", gio::ApplicationFlags::HANDLES_COMMAND_LINE)
            .build();

        app.add_main_option(
            "new-note",
            glib::Char::from(b'n'),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            "Create a new note",
            None,
        );
        app.add_main_option(
            "diagnose",
            glib::Char::from(b'd'),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            "Report the state of both halves and exit",
            None,
        );
        app.add_main_option(
            "show-all",
            glib::Char::from(b'a'),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            "Show every saved note",
            None,
        );
        app
    }

    // ---- session ---------------------------------------------------------

    fn load_store(&self) {
        let (store, outcome) = Store::open(Store::default_path());
        if let LoadOutcome::Recovered { backup, reason } = &outcome {
            glib::g_warning!(
                "stickies",
                "could not read notes ({reason}); previous file kept at {}",
                backup.display()
            );
        }
        self.imp().store.replace(store);
    }

    /// Open the notes that should be on screen.
    ///
    /// Normally that is exactly the notes that were visible when the app last
    /// exited. If none were, open the **most recently edited one** — launching
    /// and getting no window at all is indistinguishable from the app being
    /// broken, but opening *everything* is worse: it buries the screen in notes
    /// you had deliberately put away. "Show All Notes" is one click away in the
    /// menu when you actually want it. An empty store gets one fresh note.
    fn restore_session(&self) {
        let ids: Vec<String> = {
            let store = self.imp().store.borrow();
            let visible: Vec<String> = store
                .notes()
                .iter()
                .filter(|n| n.visible)
                .map(|n| n.id.clone())
                .collect();
            if visible.is_empty() {
                store
                    .notes()
                    .iter()
                    .max_by_key(|n| n.modified)
                    .map(|n| vec![n.id.clone()])
                    .unwrap_or_default()
            } else {
                visible
            }
        };

        if ids.is_empty() {
            self.new_note();
            return;
        }

        for id in ids {
            self.mark_visible(&id, true);
            self.open_note(&id);
        }
    }

    // ---- note operations -------------------------------------------------

    /// Create, store and open a new note. It inherits the colour of the
    /// most recently used note so a themed set stays themed.
    fn new_note(&self) {
        let palette = self.next_palette();
        let note = Note::new(palette);
        let id = note.id.clone();

        self.imp().store.borrow_mut().upsert(note);
        self.mark_dirty();
        if let Some(window) = self.open_note(&id) {
            window.focus_text();
        }
    }

    fn duplicate_note(&self, source_id: &str) {
        let copy = {
            let store = self.imp().store.borrow();
            let Some(source) = store.get(source_id) else {
                return;
            };
            let mut copy = Note::new(source.palette);
            copy.body.clone_from(&source.body);
            // Deliberately not copied: position (it would land exactly on top
            // of the original) and pinned state.
            copy.geometry.monitor.clone_from(&source.geometry.monitor);
            copy
        };

        let id = copy.id.clone();
        self.imp().store.borrow_mut().upsert(copy);
        self.mark_dirty();
        if let Some(window) = self.open_note(&id) {
            window.focus_text();
        }
    }

    fn delete_note(&self, id: &str) {
        if let Some(window) = self.imp().windows.borrow_mut().remove(id) {
            window.set_visible(false);
            window.destroy();
        }
        // Logged at warning level: this is the only irreversible action in the
        // app, and without a record there is no way to answer "where did my
        // note go?" after the fact.
        if let Some(removed) = self.imp().store.borrow_mut().remove(id) {
            glib::g_warning!(
                "stickies",
                "deleted note {id} ({:?}, {} chars)",
                removed.title(),
                removed.body.chars().count()
            );
        }
        self.mark_dirty();
        self.save_now();
        self.refresh_tray();
        self.quit_if_nothing_visible();
    }

    /// Show a note, creating its window on first use. Returns `None` only if
    /// the note is not in the store.
    fn open_note(&self, id: &str) -> Option<NoteWindow> {
        let existing = self.imp().windows.borrow().get(id).cloned();
        if let Some(window) = existing {
            window.present();
            return Some(window);
        }

        let note = self.imp().store.borrow().get(id)?.clone();
        let window = NoteWindow::new(self);
        window.bind(&note);
        window.set_placement_available(self.imp().placement_available.get());
        window.set_save_error(self.imp().save_error.borrow().as_deref());
        self.connect_window(&window);
        self.imp()
            .windows
            .borrow_mut()
            .insert(id.to_string(), window.clone());

        window.present();
        self.place_when_mapped(&window, note.geometry.clone(), note.pinned);
        self.refresh_tray();
        Some(window)
    }

    fn connect_window(&self, window: &NoteWindow) {
        window.connect_closure(
            "body-changed",
            false,
            glib::closure_local!(
                #[watch(rename_to = app)]
                self,
                move |window: NoteWindow| {
                    let body = window.body();
                    let mut store = app.imp().store.borrow_mut();
                    if let Some(note) = store.get_mut(&window.note_id()) {
                        if note.body != body {
                            note.body = body;
                            note.touch();
                            drop(store);
                            app.mark_dirty();
                        }
                    }
                }
            ),
        );

        window.connect_closure(
            "palette-changed",
            false,
            glib::closure_local!(
                #[watch(rename_to = app)]
                self,
                move |window: NoteWindow, id: &str| {
                    let Some(palette) = Palette::from_id(id) else {
                        return;
                    };
                    let mut store = app.imp().store.borrow_mut();
                    if let Some(note) = store.get_mut(&window.note_id()) {
                        note.palette = palette;
                        note.touch();
                    }
                    drop(store);
                    app.mark_dirty();
                }
            ),
        );

        window.connect_closure(
            "pin-changed",
            false,
            glib::closure_local!(
                #[watch(rename_to = app)]
                self,
                move |window: NoteWindow, pinned: bool| {
                    let mut store = app.imp().store.borrow_mut();
                    if let Some(note) = store.get_mut(&window.note_id()) {
                        note.pinned = pinned;
                    }
                    drop(store);
                    app.mark_dirty();

                    let Some(path) = window.object_path() else {
                        return;
                    };
                    let shell = app.imp().shell.borrow().clone();
                    glib::spawn_future_local(async move {
                        shell.set_pinned(&path, pinned).await;
                    });
                }
            ),
        );

        window.connect_closure(
            "delete-requested",
            false,
            glib::closure_local!(
                #[watch(rename_to = app)]
                self,
                move |window: NoteWindow| app.delete_note(&window.note_id())
            ),
        );

        window.connect_closure(
            "hide-requested",
            false,
            glib::closure_local!(
                #[watch(rename_to = app)]
                self,
                move |window: NoteWindow| {
                    // Capture the final position before the window goes away;
                    // once it is unmapped the compositor forgets it.
                    let id = window.note_id();
                    app.sync_one_blocking(&window);
                    app.mark_visible(&id, false);
                    app.save_now();
                    app.refresh_tray();
                    app.quit_if_nothing_visible();
                }
            ),
        );
    }

    fn mark_visible(&self, id: &str, visible: bool) {
        let mut store = self.imp().store.borrow_mut();
        if let Some(note) = store.get_mut(id) {
            if note.visible != visible {
                note.visible = visible;
                drop(store);
                self.mark_dirty();
            }
        }
    }

    fn show_all(&self) {
        let ids: Vec<String> = self
            .imp()
            .store
            .borrow()
            .notes()
            .iter()
            .map(|n| n.id.clone())
            .collect();
        for id in ids {
            self.mark_visible(&id, true);
            self.open_note(&id);
        }
        self.refresh_tray();
    }

    /// Put every note away without deleting anything. Only reachable when a
    /// tray icon is present, since otherwise it would leave no way back in.
    fn hide_all(&self) {
        let windows: Vec<NoteWindow> = self
            .imp()
            .windows
            .borrow()
            .values()
            .filter(|w| w.is_visible())
            .cloned()
            .collect();
        for window in windows {
            // close() runs the same path as the titlebar button, so geometry is
            // captured and the note is marked away exactly as it would be.
            window.close();
        }
        self.refresh_tray();
    }

    /// Nothing on screen means nothing to interact with, and the state is
    /// already on disk — so exit rather than lingering as an invisible process.
    ///
    /// Deferred to an idle callback because the caller is usually a window's
    /// `close_request` handler, which runs *before* GTK hides the window: an
    /// immediate check would still count the note being closed as visible and
    /// the app would linger forever with no way to reach it.
    ///
    /// With a tray icon there *is* still a way to reach it, so the app stays
    /// running — that is the whole point of the icon. Quitting then only
    /// happens through the menu or Ctrl+Q.
    fn quit_if_nothing_visible(&self) {
        if self.imp().quitting.get() || self.imp().tray.borrow().is_some() {
            return;
        }
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                if app.imp().quitting.get() {
                    return;
                }
                let any_visible = app.imp().windows.borrow().values().any(|w| w.is_visible());
                if !any_visible {
                    app.quit();
                }
            }
        ));
    }

    /// The colour a new note should take. The rule (and its tests) live in the
    /// display-free model.
    fn next_palette(&self) -> Palette {
        crate::model::note::least_used_palette(self.imp().store.borrow().notes())
    }

    // ---- placement -------------------------------------------------------

    /// Ask the shell extension to put a window where its note says it belongs.
    ///
    /// Runs on the main loop after presenting, retrying while the compositor
    /// catches up with the new toplevel. Silent no-op without the extension.
    fn place_when_mapped(
        &self,
        window: &NoteWindow,
        geometry: crate::model::note::NoteGeometry,
        pinned: bool,
    ) {
        let Some(path) = window.object_path() else {
            return;
        };
        let shell = self.imp().shell.borrow().clone();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let monitors = app.refresh_monitors(&shell).await;
                if monitors.is_empty() {
                    return; // No extension: let Mutter place it.
                }

                let placement = match geometry::resolve(&geometry, &monitors) {
                    Some(placement) => placement,
                    None => return,
                };

                for attempt in 0..PLACE_ATTEMPTS {
                    if shell.place(&path, &placement).await {
                        if pinned {
                            shell.set_pinned(&path, true).await;
                        }
                        return;
                    }
                    if attempt + 1 < PLACE_ATTEMPTS {
                        glib::timeout_future(PLACE_RETRY).await;
                    }
                }
                glib::g_warning!(
                    "stickies",
                    "gave up positioning {path} after {PLACE_ATTEMPTS} attempts; \
                     the note is where the compositor put it"
                );
            }
        ));
    }

    async fn refresh_monitors(&self, shell: &Shell) -> Vec<Monitor> {
        let monitors = shell.list_monitors().await;
        if !monitors.is_empty() {
            self.imp().monitors.replace(monitors.clone());
        }
        monitors
    }

    /// Read every open window's geometry back from the compositor and record
    /// it. This is the only way the app learns that the user dragged a note.
    ///
    /// One `QueryAll` round trip covers every window, so the cost of the tick
    /// does not grow with the number of notes.
    fn sync_geometry(&self) {
        let shell = self.imp().shell.borrow().clone();
        let by_path: HashMap<String, String> = self
            .imp()
            .windows
            .borrow()
            .iter()
            .filter(|(_, w)| w.is_visible())
            .filter_map(|(id, w)| Some((w.object_path()?, id.clone())))
            .collect();

        if by_path.is_empty() {
            return;
        }

        glib::spawn_future_local(clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                for (path, placement) in shell.query_all().await {
                    if let Some(id) = by_path.get(&path) {
                        app.record_geometry(id, placement);
                    }
                }
            }
        ));
    }

    /// A display was plugged, unplugged or rearranged. Re-resolve every open
    /// note against the new layout so notes on a monitor that just disappeared
    /// come back somewhere visible instead of being stranded off-screen.
    fn handle_monitors_changed(&self) {
        let windows: Vec<NoteWindow> = self
            .imp()
            .windows
            .borrow()
            .values()
            .filter(|w| w.is_visible())
            .cloned()
            .collect();

        for window in windows {
            let geometry = {
                let store = self.imp().store.borrow();
                match store.get(&window.note_id()) {
                    Some(note) => note.geometry.clone(),
                    None => continue,
                }
            };
            let pinned = window.is_pinned();
            self.place_when_mapped(&window, geometry, pinned);
        }
    }

    /// Synchronous variant for shutdown, where there is no later tick to pick
    /// the result up. One round trip, so quitting with many notes open is not
    /// N blocking D-Bus calls.
    fn sync_geometry_blocking(&self) {
        let shell = self.imp().shell.borrow().clone();
        let by_path: HashMap<String, String> = self
            .imp()
            .windows
            .borrow()
            .iter()
            .filter(|(_, w)| w.is_visible())
            .filter_map(|(id, w)| Some((w.object_path()?, id.clone())))
            .collect();

        if by_path.is_empty() {
            return;
        }

        let results = glib::MainContext::default().block_on(shell.query_all());
        for (path, placement) in results {
            if let Some(id) = by_path.get(&path) {
                self.record_geometry(id, placement);
            }
        }
    }

    fn sync_one_blocking(&self, window: &NoteWindow) {
        let Some(path) = window.object_path() else {
            return;
        };
        let shell = self.imp().shell.borrow().clone();
        let id = window.note_id();
        let placement = glib::MainContext::default().block_on(shell.query(&path));
        if let Some(placement) = placement {
            self.record_geometry(&id, placement);
        }
    }

    fn record_geometry(&self, id: &str, placement: crate::model::Placement) {
        let mut store = self.imp().store.borrow_mut();
        let Some(note) = store.get_mut(id) else {
            return;
        };
        let changed = note.geometry.monitor.as_deref() != Some(placement.connector.as_str())
            || note.geometry.x != placement.x
            || note.geometry.y != placement.y
            || note.geometry.width != placement.width
            || note.geometry.height != placement.height;
        if !changed {
            return;
        }
        note.geometry.monitor = Some(placement.connector);
        note.geometry.x = placement.x;
        note.geometry.y = placement.y;
        note.geometry.width = placement.width;
        note.geometry.height = placement.height;
        drop(store);
        self.mark_dirty();
    }

    // ---- persistence -----------------------------------------------------

    fn mark_dirty(&self) {
        self.imp().dirty.set(true);
    }

    fn save_now(&self) {
        if !self.imp().dirty.get() {
            return;
        }
        match self.imp().store.borrow().save() {
            Ok(()) => {
                self.imp().dirty.set(false);
                self.report_save_error(None);
            }
            Err(err) => {
                // Logged once per distinct failure, not once per tick: the tick
                // retries every two seconds and would otherwise fill the
                // journal with the same line forever.
                let message = err.to_string();
                if self.imp().save_error.borrow().as_deref() != Some(message.as_str()) {
                    glib::g_warning!("stickies", "could not save notes: {message}");
                }
                self.report_save_error(Some(message));
            }
        }
    }

    /// One timer drives both jobs: pull geometry from the compositor, then
    /// flush the store if anything changed. Coalescing keystrokes into a
    /// periodic write means typing never blocks on I/O.
    fn start_tick(&self) {
        let id = glib::timeout_add_local(
            TICK,
            clone!(
                #[weak(rename_to = app)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    app.sync_geometry();
                    app.save_now();
                    glib::ControlFlow::Continue
                }
            ),
        );
        self.imp().tick.replace(Some(id));
    }

    /// Subscribe to the extension's monitor-layout signal. The subscription is
    /// stored on the application because dropping it would unsubscribe.
    fn watch_monitors(&self) {
        let shell = self.imp().shell.borrow().clone();
        let subscription = shell.connect_monitors_changed(clone!(
            #[weak(rename_to = app)]
            self,
            move || app.handle_monitors_changed()
        ));
        self.imp().monitors_subscription.replace(subscription);
    }

    // ---- diagnostics -----------------------------------------------------

    /// Build the `--diagnose` report, querying the shell extension for the
    /// parts only it knows.
    fn diagnose(&self) -> String {
        let shell = self.imp().shell.borrow().clone();
        let (version, monitors) = glib::MainContext::default().block_on(async {
            let version = shell.version().await;
            let monitors = shell
                .list_monitors()
                .await
                .into_iter()
                .map(|m| {
                    (
                        m.connector,
                        format!("{}x{}+{}+{}", m.width, m.height, m.x, m.y),
                    )
                })
                .collect();
            (version, monitors)
        });

        // Probing the tray host must not create an icon, so this is a separate
        // cheap check rather than reusing the live one.
        let tray_host = crate::tray::watcher_available();

        crate::diagnostics::Report::gather(version, monitors, tray_host).render()
    }

    // ---- tray ------------------------------------------------------------

    /// Create the status-bar icon. Silently does nothing where there is no
    /// StatusNotifierWatcher (a plain GNOME session without the appindicator
    /// extension, say), in which case the app keeps its old quit-on-last-close
    /// behaviour so it cannot get stranded with no way back in.
    fn setup_tray(&self) {
        let tray = Tray::new(
            self.tray_entries(),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |action: &str| {
                    // Entries carry detailed names ("app.new-note"); the action
                    // group wants the bare name.
                    let name = action.strip_prefix("app.").unwrap_or(action);
                    app.activate_action(name, None);
                }
            ),
        );

        if tray.is_none() {
            glib::g_debug!("stickies", "no tray icon; quitting with the last note");
        }
        self.imp().tray.replace(tray);
    }

    /// Rebuild the menu so counts and enabled states track reality.
    fn refresh_tray(&self) {
        let entries = self.tray_entries();
        if let Some(tray) = self.imp().tray.borrow().as_ref() {
            tray.set_entries(entries);
        }
    }

    fn tray_entries(&self) -> Vec<MenuEntry> {
        let total = self.imp().store.borrow().len();
        let on_screen = self
            .imp()
            .windows
            .borrow()
            .values()
            .filter(|w| w.is_visible())
            .count();

        let count_label = match total {
            0 => "No notes".to_string(),
            1 => "1 note".to_string(),
            _ => format!("{total} notes, {on_screen} on screen"),
        };

        // Rows that cannot do anything are shown disabled rather than hidden:
        // a menu that changes shape is harder to use than one that greys out.
        let show_all = if on_screen < total {
            MenuEntry::item("Show All Notes", "app.show-all")
        } else {
            MenuEntry::disabled("Show All Notes", "app.show-all")
        };
        let hide_all = if on_screen > 0 {
            MenuEntry::item("Hide All Notes", "app.hide-all")
        } else {
            MenuEntry::disabled("Hide All Notes", "app.hide-all")
        };

        vec![
            MenuEntry::item("New Note", "app.new-note"),
            MenuEntry::Separator,
            MenuEntry::info(&count_label),
            show_all,
            hide_all,
            MenuEntry::Separator,
            MenuEntry::item("Keyboard Shortcuts", "app.shortcuts"),
            MenuEntry::item("About Stickies", "app.about"),
            MenuEntry::item("Quit", "app.quit"),
        ]
    }

    /// Tell every window whether notes are reaching disk.
    ///
    /// Silent data loss is the one failure worth interrupting for: without
    /// this, a permissions problem meant the app warned into a journal nobody
    /// reads while the user kept typing, and they found out on next launch.
    fn report_save_error(&self, message: Option<String>) {
        if *self.imp().save_error.borrow() == message {
            return;
        }
        self.imp().save_error.replace(message.clone());
        for window in self.imp().windows.borrow().values() {
            window.set_save_error(message.as_deref());
        }
    }

    // ---- capability reporting --------------------------------------------

    /// Tell every note window whether "keep on top" can actually do anything.
    ///
    /// Without the shell extension it cannot — Wayland offers no client-side
    /// always-on-top — and a pin button that visibly latches while changing
    /// nothing is worse than one that is plainly unavailable.
    fn check_placement_available(&self) {
        let shell = self.imp().shell.borrow().clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let available = shell.is_available().await;
                app.imp().placement_available.set(available);
                for window in app.imp().windows.borrow().values() {
                    window.set_placement_available(available);
                }
            }
        ));
    }

    // ---- actions ---------------------------------------------------------

    fn install_actions(&self) {
        let entries = [
            gio::ActionEntry::builder("new-note")
                .activate(|app: &Self, _, _| app.new_note())
                .build(),
            gio::ActionEntry::builder("show-all")
                .activate(|app: &Self, _, _| app.show_all())
                .build(),
            gio::ActionEntry::builder("hide-all")
                .activate(|app: &Self, _, _| app.hide_all())
                .build(),
            gio::ActionEntry::builder("duplicate-note")
                .parameter_type(Some(glib::VariantTy::STRING))
                .activate(|app: &Self, _, parameter| {
                    if let Some(id) = parameter.and_then(|p| p.get::<String>()) {
                        app.duplicate_note(&id);
                    }
                })
                .build(),
            gio::ActionEntry::builder("about")
                .activate(|app: &Self, _, _| app.show_about())
                .build(),
            gio::ActionEntry::builder("shortcuts")
                .activate(|app: &Self, _, _| app.show_shortcuts())
                .build(),
            gio::ActionEntry::builder("quit")
                .activate(|app: &Self, _, _| app.quit())
                .build(),
        ];
        self.add_action_entries(entries);

        self.set_accels_for_action("app.new-note", &["<Control>n"]);
        self.set_accels_for_action("app.show-all", &["<Control><Shift>a"]);
        self.set_accels_for_action("app.quit", &["<Control>q"]);
        self.set_accels_for_action("app.shortcuts", &["<Control>question"]);
        self.set_accels_for_action("note.hide", &["<Control>w"]);
        self.set_accels_for_action("note.delete", &["<Control>Delete"]);
        self.set_accels_for_action("note.toggle-pin", &["<Control>t"]);
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Stickies")
            .application_icon(crate::APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("Matthew Hagrelius")
            .license_type(gtk::License::Gpl30)
            .comments(
                "Sticky notes that stay where you put them.\n\n\
                 Position restore needs the companion GNOME Shell extension: \
                 Wayland gives applications no way to place their own windows.",
            )
            .build();

        let shell = self.imp().shell.borrow().clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let status = if shell.is_available().await {
                    "Shell extension: connected"
                } else {
                    "Shell extension: not detected — notes will not be repositioned"
                };
                about.set_debug_info(status);
                about.present(app.active_window().as_ref());
            }
        ));
    }

    fn show_shortcuts(&self) {
        let dialog = adw::ShortcutsDialog::new();

        let general = adw::ShortcutsSection::new(Some("General"));
        for (accel, title) in [
            ("<Control>n", "New note"),
            ("<Control>w", "Put note away (× deletes)"),
            ("<Control><Shift>a", "Show all notes"),
            ("<Control>q", "Quit"),
        ] {
            general.add(adw::ShortcutsItem::new(title, accel));
        }
        dialog.add(general);

        let note = adw::ShortcutsSection::new(Some("Note"));
        for (accel, title) in [
            ("<Control>t", "Keep on top"),
            ("<Control>Delete", "Delete note"),
        ] {
            note.add(adw::ShortcutsItem::new(title, accel));
        }
        dialog.add(note);

        dialog.present(self.active_window().as_ref());
    }
}
