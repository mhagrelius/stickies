//! One window per sticky note.
//!
//! The window is deliberately thin: it renders a [`Note`] and reports what the
//! user did to it, but it never owns the data and never touches the disk. The
//! store is canonical and [`crate::ui::application::StickiesApplication`] is the
//! only thing that mutates it. That split is what keeps two windows showing the
//! same note (after "Duplicate", say) from drifting apart, and it keeps this
//! file testable — a `NoteWindow` can be built, bound, and inspected without a
//! store or a session bus.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::subclass::Signal;
use gtk::glib::{self, clone, prelude::ToVariant};
use gtk::{gdk, gio, pango};
use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use crate::model::markdown::{self, Style};
use crate::model::note::{Note, Palette, MIN_HEIGHT, MIN_WIDTH};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct NoteWindow {
        /// The note this window is showing. Empty until `bind` is called.
        pub note_id: RefCell<String>,
        pub palette: Cell<Palette>,
        pub pinned: Cell<bool>,
        pub text_view: RefCell<Option<gtk::TextView>>,
        pub title_label: RefCell<Option<gtk::Label>>,
        pub pin_button: RefCell<Option<gtk::ToggleButton>>,
        pub toasts: RefCell<Option<adw::ToastOverlay>>,
        /// Shown when notes are not reaching disk.
        pub save_banner: RefCell<Option<adw::Banner>>,
        /// The tag applied to Markdown syntax characters. Its `invisible`
        /// property is what makes the note look rendered when unfocused.
        pub marker_tag: RefCell<Option<gtk::TextTag>>,
        pub swatches: RefCell<Vec<gtk::ToggleButton>>,
        /// Set while `bind` is writing widget state, so the handlers it trips
        /// do not report those writes back as user edits.
        pub loading: Cell<bool>,
        /// A renumbering pass is already waiting to run. Deleting a selection
        /// can raise several delete signals; one pass settles them all.
        pub renumber_queued: Cell<bool>,
        /// Whether the shell extension is reachable. "Keep on top" cannot work
        /// without it, so the button reflects that rather than pretending.
        pub placement_available: Cell<bool>,
        /// Handler on AdwStyleManager, which outlives this window and so must
        /// be disconnected in `dispose`.
        pub style_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NoteWindow {
        const NAME: &'static str = "StickiesNoteWindow";
        type Type = super::NoteWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for NoteWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // The body text changed. Emitted on every keystroke; the
                    // application debounces before writing to disk.
                    Signal::builder("body-changed").build(),
                    // The user picked a colour. Carries the palette id.
                    Signal::builder("palette-changed")
                        .param_types([str::static_type()])
                        .build(),
                    // The pin toggle changed. Carries the new state.
                    Signal::builder("pin-changed")
                        .param_types([bool::static_type()])
                        .build(),
                    // The user confirmed deletion of this note.
                    Signal::builder("delete-requested").build(),
                    // The note was put away: kept, but not reopened next
                    // launch. Reached from the menu and Ctrl+W, never from ×.
                    Signal::builder("hide-requested").build(),
                ]
            })
        }

        fn dispose(&self) {
            // The style manager is process-global and outlives every window;
            // leaving the handler connected would keep firing into a dead
            // window for the rest of the session.
            if let Some(id) = self.style_handler.borrow_mut().take() {
                adw::StyleManager::default().disconnect(id);
            }
        }
    }

    impl WidgetImpl for NoteWindow {}
    impl WindowImpl for NoteWindow {
        fn close_request(&self) -> glib::Propagation {
            // × deletes, the way taking a sticky note off the wall does. A
            // blank note goes straight in the bin; one with writing on it asks
            // first, so a misclick cannot cost anything you would miss.
            //
            // Always Stop: the window is torn down by the application once the
            // note is actually gone, never by GTK behind its back.
            self.obj().confirm_delete();
            glib::Propagation::Stop
        }
    }
    impl ApplicationWindowImpl for NoteWindow {}
    impl AdwApplicationWindowImpl for NoteWindow {}
}

