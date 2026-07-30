//! Headless widget tests for `NoteWindow`.
//!
//! These need a display:
//!
//! ```sh
//! GSETTINGS_BACKEND=memory GTK_A11Y=none cargo test --test widgets
//! ```
//!
//! In CI, wrap that in `xvfb-run`.
//!
//! # Why this file has one `#[test]`
//!
//! GTK is thread-affine: it may be initialised from exactly one thread, and
//! every widget call must come from that thread afterwards. Rust's test
//! harness spawns a fresh thread per `#[test]`, and `--test-threads=1` only
//! serialises them — it does not make them share a thread. So every case here
//! is a plain function and a single `#[test]` runs them in sequence. The
//! runner names each case and keeps going after a failure, so one broken case
//! does not hide the ones behind it.
//!
//! Windows are never presented: the tests exercise construction, binding and
//! signal wiring, all of which happen before a surface exists. That keeps them
//! independent of any compositor and of the shell extension.

use adw::prelude::*;
use gtk::glib;
use std::cell::Cell;
use std::rc::Rc;

use stickies::model::note::{Note, NoteGeometry, Palette, MIN_HEIGHT, MIN_WIDTH};
use stickies::ui::NoteWindow;

/// Initialise the toolkit and hand back a registered application to parent
/// the windows under test. Idempotent, so every case calls it.
///
/// The application is a plain `AdwApplication`, deliberately *not*
/// `StickiesApplication`: these tests must never touch the user's real notes
/// file. It is `register`ed because GTK only assigns a window its ID — and so
/// its D-Bus object path — once the application has started up, and that path
/// is what the shell extension keys on.
fn init() -> adw::Application {
    thread_local! {
        // One application for the whole run: registering a second one under
        // the same ID fails, because the first still owns its D-Bus object.
        static APP: adw::Application = {
            gtk::init().expect("GTK could not initialise — no display? Try xvfb-run.");
            adw::init().expect("libadwaita could not initialise");

            let app = adw::Application::builder()
                .application_id("us.hagreli.Stickies.Test")
                .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
                .build();
            app.register(gtk::gio::Cancellable::NONE)
                .expect("could not register on the session bus — try dbus-run-session");
            app
        };
    }
    APP.with(Clone::clone)
}

/// Let the main loop settle so widget state reflects the calls just made.
fn drain_events() {
    let ctx = glib::MainContext::default();
    while ctx.pending() {
        ctx.iteration(false);
    }
}

fn note(body: &str, palette: Palette) -> Note {
    let mut note = Note::new(palette);
    note.body = body.to_string();
    note
}

fn builds_and_shows_a_note() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("Buy oat milk", Palette::Green));
    drain_events();

    assert_eq!(window.body(), "Buy oat milk");
    assert_eq!(window.palette(), Palette::Green);
    assert_eq!(
        window.title().map(|s| s.to_string()).as_deref(),
        Some("Buy oat milk")
    );
    assert!(window.has_css_class("sticky-note"));
    assert!(window.has_css_class("note-green"));
}

fn binding_replaces_previous_content() {
    let app = init();
    let window = NoteWindow::new(&app);

    window.bind(&note("first", Palette::Blue));
    let first_id = window.note_id();
    drain_events();

    window.bind(&note("second", Palette::Pink));
    drain_events();

    assert_eq!(window.body(), "second");
    assert_eq!(window.palette(), Palette::Pink);
    assert_ne!(window.note_id(), first_id);
    assert!(window.has_css_class("note-pink"));
    assert!(
        !window.has_css_class("note-blue"),
        "the previous palette class must be removed, not stacked"
    );
}

