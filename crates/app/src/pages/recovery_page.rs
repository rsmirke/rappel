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

/// Called when the waiting copy turns out to come from a newer Rappel,
/// with its version and the latest this build supports.
type OnTooNew = dyn Fn(i64, u32);

/// Called when the database the page installed has a damaged stored
/// setting, with its key (ADR 0052).
/// Also says how the database was installed and where the old files went
/// (`replaced`, `set_aside`), for the window's toast.
type OnDamagedSetting =
    dyn Fn(rappel_core::domain::SettingKey, Recovered, Option<PathBuf>, Option<PathBuf>);

mod imp {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use super::{OnDamagedSetting, OnRestored, OnTooNew};
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
        pub on_too_new: RefCell<Option<Callback<OnTooNew>>>,
        pub on_damaged_setting: RefCell<Option<Callback<OnDamagedSetting>>>,
        /// The last failure's toast: replaced by the next one, and
        /// dismissed once a database is open, so it does not stay over
        /// the toast that says what was done.
        pub errors: crate::ui::toast::ErrorSlot,
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

/// The page's icon, as in `data/recovery_page.blp`, given back after a
/// short window took it off (`ui::breakpoint::compact_status_page`).
pub const ICON: &str = "dialog-warning-symbolic";

impl RecoveryPage {
    /// The page's status page, which the window makes compact on a short
    /// window (ADR 0058).
    pub fn status_page(&self) -> adw::StatusPage {
        self.imp().status.get()
    }

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

    /// Calls `f` when Finish Restore finds that the waiting copy comes from
    /// a newer Rappel: the window then shows its "too new" page, as for a
    /// database that is too new. There is one such callback.
    pub fn set_on_too_new(&self, f: impl Fn(i64, u32) + 'static) {
        let f: Rc<OnTooNew> = Rc::new(f);
        self.imp().on_too_new.replace(Some(Callback::new(f)));
    }

    /// Calls `f` when the database a restore, Finish Restore or Start Fresh
    /// installed has a damaged stored setting: the window then shows the
    /// page that resets it, as at startup (ADR 0052). There is one such
    /// callback.
    pub fn set_on_damaged_setting(
        &self,
        f: impl Fn(rappel_core::domain::SettingKey, Recovered, Option<PathBuf>, Option<PathBuf>)
        + 'static,
    ) {
        let f: Rc<OnDamagedSetting> = Rc::new(f);
        self.imp()
            .on_damaged_setting
            .replace(Some(Callback::new(f)));
    }

