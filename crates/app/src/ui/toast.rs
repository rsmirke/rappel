//! Toasts: persistent ones for errors and warnings, plain notices, and
//! Undo toasts.
//!
//! A toast's title is one line, cut with "…" where it does not fit
//! (libadwaita's `AdwToast` title label ellipsizes). At 360 px a title shows
//! about [`TOAST_FITS`] characters beside the close button, and about
//! [`TOAST_FITS_WITH_BUTTON`] beside a button too (measured on the
//! harness's screenshots, ADR 0058). So a longer text is not put in the
//! title: the toast shows its first clause, or "Something went wrong", with
//! a **Details** button that opens an alert with the whole text
//! ([`toast_text`], pure). The HIG asks for short toasts; the details stay
//! readable on a phone.

use std::cell::RefCell;

use adw::prelude::*;
use rappel_core::storage::OpenWarning;

use crate::window::RappelWindow;

/// The most characters a toast title shows whole at 360 px beside its
/// close button (ADR 0058).
pub const TOAST_FITS: usize = 34;

/// The most characters a toast title shows whole at 360 px beside a button
/// ("Undo", "Details") and the close button (ADR 0058).
pub const TOAST_FITS_WITH_BUTTON: usize = 24;

/// The title of a toast whose text has no first clause short enough.
pub const FALLBACK_TITLE: &str = "Something went wrong";

/// What a toast shows for a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastText {
    /// The text fits: it is the title.
    Whole(String),
    /// The text is too long: a short title, and the whole text behind a
    /// Details button.
    Detailed { title: String, details: String },
}

/// The clause separators a title may end before, in the order tried.
const CLAUSE_ENDS: [&str; 4] = [". ", ": ", "; ", ", "];

/// What a toast shows for `text`: the whole text if it has at most
/// [`TOAST_FITS`] characters; else its first clause (up to the first ". ",
/// ": ", "; " or ", ", whichever comes first) if that has at most
/// [`TOAST_FITS_WITH_BUTTON`], else [`FALLBACK_TITLE`], with the whole text
/// as the details (without a first sentence, which the details' heading
/// already says).
pub fn toast_text(text: &str) -> ToastText {
    if text.chars().count() <= TOAST_FITS {
        return ToastText::Whole(text.to_owned());
    }
    let lead = CLAUSE_ENDS
        .iter()
        .filter_map(|end| text.find(end).map(|at| (at, *end)))
        .min()
        .filter(|(at, _)| {
            let lead = &text[..*at];
            !lead.trim().is_empty() && lead.chars().count() <= TOAST_FITS_WITH_BUTTON
        });
    match lead {
        // A first sentence is the alert's heading: its body is the rest.
        Some((at, ". ")) => ToastText::Detailed {
            title: text[..at].to_owned(),
            details: text[at + 2..].to_owned(),
        },
        Some((at, _)) => ToastText::Detailed {
            title: text[..at].to_owned(),
            details: text.to_owned(),
        },
        None => ToastText::Detailed {
            title: FALLBACK_TITLE.to_owned(),
            details: text.to_owned(),
        },
    }
}

/// `text` if it fits a toast's title whole, else `title` (at most
/// [`TOAST_FITS_WITH_BUTTON`] characters, no ". " in it) as its first
/// sentence, which [`toast_text`] then shows as the title with `text` in
/// the details.
pub fn titled(title: &str, text: String) -> String {
    if text.chars().count() <= TOAST_FITS {
        text
    } else {
        format!("{title}. {text}")
    }
}

/// A toast showing `text` as [`toast_text`] decides, with `timeout`
/// seconds (0: until dismissed). Plain text: paths and errors may hold `<`
/// or `&`.
fn fitted_toast(text: &str, timeout: u32) -> adw::Toast {
    match toast_text(text) {
        ToastText::Whole(title) => adw::Toast::builder()
            .title(title)
            .use_markup(false)
            .timeout(timeout)
            .build(),
        ToastText::Detailed { title, details } => {
            let toast = adw::Toast::builder()
                .title(title.as_str())
                .use_markup(false)
                .timeout(timeout)
                .button_label("_Details")
                .build();
            toast.connect_button_clicked(move |_| show_details(&title, &details));
            toast
        }
    }
}

