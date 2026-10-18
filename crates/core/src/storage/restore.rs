//! Restoring a backup over the current database.
//!
//! The current database is never deleted and never opened, so a restore
//! works even when it is damaged. In order:
//! 1. The backup is byte-copied to `rappel.sqlite3.restore.checking` in the
//!    data directory, and **the copy** is validated (integrity, version,
//!    schema), so what gets installed is exactly what was checked. A copy
//!    older than this build is migrated there and validated again, so a
//!    migration that fails refuses the restore before anything changed.
//!    Only then is it renamed to `rappel.sqlite3.restore.partial`: that name
//!    always means "a validated copy", and a later attempt that fails never
//!    deletes it.
//! 2. The current database and its `-wal`, `-shm` and `-journal` files are
//!    renamed, main file first, to `rappel.sqlite3.replaced-YYYYMMDD-HHMMSS`
//!    (plus the same suffixes). SQLite finds `<name>-wal` next to
//!    `<name>`, so the moved-aside set still opens as one database. The
//!    name is written to `rappel.sqlite3.moving-aside` first, so a retry
//!    after a crash between two renames fills the same set (ADR 0026).
//! 3. The copy is renamed to `rappel.sqlite3`, and opened with
//!    [`Store::open`].
//!
//! Once step 2 has started, a failure or crash can leave no main file. The
//! data is never lost, but a plain [`Store::open`] would create a new,
//! empty database. It refuses instead: [`Error::OrphanedSidecar`] for a
//! `-wal` left without its main file, [`Error::InterruptedRestore`] when
//! the validated copy is waiting to be installed. Running the restore again
//! finishes the job. See ADR 0007.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::backup::{BackupInfo, SQLITE_HEADER, parse_backup_name, utc_stamp};
use super::migrations::{MIGRATIONS, Migration, latest, migrate};
use super::{
    BACKUP_DIR_NAME, DB_FILE_NAME, FILE_MODE, Protect, SIDECAR_SUFFIXES, Store, check_integrity,
    check_version, exists, sidecar_paths,
};
use crate::data_dir::ensure_data_dir;
use crate::error::{Error, Result};
use crate::time::now_ms;

/// The result of a successful [`restore_backup`].
#[derive(Debug)]
pub struct Restored {
    /// The restored database, open and migrated.
    pub store: Store,
    /// Where the previous database was moved: its main file, with its
    /// `-wal`/`-shm`/`-journal` files next to it under the same name plus
    /// suffix. `None` if there was nothing to move. If only leftover
    /// sidecar files were moved, this main file does not exist.
    pub replaced: Option<PathBuf>,
    /// Where [`start_fresh`] moved a waiting restore copy that could not be
    /// used (damaged, too new, not Rappel's): `rappel.sqlite3.restore.refused-
    /// YYYYMMDD-HHMMSS`. `None` otherwise.
    pub set_aside: Option<PathBuf>,
}

/// Restores `backup` (from [`super::list_backups`]) as the database in
/// `data_dir`, and opens it.
///
/// `current` is the open store, if there is one (`None` when the database
/// could not be opened). It is closed before any file is moved, so no
/// connection keeps writing to the moved-aside file.
///
/// The backup is validated before anything is changed. The previous
/// database is never deleted; it is moved to
/// `rappel.sqlite3.replaced-YYYYMMDD-HHMMSS` (mode `0600`), see
/// [`Restored::replaced`]. This works when the current database is damaged.
///
/// # Errors
/// - [`Error::RestoreRefused`] if the backup is not a backup file in
///   `<data_dir>/backups/`, is damaged, is newer than this build, or does
///   not have the Rappel schema, or cannot be migrated to this build's
///   schema. Nothing was changed.
/// - IO errors while copying or moving files, and any error from
///   [`Store::open`] on the restored database. If the old files were
///   already moved aside, they stay under their `replaced-…` name, and
///   [`Store::open`] refuses to start an empty database in their place
///   until a restore succeeds.
pub fn restore_backup(
    data_dir: &Path,
    current: Option<Store>,
    backup: &BackupInfo,
) -> Result<Restored> {
    drop(current);
    restore_with(data_dir, backup.path(), now_ms(), MIGRATIONS)
}

