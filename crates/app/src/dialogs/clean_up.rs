//! Clean Up Media, in Preferences (ADR 0057): the row that says how many
//! media files no card uses and their size, the question before they move
//! to the trash, and the toast after, with Undo. The count, the clean-up
//! and the undo run on a worker thread, on their own connection. Layout in
//! `data/preferences_dialog.blp` (the "Storage" group) and
//! `data/clean_up_media_dialog.blp`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use rappel_core::media::{CleanedUp, PURGE_AFTER_DAYS, UnusedMedia};

use crate::state::AppState;
use crate::ui::dialog;
use crate::ui::text::count;
use crate::ui::toast::{ErrorSlot, undo_toast};
use crate::ui::worker;

/// A size as GNOME writes one: decimal units, one decimal ("4.3 MB",
/// `g_format_size`).
pub fn size_text(bytes: u64) -> String {
    glib::format_size(bytes).to_string()
}

/// The row's subtitle for what the count found: "12 unused files, 4.3 MB",
/// or "No unused files".
pub fn unused_text(unused: UnusedMedia) -> String {
    if unused.files == 0 {
        return "No unused files".to_owned();
    }
    format!(
        "{}, {}",
        count(unused.files, "unused file", "unused files"),
        size_text(unused.bytes)
    )
}

/// The row's subtitle while the files are counted.
pub const COUNTING_TEXT: &str = "Looking for files no card uses…";

/// The row's subtitle when they could not be counted.
pub const COUNT_FAILED_TEXT: &str = "Unused files could not be counted";

/// The question's body: what moves where, and for how long it can be
/// brought back.
pub fn question_text(unused: UnusedMedia) -> String {
    let files = count(unused.files, "media file", "media files");
    format!(
        "{files} that no card uses ({}) will move to the trash in Rappel’s data folder. \
         After {PURGE_AFTER_DAYS} days they are deleted, except those a backup still uses.",
        size_text(unused.bytes)
    )
}

/// The toast after a clean-up of `files`: "12 files trashed"; none
/// when nothing moved (another window cleaned up meanwhile: the row's count
/// says "No unused files"). Short, so it fits beside Undo at 360 px
/// (`ui::toast::TOAST_FITS_WITH_BUTTON`, ADR 0058); the size was in the
/// question, and the row counts again.
pub fn moved_text(files: usize) -> Option<String> {
    (files > 0).then(|| format!("{} trashed", count(files, "file", "files")))
}

/// Whether the row's button can start a clean-up: only with something to
/// move, and not while a count, a clean-up or an undo runs.
pub fn can_clean_up(unused: Option<UnusedMedia>, busy: bool) -> bool {
    !busy && unused.is_some_and(|u| u.files > 0)
}

/// The row and the state its handlers share.
#[derive(Clone)]
struct Context {
    state: AppState,
    dialog: glib::WeakRef<adw::PreferencesDialog>,
    row: glib::WeakRef<adw::ActionRow>,
    button: glib::WeakRef<gtk::Button>,
    /// What the last count found, `None` while counting or after a failure.
    unused: Rc<Cell<Option<UnusedMedia>>>,
    /// A count, a clean-up or an undo runs.
    busy: Rc<Cell<bool>>,
    errors: Rc<ErrorSlot>,
}

impl Context {
    /// The button follows what is known and what runs. A button turned
    /// off with the focus on it hands the focus to its row, so it does not
    /// jump to the top of Preferences (and the alert, which gives the focus
    /// back to where it was when it closes, gives it back to the row).
    fn update(&self) {
        if let Some(button) = self.button.upgrade() {
            let sensitive = can_clean_up(self.unused.get(), self.busy.get());
            if !sensitive
                && button.has_focus()
                && let Some(row) = self.row.upgrade()
            {
                row.grab_focus();
            }
            button.set_sensitive(sensitive);
        }
    }

    fn set_subtitle(&self, text: &str) {
        if let Some(row) = self.row.upgrade() {
            row.set_subtitle(text);
        }
    }

    fn show_error(&self, what: &str, message: &str) {
        if let Some(dialog) = self.dialog.upgrade() {
            self.errors.show_in_preferences(&dialog, what, message);
        }
    }
}