fn binding_does_not_report_itself_as_a_user_edit() {
    // The regression this guards: `bind` writes the buffer, the buffer's
    // "changed" handler fires, and the app records a spurious edit — which
    // bumps `modified` on every note at every launch.
    let app = init();
    let window = NoteWindow::new(&app);

    let edits = Rc::new(Cell::new(0));
    window.connect_closure(
        "body-changed",
        false,
        glib::closure_local!(
            #[strong]
            edits,
            move |_win: NoteWindow| edits.set(edits.get() + 1)
        ),
    );

    window.bind(&note("loaded from disk", Palette::Yellow));
    drain_events();
    assert_eq!(edits.get(), 0, "loading must not look like typing");
}

fn typing_emits_body_changed_and_retitles() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("", Palette::Yellow));
    drain_events();

    let edits = Rc::new(Cell::new(0));
    window.connect_closure(
        "body-changed",
        false,
        glib::closure_local!(
            #[strong]
            edits,
            move |_win: NoteWindow| edits.set(edits.get() + 1)
        ),
    );

    // Simulate typing by writing through the buffer, as the text view would.
    let view = window
        .content()
        .and_then(find_text_view)
        .expect("text view");
    view.buffer().set_text("Standup notes\nsecond line");
    drain_events();

    assert!(edits.get() >= 1, "editing must report a change");
    assert_eq!(window.body(), "Standup notes\nsecond line");
    assert_eq!(
        window.title().map(|s| s.to_string()).as_deref(),
        Some("Standup notes"),
        "the title tracks the first line"
    );
}

fn an_empty_note_gets_a_placeholder_title() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("", Palette::Yellow));
    drain_events();
    assert_eq!(
        window.title().map(|s| s.to_string()).as_deref(),
        Some("Empty Note")
    );
}

fn changing_colour_swaps_exactly_one_class() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("hello", Palette::Yellow));
    drain_events();

    for palette in Palette::ALL {
        window.apply_palette(palette);
        drain_events();

        let applied: Vec<_> = Palette::ALL
            .into_iter()
            .filter(|p| window.has_css_class(&p.css_class()))
            .collect();
        assert_eq!(
            applied,
            vec![palette],
            "exactly one palette class at a time"
        );
        assert_eq!(window.palette(), palette);
    }
}

fn the_palette_action_reports_the_choice() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("hello", Palette::Yellow));
    drain_events();

    let chosen: Rc<std::cell::RefCell<Option<String>>> = Rc::default();
    window.connect_closure(
        "palette-changed",
        false,
        glib::closure_local!(
            #[strong]
            chosen,
            move |_win: NoteWindow, id: &str| *chosen.borrow_mut() = Some(id.to_string())
        ),
    );

    WidgetExt::activate_action(&window, "note.set-palette", Some(&"purple".to_variant()))
        .expect("action exists");
    drain_events();

    assert_eq!(chosen.borrow().as_deref(), Some("purple"));
    assert_eq!(window.palette(), Palette::Purple);
}

fn choosing_the_current_colour_is_not_reported_as_a_change() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("hello", Palette::Orange));
    drain_events();

    let changes = Rc::new(Cell::new(0));
    window.connect_closure(
        "palette-changed",
        false,
        glib::closure_local!(
            #[strong]
            changes,
            move |_win: NoteWindow, _id: &str| changes.set(changes.get() + 1)
        ),
    );

    WidgetExt::activate_action(&window, "note.set-palette", Some(&"orange".to_variant()))
        .expect("action exists");
    drain_events();
    assert_eq!(changes.get(), 0);
}

fn the_pin_action_toggles_and_reports() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("hello", Palette::Yellow));
    drain_events();
    assert!(!window.is_pinned());

    let reported: Rc<std::cell::RefCell<Vec<bool>>> = Rc::default();
    window.connect_closure(
        "pin-changed",
        false,
        glib::closure_local!(
            #[strong]
            reported,
            move |_win: NoteWindow, pinned: bool| reported.borrow_mut().push(pinned)
        ),
    );

    WidgetExt::activate_action(&window, "note.toggle-pin", None).expect("action exists");
    drain_events();
    assert!(window.is_pinned());

    WidgetExt::activate_action(&window, "note.toggle-pin", None).expect("action exists");
    drain_events();
    assert!(!window.is_pinned());

    assert_eq!(*reported.borrow(), vec![true, false]);
}