/// Starts over with an empty database in `data_dir`, for when it cannot be
/// opened and there is no backup to restore (ADR 0016).
///
/// `current` is the open store, if any; it is closed first. The current
/// database and its `-wal`/`-shm`/`-journal` files are moved aside exactly
/// as [`restore_backup`] does, to `rappel.sqlite3.replaced-YYYYMMDD-HHMMSS`
/// (see [`Restored::replaced`]). Nothing is deleted. Then an empty
/// database is created and opened.
///
/// A copy an interrupted restore left waiting is checked first, as
/// [`finish_restore`] would check it. If it can be used, starting over is
/// refused (below). If it cannot (it is damaged, too new or not Rappel's),
/// it is renamed to `rappel.sqlite3.restore.refused-YYYYMMDD-HHMMSS`, never
/// deleted or overwritten, and [`Restored::set_aside`] says where: otherwise
/// a damaged copy with no backup left no way on (ADR 0050, row 35).
///
/// # Errors
/// - [`Error::InterruptedRestore`] if a validated copy from an interrupted
///   restore is waiting to be installed, and still checks: that copy is the
///   user's data, and starting over would hide it. Nothing was changed.
/// - [`Error::BackupsAvailable`] if `<data_dir>/backups/` holds backups:
///   restore one instead. Nothing was changed, a refused copy included.
/// - IO errors while moving files, and any error from [`Store::open`]. If
///   the old files were already moved aside, they stay under their
///   `replaced-…` name.
pub fn start_fresh(data_dir: &Path, current: Option<Store>) -> Result<Restored> {
    drop(current);
    start_fresh_with(data_dir, now_ms(), MIGRATIONS)
}

/// Installs the validated copy an interrupted restore left waiting, when
/// the backup it came from is gone (ADR 0026, #22). `current` is the open
/// store, if any; it is closed first.
///
/// The waiting copy was validated before it got its name (ADR 0007), but it
/// is copied to a scratch name and checked again (and migrated, if this
/// build is newer), so what is installed is exactly what was checked, and
/// the waiting file itself is only replaced by that checked copy. The
/// database and sidecar files still in place are moved aside as
/// [`restore_backup`] does.
///
/// # Errors
/// - [`Error::NoRestoreWaiting`] if there is no waiting copy (or it is not
///   a regular file). Nothing was changed.
/// - [`Error::RestoreRefused`] if the copy is damaged, too new, or not a
///   Rappel database. The copy stays where it is and nothing was changed.
/// - IO errors while copying or moving files, and any error from
///   [`Store::open`] on the installed database.
pub fn finish_restore(data_dir: &Path, current: Option<Store>) -> Result<Restored> {
    drop(current);
    finish_restore_with(data_dir, now_ms(), MIGRATIONS)
}

/// [`finish_restore`] with every input explicit, for tests.
fn finish_restore_with(data_dir: &Path, now_ms: i64, migrations: &[Migration]) -> Result<Restored> {
    let partial = restore_partial_path(data_dir);
    let waiting = match fs::symlink_metadata(&partial) {
        Ok(meta) => meta.file_type().is_file(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(Error::Io {
                path: partial,
                source,
            });
        }
    };
    if !waiting {
        return Err(Error::NoRestoreWaiting);
    }
    let refused = |source: Error| Error::RestoreRefused {
        path: partial.clone(),
        source: Box::new(source),
    };

    let checking = restore_checking_path(data_dir);
    copy_private(&partial, &checking).inspect_err(|_| remove_copy(&checking))?;
    if let Err(err) = check_copy(&checking, migrations) {
        remove_copy(&checking);
        tracing::warn!("the waiting restore copy was refused: {err}");
        return Err(refused(err));
    }
    // Replaces the waiting file atomically with the checked (and perhaps
    // migrated) copy.
    fs::rename(&checking, &partial).map_err(|source| Error::Io {
        path: checking.clone(),
        source,
    })?;
    sync_parent(&partial)?;
    let db_path = data_dir.join(DB_FILE_NAME);
    let replaced = move_aside(&db_path, now_ms)?;
    if let Some(path) = &replaced {
        tracing::info!(path = %path.display(), "previous database moved aside");
    }
    install(&partial, &db_path)?;
    tracing::info!("interrupted restore finished");
    // The backup the copy came from is not known, and may be the oldest:
    // this open prunes nothing (#50).
    let store = Store::open_protecting(
        &db_path,
        &data_dir.join(BACKUP_DIR_NAME),
        now_ms,
        migrations,
        Protect::Everything,
    )?;
    Ok(Restored {
        store,
        replaced,
        set_aside: None,
    })
}