/// An alert with a toast's whole text, over the application's active
/// window (where the toast was: the window's overlay, or a dialog's inside
/// it, which the alert then stacks over).
fn show_details(title: &str, details: &str) {
    let parent = gtk::gio::Application::default()
        .and_downcast::<gtk::Application>()
        .and_then(|app| app.active_window());
    let alert = adw::AlertDialog::builder()
        .heading(title)
        .body(details)
        .body_use_markup(false)
        .build();
    alert.add_response("close", "_Close");
    alert.set_default_response(Some("close"));
    alert.set_close_response("close");
    alert.present(parent.as_ref());
}

/// A toast with plain text (paths and errors may contain `<` or `&`) that
/// stays until dismissed, so the user cannot miss it. A long text gets a
/// short title and a Details button ([`toast_text`]).
pub fn persistent_toast(text: &str) -> adw::Toast {
    fitted_toast(text, 0)
}

/// A toast with plain text that goes away by itself, for a notice such as
/// "Card added". A long text gets a short title and a Details button
/// ([`toast_text`]), and then stays until dismissed, so the button can be
/// reached.
pub fn info_toast(text: &str) -> adw::Toast {
    match toast_text(text) {
        ToastText::Whole(_) => fitted_toast(text, 5),
        ToastText::Detailed { .. } => fitted_toast(text, 0),
    }
}

/// Shows `toast` in the toast overlay that contains `widget` (`widget`
/// itself if it is one: `gtk_widget_get_ancestor` counts the widget as its
/// own ancestor, gtkwidget.c), else in the window's own overlay (the window
/// is not inside its overlay).
pub fn show_toast_from(widget: &impl IsA<gtk::Widget>, toast: adw::Toast) {
    if let Some(overlay) = widget
        .ancestor(adw::ToastOverlay::static_type())
        .and_downcast::<adw::ToastOverlay>()
    {
        overlay.add_toast(toast);
    } else if let Some(window) = widget.root().and_downcast::<RappelWindow>() {
        window.add_toast(toast);
    } else {
        tracing::error!("widget is not inside a toast overlay");
    }
}

/// Logs that `what` failed, e.g. "a deck operation". `message` may hold
/// user content (names, card text), so it is only logged at debug level
/// (SPEC §2).
pub fn log_failure(what: &str, message: &str) {
    tracing::warn!("{what} failed");
    tracing::debug!("{message}");
}

/// Logs the failure (see [`log_failure`]) and shows `message` in a
/// persistent toast near `widget`.
pub fn show_error(widget: &impl IsA<gtk::Widget>, what: &str, message: &str) {
    log_failure(what, message);
    show_toast_from(widget, persistent_toast(message));
}

/// The error toast a page shows last. A new error dismisses it first, so a
/// toast that stays until dismissed does not hold the new one back.
#[derive(Debug, Default)]
pub struct ErrorSlot(RefCell<Option<(adw::Toast, String)>>);

impl ErrorSlot {
    /// Like [`show_error`], replacing this slot's previous error toast.
    pub fn show(&self, widget: &impl IsA<gtk::Widget>, what: &str, message: &str) {
        log_failure(what, message);
        let toast = persistent_toast(message);
        if let Some((old, _)) = self.0.replace(Some((toast.clone(), message.to_owned()))) {
            old.dismiss();
        }
        show_toast_from(widget, toast);
    }

    /// The whole text of the error toast last shown through this slot, if
    /// any (its title may be only the first clause, [`toast_text`]).
    #[cfg(test)]
    pub fn shown_text(&self) -> Option<String> {
        self.0.borrow().as_ref().map(|(_, text)| text.clone())
    }

    /// Whether no error toast was shown through this slot since the last
    /// [`Self::clear`].
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_none()
    }

    /// Dismisses this slot's error toast, if one is still shown: what it
    /// reported has since worked (e.g. the decks could be read again).
    pub fn clear(&self) {
        if let Some((old, _)) = self.0.take() {
            old.dismiss();
        }
    }

    /// Like [`Self::show`], in a preferences dialog's own toast overlay,
    /// which covers the window's (`adw_preferences_dialog_add_toast`).
    pub fn show_in_preferences(&self, dialog: &adw::PreferencesDialog, what: &str, message: &str) {
        log_failure(what, message);
        let toast = persistent_toast(message);
        if let Some((old, _)) = self.0.replace(Some((toast.clone(), message.to_owned()))) {
            old.dismiss();
        }
        dialog.add_toast(toast);
    }
}