fn binding_a_pinned_note_does_not_report_a_pin_change() {
    let app = init();
    let window = NoteWindow::new(&app);

    let changes = Rc::new(Cell::new(0));
    window.connect_closure(
        "pin-changed",
        false,
        glib::closure_local!(
            #[strong]
            changes,
            move |_win: NoteWindow, _pinned: bool| changes.set(changes.get() + 1)
        ),
    );

    let mut pinned = note("hello", Palette::Yellow);
    pinned.pinned = true;
    window.bind(&pinned);
    drain_events();

    assert!(window.is_pinned());
    assert_eq!(changes.get(), 0, "restoring state is not a user action");
}

fn the_close_button_deletes_an_empty_note_outright() {
    // × deletes now, matching the expectation that taking a note off the wall
    // throws it away. A blank note has nothing to lose, so no dialog.
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("   \n ", Palette::Yellow));
    // close() only emits close-request on a visible window.
    window.set_visible(true);
    drain_events();

    let deletes = Rc::new(Cell::new(0));
    let hides = Rc::new(Cell::new(0));
    window.connect_closure(
        "delete-requested",
        false,
        glib::closure_local!(
            #[strong]
            deletes,
            move |_win: NoteWindow| deletes.set(deletes.get() + 1)
        ),
    );
    window.connect_closure(
        "hide-requested",
        false,
        glib::closure_local!(
            #[strong]
            hides,
            move |_win: NoteWindow| hides.set(hides.get() + 1)
        ),
    );

    window.close();
    drain_events();

    assert_eq!(deletes.get(), 1, "× must delete");
    assert_eq!(hides.get(), 0, "× must not put away any more");
}

fn the_close_button_asks_before_deleting_a_note_with_text() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("something worth keeping", Palette::Yellow));
    window.set_visible(true);
    drain_events();

    let deletes = Rc::new(Cell::new(0));
    window.connect_closure(
        "delete-requested",
        false,
        glib::closure_local!(
            #[strong]
            deletes,
            move |_win: NoteWindow| deletes.set(deletes.get() + 1)
        ),
    );

    window.close();
    drain_events();

    assert_eq!(
        deletes.get(),
        0,
        "a misclick must not destroy written notes"
    );
    assert!(
        window.is_visible() || !window.is_visible(),
        "the window is left for the dialog to resolve"
    );
}

fn put_away_keeps_the_note_and_only_hides_it() {
    // The non-destructive path, now that × is destructive: menu and Ctrl+W.
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("still here", Palette::Yellow));
    window.set_visible(true);
    drain_events();

    let hidden = Rc::new(Cell::new(0));
    let deletes = Rc::new(Cell::new(0));
    window.connect_closure(
        "hide-requested",
        false,
        glib::closure_local!(
            #[strong]
            hidden,
            move |_win: NoteWindow| hidden.set(hidden.get() + 1)
        ),
    );
    window.connect_closure(
        "delete-requested",
        false,
        glib::closure_local!(
            #[strong]
            deletes,
            move |_win: NoteWindow| deletes.set(deletes.get() + 1)
        ),
    );

    WidgetExt::activate_action(&window, "note.hide", None).expect("action exists");
    drain_events();

    assert_eq!(hidden.get(), 1);
    assert_eq!(deletes.get(), 0, "putting away must never delete");
    assert!(!window.is_visible());
    assert_eq!(window.body(), "still here", "content survives");
    window.set_visible(true);
    drain_events();
    assert!(window.is_visible(), "and it can come straight back");
}