/// Whether `err`, from checking a restore copy, is about the copy's content,
/// so the copy can never be installed: it is damaged, not an SQLite file,
/// not a Rappel database, in WAL mode, which no backup is, or has a
/// negative version, which no Rappel writes. Any other error (a full disk,
/// an IO error, too little memory, a copy made by a newer Rappel) says
/// nothing against the copy, so it is kept where it is (ADR 0050, row 35).
#[must_use]
pub fn is_unusable_copy(err: &Error) -> bool {
    match err {
        Error::IntegrityCheckFailed { .. }
        | Error::NotARappelDatabase
        | Error::WalModeCopy { .. } => true,
        Error::UnsupportedVersion { found, .. } => *found < 0,
        Error::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) => matches!(
            failure.code,
            rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
        ),
        _ => false,
    }
}

/// [`start_fresh`] with every input explicit, for tests.
fn start_fresh_with(data_dir: &Path, now_ms: i64, migrations: &[Migration]) -> Result<Restored> {
    start_fresh_checking(data_dir, now_ms, migrations, &check_copy)
}

/// A check of a restore copy, as [`check_copy`]; injected by tests.
type CopyCheck<'a> = &'a dyn Fn(&Path, &[Migration]) -> Result<()>;

/// [`start_fresh_with`] with the copy's check injected, so a test can make
/// it fail as a full disk would.
fn start_fresh_checking(
    data_dir: &Path,
    now_ms: i64,
    migrations: &[Migration],
    check: CopyCheck<'_>,
) -> Result<Restored> {
    let partial = restore_partial_path(data_dir);
    // Only a regular file is a waiting copy, as for `finish_restore`.
    // Anything else at that name (a folder, a link) is neither checked nor
    // moved, and refuses as before: the open would refuse an empty database
    // beside it anyway.
    let waiting = match fs::symlink_metadata(&partial) {
        Ok(meta) if meta.file_type().is_file() => true,
        Ok(_) => return Err(Error::InterruptedRestore(partial)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(Error::Io {
                path: partial,
                source,
            });
        }
    };
    if waiting {
        // Checked on a scratch copy, as `finish_restore` checks it: one that
        // can still be installed is the user's data, and one whose check
        // failed for another reason than its content may be too.
        let checking = restore_checking_path(data_dir);
        copy_private(&partial, &checking).inspect_err(|_| remove_copy(&checking))?;
        let checked = check(&checking, migrations);
        remove_copy(&checking);
        match checked {
            Ok(()) => return Err(Error::InterruptedRestore(partial)),
            Err(err) if is_unusable_copy(&err) => {
                tracing::warn!("the waiting restore copy cannot be used: {err}");
            }
            Err(err) => return Err(err),
        }
    }
    let backups = super::list_backups(data_dir)?.len();
    if backups > 0 {
        return Err(Error::BackupsAvailable(backups));
    }
    ensure_data_dir(data_dir)?;
    let set_aside = if waiting {
        let target = set_aside_copy(data_dir, &partial, now_ms)?;
        tracing::info!(path = %target.display(), "unusable restore copy set aside");
        Some(target)
    } else {
        None
    };
    let db_path = data_dir.join(DB_FILE_NAME);
    let replaced = move_aside(&db_path, now_ms)?;
    if let Some(path) = &replaced {
        tracing::info!(path = %path.display(), "previous database moved aside to start fresh");
    }
    let store = Store::open_with(
        &db_path,
        &data_dir.join(BACKUP_DIR_NAME),
        now_ms,
        migrations,
    )?;
    tracing::info!("started with an empty database");
    Ok(Restored {
        store,
        replaced,
        set_aside,
    })
}

/// Path of the validated copy waiting to be installed.
pub(super) fn restore_partial_path(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{DB_FILE_NAME}.restore.partial"))
}

/// Path of the copy while it is being validated.
fn restore_checking_path(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{DB_FILE_NAME}.restore.checking"))
}