    /// Hands a damaged setting in `result` to the window; whether it did.
    /// The database is installed then, so this page has nothing left to do.
    fn damaged_setting<T>(
        &self,
        result: &Result<rappel_core::Result<T>, worker::Panicked>,
        how: Recovered,
    ) -> bool {
        let (key, replaced, set_aside) = match result {
            Ok(Err(rappel_core::Error::InstalledWithDamagedSetting {
                key,
                replaced,
                set_aside,
            })) => (*key, replaced.clone(), set_aside.clone()),
            Ok(Err(rappel_core::Error::DamagedSetting { key, .. })) => (*key, None, None),
            _ => return false,
        };
        tracing::warn!(%key, "the installed database has a damaged setting");
        let imp = self.imp();
        imp.errors.clear();
        let handler = imp.on_damaged_setting.borrow().as_ref().map(Callback::get);
        if let Some(handler) = handler {
            handler(key, how, replaced, set_aside);
        }
        true
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
        let description = match AppState::list_backups(&dir) {
            Ok(backups) if backups.is_empty() => {
                imp.start_fresh_button
                    .set_visible(offers_start_fresh(imp.reason.get()));
                description_text(imp.reason.get(), Some(false))
            }
            Ok(backups) => {
                for backup in backups {
                    imp.backup_list.append(&self.backup_row(backup));
                }
                description_text(imp.reason.get(), Some(true))
            }
            Err(err) => {
                tracing::error!("cannot list backups: {err}");
                let mut text = description_text(imp.reason.get(), None);
                text.push_str("\n\nThe backups could not be listed: ");
                text.push_str(&err.to_string());
                text
            }
        };
        imp.backup_list
            .set_visible(imp.backup_list.first_child().is_some());
        // The description is Pango markup; the error text and paths are not.
        imp.status
            .set_description(Some(&glib::markup_escape_text(&description)));
        // Start Fresh as the only thing to do: give it the focus, so the
        // keyboard reaches it, and the status page's scrolled window
        // brings it into view on a short window (GtkViewport's
        // `scroll-to-focus`, on by default).
        if only_start_fresh(
            imp.start_fresh_button.is_visible(),
            imp.finish_restore_button.is_visible(),
            imp.backup_list.first_child().is_some(),
        ) {
            imp.start_fresh_button.grab_focus();
        }
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
    /// The page logs its errors itself, in full, at warning and error level,
    /// not only through its `ErrorSlot` (which keeps the details at debug
    /// level): it runs before any database is open, and the paths
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
        if self.damaged_setting(&result, Recovered::Backup) {
            return;
        }

        let message = match result {
            Ok(Ok(restored)) => {
                // Clone the callback out so no borrow is held while it runs.
                let handler = imp.on_restored.borrow().as_ref().map(Callback::get);
                imp.errors.clear();
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
        imp.errors.show(self, "recovering the database", &message);
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
    pub(crate) async fn finish_restore(&self) {
        let imp = self.imp();
        let Some(dir) = imp.data_dir.borrow().clone() else {
            return;
        };
        self.set_busy();

        let worker_dir = dir.clone();
        let result =
            worker::run(self, move || AppState::finish_restore_unopened(&worker_dir)).await;
        if self.damaged_setting(&result, Recovered::Finished) {
            return;
        }

        let message = match result {
            Ok(Ok(restored)) => {
                let handler = imp.on_restored.borrow().as_ref().map(Callback::get);
                imp.errors.clear();
                if let Some(handler) = handler {
                    handler(dir, restored, Recovered::Finished);
                }
                return;
            }
            Ok(Err(err)) => match finish_failure(&err) {
                FinishFailure::Unusable => {
                    // The page itself now says the copy cannot be used: no
                    // toast over it. The details are for the log.
                    tracing::warn!("the waiting restore copy cannot be used: {err}");
                    imp.reason.set(Some(RecoveryReason::UnusableRestoreCopy));
                    self.reload();
                    return;
                }
                FinishFailure::TooNew { found, supported } => {
                    tracing::warn!("the waiting restore copy is too new: {err}");
                    let handler = imp.on_too_new.borrow().as_ref().map(Callback::get);
                    imp.errors.clear();
                    if let Some(handler) = handler {
                        handler(found, supported);
                    }
                    self.reload();
                    return;
                }
                FinishFailure::Other => {
                    // The copy may be fine (a full disk, an IO error): the
                    // page stays as it was, with Finish Restore.
                    tracing::error!("finishing the restore failed: {err}");
                    finish_failure_text(&err)
                }
            },
            Err(_) => {
                tracing::error!("finish restore worker panicked");
                "Finishing the restore stopped unexpectedly".to_owned()
            }
        };
        self.reload();
        imp.errors.show(self, "recovering the database", &message);
    }

    fn confirm_start_fresh(&self) {
        let Ok(dialog) = dialog::START_FRESH.load() else {
            return;
        };
        if let Some(body) = start_fresh_body(self.imp().reason.get()) {
            dialog.set_body(body);
        }
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
        if self.damaged_setting(&result, Recovered::Fresh) {
            return;
        }

        let message = match result {
            Ok(Ok(fresh)) => {
                let handler = imp.on_restored.borrow().as_ref().map(Callback::get);
                imp.errors.clear();
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
        imp.errors.show(self, "recovering the database", &message);
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

/// The page's explanation for `reason`, with `backups` saying whether
/// there are any (`None`: they could not be listed). With a copy that
/// cannot be used and no backup, one short text says what happened and
/// what Start Fresh does, and never asks to restore a backup.
fn description_text(reason: Option<RecoveryReason>, backups: Option<bool>) -> String {
    if reason == Some(RecoveryReason::UnusableRestoreCopy) && backups == Some(false) {
        return "A restore was interrupted, and the copy it left waiting is damaged or was not \
                made by this version of Rappel. There is no backup to restore. Start Fresh sets \
                the copy aside and starts with no decks; nothing is deleted, and the files stay \
                in the data folder."
            .to_owned();
    }
    let mut text = reason
        .map(RecoveryReason::description)
        .unwrap_or_default()
        .to_owned();
    if backups == Some(false) {
        text.push_str("\n\n");
        text.push_str(no_backups_text(reason));
    }
    text
}

/// Whether Start Fresh is the page's only action, and so takes the focus.
fn only_start_fresh(start_fresh: bool, finish_restore: bool, backups: bool) -> bool {
    start_fresh && !finish_restore && !backups
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

/// What a failed Finish Restore means for the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinishFailure {
    /// The copy's content can never be installed (`is_unusable_copy`): the
    /// page says so and, without backups, offers Start Fresh, which sets it
    /// aside (ADR 0050, row 35).
    Unusable,
    /// The copy comes from a newer Rappel: the "too new" page.
    TooNew { found: i64, supported: u32 },
    /// Anything else (a full disk, an IO error): the copy may be fine, so
    /// the page keeps Finish Restore and a toast says what failed.
    Other,
}

/// How the page answers Finish Restore failing with `err`.
fn finish_failure(err: &rappel_core::Error) -> FinishFailure {
    use rappel_core::Error;
    match err {
        Error::RestoreRefused { source, .. } => match **source {
            // A negative version is no newer Rappel's: unusable, below.
            Error::UnsupportedVersion { found, supported } if found >= 0 => {
                FinishFailure::TooNew { found, supported }
            }
            ref source if rappel_core::storage::is_unusable_copy(source) => FinishFailure::Unusable,
            _ => FinishFailure::Other,
        },
        _ => FinishFailure::Other,
    }
}

/// The body of the question before Start Fresh when it differs from the
/// one in `data/start_fresh_dialog.blp`: with an unusable waiting copy, that
/// copy is renamed too. Either way, nothing is deleted.
fn start_fresh_body(reason: Option<RecoveryReason>) -> Option<&'static str> {
    (reason == Some(RecoveryReason::UnusableRestoreCopy)).then_some(
        "The copy the restore left waiting is renamed to “rappel.sqlite3.restore.refused-” \
         followed by the date and time, and the current database files to \
         “rappel.sqlite3.replaced-” and the date and time. Both are kept in the data folder. \
         Nothing is deleted. Rappel then starts with no decks.",
    )
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
            format!("The waiting copy could not be checked: {source}. It is kept; try again.")
        }
        err => format!("Finishing the restore failed: {err}"),
    }
}

/// Whether the page offers Start Fresh when there is no backup to restore:
/// never while an interrupted restore's validated copy waits (that copy is
/// the user's data; ADR 0016), but once Finish Restore found that copy
/// unusable, so a damaged copy and no backup is no dead end: core checks
/// the copy again and renames it aside (ADR 0050, row 35). With backups it
/// is never offered, and core refuses it too.
fn offers_start_fresh(reason: Option<RecoveryReason>) -> bool {
    reason.is_some_and(|r| r != RecoveryReason::InterruptedRestore)
}

#[cfg(test)]
pub(crate) mod tests {
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
        // A check that failed for another reason than the copy's content.
        let refused = Error::RestoreRefused {
            path: PathBuf::from("/data/rappel.sqlite3.restore.partial"),
            source: Box::new(Error::Io {
                path: PathBuf::from("/data"),
                source: std::io::Error::from(std::io::ErrorKind::StorageFull),
            }),
        };
        let text = finish_failure_text(&refused);
        assert!(
            text.starts_with("The waiting copy could not be checked:"),
            "{text}"
        );
        assert!(text.ends_with("It is kept; try again."), "{text}");
    }

    /// Only a refusal of the copy's content leaves Start Fresh; a newer
    /// version goes to the "too new" page, and anything else keeps Finish
    /// Restore (a full disk is a likely reason to be on this page).
    #[test]
    fn a_failed_finish_is_classified_by_what_it_says_about_the_copy() {
        use rappel_core::Error;
        let refused = |source: Error| Error::RestoreRefused {
            path: PathBuf::from("/data/rappel.sqlite3.restore.partial"),
            source: Box::new(source),
        };
        let io = |kind: std::io::ErrorKind| Error::Io {
            path: PathBuf::from("/data/rappel.sqlite3.restore.checking"),
            source: std::io::Error::from(kind),
        };
        assert_eq!(
            finish_failure(&refused(Error::NotARappelDatabase)),
            FinishFailure::Unusable
        );
        assert_eq!(
            finish_failure(&refused(Error::IntegrityCheckFailed {
                messages: vec!["bad".into()]
            })),
            FinishFailure::Unusable
        );
        assert_eq!(
            finish_failure(&refused(Error::UnsupportedVersion {
                found: 9,
                supported: 7
            })),
            FinishFailure::TooNew {
                found: 9,
                supported: 7
            }
        );
        // SQLite's own codes are sorted by core (`is_unusable_copy`).
        assert_eq!(
            finish_failure(&refused(io(std::io::ErrorKind::StorageFull))),
            FinishFailure::Other
        );
        // The WAL-mode refusal, and a version no Rappel writes.
        assert_eq!(
            finish_failure(&refused(Error::WalModeCopy {
                path: PathBuf::from("/data/x")
            })),
            FinishFailure::Unusable
        );
        assert_eq!(
            finish_failure(&refused(Error::UnsupportedVersion {
                found: -1,
                supported: 7
            })),
            FinishFailure::Unusable
        );
        // Any other IO error, `InvalidData` included, keeps the copy.
        assert_eq!(
            finish_failure(&refused(io(std::io::ErrorKind::InvalidData))),
            FinishFailure::Other
        );
        assert_eq!(
            finish_failure(&Error::NoRestoreWaiting),
            FinishFailure::Other
        );
    }

    /// The dead end of ADR 0050, row 35: the waiting copy is refused and
    /// there is no backup. The page then offers Start Fresh, which says
    /// that the copy is renamed and kept, and no longer Finish Restore.
    #[test]
    fn an_unusable_waiting_copy_leaves_start_fresh() {
        use rappel_core::Error;
        let refused = Error::RestoreRefused {
            path: PathBuf::from("/data/rappel.sqlite3.restore.partial"),
            source: Box::new(Error::NotARappelDatabase),
        };
        assert_eq!(finish_failure(&refused), FinishFailure::Unusable);
        let reason = Some(RecoveryReason::UnusableRestoreCopy);
        assert!(offers_start_fresh(reason));
        assert!(!offers_finish_restore(reason));
        assert!(no_backups_text(reason).contains("start with an empty"));
        let body = start_fresh_body(reason).unwrap_or_default();
        assert!(body.contains("restore.refused-"));
        assert!(body.contains("Nothing is deleted."));
        assert_eq!(start_fresh_body(Some(RecoveryReason::Damaged)), None);
    }

    #[test]
    fn an_unusable_copy_without_backups_never_asks_for_one() {
        let reason = Some(RecoveryReason::UnusableRestoreCopy);
        let text = description_text(reason, Some(false));
        assert!(!text.contains("Restore a backup"), "{text}");
        assert!(text.contains("no backup to restore"));
        assert!(text.contains("Start Fresh sets the copy aside"));
        assert_eq!(text.matches("deleted").count(), 1);
        assert!(!text.contains('\n'), "one paragraph");
        let with = description_text(reason, Some(true));
        assert!(with.contains("Restore a backup"));
        assert!(!with.contains("Start Fresh"));
        // Other reasons keep their explanation and the no-backup note.
        let damaged = description_text(Some(RecoveryReason::Damaged), Some(false));
        assert!(damaged.ends_with(no_backups_text(Some(RecoveryReason::Damaged))));
        assert_eq!(
            description_text(Some(RecoveryReason::Damaged), None),
            RecoveryReason::Damaged.description()
        );
    }

    #[test]
    fn start_fresh_takes_the_focus_only_when_it_is_the_only_action() {
        assert!(only_start_fresh(true, false, false));
        assert!(!only_start_fresh(true, true, false));
        assert!(!only_start_fresh(true, false, true));
        assert!(!only_start_fresh(false, false, false));
    }

    #[test]
    fn start_fresh_never_hides_a_waiting_restore_copy() {
        assert!(!offers_start_fresh(Some(
            RecoveryReason::InterruptedRestore
        )));
        assert!(!offers_start_fresh(None));
    }

    /// A data folder as an interrupted restore leaves it: no
    /// `rappel.sqlite3`, the waiting copy, and a stray `-wal` beside it
    /// (ADR 0050, rows 25 and 35). The database moved aside stays, and the
    /// backups are taken away unless `keep_backups`. Returns the folder and
    /// the waiting copy's path.
    pub(crate) fn interrupted(tmp: &tempfile::TempDir, keep_backups: bool) -> (PathBuf, PathBuf) {
        let data = tmp.path().join("data");
        let state = AppState::open(data.clone()).expect("a new database opens");
        state.create_deck("Maths").expect("a deck");
        drop(state);
        // The next open backs the deck up (the day's backup).
        let state = AppState::open(data.clone()).expect("the database opens again");
        let backups = AppState::list_backups(&data).expect("the backups are listed");
        let backup = backups
            .first()
            .expect("opening made a backup")
            .path()
            .to_path_buf();
        drop(state);
        let partial = data.join("rappel.sqlite3.restore.partial");
        std::fs::copy(&backup, &partial).expect("the copy is made");
        std::fs::rename(
            data.join("rappel.sqlite3"),
            data.join("rappel.sqlite3.replaced-20261018-120000"),
        )
        .expect("the database is moved aside");
        for sidecar in ["-wal", "-shm"] {
            let from = data.join(format!("rappel.sqlite3{sidecar}"));
            if from.exists() {
                let to = data.join(format!("rappel.sqlite3.replaced-20261018-120000{sidecar}"));
                std::fs::rename(from, to).expect("its sidecars too");
            }
        }
        std::fs::write(data.join("rappel.sqlite3-wal"), b"stray").expect("a stray -wal");
        if !keep_backups {
            let aside = tmp.path().join("backups-elsewhere");
            std::fs::rename(data.join("backups"), aside).expect("the backups are moved away");
        }
        (data, partial)
    }

    /// What `open` makes of that folder: the page with Finish Restore.
    fn reason_at_startup(data: &std::path::Path) -> RecoveryReason {
        let err = AppState::open(data.to_path_buf()).expect_err("the open is refused");
        match crate::startup::open_problem(&err, data) {
            crate::startup::Problem::Recovery { reason, .. } => reason,
            other => panic!("expected the recovery page, got {other:?}"),
        }
    }

    #[test]
    fn a_waiting_copy_beside_a_stray_wal_offers_finish_restore() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let (data, partial) = interrupted(&tmp, false);
            let reason = reason_at_startup(&data);
            assert_eq!(reason, RecoveryReason::InterruptedRestore);
            let page: RecoveryPage = glib::Object::new();
            let done = Rc::new(std::cell::Cell::new(None));
            let sink = Rc::clone(&done);
            page.show(reason, data.clone(), move |_, restored, how| {
                sink.set(Some((how, restored.store.decks().map(|d| d.len()).ok())));
            });
            let imp = page.imp();
            assert!(imp.finish_restore_button.is_visible());
            assert!(!imp.start_fresh_button.is_visible());
            glib::MainContext::default().block_on(page.finish_restore());
            assert_eq!(done.get(), Some((Recovered::Finished, Some(1))));
            assert!(!partial.exists());
        });
    }

