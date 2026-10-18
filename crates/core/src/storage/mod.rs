//! SQLite storage: opening the database safely, migrations and backups.
//!
//! See ADR 0005 for the decisions behind this module.

mod backup;
mod bulk;
mod cards;
mod decks;
mod migrations;
mod restore;
mod review;
mod schedule;
mod search;
mod settings;
mod stats;
mod tags;

use std::fs::{self, OpenOptions, Permissions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, ErrorCode, OpenFlags};

use crate::data_dir::ensure_data_dir;
use crate::error::{Error, Result};
use crate::id::IdGen;
use crate::time::now_ms;
pub use backup::{BackupInfo, list_backups};
use backup::{check_rows, has_backup_for_day};
pub(crate) use backup::{create_backup, prune_backups};
pub use bulk::{Moved, TagsAdded};
use migrations::{MIGRATIONS, Migration, latest, migrate};
pub use restore::{Restored, finish_restore, is_unusable_copy, restore_backup, start_fresh};
pub(crate) use schedule::rebuild_cards;
pub(crate) use search::index_card_in;
pub use search::{BrowseQuery, BrowseRow, CardStatus, MAX_QUERY_BYTES, StateFilter};
pub use settings::reset_setting;
pub(crate) use settings::{StoredSettings, replace_settings, scheduler_in, stored_settings};
pub use stats::Scope;

/// File name of the database inside the data directory.
pub(crate) const DB_FILE_NAME: &str = "rappel.sqlite3";

/// Directory name for backups inside the data directory.
pub(crate) const BACKUP_DIR_NAME: &str = "backups";

/// The directory backups are kept in, inside `data_dir`.
#[must_use]
pub fn backup_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(BACKUP_DIR_NAME)
}

const FILE_MODE: u32 = 0o600;

/// An open Rappel database.
#[derive(Debug)]
pub struct Store {
    pub(crate) conn: Connection,
    /// Issues the ids of new decks, cards and review events.
    ids: IdGen,
    /// Where backups go; `None` only for in-memory test stores, which
    /// cannot be backed up (an import then refuses to run).
    pub(crate) backup_dir: Option<PathBuf>,
    /// Problems [`Store::open`] met that did not stop it, for the UI.
    open_warnings: Vec<OpenWarning>,
}

/// A problem met by [`Store::open`] that did not stop it from opening the
/// database. Each one is also logged as a warning.
#[derive(Debug)]
pub enum OpenWarning {
    /// Today's backup could not be made. The database opened normally.
    DailyBackupFailed(Error),
    /// Old backups could not be pruned.
    PruneFailed(Error),
}

impl Store {
    /// Opens (or creates) the database in `data_dir`, checks it, backs it up
    /// when needed and migrates it to the current schema.
    ///
    /// In order:
    /// 0. If the database file is missing but a `-wal` or `-journal` file
    ///    is there, the open is refused: SQLite would replay that stale
    ///    file into a new, empty database. Likewise if a restore was
    ///    interrupted after moving the old database aside.
    /// 1. The directory is created with mode `0700` and the database file
    ///    with `0600` (an existing one is tightened, with its `-wal`/`-shm`).
    /// 2. `PRAGMA integrity_check`, then the version check. A damaged file,
    ///    a database newer than this build, or a non-Rappel database is
    ///    refused before anything is written.
    /// 3. WAL mode, `synchronous=FULL`, foreign keys on.
    /// 4. If migrations are pending on an existing database, a backup is
    ///    made first; if it fails, nothing is migrated. Otherwise one is
    ///    made if none exists for today (UTC); that one failing only logs a
    ///    warning. Old backups are pruned by the rule in ADR 0026, never
    ///    deleting the one just made.
    /// 5. Pending migrations run, one transaction each.
    /// 6. The stored settings are read and checked, so a damaged value stops
    ///    the open here, naming its key, and not later by chance in the
    ///    first read that needs it (ADR 0052). [`reset_setting`] puts its
    ///    default back.
    ///
    /// # Errors
    /// [`Error::IntegrityCheckFailed`], [`Error::UnsupportedVersion`],
    /// [`Error::NotARappelDatabase`], [`Error::OrphanedSidecar`], [`Error::InterruptedRestore`],
    /// [`Error::Migration`], [`Error::ForeignKeyViolation`],
    /// [`Error::DamagedSetting`], backup errors, and IO or SQLite errors.
    pub fn open(data_dir: &Path) -> Result<Self> {
        Self::open_with(
            &data_dir.join(DB_FILE_NAME),
            &data_dir.join(BACKUP_DIR_NAME),
            now_ms(),
            MIGRATIONS,
        )
    }

