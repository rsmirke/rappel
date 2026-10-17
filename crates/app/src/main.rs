//! Rappel: a native GNOME flashcard app.

mod clock;
mod dialogs;
mod pages;
mod render;
mod startup;
mod state;
#[cfg(test)]
mod testing;
mod transfer;
mod ui;
#[cfg(test)]
mod ui_files;
mod widgets;
mod window;
mod window_state;

use adw::prelude::*;
use gtk::{gio, glib};
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

use crate::window::RappelWindow;

// The `catch_unwind` guards around RaTeX (`render::math`, `render::math_view`;
// ADR 0023) only work if a panic unwinds. With `panic = "abort"` a bug in
// RaTeX on one hostile formula would close the window instead of showing its
// source, so such a build is refused.
#[cfg(panic = "abort")]
compile_error!(
    "Rappel needs panic = \"unwind\": the RaTeX catch_unwind guards (ADR 0023) rely on it"
);

/// The application's accelerators. `app.shortcuts` (Ctrl+?) is set up by
/// `AdwApplication` itself from `data/shortcuts-dialog.blp`, which lists
/// these too (a test in `dialogs/shortcuts.rs` checks it).
const ACCELS: [(&str, &str); 3] = [
    ("win.new", "<Control>n"),
    ("win.search", "<Control>f"),
    ("win.preferences", "<Control>comma"),
];

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            // Without RUST_LOG, still show warnings and errors.
            EnvFilter::builder()
                .with_default_directive(LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .init();

    if let Err(err) = gio::resources_register_include!("rappel.gresource") {
        tracing::error!("failed to register bundled resources: {err}");
        return glib::ExitCode::FAILURE;
    }

    // `data/style.css` (table borders) needs no code: AdwApplication loads
    // `style.css` from the resource base path, which is the app ID's.
    let app = adw::Application::builder()
        .application_id(rappel_core::APP_ID)
        .build();
    // GtkSourceView (the card editor) needs GTK initialised first. `startup`
    // is a GApplication RUN_FIRST signal, so GtkApplication's own handler,
    // which initialises GTK, runs before this one (gio's GApplication docs).
    app.connect_startup(|_| sourceview5::init());
    for (action, accel) in ACCELS {
        app.set_accels_for_action(action, &[accel]);
    }
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        // The database is opened here, in the one running instance, and not
        // in main(): a second launch only forwards `activate` to it, and must
        // not open (back up, migrate) the database too.
        let window = RappelWindow::new(app);
        match startup::start() {
            Ok(state) => window.show_main(state),
            Err(problem) => window.show_problem(problem),
        }
        window.present();
    });

    tracing::debug!("starting {}", rappel_core::APP_NAME);
    app.run()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The stylesheet parses without an error: GTK drops a rule it cannot
    /// parse and only warns, so a typo would silently lose a style.
    #[test]
    fn the_stylesheet_parses() {
        let errors = crate::testing::with_gtk(|| {
            let provider = gtk::CssProvider::new();
            let errors = Rc::new(RefCell::new(Vec::new()));
            let seen = Rc::clone(&errors);
            provider.connect_parsing_error(move |_, section, error| {
                seen.borrow_mut()
                    .push(format!("{}: {error}", section.to_str()));
            });
            provider.load_from_string(include_str!("../data/style.css"));
            errors.take()
        });
        assert!(errors.is_empty(), "{errors:#?}");
    }
}
