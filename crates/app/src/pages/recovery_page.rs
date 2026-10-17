//! The page shown when the database must be replaced by a backup: why,
//! the backups newest first, and a confirmed "Restore" for each. With no
//! backup to restore, a confirmed "Start Fresh" moves the database aside
//! and starts an empty one (ADR 0016). While an interrupted restore's
//! checked copy waits, "Finish Restore" installs it (ADR 0026). Layout in `data/recovery_page.blp`,
//! `data/restore_dialog.blp` and `data/start_fresh_dialog.blp`; each
//! backup is a [`BackupRow`].

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use rappel_core::storage::{BackupInfo, Restored};

use crate::startup::{RecoveryReason, data_folder_line};
use crate::state::AppState;
use crate::ui::callback::Callback;
use crate::ui::toast::{persistent_toast, show_toast_from};
use crate::ui::{dialog, worker};
use crate::widgets::BackupRow;

/// How the page got an open database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovered {
    /// A backup was restored.
    Backup,
    /// An interrupted restore's waiting copy was installed.
    Finished,
    /// The old database was moved aside and an empty one started.
    Fresh,
}

/// Called with the data directory, the new store and how it was made.
type OnRestored = dyn Fn(PathBuf, Restored, Recovered);

mod imp {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use super::OnRestored;
    use crate::startup::RecoveryReason;
    use crate::ui::callback::Callback;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/recovery_page.ui")]
    pub struct RecoveryPage {
        #[template_child]
        pub status: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub backup_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub finish_restore_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub start_fresh_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub path_label: TemplateChild<gtk::Label>,
        pub reason: Cell<Option<RecoveryReason>>,
        pub data_dir: RefCell<Option<PathBuf>>,
        pub on_restored: RefCell<Option<Callback<OnRestored>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecoveryPage {
        const NAME: &'static str = "RappelRecoveryPage";
        type Type = super::RecoveryPage;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RecoveryPage {
        fn constructed(&self) {
            self.parent_constructed();
            // The page's only focusable widget when it offers nothing to do.
            crate::ui::focus::unselect_on_first_focus(&self.path_label);
            // No confirmation: nothing is deleted, the old files are moved
            // aside, and this restore was already chosen.
            self.finish_restore_button.connect_clicked(glib::clone!(
                #[weak(rename_to = page)]
                self.obj(),
                move |_| {
                    glib::spawn_future_local(async move { page.finish_restore().await });
                }
            ));
            self.start_fresh_button.connect_clicked(glib::clone!(
                #[weak(rename_to = page)]
                self.obj(),
                move |_| page.confirm_start_fresh()
            ));
        }
    }
    impl WidgetImpl for RecoveryPage {}
    impl BinImpl for RecoveryPage {}
}

glib::wrapper! {
    pub struct RecoveryPage(ObjectSubclass<imp::RecoveryPage>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl RecoveryPage {
    /// Explains `reason`, lists the backups in `data_dir`, and calls
    /// `on_restored` once a restore or a fresh start succeeds.
    pub fn show(
        &self,
        reason: RecoveryReason,
        data_dir: PathBuf,
        on_restored: impl Fn(PathBuf, Restored, Recovered) + 'static,
    ) {
        let imp = self.imp();
        imp.reason.set(Some(reason));
        imp.path_label.set_label(&data_folder_line(&data_dir));
        imp.data_dir.replace(Some(data_dir));
        imp.on_restored
            .replace(Some(Callback::new(Rc::new(on_restored))));
        self.reload();
    }

    /// Rebuilds the backup list and the explanation under the title.
    fn reload(&self) {
        let imp = self.imp();
        imp.backup_list.remove_all();
        imp.backup_list.set_sensitive(true);
        imp.start_fresh_button.set_visible(false);
        imp.start_fresh_button.set_sensitive(true);
        imp.finish_restore_button
            .set_visible(offers_finish_restore(imp.reason.get()));
        imp.finish_restore_button.set_sensitive(true);
        let Some(dir) = imp.data_dir.borrow().clone() else {
            return;
        };
        // The reason can change after a failed restore or start; the title
        // follows it.
        if let Some(reason) = imp.reason.get() {
            imp.status.set_title(reason.title());
        }
        let mut description = imp
            .reason
            .get()
            .map(RecoveryReason::description)
            .unwrap_or_default()
            .to_owned();
        match AppState::list_backups(&dir) {
            Ok(backups) if backups.is_empty() => {
                imp.start_fresh_button
                    .set_visible(offers_start_fresh(imp.reason.get()));
                description.push_str("\n\n");
                description.push_str(no_backups_text(imp.reason.get()));
            }
            Ok(backups) => {
                for backup in backups {
                    imp.backup_list.append(&self.backup_row(backup));
                }
            }
            Err(err) => {
                tracing::error!("cannot list backups: {err}");
                description.push_str("\n\nThe backups could not be listed: ");
                description.push_str(&err.to_string());
            }
        }
        imp.backup_list
            .set_visible(imp.backup_list.first_child().is_some());
        // The description is Pango markup; the error text and paths are not.
        imp.status
            .set_description(Some(&glib::markup_escape_text(&description)));
    }

    fn backup_row(&self, backup: BackupInfo) -> BackupRow {
        let when = crate::ui::time::date_time_seconds(backup.created_ms());
        let row = BackupRow::new(&when, backup.size_bytes());
        row.restore_button().connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |button| page.confirm_restore(button, backup.clone(), &when)
        ));
        row
    }

    fn confirm_restore(&self, button: &gtk::Button, backup: BackupInfo, when: &str) {
        let Ok(dialog) = dialog::RESTORE_BACKUP.load() else {
            return;
        };
        let (heading, body) = restore_question(when);
        dialog.set_heading(Some(heading));
        dialog.set_body(&body);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            button,
            async move {
                if dialog.choose_future(Some(&page)).await == "restore" {
                    page.restore(&button, backup).await;
                }
            }
        ));
    }