    /// [`Store::open`] with every input explicit, for tests.
    fn open_with(
        db_path: &Path,
        backup_dir: &Path,
        now_ms: i64,
        migrations: &[Migration],
    ) -> Result<Self> {
        Self::open_protecting(db_path, backup_dir, now_ms, migrations, Protect::Nothing)
    }

    /// [`Store::open_with`] that also keeps backups from being pruned: a
    /// restore passes the backup it installed (#8), finishing an
    /// interrupted restore keeps them all (#50).
    fn open_protecting(
        db_path: &Path,
        backup_dir: &Path,
        now_ms: i64,
        migrations: &[Migration],
        protect: Protect<'_>,
    ) -> Result<Self> {
        Self::open_unchecked(db_path, backup_dir, now_ms, migrations, protect)?.checked()
    }

    /// The store, once its stored settings are checked: step 6 of
    /// [`Store::open`]. It runs after the migrations, so it reads the
    /// current schema; no migration from 0006, which made the settings
    /// table, on reads the settings (`settings::tests`).
    fn checked(self) -> Result<Self> {
        stored_settings(&self.conn)?;
        Ok(self)
    }

    /// Steps 0 to 5 of [`Store::open`], without the settings check: for
    /// [`reset_setting`], which opens a database whose setting is damaged.
    fn open_unchecked(
        db_path: &Path,
        backup_dir: &Path,
        now_ms: i64,
        migrations: &[Migration],
        protect: Protect<'_>,
    ) -> Result<Self> {
        refuse_orphaned_sidecars(db_path)?;
        if let Some(parent) = db_path.parent() {
            ensure_data_dir(parent)?;
        }
        prepare_private_file(db_path)?;

        let mut conn = connect(db_path, true)?;
        check_integrity(&conn)?;

        // Read-only checks, before anything that writes to the file (even
        // switching to WAL rewrites header bytes).
        let supported = latest(migrations);
        let (current, is_new) = check_version(&conn, migrations)?;

        apply_write_settings(&conn)?;
        let warnings = if is_new {
            Vec::new()
        } else {
            back_up(&conn, backup_dir, now_ms, current < supported, protect)?
        };

        migrate(&mut conn, migrations, current)?;
        let mut store = Self::from_connection(conn);
        store.open_warnings = warnings;
        store.backup_dir = Some(backup_dir.to_path_buf());
        Ok(store)
    }

    /// Opens another connection to the database in `data_dir`, which
    /// [`Store::open`] already opened (and so checked, backed up and
    /// migrated) in this process. For work on a worker thread while the
    /// first store stays on its own: the new store is independent and may
    /// be used on any one thread.
    ///
    /// It never creates the file and runs no integrity check, backup or
    /// migration. Both connections may read and write: in WAL mode each
    /// read transaction sees one fixed snapshot, and SQLite lets one writer
    /// in at a time, the other waiting up to 5 seconds (ADR 0018). Ids
    /// stay unique because each store raises its id generator above the
    /// largest stored id before each write that issues one.
    ///
    /// # Errors
    /// [`Error::NotMigrated`] if the schema is not this build's latest (or
    /// [`Error::UnsupportedVersion`] if it is newer),
    /// [`Error::NotARappelDatabase`], [`Error::PragmaRejected`], and SQLite
    /// errors, including for a missing file.
    pub fn open_concurrent(data_dir: &Path) -> Result<Self> {
        Self::open_concurrent_with(
            &data_dir.join(DB_FILE_NAME),
            &data_dir.join(BACKUP_DIR_NAME),
            MIGRATIONS,
        )
    }

    /// [`Store::open_concurrent`] with every input explicit, for tests.
    fn open_concurrent_with(
        db_path: &Path,
        backup_dir: &Path,
        migrations: &[Migration],
    ) -> Result<Self> {
        let conn = connect(db_path, false)?;
        let supported = latest(migrations);
        let (current, _) = check_version(&conn, migrations)?;
        if current != supported {
            return Err(Error::NotMigrated {
                found: current,
                supported,
            });
        }
        apply_write_settings(&conn)?;
        let mut store = Self::from_connection(conn);
        store.backup_dir = Some(backup_dir.to_path_buf());
        Ok(store)
    }

    /// A store around an open, migrated connection.
    fn from_connection(conn: Connection) -> Self {
        Self {
            conn,
            ids: IdGen::new(),
            backup_dir: None,
            open_warnings: Vec::new(),
        }
    }

