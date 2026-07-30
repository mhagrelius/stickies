//! End-to-end lifecycle tests that drive the real `StickiesApplication`.
//!
//! Unlike `tests/widgets.rs`, this builds the actual application object, lets
//! it load and save through the real store, and runs its main loop. It needs a
//! display and a session bus:
//!
//! ```sh
//! GSETTINGS_BACKEND=memory GTK_A11Y=none cargo test --test lifecycle
//! # or, headless:  xvfb-run -a dbus-run-session -- cargo test --test lifecycle
//! ```
//!
//! `XDG_DATA_HOME` is redirected to a temporary directory before the
//! application starts, so these never touch real notes.
//!
//! # Two rules these tests have to respect
//!
//! **Never assert inside a signal handler.** Handlers are called across an
//! `extern "C"` boundary, where a Rust panic cannot unwind — it aborts the
//! whole process instead of failing the test. Handlers here only record state
//! into a `Cell`; every assertion happens after `run` returns.
//!
//! **`connect_activate` runs before the app's own `activate`.** The class
//! closure is `RUN_LAST`, so a handler connected from outside sees the state
//! from *before* windows were restored. Work that needs the restored session
//! is scheduled onto an idle callback, which runs after the emission finishes.
//!
//! As in the widget tests, everything runs from one `#[test]`: GTK is
//! thread-affine and the harness gives each `#[test]` its own thread.

use adw::prelude::*;
use gtk::glib;
use gtk::prelude::WidgetExt;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use stickies::model::note::Note;
use stickies::model::store::Store;
use stickies::ui::StickiesApplication;

/// Hard ceiling on any main-loop run. Without it, "the app never quits" — the
/// exact regression these tests exist for — would hang CI instead of failing.
const RUN_TIMEOUT: Duration = Duration::from_secs(10);

fn drain_events() {
    let ctx = glib::MainContext::default();
    while ctx.pending() {
        ctx.iteration(false);
    }
}

/// Read the notes file the application under test is writing to.
fn load_notes(data_home: &std::path::Path) -> Vec<Note> {
    Store::open(data_home.join("stickies").join("notes.json"))
        .0
        .notes()
        .to_vec()
}

/// Run an application's main loop with a timeout, returning whether it hit it.
///
/// A `run` that never returns is a real failure mode here, so it is bounded
/// rather than left to hang the test binary.
fn run_bounded(app: &StickiesApplication) -> bool {
    run_bounded_for(app, RUN_TIMEOUT)
}

fn run_bounded_for(app: &StickiesApplication, timeout: Duration) -> bool {
    let timed_out = Rc::new(Cell::new(false));
    let guard = glib::timeout_add_local_once(
        timeout,
        glib::clone!(
            #[weak]
            app,
            #[strong]
            timed_out,
            move || {
                timed_out.set(true);
                app.quit();
            }
        ),
    );

    app.run_with_args::<&str>(&[]);
    if !timed_out.get() {
        guard.remove();
    }
    drain_events();
    timed_out.get()
}

#[test]
fn lifecycle_suite() {
    let temp = tempfile::tempdir().expect("temp dir");

    // Must be set before the application registers: `startup` reads the store
    // path from the environment. Safe here because this binary runs one test.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", temp.path());
    }

    // Most cases exercise the no-tray path, where closing the last note exits.
    // The tray case turns this off for itself.
    unsafe {
        std::env::set_var("STICKIES_NO_TRAY", "1");
    }

    gtk::init().expect("GTK could not initialise — no display? Try xvfb-run.");
    adw::init().expect("libadwaita could not initialise");

    let mut failures: Vec<String> = Vec::new();
    macro_rules! case {
        ($case:ident) => {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $case(temp.path())))
                .is_err()
            {
                failures.push(stringify!($case).to_string());
            }
        };
    }

    case!(a_fresh_store_opens_one_note);
    case!(closing_the_last_note_quits_the_app);
    case!(a_relaunch_brings_back_notes_that_were_put_away);
    case!(a_tray_icon_keeps_the_app_alive_with_no_notes_on_screen);
    case!(quitting_does_not_mark_open_notes_as_put_away);
    case!(launching_with_everything_put_away_opens_one_note_not_all_of_them);

    assert!(
        failures.is_empty(),
        "{} of 6 lifecycle cases failed: {:#?}",
        failures.len(),
        failures
    );
}

