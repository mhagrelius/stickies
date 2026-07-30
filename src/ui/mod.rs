//! The GTK 4/libadwaita layer.

pub mod application;
pub mod note_window;

pub use application::StickiesApplication;
pub use note_window::NoteWindow;

/// The app stylesheet, compiled into the binary. Notes need per-palette colours
/// that no platform style class provides, so this is loaded as an application
/// provider layered on top of Adwaita.
pub const STYLE: &str = include_str!("style.css");

/// Install the stylesheet on the given display.
pub fn load_stylesheet(display: &gtk::gdk::Display) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(STYLE);
    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