    /// Takes the warnings from opening this store (empty after the first
    /// call). See [`OpenWarning`].
    pub fn take_open_warnings(&mut self) -> Vec<OpenWarning> {
        std::mem::take(&mut self.open_warnings)
    }

    /// A migrated in-memory store, for tests of the store's operations.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        migrate(&mut conn, MIGRATIONS, 0).unwrap();
        Self::from_connection(conn)
    }

    /// The schema version of the open database.
    ///
    /// # Errors
    /// If SQLite fails to read it.
    #[cfg(test)]
    pub(crate) fn schema_version(&self) -> Result<u32> {
        let v: u32 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok(v)
    }
}

/// Opens the connection with its connection-only settings; none of them
/// write to the file.
/// With `create` false, a missing file is an error instead of a new,
/// empty database.
fn connect(db_path: &Path, create: bool) -> Result<Connection> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(db_path, flags)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "trusted_schema", false)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// WAL mode, `synchronous=FULL`, and a check that foreign keys are on.
/// These write to the file, so they come after the read-only checks.
fn apply_write_settings(conn: &Connection) -> Result<()> {
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::PragmaRejected {
            pragma: "journal_mode",
            value: mode,
        });
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    if !foreign_keys {
        return Err(Error::PragmaRejected {
            pragma: "foreign_keys",
            value: "0".into(),
        });
    }
    Ok(())
}

/// Which backups an open keeps beyond the rule of ADR 0026.
#[derive(Debug, Clone, Copy)]
enum Protect<'a> {
    /// Only the backup the open makes.
    Nothing,
    /// Also this one: the backup a restore just installed (#8).
    Backup(&'a Path),
    /// Every backup: nothing is pruned. Finishing an interrupted restore
    /// does not know which backup its copy came from, and that backup may
    /// be the oldest (#50). The next open prunes as usual and may delete
    /// it then; the restored data is in the live database (and in the
    /// day's backup, unless one existed earlier that UTC day).
    Everything,
}

/// Backs up an existing database at open and prunes old backups. Before a
/// migration (`migrating`) a failed backup is an error; otherwise the
/// daily backup and the prune only produce warnings.
fn back_up(
    conn: &Connection,
    backup_dir: &Path,
    now_ms: i64,
    migrating: bool,
    protect: Protect<'_>,
) -> Result<Vec<OpenWarning>> {
    let mut warnings = Vec::new();
    let fresh_backup = if migrating {
        // No migration without a backup: failure aborts the open.
        let path = create_backup(conn, backup_dir, now_ms)?;
        tracing::info!(path = %path.display(), "database backed up before migrating");
        Some(path)
    } else {
        // A failed daily backup must not lock the user out of a
        // healthy database.
        daily_backup(conn, backup_dir, now_ms).unwrap_or_else(|err| {
            tracing::warn!("daily backup failed: {err}");
            warnings.push(OpenWarning::DailyBackupFailed(err));
            None
        })
    };
    let restored = match protect {
        Protect::Nothing => None,
        Protect::Backup(path) => Some(path),
        Protect::Everything => return Ok(warnings),
    };
    let protected: Vec<&Path> = fresh_backup
        .as_deref()
        .into_iter()
        .chain(restored)
        .collect();
    if let Err(err) = prune_backups(backup_dir, now_ms, &protected) {
        tracing::warn!("pruning old backups failed: {err}");
        // A failed backup and a failed prune almost always share one
        // cause (the backup directory), so the user hears it once.
        if warnings.is_empty() {
            warnings.push(OpenWarning::PruneFailed(err));
        }
    }
    Ok(warnings)
}

/// Raises `ids` above the largest id in `column` (`decks.id`, `cards.id`
/// or `review_events.id`). Called, inside the write's transaction, before
/// every new id: a previous run (or another process) may have issued ids
/// above what this generator has seen, and after `x` it may have issued
/// `x + 1`, which a fresh generator would issue again.
///
/// # Errors
/// SQLite errors, or [`Error::InvalidColumn`] if that id is not a UUID
/// (the schema's `CHECK` only enforces length and case). The write is then
/// refused rather than risk an id that sorts out of order.
fn observe_max_id(conn: &Connection, ids: &mut IdGen, column: &'static str) -> Result<()> {
    let (table, id) = column.split_once('.').unwrap_or((column, "id"));
    let max: Option<String> =
        conn.query_row(&format!("SELECT max({id}) FROM {table}"), [], |r| r.get(0))?;
    match max {
        Some(value) if !ids.observe(&value) => Err(Error::InvalidColumn { column, value }),
        _ => Ok(()),
    }
}

