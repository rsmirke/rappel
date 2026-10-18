//! Toasts: persistent ones for errors and warnings, plain notices, and
//! Undo toasts.

use std::cell::RefCell;

use adw::prelude::*;
use rappel_core::storage::OpenWarning;

use crate::window::RappelWindow;

/// A toast with plain text (paths and errors may contain `<` or `&`) that
/// stays until dismissed, so the user cannot miss it.
pub fn persistent_toast(text: &str) -> adw::Toast {
    adw::Toast::builder()
        .title(text)
        .use_markup(false)
        .timeout(0)
        .build()
}

/// A toast with plain text that goes away by itself, for a notice such as
/// "Card added".
pub fn info_toast(text: &str) -> adw::Toast {
    adw::Toast::builder().title(text).use_markup(false).build()
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
pub struct ErrorSlot(RefCell<Option<adw::Toast>>);

impl ErrorSlot {
    /// Like [`show_error`], replacing this slot's previous error toast.
    pub fn show(&self, widget: &impl IsA<gtk::Widget>, what: &str, message: &str) {
        log_failure(what, message);
        let toast = persistent_toast(message);
        if let Some(old) = self.0.replace(Some(toast.clone())) {
            old.dismiss();
        }
        show_toast_from(widget, toast);
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
        if let Some(old) = self.0.take() {
            old.dismiss();
        }
    }

    /// Like [`Self::show`], in a preferences dialog's own toast overlay,
    /// which covers the window's (`adw_preferences_dialog_add_toast`).
    pub fn show_in_preferences(&self, dialog: &adw::PreferencesDialog, what: &str, message: &str) {
        log_failure(what, message);
        let toast = persistent_toast(message);
        if let Some(old) = self.0.replace(Some(toast.clone())) {
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
        OpenWarning::DailyBackupFailed(err) => format!("Today’s backup could not be made: {err}"),
        OpenWarning::PruneFailed(err) => format!("Old backups could not be removed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn warnings_name_what_failed_and_why() {
        let err = || rappel_core::Error::NotADirectory(PathBuf::from("/data/backups"));
        let text = warning_message(&OpenWarning::DailyBackupFailed(err()));
        assert!(text.starts_with("Today’s backup could not be made: "));
        assert!(text.contains("/data/backups"));
        let text = warning_message(&OpenWarning::PruneFailed(err()));
        assert!(text.starts_with("Old backups could not be removed: "));
    }
}