/// Connects the row of `dialog`: counts the unused files now, and cleans
/// them up when its button is pressed and the question confirmed.
pub fn connect(
    dialog: &adw::PreferencesDialog,
    row: &adw::ActionRow,
    button: &gtk::Button,
    state: &AppState,
    errors: &Rc<ErrorSlot>,
) {
    let ctx = Context {
        state: state.clone(),
        dialog: dialog.downgrade(),
        row: row.downgrade(),
        button: button.downgrade(),
        unused: Rc::default(),
        busy: Rc::default(),
        errors: errors.clone(),
    };
    button.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| {
            // A second press while one runs does nothing.
            if !can_clean_up(ctx.unused.get(), ctx.busy.get()) {
                return;
            }
            glib::spawn_future_local(glib::clone!(
                #[strong]
                ctx,
                async move { ask_and_clean_up(&ctx).await }
            ));
        }
    ));
    // At once, so the row never shows an old count while the worker runs.
    row.set_subtitle(COUNTING_TEXT);
    button.set_sensitive(false);
    glib::spawn_future_local(async move { refresh(&ctx).await });
}

/// Counts the unused files on a worker and shows the result.
async fn refresh(ctx: &Context) {
    let Some(dialog) = ctx.dialog.upgrade() else {
        return;
    };
    ctx.busy.set(true);
    ctx.unused.set(None);
    ctx.update();
    ctx.set_subtitle(COUNTING_TEXT);
    let data_dir = ctx.state.data_dir().to_path_buf();
    let result = worker::run(&dialog, move || AppState::unused_media_on_worker(&data_dir)).await;
    drop(dialog);
    ctx.busy.set(false);
    match result {
        Ok(Ok(unused)) => {
            ctx.unused.set(Some(unused));
            ctx.set_subtitle(&unused_text(unused));
        }
        Ok(Err(err)) => {
            ctx.set_subtitle(COUNT_FAILED_TEXT);
            crate::ui::toast::log_failure("counting unused media", &err.to_string());
        }
        Err(_) => {
            ctx.set_subtitle(COUNT_FAILED_TEXT);
            crate::ui::toast::log_failure("counting unused media", "the worker stopped");
        }
    }
    ctx.update();
}

/// Asks "Clean Up Media?" and, if confirmed, moves the files on a worker,
/// then says so in a toast with Undo and counts again.
async fn ask_and_clean_up(ctx: &Context) {
    let (Some(dialog), Some(unused)) = (ctx.dialog.upgrade(), ctx.unused.get()) else {
        return;
    };
    let Ok(alert) = dialog::CLEAN_UP_MEDIA.load() else {
        return;
    };
    alert.set_body(&question_text(unused));
    ctx.busy.set(true);
    ctx.update();
    let answer = alert.choose_future(Some(&dialog)).await;
    if answer != "clean-up" {
        ctx.busy.set(false);
        ctx.update();
        return;
    }
    let data_dir = ctx.state.data_dir().to_path_buf();
    let result = worker::run(&dialog, move || {
        AppState::clean_up_media_on_worker(&data_dir)
    })
    .await;
    ctx.busy.set(false);
    match result {
        Ok(Ok(cleaned)) => {
            // Nothing moved: no toast; the count below says so.
            let Some(text) = moved_text(cleaned.files) else {
                refresh(ctx).await;
                return;
            };
            // Each toast's Undo puts back its own clean-up.
            let cleaned = Rc::new(RefCell::new(Some(cleaned)));
            let toast = undo_toast(
                &text,
                glib::clone!(
                    #[strong]
                    ctx,
                    move || {
                        let Some(cleaned) = cleaned.take() else {
                            return;
                        };
                        glib::spawn_future_local(glib::clone!(
                            #[strong]
                            ctx,
                            async move { undo(&ctx, cleaned).await }
                        ));
                    }
                ),
            );
            dialog.add_toast(toast);
        }
        Ok(Err(err)) => ctx.show_error(
            "cleaning up media",
            &format!("The unused files were not moved: {err}"),
        ),
        Err(_) => ctx.show_error(
            "cleaning up media",
            "Cleaning up stopped unexpectedly; the unused files are where they were or in the trash",
        ),
    }
    refresh(ctx).await;
}

