//! Objects from the `.ui` files compiled from `data/*.blp`, for dialogs
//! that are not composite templates (alert dialogs).

use gtk::glib;
use gtk::prelude::*;

/// The gresource prefix of every `.ui` file (`data/resources.gresource.xml`).
const RESOURCE_PREFIX: &str = "/io/github/rsmirke/Rappel/";

/// The resource path of the bundled `file`, e.g. `"restore_dialog.ui"`.
pub fn resource_path(file: &str) -> String {
    format!("{RESOURCE_PREFIX}{file}")
}

/// A builder over the bundled `file`, e.g. `"restore_dialog.ui"`.
pub fn builder(file: &str) -> gtk::Builder {
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
    /// missing.
    pub fn load(&self) -> Result<adw::AlertDialog, &'static str> {
        object(&builder(self.file), self.file, self.id)
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

/// "Delete Card?" of the review page.
pub const DELETE_CARD: Alert = Alert {
    file: "delete_card_dialog.ui",
    id: "delete_card_dialog",
};
