//! Error type shared by all of `rappel-core`.

use std::io;
use std::path::PathBuf;

/// Everything that can go wrong in `rappel-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No usable base directory: `RAPPEL_DATA_DIR` and `XDG_DATA_HOME` are
    /// unset (or unusable) and `HOME` is unset, empty or relative.
    #[error("cannot find the data directory: set HOME, XDG_DATA_HOME or RAPPEL_DATA_DIR")]
    NoHomeDir,

    /// `RAPPEL_DATA_DIR` is a relative path. It is rejected instead of being
    /// resolved against whatever the current directory happens to be.
    #[error("RAPPEL_DATA_DIR must be an absolute path, got {}", .0.display())]
    RelativeDataDir(PathBuf),

    /// A debug build resolved to the release data directory (or a directory
    /// inside it), which would let development touch real data.
    #[error(
        "debug builds must not use the release data directory: {} is {} or inside it",
        path.display(),
        release.display()
    )]
    DebugUsesReleaseDir {
        /// The directory the debug build would have used.
        path: PathBuf,
        /// The release data directory it collides with.
        release: PathBuf,
    },

    /// The data directory path exists but is not a directory.
    #[error("{} exists but is not a directory", .0.display())]
    NotADirectory(PathBuf),

    /// Creating a directory or setting its permissions failed.
    #[error("cannot create directory {}: {source}", path.display())]
    CreateDir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The underlying IO error.
        source: io::Error,
    },

    /// A file operation on the database or a backup failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file the operation was on.
        path: PathBuf,
        /// The underlying IO error.
        source: io::Error,
    },

    /// An SQLite call failed.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// SQLite did not accept a required setting.
    #[error("database setting {pragma} could not be applied (got {value})")]
    PragmaRejected {
        /// The pragma, e.g. `journal_mode`.
        pragma: &'static str,
        /// The value SQLite reported back.
        value: String,
    },

    /// `PRAGMA integrity_check` found damage. The store refuses to open so
    /// nothing is written to a damaged file.
    #[error("database integrity check failed: {}", messages.join("; "))]
    IntegrityCheckFailed {
        /// What SQLite reported, one entry per problem.
        messages: Vec<String>,
    },

    /// The database was written by a newer version of Rappel (or is not a
    /// Rappel database). It is left untouched.
    #[error(
        "database version {found} is not supported (this version of Rappel supports up to {supported})"
    )]
    UnsupportedVersion {
        /// `PRAGMA user_version` of the file.
        found: i64,
        /// The latest migration this build knows.
        supported: u32,
    },

    /// The file is an SQLite database with tables but no Rappel schema
    /// version, so it belongs to some other application. It is left
    /// untouched.
    #[error("the file is a database, but not a Rappel database")]
    NotARappelDatabase,

    /// A migration failed and was rolled back.
    #[error("migration {version} failed: {source}")]
    Migration {
        /// The migration that failed.
        version: u32,
        /// The SQLite error.
        source: rusqlite::Error,
    },

    /// A migration's SQL ended the transaction itself. Its version was not
    /// recorded; this is a bug in the migration.
    #[error("migration {version} ended its own transaction")]
    MigrationEndedTransaction {
        /// The faulty migration.
        version: u32,
    },

    /// A migration left rows that violate a foreign key. It was rolled back.
    #[error("migration {version} left foreign key violations")]
    ForeignKeyViolation {
        /// The migration that was rolled back.
        version: u32,
    },

    /// Writing a backup failed. No partial backup is left behind.
    #[error("backup to {} failed: {source}", path.display())]
    Backup {
        /// The backup file that could not be written.
        path: PathBuf,
        /// The SQLite error.
        source: rusqlite::Error,
    },

    /// The finished backup did not pass `PRAGMA quick_check`. It was deleted.
    #[error("backup {} failed its check: {}", path.display(), messages.join("; "))]
    BackupCheckFailed {
        /// The backup file that failed.
        path: PathBuf,
        /// What SQLite reported.
        messages: Vec<String>,
    },

    /// A setting outside its allowed range (ADR 0027).
    #[error("setting {key} cannot be {value}")]
    InvalidSetting {
        /// The setting's key (`desired_retention`, `max_new_per_day`, ...).
        key: &'static str,
        /// The refused value.
        value: String,
    },

    /// A value stored in the `settings` table is not valid for its key (a
    /// damaged or hand-edited database). Every read of the settings refuses
    /// it, and so does [`Store::open`](crate::storage::Store::open), so
    /// nothing runs with it; [`crate::storage::reset_setting`] puts the
    /// default back after a backup (ADR 0052).
    #[error("the stored setting {key} cannot be read: {value}")]
    DamagedSetting {
        /// The damaged setting, one of the known keys.
        key: crate::domain::SettingKey,
        /// The start of the stored text, at most 64 characters and "…",
        /// for the log only.
        value: String,
    },

    /// A restore or Finish Restore installed its copy, and the copy has a
    /// damaged stored setting, so it was not opened: the files were moved
    /// all the same, and this says where, as
    /// [`Restored`](crate::storage::Restored) would. The setting is reset
    /// with [`crate::storage::reset_setting`] (ADR 0052).
    #[error("the database was installed, but its stored setting {key} cannot be read")]
    InstalledWithDamagedSetting {
        /// The damaged setting.
        key: crate::domain::SettingKey,
        /// Where the previous database was moved, if anywhere.
        replaced: Option<std::path::PathBuf>,
        /// Where a waiting restore copy was kept, if anywhere.
        set_aside: Option<std::path::PathBuf>,
    },

    /// FSRS could not compute a memory state (non-finite input state).
    #[error("scheduler error: {0}")]
    Fsrs(#[from] fsrs::FSRSError),

    /// A review event of another card was passed to a card's rebuild.
    #[error("review event {event_id} does not belong to card {card_id}")]
    EventForOtherCard {
        /// The stray event.
        event_id: String,
        /// The card being rebuilt.
        card_id: String,
    },

    /// A review is dated before the card's last review (the clock went
    /// backwards). Recording code stores `max(now, last_review_at)`.
    #[error("review at {reviewed_at} is before the card's last review at {last_review_at}")]
    ReviewBeforeLastReview {
        /// The rejected review time, UTC milliseconds.
        reviewed_at: i64,
        /// The card's last review, UTC milliseconds.
        last_review_at: i64,
    },

    /// Rebuilding the schedule cache during a migration failed. The
    /// migration was rolled back.
    #[error("migration {version} failed to rebuild the schedule cache: {source}")]
    MigrationRebuild {
        /// The migration that was rolled back.
        version: u32,
        /// What went wrong.
        source: Box<Error>,
    },

    /// A migration's Rust backfill step failed (for migration 0004: two
    /// live decks whose names now count as the same). The migration was
    /// rolled back.
    #[error("migration {version} failed to fill in new values: {source}")]
    MigrationBackfill {
        /// The migration that was rolled back.
        version: u32,
        /// What went wrong.
        source: Box<Error>,
    },

    /// A value read from the database is not valid for its column.
    #[error("invalid value in {column}: {value}")]
    InvalidColumn {
        /// `table.column`.
        column: &'static str,
        /// What was found.
        value: String,
    },

    /// A backup was not restored because it failed validation (`source`
    /// says why: damaged, too new, not a Rappel database, or not a backup
    /// file at all). Nothing in the data directory was changed.
    #[error("backup {} cannot be restored: {source}", path.display())]
    RestoreRefused {
        /// The backup that was refused.
        path: PathBuf,
        /// Why it was refused.
        source: Box<Error>,
    },

    /// A `-wal` or `-journal` file exists without its database file. Opening
    /// would create an empty database and replay this stale file into it,
    /// so the store refuses. Restoring a backup moves it aside safely.
    #[error("{} exists without its database file", .0.display())]
    OrphanedSidecar(PathBuf),

    /// A restore was interrupted after the old database was moved aside and
    /// before the backup was installed (the path is the validated copy
    /// waiting to be installed). Opening would start an empty database in
    /// its place; restoring a backup again, or [`crate::storage::finish_restore`],
    /// finishes the job.
    #[error("a restore was interrupted before it finished ({} is waiting to be installed)", .0.display())]
    InterruptedRestore(PathBuf),

    /// A copy to restore is in WAL mode, which no backup Rappel makes is:
    /// even reading it would create and read files beside it, so what is
    /// checked would not be the file alone. The copy is refused.
    #[error("{} is in WAL mode, which backups never are", path.display())]
    WalModeCopy {
        /// The refused copy.
        path: PathBuf,
    },

    /// Finishing a restore was asked for, but no validated copy is waiting
    /// to be installed. Nothing was changed.
    #[error("no interrupted restore is waiting to be finished")]
    NoRestoreWaiting,

    /// The operating system gave no random bytes for a new id. The cause
    /// is boxed so the random-number crate stays out of this API (ADR 0015).
    #[error("cannot generate an id: no randomness available: {0}")]
    Random(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Starting fresh was refused because backups exist: restoring one
    /// keeps more data, and the empty database's daily backups would
    /// rotate them out (ADR 0016). Nothing was changed.
    #[error("{0} backups can be restored instead of starting with an empty database")]
    BackupsAvailable(usize),

    /// Every `UUIDv7` up to the year 10889 has been issued.
    #[error("no more ids can be generated")]
    IdsExhausted,

    /// A deck name that is empty or only whitespace.
    #[error("a deck name cannot be empty")]
    EmptyDeckName,

    /// Another deck that is not deleted already has this name, ignoring
    /// case and Unicode composition ([`crate::domain::deck_name_key`]).
    #[error("a deck named {0:?} already exists")]
    DuplicateDeckName(String),

    /// [`Store::open_concurrent`](crate::storage::Store::open_concurrent)
    /// found a database that [`Store::open`](crate::storage::Store::open)
    /// has not migrated yet.
    #[error("the database has schema version {found}, not {supported}; open it first")]
    NotMigrated {
        /// The version found.
        found: u32,
        /// The latest version this build knows.
        supported: u32,
    },

    /// A deck name longer than
    /// [`MAX_DECK_NAME_BYTES`](crate::domain::MAX_DECK_NAME_BYTES).
    #[error("a deck name can be at most {limit} bytes long")]
    DeckNameTooLong {
        /// The limit, in bytes.
        limit: usize,
    },

    /// A card side longer than
    /// [`MAX_CARD_SIDE_BYTES`](crate::domain::MAX_CARD_SIDE_BYTES).
    #[error("the {side} of a card can be at most {limit} bytes long")]
    CardSideTooLong {
        /// The side that is too long (the front if both are).
        side: crate::domain::CardSide,
        /// The limit, in bytes.
        limit: usize,
    },

    /// A deck name or card side with a NUL character (U+0000).
    #[error("text cannot contain a NUL character")]
    ContainsNul,

    /// A card front that is empty or only whitespace.
    #[error("the front of a card cannot be empty")]
    EmptyCardFront,

    /// A tag name that is empty or only whitespace.
    #[error("a tag name cannot be empty")]
    EmptyTagName,

    /// A tag name longer than
    /// [`MAX_TAG_NAME_BYTES`](crate::domain::MAX_TAG_NAME_BYTES) once
    /// trimmed.
    #[error("a tag name can be at most {limit} bytes long")]
    TagNameTooLong {
        /// The limit, in bytes.
        limit: usize,
    },

    /// A tag name with a comma, which separates tags in a list (ADR 0029).
    #[error("a tag name cannot contain a comma")]
    TagNameHasComma,

    /// No tag with this id.
    #[error("tag {0} does not exist")]
    TagNotFound(String),

    /// No log entry with this id for the card.
    #[error("entry {0} is not in the card's log")]
    EventNotFound(String),

    /// The entry is not one an undo may cancel: only the card's latest
    /// review or reset that is not already undone can be (ADR 0028).
    #[error("entry {0} is not the card's latest review or reset")]
    NotLatestEntry(String),

    /// A log whose undo names no review or reset of its card, or an entry
    /// undone twice; the schema refuses such a log (ADR 0028).
    #[error("undo {event_id} does not cancel exactly one review or reset of its card")]
    InvalidUndo {
        /// The undo's id.
        event_id: String,
    },

    /// Day bounds for the statistics that are fewer than two or not strictly
    /// increasing (ADR 0032).
    #[error("day bounds must be at least two, strictly increasing timestamps")]
    InvalidDayGrid,

    /// No deck with this id, or it was deleted.
    #[error("deck {0} does not exist")]
    DeckNotFound(String),

    /// No card with this id, or it was deleted.
    #[error("card {0} does not exist")]
    CardNotFound(String),

    /// The card is new (no surviving review since it was created or last
    /// reset), so a reset would only add a log entry that changes nothing.
    #[error("card {0} is already new, so there is no progress to reset")]
    CardAlreadyNew(String),

    /// Encoding an export failed. The cause is boxed so the JSON and zip
    /// crates stay out of this API (ADR 0015).
    #[error("cannot write the export: {0}")]
    ExportFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The file is not a valid Rappel export (ADR 0017). Nothing was
    /// written.
    #[error("cannot import this file: {0}")]
    ImportRefused(#[source] crate::interchange::ImportProblem),

    /// The import plan has conflicts with the database, so the whole
    /// import is refused. Nothing was written.
    #[error("cannot import: {0} conflicts with the existing data")]
    ImportConflicts(usize),

    /// The database changed between planning and applying the import.
    /// Nothing was written; plan again.
    #[error("the data changed since the import was checked; check the file again")]
    ImportPlanStale,

    /// The file changed since it was checked (or while it was read), so it
    /// is not the file the plan describes. Nothing was written; check it
    /// again (ADR 0053).
    #[error("the file changed since it was checked; check it again")]
    ImportFileChanged,

    /// This store has no backup directory (an in-memory store), so an
    /// import, which needs a backup first, cannot run.
    #[error("this database cannot be backed up, so nothing can be imported into it")]
    NoBackupDir,

    /// A file offered as an image or an audio file is not one Rappel
    /// keeps (ADR 0054). Nothing was recorded.
    #[error("cannot add this file: {0}")]
    MediaRefused(crate::media::MediaProblem),

    /// Reading a file being added as media failed. Nothing was recorded.
    #[error("cannot read the media file: {0}")]
    MediaRead(io::Error),

    /// This store has no data directory (an in-memory store), so it has
    /// no media folder.
    #[error("this database has no media folder")]
    NoMediaDir,
}

/// `Result` with [`Error`] as the error type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn random_keeps_its_cause_in_the_message_and_the_source() {
        let cause = io::Error::other("entropy pool closed");
        let err = Error::Random(Box::new(cause));
        assert_eq!(
            err.to_string(),
            "cannot generate an id: no randomness available: entropy pool closed"
        );
        let source = err.source().map(ToString::to_string);
        assert_eq!(source.as_deref(), Some("entropy pool closed"));
    }
}