/// Creates `path` with mode `0600` if missing (never truncates), and sets
/// `0600` on it and on any existing `-wal`, `-shm` or `-journal` file.
/// SQLite gives the files it creates next to the database the database
/// file's mode, so they stay private too.
fn prepare_private_file(path: &Path) -> Result<()> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(FILE_MODE)
        .open(path)
        .map_err(io_err(path))?;

    for p in std::iter::once(path.to_path_buf()).chain(sidecar_paths(path)) {
        match fs::metadata(&p) {
            Ok(meta) if meta.permissions().mode() & 0o7777 != FILE_MODE => {
                fs::set_permissions(&p, Permissions::from_mode(FILE_MODE)).map_err(io_err(&p))?;
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err(&p)(err)),
        }
    }
    Ok(())
}

/// Suffixes of the files SQLite keeps next to a database.
const SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// The `-wal`, `-shm` and `-journal` paths that belong to the database at
/// `path`, in that order.
fn sidecar_paths(path: &Path) -> [PathBuf; 3] {
    SIDECAR_SUFFIXES.map(|suffix| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    })
}

/// Whether anything (file, directory or dangling symlink) exists at `path`.
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Refuses a missing database whose restore was interrupted, or whose
/// `-wal` or `-journal` is still there.
/// SQLite would create a new, empty database and replay the stale file into
/// it. A leftover `-shm` is harmless: it is only an index and is rebuilt.
///
/// A waiting restore copy is reported first, even next to a stray `-wal`:
/// finishing the restore moves that file aside with the rest, while the
/// missing-file page would offer Start Fresh, which the waiting copy
/// refuses anyway (#50).
fn refuse_orphaned_sidecars(db_path: &Path) -> Result<()> {
    if exists(db_path)? {
        return Ok(());
    }
    // A restore that moved the old database aside and was then interrupted:
    // an empty database here would hide the user's data, and its daily
    // backups would prune away the real ones.
    if let Some(dir) = db_path.parent() {
        let partial = restore::restore_partial_path(dir);
        if exists(&partial)? {
            tracing::error!(path = %partial.display(), "database file missing during a restore");
            return Err(Error::InterruptedRestore(partial));
        }
    }
    let [wal, _shm, journal] = sidecar_paths(db_path);
    for p in [wal, journal] {
        if exists(&p)? {
            tracing::error!(path = %p.display(), "database file missing but its journal is present");
            return Err(Error::OrphanedSidecar(p));
        }
    }
    Ok(())
}

/// Checks `PRAGMA user_version` against `migrations` without writing.
/// Returns the current version and whether the database is brand new (no
/// schema at all).
///
/// # Errors
/// [`Error::UnsupportedVersion`] for a negative version or one newer than
/// the last migration, [`Error::NotARappelDatabase`] for a database with
/// tables but version 0.
fn check_version(conn: &Connection, migrations: &[Migration]) -> Result<(u32, bool)> {
    let supported = latest(migrations);
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = u32::try_from(found)
        .ok()
        .filter(|&v| v <= supported)
        .ok_or(Error::UnsupportedVersion { found, supported })?;
    let is_new = current == 0 && is_empty(conn)?;
    if current == 0 && !is_new {
        // Migration 0001 sets user_version in the same transaction that
        // creates the tables, so this is some other application's file.
        return Err(Error::NotARappelDatabase);
    }
    Ok((current, is_new))
}

/// Runs `PRAGMA integrity_check`. A file SQLite cannot even read as a
/// database counts as a failed check.
fn check_integrity(conn: &Connection) -> Result<()> {
    let messages = match check_rows(conn, "integrity_check") {
        Ok(rows) => rows,
        Err(err)
            if matches!(
                err.sqlite_error_code(),
                Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
            ) =>
        {
            vec![err.to_string()]
        }
        Err(err) => return Err(err.into()),
    };
    if messages == ["ok"] {
        Ok(())
    } else {
        tracing::error!(problems = messages.len(), "database integrity check failed");
        Err(Error::IntegrityCheckFailed { messages })
    }
}

/// Makes a backup unless one exists from today (UTC). Returns its path if
/// one was made.
fn daily_backup(conn: &Connection, backup_dir: &Path, now_ms: i64) -> Result<Option<PathBuf>> {
    if has_backup_for_day(backup_dir, now_ms)? {
        return Ok(None);
    }
    let path = create_backup(conn, backup_dir, now_ms)?;
    tracing::info!(path = %path.display(), "daily database backup");
    Ok(Some(path))
}

/// Whether the database has no schema at all (a file SQLite just created).
fn is_empty(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
    Ok(n == 0)
}

#[cfg(test)]
mod tests;