/// Launching with nothing saved gives you something to type in, and that note
/// is persisted immediately rather than only on exit.
fn a_fresh_store_opens_one_note(data_home: &std::path::Path) {
    // A distinct application ID per case. Sharing the real one would make this
    // process a *remote* for the user's running Stickies: `run` would forward
    // its command line, return at once, and never emit startup or activate —
    // so the test would silently drive the live app and assert on nothing.
    let app = StickiesApplication::with_application_id("us.hagreli.Stickies.TestFresh");
    let window_count = Rc::new(Cell::new(usize::MAX));

    app.connect_activate(glib::clone!(
        #[strong]
        window_count,
        move |app| {
            // Deferred: the app's own activate handler has not run yet.
            glib::idle_add_local_once(glib::clone!(
                #[weak]
                app,
                #[strong]
                window_count,
                move || {
                    window_count.set(app.windows().len());
                    app.quit();
                }
            ));
        }
    ));

    assert!(!run_bounded(&app), "the app hung on first launch");
    assert_eq!(
        window_count.get(),
        1,
        "a fresh store opens exactly one note"
    );

    let notes = load_notes(data_home);
    assert_eq!(notes.len(), 1, "the new note reached disk");
    assert!(notes[0].visible);
    assert_eq!(notes[0].body, "", "a new note starts empty");
}

/// The regression this guards: `quit_if_nothing_visible` used to run inside
/// `close_request`, before GTK had hidden the window, so it always counted the
/// note being closed as still visible. Closing the last note left the app
/// running with nothing on screen and no way to reach it.
fn closing_the_last_note_quits_the_app(data_home: &std::path::Path) {
    let before = load_notes(data_home);
    assert_eq!(before.len(), 1, "carried over from the previous case");
    let id = before[0].id.clone();

    let app = StickiesApplication::with_application_id("us.hagreli.Stickies.TestClose");

    app.connect_activate(|app| {
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            app,
            move || {
                // Putting the only note away. × would delete it, which is a
                // different path; this is the "nothing left on screen" case.
                if let Some(window) = app.windows().first() {
                    let _ = WidgetExt::activate_action(window, "note.hide", None);
                }
            }
        ));
    });

    assert!(
        !run_bounded(&app),
        "the app did not quit after its last note was closed"
    );

    let notes = load_notes(data_home);
    assert_eq!(
        notes.len(),
        1,
        "closing puts a note away, it does not delete it"
    );
    assert_eq!(notes[0].id, id);
    assert!(
        !notes[0].visible,
        "a closed note is recorded as put away, so it does not reopen"
    );
}

/// Relaunching restores what was on screen. With every note put away, it brings
/// them all back rather than opening to nothing — an app that launches to no
/// window is indistinguishable from one that is broken.
fn a_relaunch_brings_back_notes_that_were_put_away(data_home: &std::path::Path) {
    let before = load_notes(data_home);
    assert_eq!(before.len(), 1);
    assert!(!before[0].visible, "carried over from the previous case");
    let id = before[0].id.clone();

    let app = StickiesApplication::with_application_id("us.hagreli.Stickies.TestRelaunch");
    let window_count = Rc::new(Cell::new(usize::MAX));

    app.connect_activate(glib::clone!(
        #[strong]
        window_count,
        move |app| {
            glib::idle_add_local_once(glib::clone!(
                #[weak]
                app,
                #[strong]
                window_count,
                move || {
                    window_count.set(app.windows().len());
                    app.quit();
                }
            ));
        }
    ));

    assert!(!run_bounded(&app), "the app hung on relaunch");
    assert_eq!(
        window_count.get(),
        1,
        "with nothing marked visible, every note is brought back"
    );

    let after = load_notes(data_home);
    assert_eq!(after.len(), 1, "no note was created or lost");
    assert_eq!(after[0].id, id, "the same note came back");
    assert!(after[0].visible, "reopening marks it visible again");
}

/// With a tray icon there is still a way to reach the app, so closing the last
/// note must *not* exit — otherwise the icon and the global shortcut vanish the
/// moment you tidy your desktop.
///
/// Skipped where nothing hosts tray icons: `Tray::new` refuses to create an
/// invisible one, precisely so the app cannot strand itself, and then the
/// quit-on-last-close behaviour above is correct instead.
fn a_tray_icon_keeps_the_app_alive_with_no_notes_on_screen(_data_home: &std::path::Path) {
    unsafe {
        std::env::remove_var("STICKIES_NO_TRAY");
    }
    let has_host = stickies::tray::Tray::new(Vec::new(), |_| {}).is_some();
    if !has_host {
        eprintln!("  (skipped: no StatusNotifierWatcher on this session)");
        unsafe {
            std::env::set_var("STICKIES_NO_TRAY", "1");
        }
        return;
    }

    let app = StickiesApplication::with_application_id("us.hagreli.Stickies.TestTray");
    let closed = Rc::new(Cell::new(false));

    app.connect_activate(glib::clone!(
        #[strong]
        closed,
        move |app| {
            glib::idle_add_local_once(glib::clone!(
                #[weak]
                app,
                #[strong]
                closed,
                move || {
                    for window in app.windows() {
                        let _ = WidgetExt::activate_action(&window, "note.hide", None);
                    }
                    closed.set(true);
                }
            ));
        }
    ));

    // Short timeout: here, hitting it is the *pass* condition — the app is
    // supposed to still be running when it fires.
    let stayed_alive = run_bounded_for(&app, Duration::from_secs(3));

    unsafe {
        std::env::set_var("STICKIES_NO_TRAY", "1");
    }

    assert!(closed.get(), "the test never got as far as closing a note");
    assert!(
        stayed_alive,
        "the app exited after its last note was closed, losing the tray icon"
    );
}

