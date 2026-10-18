//! One backup on the recovery page: when it was made, its size and a
//! Restore button. Layout in `data/backup_row.blp`; the page connects the
//! button.

use crate::i18n::{fill, gettext};
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

mod imp {
    use adw::subclass::prelude::*;
    use gtk::glib;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/backup_row.ui")]
    pub struct BackupRow {
        #[template_child]
        pub restore_button: TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BackupRow {
        const NAME: &'static str = "RappelBackupRow";
        type Type = super::BackupRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for BackupRow {}
    impl WidgetImpl for BackupRow {}
    impl ListBoxRowImpl for BackupRow {}
    impl PreferencesRowImpl for BackupRow {}
    impl ActionRowImpl for BackupRow {}
}

glib::wrapper! {
    pub struct BackupRow(ObjectSubclass<imp::BackupRow>)
        @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl BackupRow {
    /// A row for the backup made at `when` (as shown), `size` bytes large.
    pub fn new(when: &str, size: u64) -> Self {
        let row: Self = glib::Object::new();
        row.set_title(when);
        row.set_subtitle(&glib::format_size(size));
        // Every row has a "Restore" button; say which backup it restores.
        row.imp()
            .restore_button
            .update_property(&[gtk::accessible::Property::Label(&restore_label(when))]);
        row
    }

    /// The row's Restore button.
    pub fn restore_button(&self) -> gtk::Button {
        self.imp().restore_button.get()
    }
}

/// The accessible label of the Restore button of the backup made at `when`.
fn restore_label(when: &str) -> String {
    // Translators: a Restore button's name for screen readers; {when} is
    // the date and time the backup was made.
    fill(&gettext("Restore backup from {when}"), &[("when", when)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restore_button_names_its_backup() {
        assert_eq!(
            restore_label("16 October 2026, 09:30"),
            "Restore backup from 16 October 2026, 09:30"
        );
    }
}