/// [`restore_backup`] with every input explicit, for tests.
fn restore_with(
    data_dir: &Path,
    backup: &Path,
    now_ms: i64,
    migrations: &[Migration],
) -> Result<Restored> {
    let refused = |source: Error| Error::RestoreRefused {
        path: backup.to_path_buf(),
        source: Box::new(source),
    };
    check_location(data_dir, backup).map_err(refused)?;

    ensure_data_dir(data_dir)?;
    // Copy and check under a scratch name, so that a failure here never
    // touches a validated copy left by an interrupted earlier restore.
    let checking = restore_checking_path(data_dir);
    copy_private(backup, &checking).inspect_err(|_| remove_copy(&checking))?;
    if let Err(err) = check_copy(&checking, migrations) {
        remove_copy(&checking);
        tracing::warn!(backup = %backup.display(), "backup refused: {err}");
        return Err(refused(err));
    }

    // From here on the validated copy is never deleted on error: if the
    // main file is gone, it is what makes `Store::open` refuse to start an
    // empty database (`InterruptedRestore`), and a retry replaces it.
    let partial = restore_partial_path(data_dir);
    fs::rename(&checking, &partial).map_err(|source| Error::Io {
        path: checking.clone(),
        source,
    })?;
    sync_parent(&partial)?;
    let db_path = data_dir.join(DB_FILE_NAME);
    let replaced = move_aside(&db_path, now_ms)?;
    if let Some(path) = &replaced {
        tracing::info!(path = %path.display(), "previous database moved aside");
    }
    install(&partial, &db_path)?;
    tracing::info!(backup = %backup.display(), "backup restored");

    // The restored backup is protected from the prune this open does: it
    // may be the oldest one (#8).
    let store = Store::open_protecting(
        &db_path,
        &data_dir.join(BACKUP_DIR_NAME),
        now_ms,
        migrations,
        Protect::Backup(backup),
    )?;
    Ok(Restored {
        store,
        replaced,
        set_aside: None,
    })
}

/// The backup must be a regular file with a backup name directly inside
/// `<data_dir>/backups/`. Restore only reads files Rappel made itself.
fn check_location(data_dir: &Path, backup: &Path) -> Result<()> {
    let not_a_backup = || Error::Io {
        path: backup.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a backup file in the backups directory",
        ),
    };
    let in_backup_dir = backup.parent() == Some(&data_dir.join(BACKUP_DIR_NAME));
    let well_named = backup
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(parse_backup_name)
        .is_some();
    if !in_backup_dir || !well_named {
        return Err(not_a_backup());
    }
    let meta = fs::symlink_metadata(backup).map_err(|source| Error::Io {
        path: backup.to_path_buf(),
        source,
    })?;
    if meta.file_type().is_file() {
        Ok(())
    } else {
        Err(not_a_backup())
    }
}

/// Copies `src` to a new file `dest` with mode `0600` and syncs it. A
/// leftover `dest` from an interrupted restore, and any `-wal`/`-shm`/
/// `-journal` next to it, are removed first so they cannot be read along
/// with the copy.
fn copy_private(src: &Path, dest: &Path) -> Result<()> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    for p in std::iter::once(dest.to_path_buf()).chain(sidecar_paths(dest)) {
        match fs::remove_file(&p) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(io_err(&p)(err)),
            _ => {}
        }
    }
    let mut from = File::open(src).map_err(io_err(src))?;
    let mut to = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(dest)
        .map_err(io_err(dest))?;
    io::copy(&mut from, &mut to).map_err(io_err(dest))?;
    to.sync_all().map_err(io_err(dest))
}

/// Validates the copy at `path`. If it is older than `migrations`, migrates
/// it in place and validates it again, so the installed file is current
/// and a migration that fails refuses the restore while nothing else has
/// changed.
fn check_copy(path: &Path, migrations: &[Migration]) -> Result<()> {
    let version = validate(path, migrations)?;
    if version < latest(migrations) {
        migrate_copy(path, migrations, version)?;
        validate(path, migrations)?;
    }
    Ok(())
}

