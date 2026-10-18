//! Database backups with the SQLite online backup API.
//!
//! Backups are named `rappel-YYYYMMDD-HHMMSS.sqlite3` (UTC), with `-2`, `-3`…
//! appended if a name is taken. Each is written to `<name>.partial`, checked
//! with `PRAGMA quick_check`, synced and only then renamed, so a file with
//! the final name is always a complete, checked backup.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use crate::data_dir::ensure_data_dir;
use crate::error::{Error, Result};
use crate::time::{UtcParts, utc_ms, utc_parts};

use super::BACKUP_DIR_NAME;

/// How many UTC days, today included, keep their newest backup (SPEC §3,
/// ADR 0026).
pub(crate) const KEEP_DAYS: i64 = 14;

const DAY_MS: i64 = 86_400_000;

const PREFIX: &str = "rappel-";
const EXTENSION: &str = ".sqlite3";
const PARTIAL: &str = ".partial";
const FILE_MODE: u32 = 0o600;

/// `rappel-YYYYMMDD-` for the UTC day of `now_ms`.
fn day_prefix(now_ms: i64) -> String {
    let t = utc_parts(now_ms);
    format!("{PREFIX}{:04}{:02}{:02}-", t.year, t.month, t.day)
}

/// `YYYYMMDD-HHMMSS` for `now_ms` (UTC).
pub(crate) fn utc_stamp(now_ms: i64) -> String {
    let t = utc_parts(now_ms);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// `rappel-YYYYMMDD-HHMMSS` for `now_ms`, without extension.
fn backup_stem(now_ms: i64) -> String {
    format!("{PREFIX}{}", utc_stamp(now_ms))
}

/// Parses a backup file name into a sort key: the `YYYYMMDD-HHMMSS` part and
/// the collision counter (1 when there is no `-N` suffix). `None` for any
/// other file, so pruning never touches files it did not create.
pub(crate) fn parse_backup_name(name: &str) -> Option<(&str, u32)> {
    let rest = name.strip_prefix(PREFIX)?.strip_suffix(EXTENSION)?;
    let (stamp, counter) = match rest.split_at_checked(15) {
        Some((stamp, "")) => (stamp, 1),
        Some((stamp, suffix)) => {
            let n: u32 = suffix.strip_prefix('-')?.parse().ok()?;
            // `-1` is never generated, and the suffix must be plain digits.
            if n < 2 || !suffix[1..].bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (stamp, n)
        }
        None => return None,
    };
    let bytes = stamp.as_bytes();
    let well_formed = bytes.len() == 15
        && bytes[8] == b'-'
        && bytes[..8].iter().chain(&bytes[9..]).all(u8::is_ascii_digit);
    well_formed.then_some((stamp, counter))
}

/// File names in `dir` that are backups, oldest first. A missing directory
/// has no backups.
fn backup_names(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        if let Some(name) = entry.file_name().to_str()
            && parse_backup_name(name).is_some()
        {
            names.push(name.to_owned());
        }
    }
    names.sort_by(|a, b| parse_backup_name(a).cmp(&parse_backup_name(b)));
    Ok(names)
}

/// The first 16 bytes of every SQLite database file.
pub(super) const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// A backup file found by [`list_backups`].
///
/// Only [`list_backups`] makes these, so a `BackupInfo` always names a file
/// in the backups directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInfo {
    path: PathBuf,
    created_ms: i64,
    size_bytes: u64,
}

impl BackupInfo {
    /// Full path of the backup file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// When the backup was made, in UTC milliseconds, read from its file
    /// name (to the second).
    #[must_use]
    pub fn created_ms(&self) -> i64 {
        self.created_ms
    }

    /// File size in bytes.
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
}