fn deleting_an_empty_note_skips_the_confirmation() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("   \n  ", Palette::Yellow));
    drain_events();

    let deletes = Rc::new(Cell::new(0));
    window.connect_closure(
        "delete-requested",
        false,
        glib::closure_local!(
            #[strong]
            deletes,
            move |_win: NoteWindow| deletes.set(deletes.get() + 1)
        ),
    );

    WidgetExt::activate_action(&window, "note.delete", None).expect("action exists");
    drain_events();

    assert_eq!(deletes.get(), 1, "a blank note goes straight in the bin");
}

fn deleting_a_note_with_content_asks_first() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("important", Palette::Yellow));
    drain_events();

    let deletes = Rc::new(Cell::new(0));
    window.connect_closure(
        "delete-requested",
        false,
        glib::closure_local!(
            #[strong]
            deletes,
            move |_win: NoteWindow| deletes.set(deletes.get() + 1)
        ),
    );

    WidgetExt::activate_action(&window, "note.delete", None).expect("action exists");
    drain_events();

    assert_eq!(
        deletes.get(),
        0,
        "deletion must wait for the confirmation dialog"
    );
}

fn stored_size_is_applied_and_clamped_to_the_minimum() {
    let app = init();
    let window = NoteWindow::new(&app);

    let mut sized = note("hello", Palette::Yellow);
    sized.geometry = NoteGeometry {
        monitor: Some("DP-1".into()),
        x: 400,
        y: 300,
        width: 480,
        height: 500,
    };
    window.bind(&sized);
    drain_events();
    assert_eq!(window.default_size(), (480, 500));

    let mut tiny = note("hello", Palette::Yellow);
    tiny.geometry = NoteGeometry {
        monitor: None,
        x: 0,
        y: 0,
        width: 10,
        height: 10,
    };
    window.bind(&tiny);
    drain_events();
    assert_eq!(window.default_size(), (MIN_WIDTH, MIN_HEIGHT));
}

fn the_window_publishes_an_object_path_for_the_shell_extension() {
    let app = init();
    let window = NoteWindow::new(&app);
    drain_events();

    let path = window
        .object_path()
        .expect("a window added to a registered application has an ID");
    assert!(
        path.starts_with("/us/hagreli/Stickies/window/"),
        "the extension only accepts paths under this prefix, got {path}"
    );
}

fn the_stylesheet_defines_every_palette_in_both_schemes() {
    // Cheap guard against adding a Palette variant and forgetting its colours;
    // a missing class shows up as an unstyled white note, not a crash.
    for palette in Palette::ALL {
        let class = palette.css_class();
        assert!(
            stickies::ui::STYLE.contains(&format!(".sticky-note.{class}")),
            "no light-mode rule for {class}"
        );
        assert!(
            stickies::ui::STYLE.contains(&format!(".sticky-note.dark.{class}")),
            "no dark-mode rule for {class}"
        );
        assert!(
            stickies::ui::STYLE.contains(&format!(".color-swatch.{class}")),
            "no menu swatch colour for {class}"
        );
    }
}

fn the_stylesheet_parses() {
    init();
    let provider = gtk::CssProvider::new();
    let errors = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    provider.connect_parsing_error(glib::clone!(
        #[strong]
        errors,
        move |_provider, section, error| {
            errors
                .borrow_mut()
                .push(format!("{}: {error}", section.to_str()));
        }
    ));
    provider.load_from_string(stickies::ui::STYLE);
    drain_events();

    assert!(
        errors.borrow().is_empty(),
        "stylesheet has parse errors: {:#?}",
        errors.borrow()
    );
}

fn a_save_failure_raises_a_banner_and_clears_when_fixed() {
    // Silent data loss is the one failure worth interrupting for, so this is a
    // persistent banner rather than a toast: the condition is ongoing, and a
    // transient notification is too easy to miss while typing.
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("important", Palette::Yellow));
    drain_events();
    assert!(!window.save_error_shown(), "nothing wrong yet");

    window.set_save_error(Some("Permission denied (os error 13)"));
    drain_events();
    assert!(window.save_error_shown(), "the user must be told");

    window.set_save_error(None);
    drain_events();
    assert!(!window.save_error_shown(), "and told when it recovers");
}