/// Puts back `cleaned` on a worker, then counts again.
async fn undo(ctx: &Context, cleaned: CleanedUp) {
    let Some(dialog) = ctx.dialog.upgrade() else {
        return;
    };
    ctx.busy.set(true);
    ctx.update();
    let data_dir = ctx.state.data_dir().to_path_buf();
    let result = worker::run(&dialog, move || {
        AppState::undo_clean_up_on_worker(&data_dir, &cleaned)
    })
    .await;
    ctx.busy.set(false);
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => ctx.show_error(
            "undoing a media clean-up",
            &format!("The files were not all put back: {err}"),
        ),
        Err(_) => ctx.show_error(
            "undoing a media clean-up",
            "Putting the files back stopped unexpectedly",
        ),
    }
    refresh(ctx).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unused(files: usize, bytes: u64) -> UnusedMedia {
        UnusedMedia { files, bytes }
    }

    #[test]
    fn sizes_are_decimal_as_in_gnome() {
        // `g_format_size` puts a no-break space before the unit.
        let plain = |s: String| s.replace('\u{a0}', " ");
        assert_eq!(plain(size_text(4_300_000)), "4.3 MB");
        assert_eq!(plain(size_text(1_000)), "1.0 kB");
        assert_eq!(plain(size_text(999)), "999 bytes");
    }

    #[test]
    fn the_row_says_what_a_clean_up_would_free() {
        let plain = |s: String| s.replace('\u{a0}', " ");
        assert_eq!(unused_text(unused(0, 0)), "No unused files");
        assert_eq!(
            plain(unused_text(unused(1, 1_200))),
            "1 unused file, 1.2 kB"
        );
        assert_eq!(
            plain(unused_text(unused(12, 4_300_000))),
            "12 unused files, 4.3 MB"
        );
        assert_eq!(
            plain(unused_text(unused(1_234, 2_000_000_000))),
            "1,234 unused files, 2.0 GB"
        );
    }

    #[test]
    fn the_question_and_the_toast() {
        let plain = |s: String| s.replace('\u{a0}', " ");
        assert_eq!(
            plain(question_text(unused(12, 4_300_000))),
            "12 media files that no card uses (4.3 MB) will move to the trash in Rappel’s \
             data folder. After 30 days they are deleted, except those a backup still uses."
        );
        assert!(
            plain(question_text(unused(1, 500)))
                .starts_with("1 media file that no card uses (500 bytes)")
        );
        assert!(plain(question_text(unused(1_500, 500))).starts_with("1,500 media files"));
        assert_eq!(moved_text(12).as_deref(), Some("12 files trashed"));
        assert_eq!(moved_text(1).as_deref(), Some("1 file trashed"));
        assert_eq!(moved_text(0), None, "no toast for nothing");
        let most = moved_text(99_999).unwrap_or_default();
        assert!(most.chars().count() <= crate::ui::toast::TOAST_FITS_WITH_BUTTON);
    }

    /// The row counts on a worker when connected, then shows the count and
    /// lets the button start; a clean-up through the state moves the file,
    /// and Undo puts it back.
    #[test]
    fn the_row_counts_and_the_state_cleans_up() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let png = crate::testing::png_bytes(2, 2);
            AppState::add_media_bytes_on_worker(state.data_dir(), &png, crate::state::Accept::Any)
                .unwrap();
            let dialog = adw::PreferencesDialog::new();
            let row = adw::ActionRow::new();
            let button = gtk::Button::with_label("Clean Up…");
            button.set_sensitive(false);
            connect(&dialog, &row, &button, &state, &Rc::default());
            assert_eq!(row.subtitle().as_deref(), Some(COUNTING_TEXT));
            let counted = crate::testing::settle_until(
                || row.subtitle().as_deref() != Some(COUNTING_TEXT),
                std::time::Duration::from_secs(10),
            );
            assert!(counted, "the count arrives");
            let expected = unused_text(UnusedMedia {
                files: 1,
                bytes: png.len() as u64,
            });
            assert_eq!(row.subtitle().as_deref(), Some(expected.as_str()));
            assert!(button.is_sensitive());

            let cleaned = AppState::clean_up_media_on_worker(state.data_dir()).unwrap();
            assert_eq!(cleaned.files, 1);
            let media = state.media_folder().unwrap();
            assert_eq!(std::fs::read_dir(&media).unwrap().count(), 0);
            assert_eq!(
                AppState::unused_media_on_worker(state.data_dir()).unwrap(),
                UnusedMedia::default()
            );
            assert_eq!(
                AppState::undo_clean_up_on_worker(state.data_dir(), &cleaned).unwrap(),
                1
            );
            assert_eq!(std::fs::read_dir(&media).unwrap().count(), 1);
            let tidied = AppState::tidy_media_trash_on_worker(state.data_dir()).unwrap();
            assert_eq!(tidied.purged, 0);
        });
    }

    #[test]
    fn the_question_asks_with_cancel_as_the_default() {
        crate::testing::with_widgets(|| {
            let alert = dialog::CLEAN_UP_MEDIA.load().unwrap();
            assert_eq!(alert.default_response().as_deref(), Some("cancel"));
            assert_eq!(alert.close_response(), "cancel");
            assert_eq!(
                alert.response_appearance("clean-up"),
                adw::ResponseAppearance::Destructive
            );
            assert_eq!(alert.heading().as_deref(), Some("Clean Up Media?"));
        });
    }

    #[test]
    fn the_button_needs_files_and_nothing_running() {
        assert!(!can_clean_up(None, false), "still counting or failed");
        assert!(!can_clean_up(Some(unused(0, 0)), false));
        assert!(can_clean_up(Some(unused(3, 10)), false));
        assert!(!can_clean_up(Some(unused(3, 10)), true));
    }
}