    /// Restores on a worker thread: it copies and checks a whole database.
    ///
    /// The page logs its errors itself, in full, at warning and error level
    /// instead of through `ui::toast::show_error` (which keeps the details
    /// at debug level): it runs before any database is open, and the paths
    /// and SQLite messages are what make the data folder fixable. These
    /// errors never hold card or deck text: a restore and Start Fresh only
    /// copy, check and move files (ADR 0025, row 13).
    async fn restore(&self, button: &gtk::Button, backup: BackupInfo) {
        let imp = self.imp();
        let Some(dir) = imp.data_dir.borrow().clone() else {
            return;
        };
        self.set_busy();
        button.set_child(Some(&adw::Spinner::new()));

        // The app is held while it runs, so closing the window mid-restore
        // lets the restore finish instead of cutting it off.
        let worker_dir = dir.clone();
        let result = worker::run(self, move || {
            AppState::restore_unopened(&worker_dir, &backup)
        })
        .await;

        let message = match result {
            Ok(Ok(restored)) => {
                // Clone the callback out so no borrow is held while it runs.
                let handler = imp.on_restored.borrow().as_ref().map(Callback::get);
                if let Some(handler) = handler {
                    handler(dir, restored, Recovered::Backup);
                }
                return;
            }
            Ok(Err(rappel_core::Error::RestoreRefused { source, .. })) => {
                tracing::warn!("backup refused: {source}");
                format!("This backup cannot be used: {source}")
            }
            Ok(Err(err)) => {
                tracing::error!("restore failed: {err}");
                // The old database may already be moved aside; say what is
                // now true on disk. Restoring again finishes it (ADR 0007).
                imp.reason.set(Some(RecoveryReason::InterruptedRestore));
                format!("Restore failed: {err}")
            }
            Err(_) => {
                tracing::error!("restore worker panicked");
                imp.reason.set(Some(RecoveryReason::InterruptedRestore));
                "Restore stopped unexpectedly".to_owned()
            }
        };
        self.reload();
        show_toast_from(self, persistent_toast(&message));
    }
}

impl RecoveryPage {
    /// Turns off every action while a job runs. Restore, Finish Restore and
    /// Start Fresh work on the same files, so two must never run at once.
    /// `reload` turns them back on.
    fn set_busy(&self) {
        let imp = self.imp();
        imp.backup_list.set_sensitive(false);
        imp.finish_restore_button.set_sensitive(false);
        imp.start_fresh_button.set_sensitive(false);
    }

