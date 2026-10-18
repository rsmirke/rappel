//! Objects from the `.ui` files compiled from `data/*.blp`, for dialogs
//! that are not composite templates (alert dialogs).

use adw::prelude::*;
use gtk::glib;

/// The gresource prefix of every `.ui` file (`data/resources.gresource.xml`).
const RESOURCE_PREFIX: &str = "/io/github/rsmirke/Rappel/";

/// The resource path of the bundled `file`, e.g. `"restore_dialog.ui"`.
pub fn resource_path(file: &str) -> String {
    format!("{RESOURCE_PREFIX}{file}")
}

/// A builder over the bundled `file`, e.g. `"restore_dialog.ui"`. The
/// app's widget types are registered first, since a file may name one
/// (`widgets::register_types`).
pub fn builder(file: &str) -> gtk::Builder {
    crate::widgets::register_types();
    gtk::Builder::from_resource(&resource_path(file))
}

/// Object `id` of `builder` (loaded from `file`), or `Err(id)` after
/// logging that it is missing or of another type: a bug in the Blueprint
/// file. The id lets a test say which object it was (tests have no log).
pub fn object<T: IsA<glib::Object>>(
    builder: &gtk::Builder,
    file: &str,
    id: &'static str,
) -> Result<T, &'static str> {
    builder.object::<T>(id).ok_or_else(|| {
        tracing::error!("{file} has no {id} object of the type the code expects");
        id
    })
}

/// An `AdwAlertDialog` that is the only object the code needs from its
/// `.ui` file. Named here, not at the call, so the test of every Blueprint
/// file (`ui_files.rs`) loads what the code loads.
pub struct Alert {
    file: &'static str,
    id: &'static str,
}

impl Alert {
    /// A new copy of the dialog, or `Err(id)` after logging that it is
    /// missing. Once shown, its default response has the keyboard focus
    /// ([`focus_default_when_shown`]).
    pub fn load(&self) -> Result<adw::AlertDialog, &'static str> {
        let alert: adw::AlertDialog = object(&builder(self.file), self.file, self.id)?;
        focus_default_when_shown(&alert);
        Ok(alert)
    }
}

/// The button of `alert`'s response `id`: the `GtkButton` among its
/// descendants whose label is that response's label.
fn response_button(alert: &adw::AlertDialog, id: &str) -> Option<gtk::Button> {
    let label = alert.response_label(id);
    let mut stack: Vec<gtk::Widget> = alert.first_child().into_iter().collect();
    while let Some(widget) = stack.pop() {
        if let Some(button) = widget.downcast_ref::<gtk::Button>()
            && button.label().is_some_and(|l| l == label)
        {
            return Some(button.clone());
        }
        stack.extend(widget.next_sibling());
        stack.extend(widget.first_child());
    }
    None
}

/// Puts the keyboard focus of `alert` on its default response each time it
/// is shown.
///
/// Observed with libadwaita 1.10 (`ui::dialog::focus_tests`, ADR 0057; its
/// C source is not on this system, only `/usr/include/libadwaita-1`, so
/// where it picks the focus could not be read): in a large window an alert
/// opens with its default response focused, but in a small one (360 × 640,
/// 640 × 360) with its last response, the destructive one, so Enter or
/// Space would answer it.
///
/// The focus is set in `map` (connected handlers run after the class
/// handler: `GtkWidget::map` is `G_SIGNAL_RUN_FIRST`), and again from an
/// idle at `G_PRIORITY_HIGH`, which runs before GDK's input events
/// (`GDK_PRIORITY_EVENTS` is `G_PRIORITY_DEFAULT`), so a key already
/// queued never reaches the destructive button, and after whatever the
/// dialog does when it is shown.
pub fn focus_default_when_shown(alert: &adw::AlertDialog) {
    alert.connect_map(|alert| {
        focus_default(alert);
        let alert = alert.downgrade();
        glib::idle_add_local_full(glib::Priority::HIGH, move || {
            if let Some(alert) = alert.upgrade() {
                focus_default(&alert);
            }
            glib::ControlFlow::Break
        });
    });
}

/// Focuses `alert`'s default response, warning if its button is not found.
fn focus_default(alert: &adw::AlertDialog) {
    let Some(id) = alert.default_response() else {
        return;
    };
    match response_button(alert, &id) {
        Some(button) => alert.set_focus(Some(&button)),
        None => {
            tracing::warn!(response = %id, "an alert's default response has no button to focus");
        }
    }
}

/// "Restore This Backup?" on the recovery page (its heading is set in code).
pub const RESTORE_BACKUP: Alert = Alert {
    file: "restore_dialog.ui",
    id: "restore_dialog",
};

/// "Start With an Empty Database?" on the recovery page.
pub const START_FRESH: Alert = Alert {
    file: "start_fresh_dialog.ui",
    id: "start_fresh_dialog",
};

/// "Discard Changes?" of the card editor.
pub const DISCARD_CHANGES: Alert = Alert {
    file: "discard_dialog.ui",
    id: "discard_dialog",
};

/// "Clean Up Media?" of Preferences (ADR 0057; its body is set in code).
pub const CLEAN_UP_MEDIA: Alert = Alert {
    file: "clean_up_media_dialog.ui",
    id: "clean_up_media_dialog",
};

/// "Delete Card?" of the review page.
pub const DELETE_CARD: Alert = Alert {
    file: "delete_card_dialog.ui",
    id: "delete_card_dialog",
};

#[cfg(test)]
mod focus_tests {
    use std::time::Duration;

    use super::*;

    /// Every alert loaded through [`Alert`] has its default response (Cancel,
    /// or Keep Editing) focused once shown, in a large window and in small
    /// ones, where libadwaita would focus the destructive response.
    #[test]
    fn the_default_response_has_the_focus_at_every_size() {
        crate::testing::with_widgets(|| {
            for (w, h) in [(1000, 720), (360, 640), (640, 360)] {
                let window = adw::Window::new();
                window.set_default_size(w, h);
                window.present();
                crate::testing::settle_until(|| window.is_mapped(), Duration::from_secs(5));
                for alert in [
                    CLEAN_UP_MEDIA,
                    DELETE_CARD,
                    START_FRESH,
                    DISCARD_CHANGES,
                    RESTORE_BACKUP,
                ] {
                    let dialog = alert.load().unwrap();
                    let default = dialog.default_response().unwrap();
                    dialog.present(Some(&window));
                    let focused = || {
                        dialog
                            .focus()
                            .and_downcast::<gtk::Button>()
                            .and_then(|b| b.label())
                            .is_some_and(|l| l == dialog.response_label(&default))
                    };
                    assert!(
                        crate::testing::settle_until(focused, Duration::from_secs(5)),
                        "{} at {w} × {h}",
                        alert.id
                    );
                    // And it stays there.
                    crate::testing::settle_until(|| false, Duration::from_millis(300));
                    assert!(focused(), "{} at {w} × {h}, later", alert.id);
                    dialog.force_close();
                }
                window.close();
            }
        });
    }
}
