//! Export and import from the primary menu (SPEC §4.1, ADR 0018).
//!
//! The file choosers and dialogs run here on the main thread; the work runs
//! on a worker thread with its own database connection
//! ([`AppState::export_on_worker`] and the import functions), so the window
//! never freezes. The texts are pure functions in [`text`]; [`names`]
//! looks up the user's own names that conflicts refer to.

mod names;
mod text;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};
use rappel_core::Error;
use rappel_core::interchange::{FILE_EXTENSION, IMPORT_EXTENSIONS, ImportPlan, ImportSummary};

use crate::state::AppState;
use crate::ui::toast::{info_toast, persistent_toast, show_error};
use crate::ui::{dialog, worker};
use crate::window::RappelWindow;
use names::Names;
use text::{
    check_failed_text, conflicts_text, export_file_name, exported_text, import_failed_text,
    imported_text, plan_view, refused_text,
};

/// Asks where to save, then exports there on a worker thread.
pub async fn export(window: &RappelWindow, state: &AppState) {
    let (year, month, day) = glib::DateTime::now_local().map_or((0, 0, 0), |now| now.ymd());
    let dialog = file_dialog("Export Data", &[FILE_EXTENSION]);
    dialog.set_initial_name(Some(&export_file_name(year, month, day)));
    let Some(path) = chosen_path(window, dialog.save_future(Some(window)).await) else {
        return;
    };

    let data_dir = state.data_dir().to_path_buf();
    let worker_path = path.clone();
    let result = worker::run(window, move || {
        AppState::export_on_worker(&data_dir, &worker_path)
    })
    .await;
    match result {
        Ok(Ok(summary)) => {
            let text = exported_text(&path, summary.missing_media);
            // Lost media files are worth reading, so that toast stays.
            let toast = if summary.missing_media > 0 {
                persistent_toast(&text)
            } else {
                info_toast(&text)
            };
            window.add_toast(toast);
        }
        Ok(Err(err)) => show_error(window, "an export", &format!("The export failed: {err}")),
        Err(_) => show_error(window, "an export", "The export stopped unexpectedly"),
    }
}

/// Asks for a file, checks it on a worker thread, shows what it would do,
/// and imports it on a worker thread if the user confirms.
pub async fn import(window: &RappelWindow, state: &AppState) {
    let dialog = file_dialog("Import Data", &IMPORT_EXTENSIONS);
    let Some(path) = chosen_path(window, dialog.open_future(Some(window)).await) else {
        return;
    };

    let data_dir = state.data_dir().to_path_buf();
    let worker_dir = data_dir.clone();
    let result = worker::run(window, move || {
        AppState::plan_import_on_worker(&worker_dir, &path)
    })
    .await;
    let plan = match result {
        Ok(Ok(plan)) => plan,
        Ok(Err(Error::ImportRefused(problem))) => {
            show_error(window, "an import check", &refused_text(&problem));
            return;
        }
        Ok(Err(err)) => {
            show_error(window, "an import check", &check_failed_text(&err));
            return;
        }
        Err(_) => {
            show_error(
                window,
                "an import check",
                "Checking the file stopped unexpectedly",
            );
            return;
        }
    };

    if !confirm(window, state, plan.summary()).await {
        return;
    }
    apply(window, data_dir, plan).await;
}

/// The import question and its conflicts label, from
/// `data/import_dialog.blp`; `Err` with the first missing object's id,
/// after logging every missing one.
pub(crate) fn confirm_layout() -> Result<(adw::AlertDialog, gtk::Label), &'static str> {
    const FILE: &str = "import_dialog.ui";
    let builder = dialog::builder(FILE);
    let (dialog, conflicts_label) = (
        dialog::object(&builder, FILE, "import_dialog"),
        dialog::object(&builder, FILE, "conflicts_label"),
    );
    Ok((dialog?, conflicts_label?))
}

/// Shows the plan; whether the user chose Import.
async fn confirm(window: &RappelWindow, state: &AppState, summary: &ImportSummary) -> bool {
    let Ok((dialog, conflicts_label)) = confirm_layout() else {
        return false;
    };
    let view = plan_view(summary);
    dialog.set_heading(Some(view.heading));
    dialog.set_body(&view.body);
    if !view.can_import {
        dialog.remove_response("import");
        dialog.set_response_label("cancel", "_Close");
        dialog.set_default_response(Some("cancel"));
    }
    if !summary.conflicts.is_empty() {
        let lookup = Names::read(state, &summary.conflicts);
        conflicts_label.set_label(&conflicts_text(
            &summary.conflicts,
            |id| lookup.deck(id),
            |id| lookup.card(id),
        ));
        conflicts_label.set_visible(true);
    }
    let response = dialog.choose_future(Some(window)).await;
    view.can_import && response == "import"
}

/// Applies the plan on a worker thread, then says what was added and
/// refreshes every page that shows decks or cards.
async fn apply(window: &RappelWindow, data_dir: PathBuf, plan: ImportPlan) {
    let result = worker::run(window, move || {
        AppState::apply_import_on_worker(&data_dir, &plan)
    })
    .await;
    match result {
        Ok(Ok(report)) => {
            window.add_toast(info_toast(&imported_text(&report)));
            window.data_changed();
        }
        Ok(Err(err)) => show_error(window, "an import", &import_failed_text(&err)),
        Err(_) => show_error(window, "an import", "The import stopped unexpectedly"),
    }
}

/// A modal file chooser for `.rappel` files. It also shows `.cards` files,
/// the name exports had before the rename (ADR 0020), so old exports import.
/// Saving offers only `.rappel`, so an old export is not overwritten by accident.
fn file_dialog(title: &str, extensions: &[&str]) -> gtk::FileDialog {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Rappel Exports"));
    for extension in extensions {
        filter.add_suffix(extension);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    gtk::FileDialog::builder()
        .title(title)
        .modal(true)
        .filters(&filters)
        .default_filter(&filter)
        .build()
}

/// The local path chosen, or `None` if the user cancelled or the chooser
/// failed (then after a toast).
fn chosen_path(window: &RappelWindow, chosen: Result<gio::File, glib::Error>) -> Option<PathBuf> {
    match chosen {
        Ok(file) => {
            let path = file.path();
            if path.is_none() {
                show_error(window, "a file choice", "Choose a file on this computer");
            }
            path
        }
        // Closing the chooser is not an error (GtkFileDialog docs: the
        // callback gets GTK_DIALOG_ERROR_DISMISSED).
        Err(err)
            if err.matches(gtk::DialogError::Dismissed)
                || err.matches(gtk::DialogError::Cancelled) =>
        {
            None
        }
        Err(err) => {
            show_error(
                window,
                "a file choice",
                &format!("The file chooser failed: {err}"),
            );
            None
        }
    }
}