    /// Installs the waiting copy on a worker thread, like a restore, and
    /// logs as `restore` does.
    async fn finish_restore(&self) {
        let imp = self.imp();
        let Some(dir) = imp.data_dir.borrow().clone() else {
            return;
        };
        self.set_busy();

        let worker_dir = dir.clone();
        let result =
            worker::run(self, move || AppState::finish_restore_unopened(&worker_dir)).await;

        let message = match result {
            Ok(Ok(restored)) => {
                let handler = imp.on_restored.borrow().as_ref().map(Callback::get);
                if let Some(handler) = handler {
                    handler(dir, restored, Recovered::Finished);
                }
                return;
            }
            Ok(Err(err)) => {
                tracing::error!("finishing the restore failed: {err}");
                finish_failure_text(&err)
            }
            Err(_) => {
                tracing::error!("finish restore worker panicked");
                "Finishing the restore stopped unexpectedly".to_owned()
            }
        };
        self.reload();
        show_toast_from(self, persistent_toast(&message));
    }

    fn confirm_start_fresh(&self) {
        let Ok(dialog) = dialog::START_FRESH.load() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                if dialog.choose_future(Some(&page)).await == "start" {
                    page.start_fresh().await;
                }
            }
        ));
    }

    /// Moves the database aside and starts an empty one, on a worker
    /// thread like a restore.
    async fn start_fresh(&self) {
        let imp = self.imp();
        let Some(dir) = imp.data_dir.borrow().clone() else {
            return;
        };
        self.set_busy();

        // The app is held while it runs, as for a restore.
        let worker_dir = dir.clone();
        let result = worker::run(self, move || AppState::start_fresh_unopened(&worker_dir)).await;

        let message = match result {
            Ok(Ok(fresh)) => {
                let handler = imp.on_restored.borrow().as_ref().map(Callback::get);
                if let Some(handler) = handler {
                    handler(dir, fresh, Recovered::Fresh);
                }
                return;
            }
            Ok(Err(rappel_core::Error::InterruptedRestore(_))) => {
                tracing::warn!("start fresh refused: a validated restore copy is waiting");
                imp.reason.set(Some(RecoveryReason::InterruptedRestore));
                "A restore did not finish. Its copy is kept; use Finish Restore to continue."
                    .to_owned()
            }
            Ok(Err(rappel_core::Error::BackupsAvailable(_))) => {
                // A backup appeared since the list was shown; reloading
                // lists it.
                tracing::warn!("start fresh refused: backups exist");
                "Backups were found. Restore one of them instead.".to_owned()
            }
            Ok(Err(err)) => {
                tracing::error!("start fresh failed: {err}");
                format!("Starting fresh failed: {err}")
            }
            Err(_) => {
                tracing::error!("start fresh worker panicked");
                "Starting fresh stopped unexpectedly".to_owned()
            }
        };
        self.reload();
        show_toast_from(self, persistent_toast(&message));
    }
}

/// The heading and body of the question before a restore of the backup
/// made at `when` (as the row shows it).
fn restore_question(when: &str) -> (&'static str, String) {
    (
        "Restore This Backup?",
        format!(
            "The backup from {when} replaces the current database, so changes made after it \
             will not be in the restored one. The current database is kept in the data \
             folder, not deleted."
        ),
    )
}

/// What the page adds to its explanation when there is no backup. Where
/// the user may be tempted to fix the folder by hand, it warns against
/// copying database files into it: a copy over `rappel.sqlite3` with an
/// old `-wal` beside it replays that WAL into the copy (ADR 0007, 0016).
fn no_backups_text(reason: Option<RecoveryReason>) -> &'static str {
    if offers_start_fresh(reason) {
        "No backups were found, so there is nothing to restore. You can start with an empty \
         database instead: the current files stay in the data folder, nothing is deleted. \
         Don’t copy database files into the data folder yourself: that can damage them."
    } else if offers_finish_restore(reason) {
        "No backups were found, but the waiting copy can be installed."
    } else {
        "No backups were found, so there is nothing to restore. Leave the files in the data \
         folder as they are, so they can still be recovered. Don’t copy database files into \
         the data folder yourself: that can damage them."
    }
}