/// The backups in `<data_dir>/backups/`, newest first.
///
/// Only well-named regular files (not symlinks) that start with the SQLite
/// header are listed. This is a cheap filter; the full check (integrity,
/// version, schema) happens in [`super::restore_backup`]. A file that cannot
/// be read is skipped with a warning. A missing directory gives an empty
/// list.
///
/// # Errors
/// If the backups directory exists but cannot be read.
pub fn list_backups(data_dir: &Path) -> Result<Vec<BackupInfo>> {
    let dir = data_dir.join(BACKUP_DIR_NAME);
    let mut found = Vec::new();
    for name in backup_names(&dir)?.into_iter().rev() {
        let path = dir.join(&name);
        let Some(created_ms) = parse_backup_name(&name).and_then(|(stamp, _)| stamp_ms(stamp))
        else {
            continue;
        };
        match inspect_backup_file(&path) {
            Ok(Some(size_bytes)) => found.push(BackupInfo {
                path,
                created_ms,
                size_bytes,
            }),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(path = %path.display(), "skipping unreadable backup: {err}");
            }
        }
    }
    Ok(found)
}

/// The size of `path` if it is a regular file (not a symlink) starting with
/// the SQLite header, `None` otherwise.
fn inspect_backup_file(path: &Path) -> std::io::Result<Option<u64>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Ok(None);
    }
    let mut header = [0u8; 16];
    match File::open(path)?.read_exact(&mut header) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    Ok((&header == SQLITE_HEADER).then_some(meta.len()))
}

/// UTC milliseconds for a `YYYYMMDD-HHMMSS` stamp (already checked to be
/// digits), or `None` if it is not a real date and time.
fn stamp_ms(stamp: &str) -> Option<i64> {
    let field = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    utc_ms(UtcParts {
        year: field(0..4)?,
        month: field(4..6)?,
        day: field(6..8)?,
        hour: field(9..11)?,
        minute: field(11..13)?,
        second: field(13..15)?,
    })
}

/// Whether `dir` holds a backup from the UTC day of `now_ms`.
pub(crate) fn has_backup_for_day(dir: &Path, now_ms: i64) -> Result<bool> {
    let prefix = day_prefix(now_ms);
    Ok(backup_names(dir)?.iter().any(|n| n.starts_with(&prefix)))
}

/// Copies the database behind `conn` into a new backup in `dir` (created
/// with mode `0700` if needed). The backup file has mode `0600` and uses a
/// rollback journal, so it is a single self-contained file.
///
/// Returns the path of the finished backup.
pub(crate) fn create_backup(conn: &Connection, dir: &Path, now_ms: i64) -> Result<PathBuf> {
    ensure_data_dir(dir)?;
    let stem = backup_stem(now_ms);
    let mut path = dir.join(format!("{stem}{EXTENSION}"));
    let mut counter = 2;
    while path.try_exists().map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })? {
        path = dir.join(format!("{stem}-{counter}{EXTENSION}"));
        counter += 1;
    }
    let partial = partial_path(&path);

    let result = write_backup(conn, &partial, &path);
    if result.is_err() {
        // Best effort: the error being returned matters more than a failed
        // cleanup, and pruning removes leftovers next time.
        let _ = fs::remove_file(&partial);
    }
    result.map(|()| path)
}

fn partial_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(PARTIAL);
    PathBuf::from(name)
}

fn write_backup(conn: &Connection, partial: &Path, path: &Path) -> Result<()> {
    let io_err = |source| Error::Io {
        path: partial.to_path_buf(),
        source,
    };
    let sql_err = |source| Error::Backup {
        path: path.to_path_buf(),
        source,
    };

    // A leftover from a crash is replaced. Created first so SQLite opens an
    // existing 0600 file instead of creating one with its default mode.
    match fs::remove_file(partial) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(io_err(err)),
        _ => {}
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(partial)
        .map_err(io_err)?;

    {
        let mut dest = Connection::open_with_flags(
            partial,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql_err)?;
        run_backup(conn, &mut dest).map_err(sql_err)?;
        // The copied header keeps the source's WAL mode; a backup should be
        // one self-contained file.
        dest.pragma_update_and_check(None, "journal_mode", "DELETE", |row| {
            row.get::<_, String>(0)
        })
        .map_err(sql_err)?;
        let messages = check_rows(&dest, "quick_check").map_err(sql_err)?;
        if messages != ["ok"] {
            return Err(Error::BackupCheckFailed {
                path: path.to_path_buf(),
                messages,
            });
        }
        dest.close().map_err(|(_, err)| sql_err(err))?;
    }

    File::open(partial)
        .and_then(|f| f.sync_all())
        .map_err(io_err)?;
    fs::rename(partial, path).map_err(io_err)?;
    // Make the rename itself durable.
    if let Some(dir) = path.parent() {
        File::open(dir).and_then(|d| d.sync_all()).map_err(io_err)?;
    }
    Ok(())
}

