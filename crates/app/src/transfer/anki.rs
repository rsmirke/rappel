//! The Anki import from the primary menu's Import… (ADR 0061): the package
//! is read on a worker thread with a toast that can cancel it, the plan is
//! shown with its report, and the import runs on a worker thread too, also
//! cancellable. The texts are pure functions in [`super::anki_text`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use rappel_core::Error;
use rappel_core::interchange::{AnkiPlan, AnkiSummary};

use super::anki_text::{
    anki_done, anki_imported_text, anki_refused_text, anki_view, cancelled_text, importing_text,
    reading_text,
};
use super::confirm_layout;
use super::text::{check_failed_text, import_failed_text};
use crate::i18n::gettext;
use crate::state::AppState;
use crate::ui::toast::{info_toast, report_toast, show_error};
use crate::ui::worker;
use crate::window::RappelWindow;

/// A toast that stays while work runs, with a Cancel button that sets
/// `cancel`. Dismissed by the caller when the work ends. The title fits
/// beside its button at 360 px (`anki_text` tests).
fn progress_toast(title: &str, cancel: &Arc<AtomicBool>) -> adw::Toast {
    let toast = adw::Toast::builder()
        .title(title)
        .use_markup(false)
        .timeout(0)
        .button_label(gettext("_Cancel"))
        // Shown at once, even over a toast that stays until dismissed
        // (`ADW_TOAST_PRIORITY_HIGH` pushes the shown toast into the queue,
        // Adw-1.gir).
        .priority(adw::ToastPriority::High)
        .build();
    let cancel = Arc::clone(cancel);
    // `button-clicked` is emitted after the button is pressed (Adw-1.gir);
    // `adw_toast_dismiss` does nothing on a toast already dismissed.
    toast.connect_button_clicked(move |toast| {
        cancel.store(true, Ordering::Relaxed);
        toast.dismiss();
    });
    toast
}

/// Reads the Anki package `path`, shows what importing it would do and
/// imports it if the user confirms.
pub(super) async fn import(window: &RappelWindow, data_dir: PathBuf, path: PathBuf) {
    let Some(plan) = plan(window, &data_dir, path).await else {
        return;
    };
    if !confirm(window, plan.summary()).await {
        return;
    }
    apply(window, &data_dir, plan).await;
}

async fn plan(window: &RappelWindow, data_dir: &Path, path: PathBuf) -> Option<AnkiPlan> {
    let cancel = Arc::new(AtomicBool::new(false));
    let toast = progress_toast(&reading_text(), &cancel);
    window.add_toast(toast.clone());
    let worker_dir = data_dir.to_path_buf();
    let worker_cancel = Arc::clone(&cancel);
    let result = worker::run(window, move || {
        AppState::plan_anki_import_on_worker(&worker_dir, &path, &worker_cancel)
    })
    .await;
    toast.dismiss();
    match result {
        Ok(Ok(plan)) => Some(plan),
        Ok(Err(Error::ImportCancelled)) => {
            window.add_toast(info_toast(&cancelled_text()));
            None
        }
        Ok(Err(Error::AnkiRefused(problem))) => {
            show_error(window, "an Anki import check", &anki_refused_text(&problem));
            None
        }
        Ok(Err(err)) => {
            show_error(window, "an Anki import check", &check_failed_text(&err));
            None
        }
        Err(_) => {
            show_error(
                window,
                "an Anki import check",
                &gettext("Checking the file stopped unexpectedly"),
            );
            None
        }
    }
}

/// Shows the plan and its report; whether the user chose Import.
async fn confirm(window: &RappelWindow, summary: &AnkiSummary) -> bool {
    let Ok((dialog, report_label)) = confirm_layout() else {
        return false;
    };
    let view = anki_view(summary);
    dialog.set_heading(Some(&view.heading));
    dialog.set_body(&view.body);
    if !view.can_import {
        dialog.remove_response("import");
        dialog.set_response_label("cancel", &gettext("_Close"));
        dialog.set_default_response(Some("cancel"));
    }
    if let Some(report) = &view.report {
        // Plain text: the examples are the package's own text.
        report_label.set_label(report);
        report_label.set_visible(true);
    }
    let response = dialog.choose_future(Some(window)).await;
    view.can_import && response == "import"
}

async fn apply(window: &RappelWindow, data_dir: &Path, plan: AnkiPlan) {
    let cancel = Arc::new(AtomicBool::new(false));
    let toast = progress_toast(&importing_text(), &cancel);
    window.add_toast(toast.clone());
    let worker_dir = data_dir.to_path_buf();
    let worker_cancel = Arc::clone(&cancel);
    let result = worker::run(window, move || {
        AppState::apply_anki_import_on_worker(&worker_dir, &plan, &worker_cancel)
    })
    .await;
    toast.dismiss();
    match result {
        Ok(Ok(report)) => {
            let done = anki_done(&report);
            // The report is worth reading: the toast stays when it has one,
            // its lines left-aligned below what was added.
            let toast = match &done.report {
                None => info_toast(&anki_imported_text(&done)),
                Some(lines) => report_toast(&done.title, &done.summary, lines),
            };
            window.add_toast(toast);
            window.data_changed();
        }
        Ok(Err(Error::ImportCancelled)) => window.add_toast(info_toast(&cancelled_text())),
        Ok(Err(err)) => show_error(window, "an Anki import", &import_failed_text(&err)),
        Err(_) => show_error(
            window,
            "an Anki import",
            &gettext("The import stopped unexpectedly"),
        ),
    }
}