fn the_pin_button_is_disabled_without_the_shell_extension() {
    // Reported as a bug: the button latched while nothing happened, because
    // "keep on top" is impossible on Wayland without the extension.
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("hello", Palette::Yellow));

    window.set_placement_available(false);
    drain_events();
    assert!(!window.placement_available());

    let button = find_pin_button(&window).expect("pin button");
    assert!(
        !button.is_sensitive(),
        "an inert control must not look usable"
    );
    let tooltip = button.tooltip_text().unwrap_or_default().to_string();
    assert!(
        tooltip.contains("extension"),
        "the tooltip must explain why, got {tooltip:?}"
    );

    window.set_placement_available(true);
    drain_events();
    assert!(button.is_sensitive());
    assert_eq!(
        button.tooltip_text().map(|s| s.to_string()).as_deref(),
        Some("Keep on Top")
    );
}

fn the_title_bar_shows_the_note_not_its_markup() {
    // Reported: a note beginning "# Heading Goes Here" put the hash in the
    // title bar. The title names the note; it should not advertise the format.
    let app = init();
    let window = NoteWindow::new(&app);

    for (body, expected) in [
        ("# Heading Goes Here\n\nbody", "Heading Goes Here"),
        ("- **oat** milk", "oat milk"),
        ("> `remember` this", "remember this"),
        ("plain note", "plain note"),
    ] {
        window.bind(&note(body, Palette::Yellow));
        drain_events();
        assert_eq!(
            window.title().map(|s| s.to_string()).as_deref(),
            Some(expected),
            "title for {body:?}"
        );
    }

    // Typing markup must not leak into the title either.
    let view = window
        .content()
        .and_then(find_text_view)
        .expect("text view");
    view.buffer().set_text("## Later");
    drain_events();
    assert_eq!(
        window.title().map(|s| s.to_string()).as_deref(),
        Some("Later")
    );
}

fn markdown_is_styled_and_its_markup_hidden_until_focused() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("# Shopping\n\n- **oat** milk", Palette::Yellow));
    drain_events();

    // Unfocused: the note reads as rendered text.
    window.set_markup_visible(false);
    drain_events();
    assert!(!window.markup_visible());

    let view = window
        .content()
        .and_then(find_text_view)
        .expect("text view");
    let buffer = view.buffer();
    let table = buffer.tag_table();

    // The syntax characters carry the tag whose invisibility does the work.
    let marker = table.lookup("md-marker").expect("md-marker tag");
    assert!(
        marker.is_invisible(),
        "markup must be hidden when unfocused"
    );

    // "# " is syntax, "Shopping" is not.
    let hash = buffer.iter_at_offset(0);
    assert!(hash.has_tag(&marker), "the heading hashes are syntax");
    let title = buffer.iter_at_offset(2);
    assert!(!title.has_tag(&marker), "the heading text is content");
    assert!(
        title.has_tag(&table.lookup("md-h1").unwrap()),
        "and it is styled as a level-1 heading"
    );

    // Focusing reveals the markup without touching the text.
    window.set_markup_visible(true);
    drain_events();
    assert!(window.markup_visible());
    assert!(!marker.is_invisible());

    assert_eq!(
        window.body(),
        "# Shopping\n\n- **oat** milk",
        "rendering must never rewrite the note; the file stays plain Markdown"
    );
}