/// Pages copied per backup step: all of them (`-1`). SQLite restarts an
/// online backup whenever another connection writes to the source between
/// two steps, so with a worker's connection backing up while the main one
/// writes (ADR 0018), small steps could restart without end. One step reads
/// one WAL snapshot, which never blocks the writers and cannot restart.
const PAGES_PER_STEP: i32 = -1;
/// Wait between retries when the source is locked.
const RETRY_PAUSE: Duration = Duration::from_millis(50);
/// Give up after this many locked steps in a row (about 5 s).
const MAX_RETRIES: u32 = 100;

/// Copies `src` into `dest` with the online backup API. Unlike
/// `Backup::run_to_completion`, it gives up with `SQLITE_BUSY` instead of
/// retrying forever if another connection keeps the source locked.
fn run_backup(src: &Connection, dest: &mut Connection) -> rusqlite::Result<()> {
    let backup = Backup::new(src, dest)?;
    let mut retries = 0;
    loop {
        match backup.step(PAGES_PER_STEP)? {
            StepResult::Done => return Ok(()),
            StepResult::More => retries = 0,
            // Busy, Locked, and any future "try again" result.
            _ => {
                retries += 1;
                if retries > MAX_RETRIES {
                    return Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        Some("database stayed locked during backup".into()),
                    ));
                }
                std::thread::sleep(RETRY_PAUSE);
            }
        }
    }
}

