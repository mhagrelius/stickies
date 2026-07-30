use gtk::glib;
use gtk::prelude::*;

fn main() -> glib::ExitCode {
    // Before anything else can log or panic. Launched from the dock the app is
    // started by D-Bus activation and inherits stdout/stderr from the bus
    // daemon, which lands nowhere — so records go to the journal directly.
    stickies::diagnostics::install_log_writer();
    stickies::diagnostics::install_panic_hook();

    // Only the D-Bus-visible application ID matters to the shell extension;
    // setting these keeps `ps`, the app switcher and the dock consistent too.
    glib::set_application_name("Stickies");
    glib::set_prgname(Some(stickies::APP_ID));

    stickies::ui::StickiesApplication::new().run()
}