/// Whether "Finish Restore" is offered: exactly while an interrupted
/// restore's checked copy waits, with or without backups (ADR 0026).
fn offers_finish_restore(reason: Option<RecoveryReason>) -> bool {
    reason == Some(RecoveryReason::InterruptedRestore)
}

/// What the toast says when finishing the restore failed.
fn finish_failure_text(err: &rappel_core::Error) -> String {
    match err {
        rappel_core::Error::NoRestoreWaiting => {
            "The waiting copy is gone. Restart Rappel to check again.".to_owned()
        }
        rappel_core::Error::RestoreRefused { source, .. } => {
            format!("The waiting copy cannot be used: {source}. It is kept in the data folder.")
        }
        err => format!("Finishing the restore failed: {err}"),
    }
}

/// Whether the page offers Start Fresh when there is no backup to restore:
/// never while an interrupted restore's validated copy waits (that copy is
/// the user's data; ADR 0016). With backups it is never offered, and core
/// refuses it too.
fn offers_start_fresh(reason: Option<RecoveryReason>) -> bool {
    reason.is_some_and(|r| r != RecoveryReason::InterruptedRestore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_fresh_is_offered_for_every_reason_with_a_file_to_move() {
        for reason in [
            RecoveryReason::Damaged,
            RecoveryReason::NotRappel,
            RecoveryReason::MissingFile,
        ] {
            assert!(offers_start_fresh(Some(reason)), "{reason:?}");
        }
    }

    #[test]
    fn finish_restore_is_offered_only_for_an_interrupted_restore() {
        assert!(offers_finish_restore(Some(
            RecoveryReason::InterruptedRestore
        )));
        for reason in [
            RecoveryReason::Damaged,
            RecoveryReason::NotRappel,
            RecoveryReason::MissingFile,
        ] {
            assert!(!offers_finish_restore(Some(reason)), "{reason:?}");
        }
        assert!(!offers_finish_restore(None));
    }

    #[test]
    fn finish_and_start_fresh_are_never_offered_together() {
        for reason in [
            None,
            Some(RecoveryReason::Damaged),
            Some(RecoveryReason::NotRappel),
            Some(RecoveryReason::MissingFile),
            Some(RecoveryReason::InterruptedRestore),
        ] {
            assert!(
                !(offers_finish_restore(reason) && offers_start_fresh(reason)),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn the_restore_question_names_the_backup() {
        let (heading, body) = restore_question("16/10/26 11:00:00");
        assert_eq!(heading, "Restore This Backup?");
        assert!(body.starts_with("The backup from 16/10/26 11:00:00 replaces"));
        assert!(body.ends_with("kept in the data folder, not deleted."));
    }

    #[test]
    fn without_backups_the_page_says_what_is_left_to_do() {
        assert!(no_backups_text(Some(RecoveryReason::Damaged)).contains("start with an empty"));
        assert!(no_backups_text(Some(RecoveryReason::InterruptedRestore)).contains("waiting copy"));
        assert!(no_backups_text(None).contains("Leave the files"));
        for reason in [None, Some(RecoveryReason::NotRappel)] {
            assert!(no_backups_text(reason).starts_with("No backups were found"));
        }
        // The one on-screen warning about a real way to lose data.
        for reason in [
            None,
            Some(RecoveryReason::Damaged),
            Some(RecoveryReason::MissingFile),
        ] {
            assert!(
                no_backups_text(reason).ends_with(
                    "Don’t copy database files into the data folder yourself: that can damage them."
                ),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn a_failed_finish_says_what_is_true_on_disk() {
        use rappel_core::Error;
        assert_eq!(
            finish_failure_text(&Error::NoRestoreWaiting),
            "The waiting copy is gone. Restart Rappel to check again."
        );
        let refused = Error::RestoreRefused {
            path: PathBuf::from("/data/rappel.sqlite3.restore.partial"),
            source: Box::new(Error::NotARappelDatabase),
        };
        assert!(finish_failure_text(&refused).ends_with("It is kept in the data folder."));
        assert!(finish_failure_text(&refused).starts_with("The waiting copy cannot be used:"));
    }

    #[test]
    fn start_fresh_never_hides_a_waiting_restore_copy() {
        assert!(!offers_start_fresh(Some(
            RecoveryReason::InterruptedRestore
        )));
        assert!(!offers_start_fresh(None));
    }
}