/// Migrates the copy at `path` from `current` with the settings
/// [`Store::open`] uses, except that it stays in rollback-journal mode.
/// Each migration commits in its own transaction; on an error the copy is
/// left part-way and must be removed.
fn migrate_copy(path: &Path, migrations: &[Migration], current: u32) -> Result<()> {
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "trusted_schema", false)?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    if !foreign_keys {
        return Err(Error::PragmaRejected {
            pragma: "foreign_keys",
            value: "0".into(),
        });
    }
    migrate(&mut conn, migrations, current)?;
    conn.close().map_err(|(_, err)| err)?;
    Ok(())
}

/// Checks the file at `path` without writing to it: it uses a rollback
/// journal (as every backup does), passes `integrity_check`, its version is
/// between 1 and the last migration, and it has exactly the tables,
/// indexes, triggers and views that `migrations` create at that version,
/// with the same columns. Returns the version.
fn validate(path: &Path, migrations: &[Migration]) -> Result<u32> {
    // A WAL-mode file would make even a read-only connection create and
    // read `-wal`/`-shm` files next to it, so what is checked would not be
    // the file alone. Backups are always written with a rollback journal.
    require_rollback_journal(path)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // The file is not trusted until it passes the checks below.
    conn.pragma_update(None, "trusted_schema", false)?;
    check_integrity(&conn)?;
    let (version, is_new) = check_version(&conn, migrations)?;
    if is_new {
        // An empty database: nothing to restore.
        return Err(Error::NotARappelDatabase);
    }
    if schema_objects(&conn)? != reference_schema(migrations, version)? {
        return Err(Error::NotARappelDatabase);
    }
    conn.close().map_err(|(_, err)| err)?;
    Ok(version)
}

/// Refuses a database file whose header does not say "rollback journal"
/// (bytes 18 and 19 both 1). A file that is not an SQLite database at all
/// is left to the integrity check, which reports it more clearly.
fn require_rollback_journal(path: &Path) -> Result<()> {
    let io_err = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut header = [0u8; 20];
    match File::open(path).map_err(io_err)?.read_exact(&mut header) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(err) => return Err(io_err(err)),
    }
    if &header[..16] != SQLITE_HEADER || (header[18] == 1 && header[19] == 1) {
        Ok(())
    } else {
        Err(Error::WalModeCopy {
            path: path.to_path_buf(),
        })
    }
}

/// A schema object: its type, its name, and for a table or an index its
/// columns as SQLite reports them.
type SchemaObject = (String, String, Vec<String>);