    /// The dead end of ADR 0050, row 35, end to end: the waiting copy is
    /// damaged and there is no backup. Finish Restore is refused, the page
    /// then offers Start Fresh, and Start Fresh keeps the copy under a new
    /// name, byte for byte.
    #[test]
    fn a_damaged_waiting_copy_without_backups_is_no_dead_end() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let (data, partial) = interrupted(&tmp, false);
            std::fs::write(&partial, vec![0x42; 8192]).expect("the copy is damaged");
            let reason = reason_at_startup(&data);
            assert_eq!(reason, RecoveryReason::InterruptedRestore);
            let page: RecoveryPage = glib::Object::new();
            let done = Rc::new(std::cell::RefCell::new(None));
            let sink = Rc::clone(&done);
            page.show(reason, data.clone(), move |_, restored, how| {
                sink.replace(Some((how, restored.set_aside)));
            });
            let imp = page.imp();
            let context = glib::MainContext::default();
            context.block_on(page.finish_restore());
            assert!(done.borrow().is_none(), "the damaged copy is refused");
            assert_eq!(imp.reason.get(), Some(RecoveryReason::UnusableRestoreCopy));
            assert_eq!(imp.status.title(), "Restore Copy Cannot Be Used");
            assert!(!imp.finish_restore_button.is_visible());
            assert!(imp.start_fresh_button.is_visible());
            assert!(imp.start_fresh_button.is_sensitive());
            // The page says it; no error toast over it.
            assert!(imp.errors.is_empty());

            context.block_on(page.start_fresh());
            let (how, set_aside) = done.take().expect("a fresh database");
            assert_eq!(how, Recovered::Fresh);
            let kept = set_aside.expect("the copy was set aside");
            assert_eq!(std::fs::read(&kept).ok(), Some(vec![0x42; 8192]));
            assert!(!partial.exists());
            // The database moved aside by the restore is still there.
            assert!(
                data.join("rappel.sqlite3.replaced-20261018-120000")
                    .exists()
            );
        });
    }

    /// With a backup, a damaged copy is not set aside: the backups are
    /// listed to restore one, and Start Fresh is not offered.
    #[test]
    fn a_damaged_waiting_copy_with_backups_lists_them() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let (data, partial) = interrupted(&tmp, true);
            std::fs::write(&partial, vec![0x42; 8192]).expect("the copy is damaged");
            let page: RecoveryPage = glib::Object::new();
            page.show(reason_at_startup(&data), data, |_, _, _| {});
            glib::MainContext::default().block_on(page.finish_restore());
            let imp = page.imp();
            assert!(imp.backup_list.is_visible());
            assert!(imp.backup_list.first_child().is_some());
            assert!(!imp.start_fresh_button.is_visible());
            assert!(partial.exists());
        });
    }
}