/// A toast offering Undo after something was deleted at once.
pub fn undo_toast(title: &str, on_undo: impl Fn() + 'static) -> adw::Toast {
    let toast = adw::Toast::builder()
        .title(title)
        .use_markup(false)
        .button_label("_Undo")
        // Never queued behind a toast that stays until dismissed.
        .priority(adw::ToastPriority::High)
        .build();
    toast.connect_button_clicked(move |_| on_undo());
    toast
}

/// The toast text for a warning from opening the database.
pub fn warning_message(warning: &OpenWarning) -> String {
    match warning {
        OpenWarning::DailyBackupFailed(err) => {
            format!("Daily backup failed. Today’s backup could not be made: {err}")
        }
        OpenWarning::PruneFailed(err) => {
            format!("Backups not tidied. Old backups could not be removed: {err}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_short_text_is_the_whole_title() {
        assert_eq!(
            toast_text("Card deleted"),
            ToastText::Whole("Card deleted".into())
        );
        let edge = "x".repeat(TOAST_FITS);
        assert_eq!(toast_text(&edge), ToastText::Whole(edge.clone()));
    }

    #[test]
    fn a_long_text_shows_its_first_clause_and_the_rest_in_details() {
        let text =
            "Backup restored. The previous database was kept as /data/rappel.sqlite3.replaced-1";
        assert_eq!(
            toast_text(text),
            ToastText::Detailed {
                title: "Backup restored".into(),
                details: "The previous database was kept as /data/rappel.sqlite3.replaced-1".into(),
            }
        );
        // The earliest separator wins, whichever kind.
        let text = "The export failed: disk full, and more words to make it long";
        assert!(matches!(
            toast_text(text),
            ToastText::Detailed { title, details } if title == "The export failed" && details == text
        ));
    }

    #[test]
    fn a_long_text_without_a_short_clause_gets_the_fallback_title() {
        let text = "The card’s information could not be read: database is locked";
        assert!(matches!(
            toast_text(text),
            ToastText::Detailed { title, details } if title == FALLBACK_TITLE && details == text
        ));
        // A path has no clause at all; counted in characters, not bytes.
        let text = "é".repeat(TOAST_FITS + 1);
        assert!(
            matches!(toast_text(&text), ToastText::Detailed { title, .. } if title == FALLBACK_TITLE)
        );
        // An empty first clause is no title.
        let text = format!(". {}", "word ".repeat(10));
        assert!(
            matches!(toast_text(&text), ToastText::Detailed { title, .. } if title == FALLBACK_TITLE)
        );
    }

    #[test]
    fn a_title_is_added_only_to_a_text_that_does_not_fit() {
        assert_eq!(
            titled("Export done", "Exported to “a.rappel”".into()),
            "Exported to “a.rappel”"
        );
        let long = "Exported to “a very long file name that goes on.rappel”".to_owned();
        let text = titled("Export done", long.clone());
        assert_eq!(text, format!("Export done. {long}"));
        assert!(
            matches!(toast_text(&text), ToastText::Detailed { title, .. } if title == "Export done")
        );
    }

    #[test]
    fn titles_fit_their_room() {
        assert!(FALLBACK_TITLE.chars().count() <= TOAST_FITS_WITH_BUTTON);
    }

    #[test]
    fn warnings_name_what_failed_and_why() {
        let err = || rappel_core::Error::NotADirectory(PathBuf::from("/data/backups"));
        let text = warning_message(&OpenWarning::DailyBackupFailed(err()));
        assert!(text.starts_with("Daily backup failed. Today’s backup could not be made: "));
        assert!(text.contains("/data/backups"));
        let text = warning_message(&OpenWarning::PruneFailed(err()));
        assert!(text.starts_with("Backups not tidied. Old backups could not be removed: "));
        for warning in [
            OpenWarning::DailyBackupFailed(err()),
            OpenWarning::PruneFailed(err()),
        ] {
            assert!(
                matches!(toast_text(&warning_message(&warning)), ToastText::Detailed { title, .. } if title != FALLBACK_TITLE)
            );
        }
    }
}