/// Every schema object, sorted, leaving out SQLite's own `sqlite_*`
/// objects (`sqlite_stat1` and `sqlite_sequence` come and go with `ANALYZE`
/// and use, and are not part of our schema).
///
/// Columns come from `pragma_table_xinfo` / `pragma_index_xinfo` (name,
/// type, NOT NULL, default, primary key; or key order and collation), not
/// from the stored SQL text, which depends on how the object was created.
/// Two schema versions with the same object names but different columns
/// (as 0001 and 0002 have) are told apart.
fn schema_objects(conn: &Connection) -> Result<Vec<SchemaObject>> {
    let mut stmt = conn.prepare(
        "SELECT type, name FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
         ORDER BY type, name",
    )?;
    let names = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut table_cols = conn.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk
         FROM pragma_table_xinfo(?1) ORDER BY cid",
    )?;
    let mut index_cols = conn.prepare(
        "SELECT name, \"desc\", coll, key
         FROM pragma_index_xinfo(?1) ORDER BY seqno",
    )?;
    let mut objects = Vec::with_capacity(names.len());
    for (kind, name) in names {
        let columns = match kind.as_str() {
            "table" => table_cols
                .query_map([&name], |r| {
                    Ok(format!(
                        "{} {} notnull={} default={:?} pk={}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?,
            "index" => index_cols
                .query_map([&name], |r| {
                    Ok(format!(
                        "{:?} desc={} coll={:?} key={}",
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?,
            _ => Vec::new(),
        };
        objects.push((kind, name, columns));
    }
    Ok(objects)
}

/// The schema objects of a fresh in-memory database migrated to `version`.
fn reference_schema(migrations: &[Migration], version: u32) -> Result<Vec<SchemaObject>> {
    let upto = migrations
        .iter()
        .take_while(|m| m.version <= version)
        .count();
    let mut conn = Connection::open_in_memory()?;
    migrate(&mut conn, &migrations[..upto], 0)?;
    schema_objects(&conn)
}

/// Marker next to the database naming the `replaced-…` set a move is
/// filling, written before the first rename and removed after the last (#9).
const MOVING_ASIDE: &str = ".moving-aside";

fn moving_aside_path(db_path: &Path) -> PathBuf {
    suffixed(db_path, MOVING_ASIDE)
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The set an interrupted [`move_aside`] was filling, from the marker.
/// `None` without a marker.
///
/// # Errors
/// If the marker cannot be read, or does not name a `<db>.replaced-…` file
/// next to the database. A name from anywhere else is never followed.
fn read_moving_aside(db_path: &Path) -> Result<Option<PathBuf>> {
    let marker = moving_aside_path(db_path);
    let io_err = |source| Error::Io {
        path: marker.clone(),
        source,
    };
    let bytes = match fs::read(&marker) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_err(err)),
    };
    let prefix = format!("{DB_FILE_NAME}.replaced-");
    let name = String::from_utf8(bytes)
        .ok()
        .filter(|n| n.starts_with(&prefix) && !n.contains(['/', '\0']));
    match name {
        Some(name) => Ok(Some(db_path.with_file_name(name))),
        None => Err(io_err(io::Error::new(
            io::ErrorKind::InvalidData,
            "does not name a moved-aside database",
        ))),
    }
}

/// Records `stem` as the set being filled, durably.
fn write_moving_aside(db_path: &Path, stem: &Path) -> Result<()> {
    let marker = moving_aside_path(db_path);
    let io_err = |source| Error::Io {
        path: marker.clone(),
        source,
    };
    let name = stem.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io_err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "moved-aside name is not UTF-8",
        ))
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(&marker)
        .map_err(io_err)?;
    file.write_all(name.as_bytes()).map_err(io_err)?;
    file.sync_all().map_err(io_err)?;
    sync_parent(&marker)
}

/// Renames the database at `db_path` and its sidecar files, main file
/// first, to a free `<db>.replaced-YYYYMMDD-HHMMSS[-N]` name with mode
/// `0600`. Returns that name, or `None` if none of the files exist.
///
/// The chosen name is recorded first, so a crash between two renames does
/// not split the set: a retry moves what is left to the same name (#9).
/// Nothing is ever overwritten.
fn move_aside(db_path: &Path, now_ms: i64) -> Result<Option<PathBuf>> {
    move_aside_with(db_path, now_ms, &|from, to| fs::rename(from, to))
}

/// `move_aside` with the rename injected, so a test can fail it part-way.
fn move_aside_with(
    db_path: &Path,
    now_ms: i64,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Option<PathBuf>> {
    let mut present = Vec::new();
    for (path, suffix) in std::iter::once((db_path.to_path_buf(), ""))
        .chain(sidecar_paths(db_path).into_iter().zip(SIDECAR_SUFFIXES))
    {
        if exists(&path)? {
            present.push((path, suffix));
        }
    }
    let recorded = read_moving_aside(db_path)?;
    if present.is_empty() {
        // Either nothing to move, or an earlier move finished but for
        // removing its marker.
        remove_moving_aside(db_path)?;
        return Ok(recorded);
    }

    // A recorded set is continued unless it cannot be one: a main file is
    // here again although the recorded set already has one, so the marker is
    // left over from a finished move and a new database came after it.
    let main_present = present.iter().any(|(_, suffix)| suffix.is_empty());
    let stem = match recorded {
        Some(stem) if !(main_present && exists(&stem)?) => stem,
        _ => {
            let stem = free_stem(db_path, now_ms)?;
            write_moving_aside(db_path, &stem)?;
            stem
        }
    };
    for (path, suffix) in present {
        let target = suffixed(&stem, suffix);
        let io_err = |source| Error::Io {
            path: path.clone(),
            source,
        };
        if exists(&target)? {
            return Err(io_err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the moved-aside name is taken",
            )));
        }
        rename(&path, &target).map_err(io_err)?;
        // Only regular files; never follow a symlink to chmod its target.
        if fs::symlink_metadata(&target)
            .map_err(io_err)?
            .file_type()
            .is_file()
        {
            fs::set_permissions(&target, Permissions::from_mode(FILE_MODE)).map_err(io_err)?;
        }
    }
    sync_parent(db_path)?;
    remove_moving_aside(db_path)?;
    Ok(Some(stem))
}

fn remove_moving_aside(db_path: &Path) -> Result<()> {
    let marker = moving_aside_path(db_path);
    match fs::remove_file(&marker) {
        Ok(()) => sync_parent(&marker),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: marker,
            source,
        }),
    }
}

/// `<db>.replaced-<stamp>`, or `<db>.replaced-<stamp>-N` with the smallest
/// N ≥ 2, such that neither it nor any of its sidecar names exist.
fn free_stem(db_path: &Path, now_ms: i64) -> Result<PathBuf> {
    free_name(db_path, ".replaced-", now_ms)
}

/// Moves the unusable copy `partial` to a free
/// `rappel.sqlite3.restore.refused-<stamp>[-N]`, never over a file. A hard
/// link fails if the name was taken in the meantime, and then the next free
/// name is tried; the copy's old name is removed only once the new one
/// exists. Where the file system refuses hard links (`Unsupported` or
/// `PermissionDenied`, as some do), it falls back to a rename to the free
/// name, which could only replace a file created there between the check
/// and the rename; any other error is returned.
fn set_aside_copy(data_dir: &Path, partial: &Path, now_ms: i64) -> Result<PathBuf> {
    let stem = data_dir.join(format!("{DB_FILE_NAME}.restore"));
    let io_err = |source| Error::Io {
        path: partial.to_path_buf(),
        source,
    };
    for _ in 0..100 {
        let target = free_name(&stem, ".refused-", now_ms)?;
        match fs::hard_link(partial, &target) {
            Ok(()) => {
                fs::remove_file(partial).map_err(io_err)?;
                sync_parent(&target)?;
                return Ok(target);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            // Only where the file system refuses hard links; any other
            // error is returned as it is.
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
                ) =>
            {
                tracing::debug!("no hard link for the restore copy, renaming it: {err}");
                fs::rename(partial, &target).map_err(io_err)?;
                sync_parent(&target)?;
                return Ok(target);
            }
            Err(err) => return Err(io_err(err)),
        }
    }
    Err(io_err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name to set the restore copy aside",
    )))
}