glib::wrapper! {
    pub struct NoteWindow(ObjectSubclass<imp::NoteWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl NoteWindow {
    pub fn new(app: &impl IsA<gtk::Application>) -> Self {
        glib::Object::builder()
            .property("application", app.as_ref())
            .build()
    }

    /// The note ID this window is showing.
    pub fn note_id(&self) -> String {
        self.imp().note_id.borrow().clone()
    }

    pub fn palette(&self) -> Palette {
        self.imp().palette.get()
    }

    pub fn is_pinned(&self) -> bool {
        self.imp().pinned.get()
    }

    /// The current text of the buffer.
    ///
    /// Hidden characters included. The invisible tag is how the note is
    /// *rendered* — the syntax is still part of the note, and asking the buffer
    /// to leave it out would make what gets saved depend on whether the window
    /// happened to have focus.
    pub fn body(&self) -> String {
        let Some(view) = self.imp().text_view.borrow().clone() else {
            return String::new();
        };
        let buffer = view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    }

    /// The D-Bus object path GTK exports for this window, which is how the
    /// shell extension identifies it. `None` before the window is added to an
    /// application (GTK assigns the ID at that point).
    pub fn object_path(&self) -> Option<String> {
        let id = self.id();
        (id != 0).then(|| crate::placement::window_object_path(id))
    }

    /// Show `note` in this window, replacing whatever was there.
    ///
    /// Widget writes performed here must not be mistaken for user edits, so the
    /// `loading` flag suppresses the signals they trigger.
    pub fn bind(&self, note: &Note) {
        let imp = self.imp();
        imp.loading.set(true);

        imp.note_id.replace(note.id.clone());

        if let Some(view) = imp.text_view.borrow().clone() {
            let buffer = view.buffer();
            if buffer.text(&buffer.start_iter(), &buffer.end_iter(), true) != note.body {
                buffer.set_text(&note.body);
            }
        }

        self.apply_palette(note.palette);
        self.set_pinned(note.pinned);
        self.refresh_title();

        let geometry = note.geometry.normalized();
        self.set_default_size(geometry.width, geometry.height);

        imp.loading.set(false);
    }

    /// Apply a palette: swap the CSS class, tick the matching swatch.
    pub fn apply_palette(&self, palette: Palette) {
        let imp = self.imp();
        for existing in Palette::ALL {
            self.remove_css_class(&existing.css_class());
        }
        self.add_css_class(&palette.css_class());
        imp.palette.set(palette);

        let was_loading = imp.loading.replace(true);
        for swatch in imp.swatches.borrow().iter() {
            let matches = swatch
                .action_target_value()
                .and_then(|v| v.get::<String>())
                .is_some_and(|id| id == palette.id());
            swatch.set_active(matches);
        }
        imp.loading.set(was_loading);
    }

    /// Create the tags the Markdown renderer applies.
    ///
    /// Sizes and weights only — colour comes from the palette's CSS, so a tag
    /// must never set a foreground or it would fight the note's own ink in one
    /// of the fourteen colour/scheme combinations.
    fn install_markdown_tags(buffer: &gtk::TextBuffer) {
        let table = buffer.tag_table();
        let add = |name: &str, configure: &dyn Fn(&gtk::TextTag)| {
            let tag = gtk::TextTag::builder().name(name).build();
            configure(&tag);
            table.add(&tag);
        };

        // Headings shrink with depth but stay above body size.
        for level in 1..=6u8 {
            let scale = match level {
                1 => 1.6,
                2 => 1.4,
                3 => 1.25,
                4 => 1.15,
                _ => 1.05,
            };
            add(&format!("md-h{level}"), &|tag| {
                tag.set_scale(scale);
                tag.set_weight(700);
                tag.set_pixels_above_lines(6);
            });
        }

        add("md-bold", &|tag| tag.set_weight(700));
        add("md-italic", &|tag| tag.set_style(gtk::pango::Style::Italic));
        add("md-strike", &|tag| tag.set_strikethrough(true));
        add("md-code", &|tag| {
            tag.set_family(Some("monospace"));
            tag.set_scale(0.95);
        });
        add("md-codeblock", &|tag| {
            tag.set_family(Some("monospace"));
            tag.set_scale(0.95);
            tag.set_left_margin(24);
        });
        add("md-quote", &|tag| {
            tag.set_style(gtk::pango::Style::Italic);
            tag.set_left_margin(24);
        });
        // One tag per nesting level. Hanging indent: the bullet sits in the
        // margin and wrapped lines line up under the item's text instead of
        // under the bullet.
        for level in 0..=markdown::MAX_LIST_DEPTH {
            let margin = 36 + i32::from(level) * 24;
            add(&format!("md-list{level}"), &|tag| {
                tag.set_left_margin(margin);
                tag.set_indent(-18);
                tag.set_pixels_above_lines(2);
            });
        }
        add("md-link", &|tag| {
            tag.set_underline(gtk::pango::Underline::Single)
        });

        // The one tag whose visibility is toggled.
        add("md-marker", &|tag| tag.set_invisible(true));
    }

    /// Re-parse the note and apply tags.
    ///
    /// Cheap enough to run on every keystroke: notes are short, and the parser
    /// does one pass with no allocation per character.
    fn restyle(&self) {
        let Some(view) = self.imp().text_view.borrow().clone() else {
            return;
        };
        let buffer = view.buffer();
        // Hidden characters included, or the offsets the parser reports would
        // be measured against a shorter string than the buffer holds and the
        // tags would land on the wrong characters.
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);

        buffer.remove_all_tags(&buffer.start_iter(), &buffer.end_iter());
        let parsed = markdown::parse(&text);

        let table = buffer.tag_table();
        let apply = |name: &str, start: usize, end: usize| {
            let Some(tag) = table.lookup(name) else {
                return;
            };
            let (Some(from), Some(to)) = (
                buffer.iter_at_offset(start as i32).into(),
                buffer.iter_at_offset(end as i32).into(),
            ) else {
                return;
            };
            buffer.apply_tag(&tag, &from, &to);
        };

        for span in &parsed.spans {
            // GtkTextView takes paragraph attributes (left_margin and friends)
            // from tags covering the start of the line. Block styles are parsed
            // as content-only — the text after "> " or "- " — so extend them
            // back to the line start, or their indents silently do nothing.
            let start = match span.style {
                Style::Heading(_) | Style::Quote | Style::ListItem(_) | Style::CodeBlock => {
                    let mut iter = buffer.iter_at_offset(span.start as i32);
                    iter.set_line_offset(0);
                    iter.offset() as usize
                }
                _ => span.start,
            };

            let name = match span.style {
                Style::Heading(level) => format!("md-h{level}"),
                Style::Bold => "md-bold".into(),
                Style::Italic => "md-italic".into(),
                Style::Strikethrough => "md-strike".into(),
                Style::Code => "md-code".into(),
                Style::CodeBlock => "md-codeblock".into(),
                Style::Quote => "md-quote".into(),
                Style::ListItem(level) => format!("md-list{level}"),
                Style::Link => "md-link".into(),
            };
            apply(&name, start, span.end);
        }
        for marker in &parsed.markers {
            apply("md-marker", marker.start, marker.end);
        }
    }

    /// Put ordered-list numbers back in sequence.
    ///
    /// Runs after a deletion, not after every keystroke: while you are typing,
    /// the numbers you write are yours, but a list left counting 1, 2, 4, 5 by
    /// a deletion is nothing anyone meant.
    pub fn renumber_lists(&self) {
        let Some(view) = self.imp().text_view.borrow().clone() else {
            return;
        };
        let buffer = view.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let edits = markdown::renumber(&text);
        if edits.is_empty() {
            return;
        }

        // One undo step: Ctrl+Z should take the numbering back in the same
        // breath as whatever it was correcting.
        buffer.begin_user_action();
        // Back to front, so an earlier edit cannot shift a later offset.
        for edit in edits.iter().rev() {
            let mut start = buffer.iter_at_offset(edit.start as i32);
            let mut end = buffer.iter_at_offset(edit.end as i32);
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &edit.number.to_string());
        }
        buffer.end_user_action();
    }

    /// Show or hide the Markdown syntax characters.
    pub fn set_markup_visible(&self, visible: bool) {
        if let Some(tag) = self.imp().marker_tag.borrow().as_ref() {
            tag.set_invisible(!visible);
        }
    }

    /// Whether the raw markup is currently on show.
    pub fn markup_visible(&self) -> bool {
        self.imp()
            .marker_tag
            .borrow()
            .as_ref()
            .map(|tag| !tag.is_invisible())
            .unwrap_or(true)
    }

    /// Put the note away: keep it, but take it off the screen.
    ///
    /// The non-destructive counterpart to ×, reached from the menu and Ctrl+W.
    pub fn put_away(&self) {
        self.emit_by_name::<()>("hide-requested", &[]);
        self.set_visible(false);
    }

    /// Show or clear the "not saving" banner.
    ///
    /// `None` means saving is working.
    pub fn set_save_error(&self, message: Option<&str>) {
        let Some(banner) = self.imp().save_banner.borrow().clone() else {
            return;
        };
        match message {
            Some(message) => {
                banner.set_title(&format!("Notes are not being saved — {message}"));
                banner.set_revealed(true);
            }
            None => banner.set_revealed(false),
        }
    }

    /// Whether the "not saving" banner is currently shown.
    pub fn save_error_shown(&self) -> bool {
        self.imp()
            .save_banner
            .borrow()
            .as_ref()
            .is_some_and(|b| b.is_revealed())
    }

    /// Record whether window placement is possible, and make the pin button
    /// say so.
    ///
    /// Wayland gives a client no way to raise itself above other windows, so
    /// without the companion shell extension this control is inert. Showing it
    /// latched while nothing happens reads as a bug in the app; showing it
    /// disabled, with a tooltip explaining why, reads as the truth.
    pub fn set_placement_available(&self, available: bool) {
        let imp = self.imp();
        imp.placement_available.set(available);

        let Some(button) = imp.pin_button.borrow().clone() else {
            return;
        };
        button.set_sensitive(available);
        if available {
            self.refresh_pin_tooltip();
        } else {
            button.set_tooltip_text(Some(
                "Keep on Top needs the Stickies GNOME Shell extension — \
                 Wayland does not let apps raise their own windows",
            ));
        }
    }

    pub fn placement_available(&self) -> bool {
        self.imp().placement_available.get()
    }

    fn refresh_pin_tooltip(&self) {
        if let Some(button) = self.imp().pin_button.borrow().clone() {
            button.set_tooltip_text(Some(if self.is_pinned() {
                "Stop Keeping on Top"
            } else {
                "Keep on Top"
            }));
        }
    }

    pub fn set_pinned(&self, pinned: bool) {
        let imp = self.imp();
        imp.pinned.set(pinned);

        let was_loading = imp.loading.replace(true);
        if let Some(button) = imp.pin_button.borrow().clone() {
            button.set_active(pinned);
            if pinned {
                button.add_css_class("pinned");
            } else {
                button.remove_css_class("pinned");
            }
        }
        imp.loading.set(was_loading);

        // Leaves the "needs the extension" explanation in place when that is
        // the state we are in.
        if imp.placement_available.get() {
            self.refresh_pin_tooltip();
        }
    }

    /// Recompute the header title from the buffer contents.
    pub fn refresh_title(&self) {
        let mut probe = Note::new(self.palette());
        probe.body = self.body();
        let title = probe.title();

        self.set_title(Some(&title));
        if let Some(label) = self.imp().title_label.borrow().clone() {
            label.set_label(&title);
        }
        // The visible title is truncated; screen readers get the whole thing.
        self.update_property(&[gtk::accessible::Property::Label(&title)]);
    }

    pub fn toast(&self, toast: adw::Toast) {
        if let Some(overlay) = self.imp().toasts.borrow().clone() {
            overlay.add_toast(toast);
        }
    }

    /// Move the keyboard focus into the note text, at the end.
    pub fn focus_text(&self) {
        if let Some(view) = self.imp().text_view.borrow().clone() {
            let buffer = view.buffer();
            buffer.place_cursor(&buffer.end_iter());
            view.grab_focus();
        }
    }

    // ---- construction ---------------------------------------------------

    fn build_ui(&self) {
        let imp = self.imp();

        self.add_css_class("sticky-note");
        self.set_size_request(MIN_WIDTH, MIN_HEIGHT);
        self.set_default_size(
            crate::model::note::DEFAULT_WIDTH,
            crate::model::note::DEFAULT_HEIGHT,
        );
        self.apply_palette(Palette::default());
        self.follow_color_scheme();

        let text_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(10)
            .bottom_margin(14)
            .left_margin(14)
            .right_margin(14)
            .pixels_below_lines(3)
            .accepts_tab(false) // Tab moves focus; notes are not code editors.
            .build();
        text_view.update_property(&[gtk::accessible::Property::Label("Note text")]);

        Self::install_markdown_tags(&text_view.buffer());
        imp.marker_tag
            .replace(text_view.buffer().tag_table().lookup("md-marker"));

        // Reveal the markup while the note is being edited, hide it otherwise.
        // One widget throughout, so clicking lands the cursor exactly where you
        // clicked rather than dumping it at the end of a freshly swapped view.
        text_view.connect_has_focus_notify(clone!(
            #[weak(rename_to = win)]
            self,
            move |view| win.set_markup_visible(view.has_focus())
        ));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(clone!(
            #[weak]
            text_view,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let enter = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter);
                let plain = !state
                    .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
                if enter && plain {
                    continue_list(&text_view)
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        text_view.add_controller(keys);

        // Deleting item 3 of 7 must not leave the rest counting 4, 5, 6. The
        // signal arrives before the text is actually gone, and a buffer cannot
        // be edited from inside its own delete handler anyway, so the pass is
        // deferred to the idle straight after.
        text_view.buffer().connect_delete_range(clone!(
            #[weak(rename_to = win)]
            self,
            move |_buffer, _start, _end| {
                if win.imp().loading.get() || win.imp().renumber_queued.replace(true) {
                    return;
                }
                glib::idle_add_local_once(clone!(
                    #[weak]
                    win,
                    move || {
                        win.imp().renumber_queued.set(false);
                        win.renumber_lists();
                    }
                ));
            }
        ));

        text_view.buffer().connect_changed(clone!(
            #[weak(rename_to = win)]
            self,
            move |_buffer| {
                // Re-tag even while loading: the styling must match the text
                // that was just put there, and applying tags is not an edit so
                // it cannot recurse back into this handler.
                win.restyle();
                if win.imp().loading.get() {
                    return;
                }
                win.refresh_title();
                win.emit_by_name::<()>("body-changed", &[]);
            }
        ));

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&text_view)
            .build();

        let header = self.build_header();

        // A banner rather than a toast: notes failing to save is an ongoing
        // condition, not an event, and it must not be possible to miss it.
        let save_banner = adw::Banner::builder().revealed(false).build();
        save_banner.add_css_class("error");

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&save_banner);
        body.append(&scroller);

        let toolbar = adw::ToolbarView::builder()
            .top_bar_style(adw::ToolbarStyle::Flat)
            .content(&body)
            .build();
        toolbar.add_top_bar(&header);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        self.set_content(Some(&toasts));

        imp.text_view.replace(Some(text_view));
        imp.save_banner.replace(Some(save_banner));
        imp.toasts.replace(Some(toasts));

        self.install_actions();
    }

    fn build_header(&self) -> adw::HeaderBar {
        let imp = self.imp();

        // Only a close button: a sticky note has no meaningful maximised or
        // minimised state, and offering those buttons implies it does.
        let header = adw::HeaderBar::builder()
            .decoration_layout(":close")
            .build();
        header.add_css_class("flat");

        let pin_button = gtk::ToggleButton::builder()
            .icon_name("view-pin-symbolic")
            .tooltip_text("Keep on Top")
            .action_name("note.toggle-pin")
            .build();
        header.pack_start(&pin_button);

        let new_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Note")
            .action_name("app.new-note")
            .build();
        header.pack_start(&new_button);

        let title_label = gtk::Label::builder()
            .ellipsize(pango::EllipsizeMode::End)
            .single_line_mode(true)
            .css_classes(["note-title"])
            .build();
        header.set_title_widget(Some(&title_label));

        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .primary(true)
            .build();
        menu_button.set_popover(Some(&self.build_menu()));
        header.pack_end(&menu_button);

        imp.pin_button.replace(Some(pin_button));
        imp.title_label.replace(Some(title_label));
        header
    }

    fn build_menu(&self) -> gtk::PopoverMenu {
        let menu = gio::Menu::new();

        // A row of colour swatches, embedded in the menu as a custom widget.
        // Colours are recognised by sight, so seven swatches beat seven labels.
        let colors = gio::Menu::new();
        let swatch_item = gio::MenuItem::new(None, None);
        swatch_item.set_attribute_value("custom", Some(&"palette".to_variant()));
        colors.append_item(&swatch_item);
        menu.append_section(None, &colors);

        let note_section = gio::Menu::new();
        note_section.append(Some("_New Note"), Some("app.new-note"));
        note_section.append(Some("_Duplicate Note"), Some("note.duplicate"));
        // Named explicitly, because × no longer does this.
        note_section.append(Some("Put _Away"), Some("note.hide"));
        note_section.append(Some("_Show All Notes"), Some("app.show-all"));
        menu.append_section(None, &note_section);

        let destructive = gio::Menu::new();
        destructive.append(Some("_Delete Note…"), Some("note.delete"));
        menu.append_section(None, &destructive);

        let app_section = gio::Menu::new();
        app_section.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
        app_section.append(Some("_About Stickies"), Some("app.about"));
        menu.append_section(None, &app_section);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.add_child(&self.build_swatches(), "palette");
        popover
    }

    fn build_swatches(&self) -> gtk::Widget {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        row.update_property(&[gtk::accessible::Property::Label("Note colour")]);

        let mut swatches = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;

        for palette in Palette::ALL {
            let button = gtk::ToggleButton::builder()
                .tooltip_text(palette.label())
                .css_classes(["color-swatch", "circular", &palette.css_class()])
                .action_name("note.set-palette")
                .action_target(&palette.id().to_variant())
                .build();
            button.update_property(&[gtk::accessible::Property::Label(palette.label())]);

            // Radio behaviour: exactly one colour at a time.
            match &group {
                Some(first) => button.set_group(Some(first)),
                None => group = Some(button.clone()),
            }

            row.append(&button);
            swatches.push(button);
        }

        self.imp().swatches.replace(swatches);
        row.upcast()
    }

    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();

        let set_palette = gio::SimpleAction::new_stateful(
            "set-palette",
            Some(glib::VariantTy::STRING),
            &Palette::default().id().to_variant(),
        );
        set_palette.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |action, parameter| {
                let Some(id) = parameter.and_then(|p| p.get::<String>()) else {
                    return;
                };
                let Some(palette) = Palette::from_id(&id) else {
                    return;
                };
                action.set_state(&id.to_variant());
                if win.imp().loading.get() || win.palette() == palette {
                    return;
                }
                win.apply_palette(palette);
                win.emit_by_name::<()>("palette-changed", &[&id]);
            }
        ));
        actions.add_action(&set_palette);

        let toggle_pin = gio::SimpleAction::new("toggle-pin", None);
        toggle_pin.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_action, _| {
                if win.imp().loading.get() {
                    return;
                }
                let pinned = !win.is_pinned();
                win.set_pinned(pinned);
                win.emit_by_name::<()>("pin-changed", &[&pinned]);
            }
        ));
        actions.add_action(&toggle_pin);

        let hide = gio::SimpleAction::new("hide", None);
        hide.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_action, _| win.put_away()
        ));
        actions.add_action(&hide);

        let delete = gio::SimpleAction::new("delete", None);
        delete.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_action, _| win.confirm_delete()
        ));
        actions.add_action(&delete);

        let duplicate = gio::SimpleAction::new("duplicate", None);
        duplicate.connect_activate(clone!(
            #[weak(rename_to = win)]
            self,
            move |_action, _| {
                WidgetExt::activate_action(
                    &win,
                    "app.duplicate-note",
                    Some(&win.note_id().to_variant()),
                )
                .unwrap_or_else(|err| {
                    glib::g_warning!("stickies", "duplicate failed: {err}");
                });
            }
        ));
        actions.add_action(&duplicate);

        self.insert_action_group("note", Some(&actions));
    }

    /// Deleting is irreversible once the window is gone, so an empty note goes
    /// straight in the bin while one with content asks first. Confirming a
    /// blank note would be a dialog that never has a wrong answer.
    fn confirm_delete(&self) {
        if self.body().trim().is_empty() {
            self.emit_by_name::<()>("delete-requested", &[]);
            return;
        }

        let dialog = adw::AlertDialog::new(
            Some("Delete Note?"),
            Some("This note and its contents will be permanently deleted."),
        );
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("delete", "_Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        dialog.connect_response(
            None,
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_dialog, response| {
                    if response == "delete" {
                        win.emit_by_name::<()>("delete-requested", &[]);
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    /// Track the system light/dark preference. GTK has no CSS selector for the
    /// colour scheme, so the class is maintained from the style manager.
    fn follow_color_scheme(&self) {
        let manager = adw::StyleManager::default();
        self.sync_color_scheme(&manager);

        let id = manager.connect_dark_notify(clone!(
            #[weak(rename_to = win)]
            self,
            move |manager| win.sync_color_scheme(manager)
        ));
        self.imp().style_handler.replace(Some(id));
    }

    fn sync_color_scheme(&self, manager: &adw::StyleManager) {
        if manager.is_dark() {
            self.add_css_class("dark");
        } else {
            self.remove_css_class("dark");
        }
    }
}

/// Carry a list on to the next line when Enter is pressed inside one.
///
/// A list you have to retype the bullet for is a list you stop using, so Enter
/// lays down the same indent and bullet, or the next number. The escape hatches
/// are the two people reach for anyway: Enter on an empty item, or Backspace
/// over the bullet just inserted.
fn continue_list(view: &gtk::TextView) -> glib::Propagation {
    if !view.is_editable() {
        return glib::Propagation::Proceed;
    }
    let buffer = view.buffer();
    let cursor = buffer.iter_at_mark(&buffer.get_insert());

    let mut line_start = cursor;
    line_start.set_line_offset(0);
    let mut line_end = line_start;
    if !line_end.ends_line() {
        line_end.forward_to_line_end();
    }
    let line = buffer.text(&line_start, &line_end, true);

    let Some(action) = markdown::list_enter(&line) else {
        return glib::Propagation::Proceed;
    };

    // One undo step, so Ctrl+Z takes back the whole line rather than the
    // newline and the bullet separately.
    buffer.begin_user_action();
    buffer.delete_selection(true, true);
    match action {
        markdown::ListEnter::Continue(prefix) => {
            buffer.insert_at_cursor(&format!("\n{prefix}"));
        }
        // Leave the cursor on the now-blank line: the list is over, and the
        // next Enter is an ordinary one.
        markdown::ListEnter::EndList => {
            let mut start = buffer.iter_at_mark(&buffer.get_insert());
            start.set_line_offset(0);
            let mut end = start;
            if !end.ends_line() {
                end.forward_to_line_end();
            }
            buffer.delete(&mut start, &mut end);
        }
    }
    buffer.end_user_action();

    view.scroll_mark_onscreen(&buffer.get_insert());
    glib::Propagation::Stop
}
