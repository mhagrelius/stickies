//! Render note windows to PNG for design review.
//!
//! Screenshotting a live GNOME Wayland session needs interactive consent, which
//! makes "does this actually look right?" awkward to answer during development.
//! This renders the real widget tree — same CSS, same palettes — straight to
//! image files instead.
//!
//! ```sh
//! cargo run --example preview -- /tmp/stickies-preview light
//! cargo run --example preview -- /tmp/stickies-preview dark
//! ```
//!
//! Writes `<dir>/<scheme>-<palette>.png`. One scheme per run: the colour scheme
//! is a process-wide setting and every open window follows it, so rendering
//! both in one pass would capture whichever was set last.

use adw::prelude::*;
use gtk::{gdk, glib, gsk};
use std::path::PathBuf;

use stickies::model::note::{Note, Palette};
use stickies::ui::NoteWindow;

const SAMPLE: &str = concat!(
    "# Standup
",
    "
",
    "- **ship** the placement fix
",
    "- chase the ~~design~~ review
",
    "- book `flights` before Friday
",
    "
",
    "> blocked on [the API](https://example.com)",
);

fn main() -> glib::ExitCode {
    let out_dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "preview".to_string())
        .into();
    let scheme_name = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "light".to_string());
    let scheme = match scheme_name.as_str() {
        "dark" => adw::ColorScheme::ForceDark,
        "light" => adw::ColorScheme::ForceLight,
        other => {
            eprintln!("unknown scheme {other:?}; expected \"light\" or \"dark\"");
            return glib::ExitCode::FAILURE;
        }
    };

    let app = adw::Application::builder()
        .application_id("us.hagreli.Stickies.Preview")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|_| {
        if let Some(display) = gdk::Display::default() {
            stickies::ui::load_stylesheet(&display);
        }
    });

    app.connect_activate(move |app| {
        std::fs::create_dir_all(&out_dir).expect("create output directory");

        // The window has to be presented for CSS to resolve and for the widget
        // to get an allocation; there is no offscreen path in GTK 4 that skips
        // that. It is on screen for a few frames only.
        adw::StyleManager::default().set_color_scheme(scheme);

        let mut pending: Vec<(NoteWindow, PathBuf)> = Vec::new();
        for palette in Palette::ALL {
            let mut note = Note::new(palette);
            note.body = SAMPLE.to_string();

            let window = NoteWindow::new(app);
            window.bind(&note);
            window.set_default_size(340, 320);
            window.present();

            pending.push((
                window,
                out_dir.join(format!("{scheme_name}-{}.png", palette.id())),
            ));
        }

        // Give the compositor a couple of frames to map and paint everything
        // before asking for pixels; an unmapped widget renders empty.
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(1500),
            glib::clone!(
                #[weak]
                app,
                move || {
                    for (window, path) in &pending {
                        // Capture the *unfocused* appearance. Presenting a
                        // window focuses its text view, which correctly
                        // reveals the markup for editing; the rendered view is
                        // what a note looks like when you are not in it.
                        window.set_markup_visible(false);
                        match capture(window) {
                            Some(texture) => {
                                texture.save_to_png(path).expect("write png");
                                println!("wrote {}", path.display());
                            }
                            None => eprintln!("could not capture {}", path.display()),
                        }
                    }
                    for (window, _) in &pending {
                        window.destroy();
                    }
                    app.quit();
                }
            ),
        );
    });

    app.run_with_args::<&str>(&[])
}

/// Snapshot a whole note window into a texture.
///
/// The *window* is the subject, not its content child: the note's background
/// colour is painted by the window node itself, so capturing the child would
/// render the text and header over transparency.
fn capture(window: &NoteWindow) -> Option<gdk::Texture> {
    let widget = window.upcast_ref::<gtk::Widget>();
    let width = widget.width();
    let height = widget.height();
    if width <= 0 || height <= 0 {
        return None;
    }

    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width as f64, height as f64);
    let node = snapshot.to_node()?;

    let renderer = gsk::CairoRenderer::new();
    renderer.realize(gdk::Surface::NONE).ok()?;
    let texture = renderer.render_texture(&node, None);
    renderer.unrealize();
    Some(texture)
}