/// `<path><infix><stamp>`, or `<path><infix><stamp>-N` with the smallest
/// N ≥ 2, such that neither it nor any of its sidecar names exist.
fn free_name(path: &Path, infix: &str, now_ms: i64) -> Result<PathBuf> {
    let mut base = path.as_os_str().to_owned();
    base.push(infix);
    base.push(utc_stamp(now_ms));
    let mut candidate = PathBuf::from(&base);
    let mut counter = 2;
    loop {
        let mut taken = exists(&candidate)?;
        for p in sidecar_paths(&candidate) {
            taken |= exists(&p)?;
        }
        if !taken {
            return Ok(candidate);
        }
        let mut next = base.clone();
        next.push(format!("-{counter}"));
        candidate = PathBuf::from(next);
        counter += 1;
    }
}

/// Renames `partial` to `db_path` once no database file or sidecar is left
/// there, and makes the rename durable.
fn install(partial: &Path, db_path: &Path) -> Result<()> {
    for p in std::iter::once(db_path.to_path_buf()).chain(sidecar_paths(db_path)) {
        if exists(&p)? {
            return Err(Error::Io {
                path: p,
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "appeared while restoring a backup",
                ),
            });
        }
    }
    fs::rename(partial, db_path).map_err(|source| Error::Io {
        path: partial.to_path_buf(),
        source,
    })?;
    sync_parent(db_path)
}

/// Syncs the directory holding `path`, so renames in it survive a crash.
fn sync_parent(path: &Path) -> Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })
}

/// Removes the copy being checked and any sidecar files next to it. Used on
/// error paths before anything else was changed, where the original error
/// matters more than a failed cleanup.
fn remove_copy(copy: &Path) {
    for p in std::iter::once(copy.to_path_buf()).chain(sidecar_paths(copy)) {
        if let Err(err) = fs::remove_file(&p)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %p.display(), "could not remove: {err}");
        }
    }
}

#[cfg(test)]
mod tests;