/// Quitting must preserve which notes were on screen, and their positions.
///
/// This locks in an invariant rather than guarding a known regression. It was
/// written while chasing a report of a note reappearing at another note's
/// coordinates; quitting turned out *not* to be the cause — `app.quit()` stops
/// the main loop without emitting `close_request`, so the "put away" path never
/// runs — but the invariant is worth holding, because the day it stops being
/// true, a launch would restore nothing and the cause would be invisible.
fn quitting_does_not_mark_open_notes_as_put_away(data_home: &std::path::Path) {
    // Start from a known state: one visible note with a recorded position.
    {
        let path = data_home.join("stickies").join("notes.json");
        let (mut store, _) = Store::open(&path);
        for note in store.notes().to_vec() {
            let mut note = note;
            note.visible = true;
            note.geometry.monitor = Some("DP-9".into());
            note.geometry.x = 1234;
            note.geometry.y = 567;
            store.upsert(note);
        }
        store.save().expect("seed");
    }
    let seeded = load_notes(data_home);
    assert!(!seeded.is_empty(), "need at least one note to test with");

    let app = StickiesApplication::with_application_id("us.hagreli.Stickies.TestQuit");
    app.connect_activate(|app| {
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            app,
            // Quit the way the menu item and Ctrl+Q do.
            move || app.activate_action("quit", None)
        ));
    });

    assert!(!run_bounded(&app), "the app hung on quit");

    for note in load_notes(data_home) {
        assert!(
            note.visible,
            "note {} was marked away merely because the app quit",
            note.id
        );
        assert_eq!(
            (note.geometry.x, note.geometry.y),
            (1234, 567),
            "quitting overwrote the stored position of note {}",
            note.id
        );
        assert_eq!(note.geometry.monitor.as_deref(), Some("DP-9"));
    }
}

/// Launching with every note put away must open *one* note, not all of them.
///
/// The original rule opened everything, on the reasoning that a launch with no
/// window looks broken. It does — but burying the screen in notes the user had
/// deliberately closed is worse, and "Show All Notes" is one click away when
/// they actually want it.
fn launching_with_everything_put_away_opens_one_note_not_all_of_them(data_home: &std::path::Path) {
    let path = data_home.join("stickies").join("notes.json");
    let newest_id;
    {
        // Start clean: earlier cases in this suite share the same store.
        let mut store = Store::new(&path);
        // Four notes, none visible, with distinct modification times.
        for (index, palette) in [
            stickies::model::note::Palette::Yellow,
            stickies::model::note::Palette::Green,
            stickies::model::note::Palette::Blue,
            stickies::model::note::Palette::Pink,
        ]
        .into_iter()
        .enumerate()
        {
            let mut note = stickies::model::note::Note::new(palette);
            note.visible = false;
            note.modified = 1_000 + index as i64;
            store.upsert(note);
        }
        newest_id = store
            .notes()
            .iter()
            .max_by_key(|n| n.modified)
            .unwrap()
            .id
            .clone();
        store.save().expect("seed");
    }

    let app = StickiesApplication::with_application_id("us.hagreli.Stickies.TestRestoreOne");
    let opened = Rc::new(Cell::new(usize::MAX));
    app.connect_activate(glib::clone!(
        #[strong]
        opened,
        move |app| {
            glib::idle_add_local_once(glib::clone!(
                #[weak]
                app,
                #[strong]
                opened,
                move || {
                    opened.set(app.windows().len());
                    app.quit();
                }
            ));
        }
    ));

    assert!(!run_bounded(&app), "the app hung");
    assert_eq!(
        opened.get(),
        1,
        "exactly one note should open, not the whole store"
    );

    let after = load_notes(data_home);
    assert_eq!(after.len(), 4, "nothing was created or deleted");
    let visible: Vec<&stickies::model::note::Note> = after.iter().filter(|n| n.visible).collect();
    assert_eq!(visible.len(), 1, "only one note was marked visible");
    assert_eq!(
        visible[0].id, newest_id,
        "and it is the most recently edited one"
    );
}