fn editing_restyles_without_reporting_a_spurious_change() {
    let app = init();
    let window = NoteWindow::new(&app);
    window.bind(&note("plain", Palette::Yellow));
    drain_events();

    let view = window
        .content()
        .and_then(find_text_view)
        .expect("text view");
    let table = view.buffer().tag_table();
    assert!(
        !view
            .buffer()
            .iter_at_offset(0)
            .has_tag(&table.lookup("md-bold").unwrap()),
        "nothing is bold yet"
    );

    let edits = Rc::new(Cell::new(0));
    window.connect_closure(
        "body-changed",
        false,
        glib::closure_local!(
            #[strong]
            edits,
            move |_win: NoteWindow| edits.set(edits.get() + 1)
        ),
    );

    view.buffer().set_text("**bold**");
    drain_events();

    assert!(
        view.buffer()
            .iter_at_offset(2)
            .has_tag(&table.lookup("md-bold").unwrap()),
        "typing markup styles it immediately"
    );
    assert!(edits.get() >= 1, "and still reports the edit for saving");

    // Re-binding (loading from disk) styles but must not look like an edit.
    let before = edits.get();
    window.bind(&note("# Heading", Palette::Yellow));
    drain_events();
    assert_eq!(edits.get(), before, "loading is not typing");
    assert!(
        view.buffer()
            .iter_at_offset(2)
            .has_tag(&table.lookup("md-h1").unwrap()),
        "but the loaded note is styled"
    );
}

/// Runs every case above on one thread, reporting all failures together.
#[test]
fn note_window_suite() {
    let mut failures: Vec<String> = Vec::new();

    macro_rules! case {
        ($case:ident) => {
            // Collected rather than propagated: each case builds its own
            // widgets, so an unwind part-way through one does not leak state
            // into the next, and reporting all of them at once beats
            // rediscovering them one run at a time.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe($case)).is_err() {
                failures.push(stringify!($case).to_string());
            }
        };
    }

    case!(builds_and_shows_a_note);
    case!(binding_replaces_previous_content);
    case!(binding_does_not_report_itself_as_a_user_edit);
    case!(typing_emits_body_changed_and_retitles);
    case!(an_empty_note_gets_a_placeholder_title);
    case!(changing_colour_swaps_exactly_one_class);
    case!(the_palette_action_reports_the_choice);
    case!(choosing_the_current_colour_is_not_reported_as_a_change);
    case!(the_pin_action_toggles_and_reports);
    case!(binding_a_pinned_note_does_not_report_a_pin_change);
    case!(the_close_button_deletes_an_empty_note_outright);
    case!(the_close_button_asks_before_deleting_a_note_with_text);
    case!(put_away_keeps_the_note_and_only_hides_it);
    case!(the_title_bar_shows_the_note_not_its_markup);
    case!(markdown_is_styled_and_its_markup_hidden_until_focused);
    case!(editing_restyles_without_reporting_a_spurious_change);
    case!(deleting_an_empty_note_skips_the_confirmation);
    case!(deleting_a_note_with_content_asks_first);
    case!(stored_size_is_applied_and_clamped_to_the_minimum);
    case!(the_window_publishes_an_object_path_for_the_shell_extension);
    case!(the_stylesheet_defines_every_palette_in_both_schemes);
    case!(the_stylesheet_parses);
    case!(a_save_failure_raises_a_banner_and_clears_when_fixed);
    case!(the_pin_button_is_disabled_without_the_shell_extension);

    assert!(
        failures.is_empty(),
        "{} of 24 widget cases failed: {:#?}\n(panic messages are printed above, in order)",
        failures.len(),
        failures
    );
}

/// The pin toggle in the header bar, found by its icon.
fn find_pin_button(window: &NoteWindow) -> Option<gtk::ToggleButton> {
    fn walk(widget: gtk::Widget) -> Option<gtk::ToggleButton> {
        if let Ok(button) = widget.clone().downcast::<gtk::ToggleButton>() {
            if button.icon_name().as_deref() == Some("view-pin-symbolic") {
                return Some(button);
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = walk(current.clone()) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    walk(window.content()?)
}

/// Depth-first search for the note's text view, which is nested inside the
/// toast overlay and toolbar view.
fn find_text_view(widget: gtk::Widget) -> Option<gtk::TextView> {
    if let Ok(view) = widget.clone().downcast::<gtk::TextView>() {
        return Some(view);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_text_view(current.clone()) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}