/// Runs `PRAGMA <pragma>` (`integrity_check` or `quick_check`) and returns
/// its rows. A healthy database returns exactly `["ok"]`.
pub(crate) fn check_rows(conn: &Connection, pragma: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA {pragma}"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Which of `names` (backup file names, oldest first) [`prune_backups`]
/// keeps (SPEC §3, ADR 0026). A backup is kept if any of these holds:
/// 1. it was made in the last 24 hours (every import and migration backup
///    of today stays);
/// 2. it is the newest of its UTC day, for each of the last [`KEEP_DAYS`]
///    days (today included) that has one;
/// 3. it is in `protect` (the backup just made, the backup just restored);
/// 4. it is dated after `now_ms` (the clock went backwards), and is the
///    newest of its UTC day, for each of the [`KEEP_DAYS`] latest such days:
///    the days a clock that ran ahead made backups on keep one each, as
///    past days do, and however far ahead it was they are a bounded number
///    beside the real days (#50; before, only the newest was kept);
/// 5. it is the newest backup that is not dated in the future, whatever its
///    age: after a long pause the last copy before it is never pruned;
/// 6. its name holds no real date, so it is not understood and not ours to
///    delete.
fn backups_to_keep(names: &[String], now_ms: i64, protect: &[&str]) -> HashSet<String> {
    let today = now_ms.div_euclid(DAY_MS);
    let mut keep: HashSet<String> = protect.iter().map(|&n| n.to_owned()).collect();
    let mut newest_of_day: BTreeMap<i64, &String> = BTreeMap::new();
    let mut newest_of_future_day: BTreeMap<i64, &String> = BTreeMap::new();
    // Oldest first, so a later name replaces an earlier one of its day.
    for name in names {
        let Some(t) = parse_backup_name(name).and_then(|(stamp, _)| stamp_ms(stamp)) else {
            keep.insert(name.clone());
            continue;
        };
        if t > now_ms {
            newest_of_future_day.insert(t.div_euclid(DAY_MS), name);
        } else {
            if now_ms - t < DAY_MS {
                keep.insert(name.clone());
            }
            newest_of_day.insert(t.div_euclid(DAY_MS), name);
        }
    }
    keep.extend(
        newest_of_future_day
            .values()
            .rev()
            .take(usize::try_from(KEEP_DAYS).unwrap_or(usize::MAX))
            .map(|&n| n.clone()),
    );
    keep.extend(newest_of_day.values().next_back().map(|&n| n.clone()));
    keep.extend(
        newest_of_day
            .into_iter()
            .filter(|&(day, _)| today - day < KEEP_DAYS)
            .map(|(_, name)| name.clone()),
    );
    keep
}

/// Deletes the backups in `dir` that [`backups_to_keep`] does not keep, and
/// any leftover `.partial` files. Other files are never touched.
///
/// `protect` lists backups that stay whatever their age: the one just made
/// (without this, a clock that was ahead earlier could make it look old and
/// have it deleted right after it was made) and, after a restore, the one
/// restored.
pub(crate) fn prune_backups(dir: &Path, now_ms: i64, protect: &[&Path]) -> Result<()> {
    let protected: Vec<&str> = protect
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    let names = backup_names(dir)?;
    let keep = backups_to_keep(&names, now_ms, &protected);
    let mut doomed: Vec<PathBuf> = names
        .iter()
        .filter(|n| !keep.contains(*n))
        .map(|n| dir.join(n))
        .collect();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && let Some(base) = name.strip_suffix(PARTIAL)
                && parse_backup_name(base).is_some()
            {
                doomed.push(entry.path());
            }
        }
    }
    for path in doomed {
        fs::remove_file(&path).map_err(|source| Error::Io { path, source })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// 2026-10-15T12:34:56.789Z
    const NOW: i64 = 1_792_067_696_789;

    #[test]
    fn names_use_utc_date_and_time() {
        assert_eq!(backup_stem(NOW), "rappel-20261015-123456");
        assert_eq!(day_prefix(NOW), "rappel-20261015-");
    }

    #[test]
    fn parses_only_backup_names() {
        assert_eq!(
            parse_backup_name("rappel-20261015-123456.sqlite3"),
            Some(("20261015-123456", 1))
        );
        assert_eq!(
            parse_backup_name("rappel-20261015-123456-3.sqlite3"),
            Some(("20261015-123456", 3))
        );
        for bad in [
            "rappel.sqlite3",
            "rappel-20261015-123456.sqlite3.partial",
            "rappel-20261015-123456-1.sqlite3",
            "rappel-20261015-123456-+2.sqlite3",
            "rappel-20261015-123456-x.sqlite3",
            "rappel-2026101-1234567.sqlite3",
            "rappel-20261015_123456.sqlite3",
            "notes.txt",
            "rappel-bad.sqlite3",
        ] {
            assert_eq!(parse_backup_name(bad), None, "{bad}");
        }
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"").unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn collision_suffix_sorts_after_the_plain_name() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "rappel-20261015-123456-2.sqlite3");
        touch(tmp.path(), "rappel-20261015-123456.sqlite3");
        touch(tmp.path(), "rappel-20261014-000000.sqlite3");
        assert_eq!(
            backup_names(tmp.path()).unwrap(),
            [
                "rappel-20261014-000000.sqlite3",
                "rappel-20261015-123456.sqlite3",
                "rappel-20261015-123456-2.sqlite3",
            ]
        );
    }

    /// The file name of a backup made at `ms`.
    fn name_at(ms: i64) -> String {
        format!("{}{EXTENSION}", backup_stem(ms))
    }

    /// 2026-10-<day>T<hour>:00:00Z.
    fn oct(day: i64, hour: i64) -> i64 {
        utc_ms(UtcParts {
            year: 2026,
            month: 10,
            day,
            hour,
            minute: 0,
            second: 0,
        })
        .unwrap()
    }

    #[test]
    fn prune_keeps_the_newest_of_each_of_the_last_fourteen_days() {
        let tmp = tempfile::tempdir().unwrap();
        // Today is Oct 15, so the window is Oct 2 to Oct 15.
        for day in 1..=15 {
            touch(tmp.path(), &name_at(oct(day, 12)));
        }
        // Earlier the same day as another: not the newest of its day.
        touch(tmp.path(), &name_at(oct(10, 8)));
        touch(tmp.path(), "notes.txt");
        touch(tmp.path(), "rappel-bad.sqlite3");
        touch(tmp.path(), "rappel-20261017-120000.sqlite3.partial");

        prune_backups(tmp.path(), NOW, &[]).unwrap();

        let mut expected: Vec<String> = (2..=15).map(|day| name_at(oct(day, 12))).collect();
        expected.push("rappel-bad.sqlite3".into());
        expected.push("notes.txt".into());
        expected.sort();
        assert_eq!(names(tmp.path()), expected);
    }

    #[test]
    fn prune_keeps_every_backup_of_the_last_24_hours() {
        let tmp = tempfile::tempdir().unwrap();
        // NOW is Oct 15 12:34. An import-heavy afternoon, and the evening
        // before, all inside 24 h.
        let recent = [oct(14, 13), oct(14, 14), oct(15, 1), oct(15, 2), oct(15, 3)];
        for t in recent {
            touch(tmp.path(), &name_at(t));
        }
        // 25 h ago, not the newest of its day: pruned.
        touch(tmp.path(), &name_at(oct(14, 11)));
        touch(tmp.path(), &name_at(oct(14, 11) + 60_000));

        prune_backups(tmp.path(), NOW, &[]).unwrap();

        let mut expected: Vec<String> = recent.iter().map(|&t| name_at(t)).collect();
        // The newest of Oct 14 is among `recent`; of the two older ones none
        // is, and neither is within 24 h.
        expected.sort();
        assert_eq!(names(tmp.path()), expected);
    }

    #[test]
    fn prune_counts_days_in_utc() {
        let tmp = tempfile::tempdir().unwrap();
        // Today is Oct 15: Oct 2 23:59:59 is in the window, Oct 1 23:59:59
        // is not, though both are about two weeks old.
        touch(tmp.path(), "rappel-20261002-235959.sqlite3");
        touch(tmp.path(), "rappel-20261001-235959.sqlite3");

        prune_backups(tmp.path(), NOW, &[]).unwrap();

        assert_eq!(names(tmp.path()), ["rappel-20261002-235959.sqlite3"]);
    }

    #[test]
    fn prune_keeps_fourteen_future_days_and_the_protected_one() {
        let tmp = tempfile::tempdir().unwrap();
        // 16 days of backups from a clock that was years ahead, two on the
        // last of them.
        for day in 1..=16 {
            touch(tmp.path(), &format!("rappel-203001{day:02}-120000.sqlite3"));
        }
        touch(tmp.path(), "rappel-20300116-080000.sqlite3");
        let fresh = tmp.path().join("rappel-20261015-123456.sqlite3");
        touch(tmp.path(), "rappel-20261015-123456.sqlite3");

        prune_backups(tmp.path(), NOW, &[&fresh]).unwrap();

        // The newest of each of the 14 latest future days (#50; before,
        // only Jan 16 12:00 was kept), and the protected one.
        let mut expected: Vec<String> = (3..=16)
            .map(|day| format!("rappel-203001{day:02}-120000.sqlite3"))
            .collect();
        expected.push("rappel-20261015-123456.sqlite3".into());
        expected.sort();
        assert_eq!(names(tmp.path()), expected);
    }

    #[test]
    fn prune_never_deletes_a_protected_backup_however_old() {
        let tmp = tempfile::tempdir().unwrap();
        // The oldest backup, restored just now (#8).
        let restored = tmp.path().join("rappel-20260101-120000.sqlite3");
        touch(tmp.path(), "rappel-20260101-120000.sqlite3");
        touch(tmp.path(), "rappel-20260102-120000.sqlite3");
        touch(tmp.path(), "rappel-20260103-120000.sqlite3");

        prune_backups(tmp.path(), NOW, &[&restored]).unwrap();

        // Not the middle one; the newest is always kept.
        assert_eq!(
            names(tmp.path()),
            [
                "rappel-20260101-120000.sqlite3",
                "rappel-20260103-120000.sqlite3"
            ]
        );
    }

    #[test]
    fn prune_keeps_the_newest_backup_after_a_long_pause() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "rappel-20260801-120000.sqlite3");
        touch(tmp.path(), "rappel-20260802-120000.sqlite3");

        prune_backups(tmp.path(), NOW, &[]).unwrap();

        assert_eq!(names(tmp.path()), ["rappel-20260802-120000.sqlite3"]);
    }

    #[test]
    fn prune_leaves_a_name_with_no_real_date_alone() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "rappel-20261340-250000.sqlite3");

        prune_backups(tmp.path(), NOW, &[]).unwrap();

        assert_eq!(names(tmp.path()), ["rappel-20261340-250000.sqlite3"]);
    }

    #[test]
    fn prune_on_missing_dir_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        prune_backups(&tmp.path().join("none"), NOW, &[]).unwrap();
    }

    mod property {
        use super::*;
        use proptest::prelude::*;
        use std::collections::BTreeSet;

        /// Backup times from 40 days before `NOW` to 5 days after it, a few
        /// of them within seconds of each other, and some from a clock that
        /// ran up to 40 days ahead.
        fn times() -> impl Strategy<Value = Vec<i64>> {
            let time = prop_oneof![
                4 => NOW - 40 * DAY_MS..NOW + 5 * DAY_MS,
                1 => NOW..NOW + 40 * DAY_MS,
            ];
            prop::collection::vec(time, 0..60)
        }

        /// The rule before #50, which kept only the newest future backup.
        /// The rule now must keep everything it kept: a retention change
        /// may only ever keep more.
        fn kept_before_50(names: &[String], now_ms: i64, protect: &[&str]) -> HashSet<String> {
            let today = now_ms.div_euclid(DAY_MS);
            let mut keep: HashSet<String> = protect.iter().map(|&n| n.to_owned()).collect();
            let mut newest_of_day: BTreeMap<i64, &String> = BTreeMap::new();
            let mut newest_future: Option<&String> = None;
            for name in names {
                let t = stamp_ms(parse_backup_name(name).unwrap().0).unwrap();
                if t > now_ms {
                    newest_future = Some(name);
                } else {
                    if now_ms - t < DAY_MS {
                        keep.insert(name.clone());
                    }
                    newest_of_day.insert(t.div_euclid(DAY_MS), name);
                }
            }
            keep.extend(newest_future.cloned());
            keep.extend(newest_of_day.values().next_back().map(|&n| n.clone()));
            keep.extend(
                newest_of_day
                    .into_iter()
                    .filter(|&(day, _)| today - day < KEEP_DAYS)
                    .map(|(_, name)| name.clone()),
            );
            keep
        }

        fn sorted_names(times: &[i64]) -> Vec<String> {
            let set: BTreeSet<String> = times.iter().map(|&t| name_at(t)).collect();
            let mut names: Vec<String> = set.into_iter().collect();
            names.sort_by(|a, b| parse_backup_name(a).cmp(&parse_backup_name(b)));
            names
        }

        proptest! {
            #[test]
            fn the_rule_is_safe_bounded_and_stable(times in times(), pick in any::<prop::sample::Index>()) {
                let names = sorted_names(&times);
                let protect: Vec<&str> = if names.is_empty() {
                    vec![]
                } else {
                    vec![names[pick.index(names.len())].as_str()]
                };
                let keep = backups_to_keep(&names, NOW, &protect);

                // Never removes the newest, nor a protected one.
                if let Some(newest) = names.last() {
                    prop_assert!(keep.contains(newest));
                }
                for p in &protect {
                    prop_assert!(keep.contains(*p));
                }

                // Everything the rule before #50 kept is kept.
                let before = kept_before_50(&names, NOW, &protect);
                prop_assert!(before.is_subset(&keep), "{:?}", before.difference(&keep));

                // Never more than 14 days + the last 24 h + protected + the
                // newest past one + 14 future days.
                let recent = names
                    .iter()
                    .filter(|n| {
                        let t = stamp_ms(parse_backup_name(n).unwrap().0).unwrap();
                        t <= NOW && NOW - t < DAY_MS
                    })
                    .count();
                prop_assert!(keep.len() <= 2 * usize::try_from(KEEP_DAYS).unwrap() + recent + 2);

                // Each of the last 14 days with a backup keeps one.
                for day in 0..KEEP_DAYS {
                    let d = NOW.div_euclid(DAY_MS) - day;
                    let has = names.iter().any(|n| {
                        let t = stamp_ms(parse_backup_name(n).unwrap().0).unwrap();
                        t <= NOW && t.div_euclid(DAY_MS) == d
                    });
                    let kept = keep.iter().any(|n| {
                        let t = stamp_ms(parse_backup_name(n).unwrap().0).unwrap();
                        t <= NOW && t.div_euclid(DAY_MS) == d
                    });
                    prop_assert_eq!(has, kept);
                }

                // Each of the 14 latest future days with a backup keeps one.
                let mut future_days: Vec<i64> = names
                    .iter()
                    .map(|n| stamp_ms(parse_backup_name(n).unwrap().0).unwrap())
                    .filter(|&t| t > NOW)
                    .map(|t| t.div_euclid(DAY_MS))
                    .collect();
                future_days.sort_unstable();
                future_days.dedup();
                for &d in future_days.iter().rev().take(usize::try_from(KEEP_DAYS).unwrap()) {
                    let kept = keep.iter().any(|n| {
                        let t = stamp_ms(parse_backup_name(n).unwrap().0).unwrap();
                        t > NOW && t.div_euclid(DAY_MS) == d
                    });
                    prop_assert!(kept, "future day {}", d);
                }

                // Pruning twice changes nothing more.
                let left: Vec<String> =
                    names.iter().filter(|n| keep.contains(*n)).cloned().collect();
                prop_assert_eq!(backups_to_keep(&left, NOW, &protect).len(), left.len());
            }
        }
    }

    #[test]
    fn has_backup_for_day_matches_utc_day_only() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!has_backup_for_day(tmp.path(), NOW).unwrap());
        touch(tmp.path(), "rappel-20261014-235959.sqlite3");
        assert!(!has_backup_for_day(tmp.path(), NOW).unwrap());
        touch(tmp.path(), "rappel-20261015-000000.sqlite3");
        assert!(has_backup_for_day(tmp.path(), NOW).unwrap());
    }

    fn source_db(dir: &Path) -> Connection {
        let conn = Connection::open(dir.join("src.sqlite3")).unwrap();
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE t(x INTEGER) STRICT;
             INSERT INTO t VALUES (1), (2), (3);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn backup_contains_uncheckpointed_wal_rows_and_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = source_db(tmp.path());
        // The rows are still in the -wal file, not the main file.
        assert!(tmp.path().join("src.sqlite3-wal").exists());

        let backups = tmp.path().join("backups");
        let path = create_backup(&conn, &backups, NOW).unwrap();

        assert_eq!(path, backups.join("rappel-20261015-123456.sqlite3"));
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&backups), 0o700);

        let copy = Connection::open(&path).unwrap();
        let n: i64 = copy
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
        let mode: String = copy
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(copy);
        // Only the finished backup is left: no -wal, -journal or .partial.
        assert_eq!(names(&backups), ["rappel-20261015-123456.sqlite3"]);
    }

    #[test]
    fn second_backup_in_the_same_second_gets_a_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = source_db(tmp.path());
        let backups = tmp.path().join("backups");
        create_backup(&conn, &backups, NOW).unwrap();
        let second = create_backup(&conn, &backups, NOW).unwrap();
        assert_eq!(second, backups.join("rappel-20261015-123456-2.sqlite3"));
    }

    fn backup_file(dir: &Path, name: &str, len: usize) {
        let mut bytes = SQLITE_HEADER.to_vec();
        bytes.resize(len, 0);
        fs::write(dir.join(name), bytes).unwrap();
    }

    #[test]
    fn list_backups_newest_first_with_time_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(BACKUP_DIR_NAME);
        fs::create_dir(&dir).unwrap();
        backup_file(&dir, "rappel-20261014-000000.sqlite3", 100);
        backup_file(&dir, "rappel-20261015-123456.sqlite3", 200);
        backup_file(&dir, "rappel-20261015-123456-2.sqlite3", 300);

        let list = list_backups(tmp.path()).unwrap();
        let names: Vec<_> = list
            .iter()
            .map(|b| b.path().file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "rappel-20261015-123456-2.sqlite3",
                "rappel-20261015-123456.sqlite3",
                "rappel-20261014-000000.sqlite3",
            ]
        );
        assert_eq!(list[0].created_ms(), NOW - 789);
        assert_eq!(list[1].created_ms(), NOW - 789);
        assert_eq!(list[2].created_ms(), 1_791_936_000_000); // 2026-10-14T00:00:00Z
        assert_eq!(
            list.iter().map(BackupInfo::size_bytes).collect::<Vec<_>>(),
            [300, 200, 100]
        );
        assert_eq!(list[2].path(), dir.join("rappel-20261014-000000.sqlite3"));
    }

    #[test]
    fn list_backups_skips_everything_that_is_not_a_backup_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(BACKUP_DIR_NAME);
        fs::create_dir(&dir).unwrap();
        backup_file(&dir, "rappel-20261015-123456.sqlite3", 100);
        // Wrong names.
        backup_file(&dir, "notes.sqlite3", 100);
        backup_file(&dir, "rappel-20261016-123456.sqlite3.partial", 100);
        // Impossible dates.
        backup_file(&dir, "rappel-20261316-123456.sqlite3", 100);
        backup_file(&dir, "rappel-20260431-123456.sqlite3", 100);
        backup_file(&dir, "rappel-20261016-243456.sqlite3", 100);
        // Not an SQLite file, empty, a directory, a symlink.
        fs::write(dir.join("rappel-20261016-000001.sqlite3"), vec![0x42; 100]).unwrap();
        fs::write(dir.join("rappel-20261016-000002.sqlite3"), b"").unwrap();
        fs::create_dir(dir.join("rappel-20261016-000003.sqlite3")).unwrap();
        std::os::unix::fs::symlink(
            dir.join("rappel-20261015-123456.sqlite3"),
            dir.join("rappel-20261016-000004.sqlite3"),
        )
        .unwrap();

        let list = list_backups(tmp.path()).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].path(), dir.join("rappel-20261015-123456.sqlite3"));
    }

    #[test]
    fn list_backups_of_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(list_backups(tmp.path()).unwrap(), []);
    }

    #[test]
    fn stale_partial_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = source_db(tmp.path());
        let backups = tmp.path().join("backups");
        fs::create_dir(&backups).unwrap();
        fs::write(
            backups.join("rappel-20261015-123456.sqlite3.partial"),
            b"junk",
        )
        .unwrap();
        let path = create_backup(&conn, &backups, NOW).unwrap();
        assert_eq!(names(&backups), ["rappel-20261015-123456.sqlite3"]);
        Connection::open(path)
            .unwrap()
            .query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
            .unwrap();
    }
}
