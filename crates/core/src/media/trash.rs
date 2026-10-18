//! Unused media and the media trash (ADR 0057).
//!
//! A media file is **unused** when no card side names its hash: no card,
//! live or deleted, in a live or deleted deck ([`super::used`]). That is a
//! `media` row whose hash no card names, or a file in the media folder with
//! a media name (`<sha256>.<ext>`) and no row whose hash no card names (left
//! by a failed import or a crash). Nothing else in the folder is looked at:
//! not `.incoming-…` temporary files, not `.damaged-…` copies kept aside
//! (ADR 0054), not folders, links or other names.
//!
//! **Clean up** moves the unused files into a dated folder of the trash,
//! `<data dir>/media-trash/<YYYY-MM-DD>/<sha256>.<ext>` (the UTC date of the
//! clean-up; `<YYYY-MM-DD>-<N>` when that name is taken there already), and
//! deletes their rows, in one write transaction: the files are moved
//! (renames, never over a file) before the rows are deleted, and if
//! anything fails, the rows stay and the files are moved back. A crash
//! between the moves and the commit leaves rows whose files are in the
//! trash: the next [`Store::open`] moves them back ([`recover_rows`]).
//!
//! **Brought back**: a file in the trash that a database uses again (its
//! row, or a card naming it) goes back to the media folder, with a row made
//! from its name and size if it has none. That happens at every open for
//! rows (the crash above, a restored backup), after a restore and an
//! import, and at startup for card sides too ([`recover_all`]), and when a
//! card is written for the files it names ([`recover_refs`]).
//!
//! **Purge**: a dated folder's files are deleted once the clock is 31 days
//! past the start of its date (so every file in it spent at least 30 days
//! there) and the database records a write 30 days past it ([`purgeable`]),
//! unless a backup, a copy of the database kept in the data folder,
//! or the database itself uses them. A backup that cannot be read keeps
//! everything. [`purge`] holds the only `remove_file` on user media in
//! Rappel.

use std::collections::HashSet;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};

use super::store::{Ingested, has_content};
use super::used::{Hashes, card_hashes, may_name_media, used_hashes};
use super::{MediaKind, MediaRef, parse_hex, sync_folder};
use crate::content::media_refs;
use crate::error::{Error, Result};
use crate::storage::{Store, backup_names};
use crate::time::{UtcParts, now_ms, utc_ms, utc_parts};

/// The trash's name inside the data directory.
pub(crate) const TRASH_DIR_NAME: &str = "media-trash";

/// How many days a file stays in the trash at least.
pub const PURGE_AFTER_DAYS: i64 = 30;

const DAY_MS: i64 = 86_400_000;

const DIR_MODE: u32 = 0o700;

/// The most folders one UTC day can have (`<date>` and `<date>-2` to
/// `<date>-999`): a name is taken in a day's folder only when the same
/// file was cleaned up, added again and cleaned up again that day.
const MAX_FOLDERS_A_DAY: u32 = 999;

/// No clean-up can happen before Rappel had one (October 2026): a clock
/// set earlier is wrong, so a clean-up then is refused, and a folder
/// dated earlier is never purged (it would look older than it is).
const EARLIEST_MS: i64 = 1_790_812_800_000; // 2026-10-01T00:00:00Z

/// The latest date a folder name can have (a four-digit year).
const LATEST_MS: i64 = 253_402_214_400_000; // 9999-12-31T00:00:00Z

/// What [`Store::unused_media`] found: how many files a clean-up would
/// move to the trash, and their size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnusedMedia {
    /// Unused files in the media folder.
    pub files: usize,
    /// Their size, in bytes.
    pub bytes: u64,
}

/// What [`Store::clean_up_media`] did, and what [`Store::undo_clean_up`]
/// needs to put it back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanedUp {
    /// The files moved to the trash.
    pub files: usize,
    /// Their size, in bytes.
    pub bytes: u64,
    moved: Vec<Moved>,
    rows: Vec<MediaRow>,
}

/// What [`Store::tidy_media_trash`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrashTidied {
    /// Files brought back from the trash, because the database uses them.
    pub recovered: usize,
    /// Files deleted from the trash.
    pub purged: usize,
    /// Their size, in bytes.
    pub purged_bytes: u64,
    /// Files old enough to go that a backup (or the database) still uses.
    pub kept: usize,
    /// Nothing was purged because a backup could not be read.
    pub blocked: bool,
}

/// A file moved to the trash.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Moved {
    media: MediaRef,
    bytes: u64,
    /// Where it is in the trash.
    to: PathBuf,
}

/// A row of the `media` table, as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaRow {
    hash: String,
    mime: String,
    bytes: i64,
    created_at: i64,
}

impl MediaRow {
    /// The file's name in the media folder, if the row names a kind.
    fn file_name(&self) -> Option<String> {
        let kind = MediaKind::from_mime(&self.mime)?;
        Some(format!("{}.{}", self.hash, kind.extension()))
    }
}

/// An unused file in the media folder.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnusedFile {
    media: MediaRef,
    bytes: u64,
}

/// What a clean-up would do: move these files and delete these rows.
#[derive(Debug, Default)]
struct Unused {
    files: Vec<UnusedFile>,
    rows: Vec<MediaRow>,
}

/// The renames a clean-up and a recovery make, so tests can make them fail.
pub(super) trait Mover {
    /// Renames `from` to `to`. The caller checked that `to` is free.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// The real file system.
pub(super) struct Disk;

impl Mover for Disk {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
}

/// A test's hook just before the clean-up commits.
pub(super) type BeforeCommit<'a> = &'a dyn Fn() -> Result<()>;

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What is at `path`, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Found {
    Nothing,
    /// A regular file of this size.
    File(u64),
    /// A folder, a link or anything else.
    Other,
}

fn found(path: &Path) -> Result<Found> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(Found::File(meta.len())),
        Ok(_) => Ok(Found::Other),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Found::Nothing),
        Err(err) => Err(io_err(path)(err)),
    }
}

/// Whether `path` is a real folder (not a link to one).
fn is_real_dir(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_dir()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(io_err(path)(err)),
    }
}

/// The names in `dir`, as UTF-8 (others are not Rappel's). A missing
/// folder has none.
fn names_in(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_err(dir)(err)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_err(dir))?;
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// The media files in `dir` (the media folder or a trash folder): regular
/// files with a media name, and their sizes, sorted by name. Anything else
/// is left out and never touched.
fn media_files(dir: &Path) -> Result<Vec<(MediaRef, u64)>> {
    let mut out = Vec::new();
    for name in names_in(dir)? {
        let Some(media) = MediaRef::from_file_name(&name) else {
            continue;
        };
        if let Found::File(bytes) = found(&dir.join(&name))? {
            out.push((media, bytes));
        }
    }
    Ok(out)
}

/// `YYYY-MM-DD` for the UTC day of `ms`, with `-N` for `n` from 2.
pub(super) fn folder_name(ms: i64, n: u32) -> String {
    let t = utc_parts(ms);
    let date = format!("{:04}-{:02}-{:02}", t.year, t.month, t.day);
    if n < 2 { date } else { format!("{date}-{n}") }
}

/// The start of the UTC day a trash folder's name gives: `YYYY-MM-DD`, or
/// `YYYY-MM-DD-N` with `N` from 2 to 999 without a leading zero. `None` for
/// any other name.
pub(super) fn folder_day(name: &str) -> Option<i64> {
    let (date, suffix) = match name.split_at_checked(10) {
        Some((date, "")) => (date, None),
        Some((date, rest)) => (date, Some(rest.strip_prefix('-')?)),
        None => return None,
    };
    if let Some(n) = suffix {
        let ok = !n.is_empty()
            && n.len() <= 3
            && !n.starts_with('0')
            && n.bytes().all(|b| b.is_ascii_digit())
            && n.parse::<u32>().is_ok_and(|n| n >= 2);
        if !ok {
            return None;
        }
    }
    let b = date.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if !(b[4] == b'-' && b[7] == b'-' && digits(0..4) && digits(5..7) && digits(8..10)) {
        return None;
    }
    let field = |r: std::ops::Range<usize>| date[r].parse::<i64>().ok();
    utc_ms(UtcParts {
        year: field(0..4)?,
        month: field(5..7)?,
        day: field(8..10)?,
        hour: 0,
        minute: 0,
        second: 0,
    })
}

/// A trash folder: a real folder directly in the trash.
#[derive(Debug, Clone)]
struct Folder {
    path: PathBuf,
    /// The start of its UTC day, if its name is a date.
    day: Option<i64>,
}

/// The trash's folders, newest name first. Links, files and anything else
/// in the trash are left out. A missing trash, or one that is not a real
/// folder, has none.
fn trash_folders(trash: &Path) -> Result<Vec<Folder>> {
    if !is_real_dir(trash)? {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for name in names_in(trash)?.into_iter().rev() {
        let path = trash.join(&name);
        if is_real_dir(&path)? {
            out.push(Folder {
                day: folder_day(&name),
                path,
            });
        }
    }
    Ok(out)
}

/// The media folder's sibling trash.
pub(crate) fn trash_of(media_dir: &Path) -> Option<PathBuf> {
    media_dir.parent().map(|dir| dir.join(TRASH_DIR_NAME))
}

/// The rows of the `media` table.
fn media_rows(conn: &Connection) -> Result<Vec<MediaRow>> {
    let mut stmt = conn.prepare("SELECT hash, mime, bytes, created_at FROM media ORDER BY hash")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(MediaRow {
                hash: r.get(0)?,
                mime: r.get(1)?,
                bytes: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// What a clean-up would do now in the database at `conn` and the media
/// folder `media_dir`.
fn unused_in(conn: &Connection, media_dir: &Path) -> Result<Unused> {
    let used = card_hashes(conn)?;
    let files = media_files(media_dir)?;
    let mut unused = Unused::default();
    for (media, bytes) in files {
        if !used.contains(media.hash()) {
            unused.files.push(UnusedFile { media, bytes });
        }
    }
    for row in media_rows(conn)? {
        let Some(hash) = parse_hex(row.hash.as_bytes()) else {
            continue;
        };
        if used.contains(&hash) {
            continue;
        }
        // A row is deleted with its file, or alone if it has none; a
        // folder or a link under its name was not made by Rappel, and the
        // row stays beside it.
        let Some(name) = row.file_name() else {
            continue;
        };
        if found(&media_dir.join(name))? != Found::Other {
            unused.rows.push(row);
        }
    }
    Ok(unused)
}

impl Store {
    /// The unused media files: how many a clean-up would move to the trash
    /// now, and their size (ADR 0057). Reads only.
    ///
    /// # Errors
    /// [`Error::NoMediaDir`] for a store without a data directory, IO and
    /// SQLite errors.
    pub fn unused_media(&self) -> Result<UnusedMedia> {
        let media_dir = self.media_dir.as_deref().ok_or(Error::NoMediaDir)?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Deferred)?;
        let unused = unused_in(&tx, media_dir)?;
        drop(tx);
        Ok(UnusedMedia {
            files: unused.files.len(),
            bytes: unused.files.iter().map(|f| f.bytes).sum(),
        })
    }

    /// Moves every unused media file to the trash and deletes the unused
    /// rows, in one write transaction (ADR 0057). The unused files are
    /// found again here, under the write lock, so a file added since
    /// [`Self::unused_media`] counted is never taken.
    ///
    /// # Errors
    /// [`Error::NoMediaDir`], [`Error::ClockWrong`] if the clock is before
    /// October 2026 or past year 9999, IO and SQLite errors. On error no row
    /// is deleted and every file moved is moved back; a file that cannot
    /// be moved back stays in the trash with its row, and the next open
    /// brings it back.
    pub fn clean_up_media(&mut self) -> Result<CleanedUp> {
        self.clean_up_media_with(now_ms(), &Disk, &|| Ok(()))
    }

    /// [`Self::clean_up_media`] at `now`, with the renames and a hook before
    /// the commit injected.
    pub(super) fn clean_up_media_with(
        &mut self,
        now: i64,
        renamer: &dyn Mover,
        before_commit: BeforeCommit<'_>,
    ) -> Result<CleanedUp> {
        let media_dir = self.media_dir.clone().ok_or(Error::NoMediaDir)?;
        let trash = trash_of(&media_dir).ok_or(Error::NoMediaDir)?;
        if !(EARLIEST_MS..LATEST_MS + DAY_MS).contains(&now) {
            return Err(Error::ClockWrong);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_clock(&tx, &trash, now)?;
        let unused = unused_in(&tx, &media_dir)?;
        let mut moved: Vec<Moved> = Vec::new();
        let result = (|| {
            if !unused.files.is_empty() {
                make_trash(&trash)?;
            }
            let mut folders: Vec<PathBuf> = Vec::new();
            for file in &unused.files {
                let name = file.media.file_name();
                let (folder, to) = free_place(&trash, now, &name)?;
                let from = media_dir.join(&name);
                renamer.rename(&from, &to).map_err(io_err(&from))?;
                moved.push(Moved {
                    media: file.media,
                    bytes: file.bytes,
                    to,
                });
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
            // The new names on disk before the rows go: after a power cut
            // either the rows are there (and the files are brought back),
            // or neither is.
            if !moved.is_empty() {
                for folder in &folders {
                    sync_folder(folder)?;
                }
                sync_folder(&trash)?;
                sync_folder(&media_dir)?;
            }
            for row in &unused.rows {
                tx.execute("DELETE FROM media WHERE hash = ?1", [&row.hash])?;
            }
            before_commit()
        })();
        // COMMIT by hand, so that if it fails the transaction (and the write
        // lock) is still held while the files move back; dropping `tx`
        // then rolls back (it does nothing once committed).
        let result = result.and_then(|()| tx.execute_batch("COMMIT").map_err(Error::from));
        if let Err(err) = result {
            // The rows roll back when `tx` drops; the files go back first,
            // under the lock, so nothing sees them in neither place.
            move_back(renamer, &media_dir, &moved);
            drop(tx);
            return Err(err);
        }
        drop(tx);
        let cleaned = CleanedUp {
            files: moved.len(),
            bytes: moved.iter().map(|m| m.bytes).sum(),
            moved,
            rows: unused.rows,
        };
        if cleaned.files > 0 || !cleaned.rows.is_empty() {
            tracing::info!(
                files = cleaned.files,
                bytes = cleaned.bytes,
                rows = cleaned.rows.len(),
                "unused media moved to the trash"
            );
        }
        Ok(cleaned)
    }

    /// Puts back what [`Self::clean_up_media`] did, as far as it still can:
    /// each file still in the trash goes back to the media folder unless a
    /// file has its name there again, and each deleted row is written again
    /// unless its hash has a row again (so a write made since is kept). A
    /// row whose file can no longer be put back stays deleted. Returns how
    /// many files went back.
    ///
    /// # Errors
    /// [`Error::NoMediaDir`], IO and SQLite errors. On error no row is
    /// written; files already moved back stay in the media folder, unused.
    pub fn undo_clean_up(&mut self, cleaned: &CleanedUp) -> Result<usize> {
        let media_dir = self.media_dir.clone().ok_or(Error::NoMediaDir)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut back: HashSet<String> = HashSet::new();
        for m in &cleaned.moved {
            let name = m.media.file_name();
            let target = media_dir.join(&name);
            if found(&target)? != Found::Nothing || !matches!(found(&m.to)?, Found::File(_)) {
                continue;
            }
            Disk.rename(&m.to, &target).map_err(io_err(&m.to))?;
            back.insert(name);
        }
        if !back.is_empty() {
            sync_folder(&media_dir)?;
        }
        for row in &cleaned.rows {
            let Some(name) = row.file_name() else {
                continue;
            };
            // A row goes back with its file, or alone if it had none.
            let had_file = cleaned.moved.iter().any(|m| m.media.file_name() == name);
            if had_file && !back.contains(&name) {
                continue;
            }
            tx.execute(
                "INSERT INTO media (hash, mime, bytes, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (hash) DO NOTHING",
                params![row.hash, row.mime, row.bytes, row.created_at],
            )?;
        }
        tx.commit()?;
        tracing::info!(files = back.len(), "media clean-up undone");
        Ok(back.len())
    }

    /// At startup, on a worker: brings back every file of the trash the
    /// database uses ([`recover_all`]), then purges what has been in the
    /// trash long enough and nothing uses ([`purge`]).
    ///
    /// # Errors
    /// [`Error::NoMediaDir`], IO and SQLite errors.
    pub fn tidy_media_trash(&mut self) -> Result<TrashTidied> {
        self.tidy_media_trash_at(now_ms())
    }

    /// [`Self::tidy_media_trash`] at `now`.
    pub(crate) fn tidy_media_trash_at(&mut self, now: i64) -> Result<TrashTidied> {
        self.tidy_media_trash_with(now, &|| {})
    }

    /// [`Self::tidy_media_trash_at`] with a test's hook between the purge's
    /// unlocked and locked passes.
    pub(super) fn tidy_media_trash_with(
        &mut self,
        now: i64,
        before_lock: &dyn Fn(),
    ) -> Result<TrashTidied> {
        let recovered = self.recover_trashed_media_at(now)?;
        let mut tidied = purge(self, now, before_lock)?;
        tidied.recovered = recovered;
        Ok(tidied)
    }

    /// Brings back every file of the trash the database uses, its rows or
    /// its cards ([`recover_all`]), in its own write transaction. Returns
    /// how many came back.
    ///
    /// # Errors
    /// [`Error::NoMediaDir`], IO and SQLite errors.
    pub(crate) fn recover_trashed_media_at(&mut self, now: i64) -> Result<usize> {
        let media_dir = self.media_dir.clone().ok_or(Error::NoMediaDir)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = recover_all(&tx, &media_dir, now)?;
        tx.commit()?;
        Ok(n)
    }
}

/// Refuses a clean-up whose clock is behind what Rappel recorded: the
/// newest trash folder's date, or the newest time in the database
/// ([`latest_time`]). Its folder would be dated earlier than the clean-up
/// really was, and the purge would come early (ADR 0057).
fn check_clock(conn: &Connection, trash: &Path, now: i64) -> Result<()> {
    let today = now - now.rem_euclid(DAY_MS);
    let newest_folder = trash_folders(trash)?.iter().filter_map(|f| f.day).max();
    let behind_trash = newest_folder.is_some_and(|day| day > today);
    let behind_data = latest_time(conn)?.is_some_and(|t| t > now);
    if behind_trash || behind_data {
        tracing::warn!("a clean-up is refused: the clock is behind the data's newest time");
        return Err(Error::ClockWrong);
    }
    Ok(())
}

/// Makes the trash (mode `0700`) unless it is there. Something there that
/// is not a real folder (a link, a file) refuses the clean-up: it is never
/// followed.
fn make_trash(trash: &Path) -> Result<()> {
    match DirBuilder::new().mode(DIR_MODE).create(trash) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            if is_real_dir(trash)? {
                Ok(())
            } else {
                Err(io_err(trash)(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "something that is not a folder has the trash's name",
                )))
            }
        }
        Err(err) => Err(io_err(trash)(err)),
    }
}

/// The first trash folder of the UTC day of `now` (made if needed, mode
/// `0700`) where `name` is free, and the path there. A name that is not a
/// real folder is skipped, never followed.
fn free_place(trash: &Path, now: i64, name: &str) -> Result<(PathBuf, PathBuf)> {
    for n in 1..=MAX_FOLDERS_A_DAY {
        let folder = trash.join(folder_name(now, n));
        match DirBuilder::new().mode(DIR_MODE).create(&folder) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                if !is_real_dir(&folder)? {
                    continue;
                }
            }
            Err(err) => return Err(io_err(&folder)(err)),
        }
        let to = folder.join(name);
        if found(&to)? == Found::Nothing {
            return Ok((folder, to));
        }
    }
    Err(io_err(trash)(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "the trash has no free place for this file today",
    )))
}

/// Moves the files of a failed clean-up back, newest first. One that cannot
/// go back (its name taken, a failing disk) stays in the trash with its row,
/// which the next open sees.
fn move_back(renamer: &dyn Mover, media_dir: &Path, moved: &[Moved]) {
    let mut failed = 0;
    for m in moved.iter().rev() {
        let target = media_dir.join(m.media.file_name());
        let ok =
            matches!(found(&target), Ok(Found::Nothing)) && renamer.rename(&m.to, &target).is_ok();
        if !ok {
            failed += 1;
        }
    }
    if failed > 0 {
        tracing::warn!(
            files = failed,
            "media files stay in the trash after a failed clean-up; the next start brings them back"
        );
    }
    if let Err(err) = sync_folder(media_dir) {
        tracing::warn!("cannot sync the media folder: {err}");
    }
}

/// What [`bring_back`] did with a trash file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Back {
    /// It is in the media folder again.
    Moved,
    /// A file has its name in the media folder already: the trash copy
    /// stays (a duplicate, which the purge keeps while the name is used).
    NameTaken,
    /// It is not the file its name says (another hash, empty, too large,
    /// or audio that is not plain audio): it stays in the trash, logged,
    /// and another copy may be tried.
    NotItsContent,
}

/// Brings back `media` from the trash file `from` (of `bytes`) to the
/// media folder, with a row if it has none, after checking that its content
/// has the hash and kind of its name (read once, at most its kind's
/// limit).
fn bring_back(
    conn: &Connection,
    renamer: &dyn Mover,
    media_dir: &Path,
    from: &Path,
    media: MediaRef,
    bytes: u64,
    now: i64,
) -> Result<Back> {
    let target = media_dir.join(media.file_name());
    if found(&target)? != Found::Nothing {
        return Ok(Back::NameTaken);
    }
    let expected = Ingested { media, bytes };
    let intact = bytes > 0
        && bytes <= media.kind().max_bytes()
        && has_content(from, &expected).map_err(io_err(from))?;
    if !intact {
        tracing::warn!(
            hash = %media.hex(),
            "a file in the media trash is not what its name says; it stays there"
        );
        return Ok(Back::NotItsContent);
    }
    crate::data_dir::ensure_data_dir(media_dir)?;
    renamer.rename(from, &target).map_err(io_err(from))?;
    conn.execute(
        "INSERT INTO media (hash, mime, bytes, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (hash) DO NOTHING",
        params![
            media.hex(),
            media.kind().mime(),
            i64::try_from(bytes).unwrap_or(i64::MAX),
            now
        ],
    )?;
    tracing::info!(hash = %media.hex(), "media file brought back from the trash");
    Ok(Back::Moved)
}

/// The media files of a trash folder; one that cannot be read is skipped
/// with a warning (it is tried again next time), so one bad folder never
/// stops a restore, an import or a card write.
fn readable_files(folder: &Path) -> Vec<(MediaRef, u64)> {
    match media_files(folder) {
        Ok(files) => files,
        Err(err) => {
            tracing::warn!("a media trash folder cannot be read: {err}");
            Vec::new()
        }
    }
}

/// The trash's folders; a trash that cannot be read has none here, with a
/// warning.
fn readable_folders(trash: &Path) -> Vec<Folder> {
    match trash_folders(trash) {
        Ok(folders) => folders,
        Err(err) => {
            tracing::warn!("the media trash cannot be read: {err}");
            Vec::new()
        }
    }
}

/// Brings back every file of the trash whose hash `wanted` accepts (the
/// newest intact copy of each name). Inside the caller's write transaction.
fn recover_where(
    conn: &Connection,
    media_dir: &Path,
    now: i64,
    wanted: &mut dyn FnMut(&MediaRef) -> Result<bool>,
) -> Result<usize> {
    let Some(trash) = trash_of(media_dir) else {
        return Ok(0);
    };
    let folders = readable_folders(&trash);
    if folders.is_empty() {
        return Ok(0);
    }
    let mut done: HashSet<MediaRef> = HashSet::new();
    let mut n = 0;
    for folder in &folders {
        for (media, bytes) in readable_files(&folder.path) {
            if done.contains(&media) || !wanted(&media)? {
                continue;
            }
            let from = folder.path.join(media.file_name());
            match bring_back(conn, &Disk, media_dir, &from, media, bytes, now)? {
                Back::Moved => {
                    n += 1;
                    done.insert(media);
                }
                Back::NameTaken => {
                    done.insert(media);
                }
                Back::NotItsContent => {}
            }
        }
    }
    if n > 0 {
        sync_folder(media_dir)?;
    }
    Ok(n)
}

/// Brings back every file of the trash that has a row in the database at
/// `conn`: a clean-up cut off between its moves and its commit, or a
/// restored backup or import that has the row. Cheap: the trash is listed
/// and the rows' hashes read once (measured in
/// `trash_tests::bringing_back_rows_at_open_is_cheap`). Inside the
/// caller's write transaction.
pub(crate) fn recover_rows(conn: &Connection, media_dir: &Path, now: i64) -> Result<usize> {
    let mut rows: Option<Hashes> = None;
    let mut has_row = |media: &MediaRef| -> Result<bool> {
        if rows.is_none() {
            rows = Some(super::used::row_hashes(conn)?);
        }
        Ok(rows.as_ref().is_some_and(|r| r.contains(media.hash())))
    };
    recover_where(conn, media_dir, now, &mut has_row)
}

/// Brings back every file of the trash the database at `conn` uses: a row,
/// or any card side naming its hash ([`used_hashes`]). The cards are read
/// only if the trash has files. Inside the caller's write transaction.
pub(crate) fn recover_all(conn: &Connection, media_dir: &Path, now: i64) -> Result<usize> {
    let mut used: Option<Hashes> = None;
    let mut is_used = |media: &MediaRef| -> Result<bool> {
        if used.is_none() {
            used = Some(used_hashes(conn)?);
        }
        Ok(used.as_ref().is_some_and(|u| u.contains(media.hash())))
    };
    recover_where(conn, media_dir, now, &mut is_used)
}

/// Brings back the files the card sides `sides` name ([`media_refs`]) that
/// are missing from the media folder and in the trash, as a card is
/// written: an editor may hold a reference to a file a clean-up took while
/// it was open. Only the names it refers to are looked up, in the trash's
/// folders listed once. Inside the caller's write transaction.
pub(crate) fn recover_refs(
    conn: &Connection,
    media_dir: &Path,
    sides: &[&str],
    now: i64,
) -> Result<usize> {
    let mut refs: Vec<MediaRef> = Vec::new();
    for side in sides {
        if may_name_media(side) {
            for media in media_refs(side) {
                if !refs.contains(&media) {
                    refs.push(media);
                }
            }
        }
    }
    let mut missing = Vec::new();
    for media in refs {
        if found(&media_dir.join(media.file_name()))? == Found::Nothing {
            missing.push(media);
        }
    }
    if missing.is_empty() {
        return Ok(0);
    }
    let Some(trash) = trash_of(media_dir) else {
        return Ok(0);
    };
    let folders = readable_folders(&trash);
    let mut n = 0;
    for media in missing {
        for folder in &folders {
            let from = folder.path.join(media.file_name());
            if let Found::File(bytes) = found(&from)? {
                match bring_back(conn, &Disk, media_dir, &from, media, bytes, now)? {
                    Back::Moved => {
                        n += 1;
                        break;
                    }
                    Back::NameTaken => break,
                    Back::NotItsContent => {}
                }
            }
        }
    }
    if n > 0 {
        sync_folder(media_dir)?;
    }
    Ok(n)
}

/// The newest time the database at `conn` records: when a deck or card was
/// made, edited or deleted, a log entry's time, a media file's
/// `created_at`. `None` for an empty database.
pub(super) fn latest_time(conn: &Connection) -> Result<Option<i64>> {
    let latest: Option<i64> = conn.query_row(
        "SELECT max(t) FROM (
             SELECT max(created_at, updated_at, coalesce(deleted_at, 0)) AS t FROM decks
             UNION ALL SELECT max(created_at, updated_at, coalesce(deleted_at, 0)) FROM cards
             UNION ALL SELECT reviewed_at FROM review_events
             UNION ALL SELECT created_at FROM media
         )",
        [],
        |r| r.get(0),
    )?;
    Ok(latest)
}

/// Whether a trash folder of day `day` may be purged at `now`, the
/// database's newest recorded time being `latest`: the clock 31 days past
/// the start of the folder's date (so every file in it spent 30 days
/// there by the clock), **and** the database holding a time at least 30
/// days past that start (Rappel was used, and wrote, a month after the
/// clean-up). So a clock set back never purges, and neither does a clock
/// that is ahead only at a start: a write made while it is ahead is what
/// it would take. A folder dated before Rappel had a clean-up was made by
/// a wrong clock and is never purged.
pub(super) fn purgeable(day: i64, now: i64, latest: Option<i64>) -> bool {
    day >= EARLIEST_MS
        && now >= day.saturating_add((PURGE_AFTER_DAYS + 1) * DAY_MS)
        && latest.is_some_and(|t| t >= day.saturating_add(PURGE_AFTER_DAYS * DAY_MS))
}

/// A database copy the purge reads, by path, size and change time (a new
/// file under an old name is read again).
type CopyKey = (PathBuf, u64, i64, i64);

/// Why the purge stops.
struct Blocked;

/// Adds to `protected` the hashes every backup and every copy of the
/// database in the data folder uses, skipping those in `read` (and adding
/// the new ones). A backup in `backups/`, or the copy a restore left
/// waiting (`rappel.sqlite3.restore.partial`, which `finish_restore`
/// installs), that cannot be read blocks the purge. The other copies
/// (`rappel.sqlite3.replaced-…`, `.restore.set-aside-…`, `.restore.refused-…`)
/// are read where they can be: the one a restore replaced is often the
/// damaged database it was restored over.
fn read_copies(
    data_dir: &Path,
    backup_dir: &Path,
    read: &mut HashSet<CopyKey>,
    protected: &mut Hashes,
) -> std::result::Result<(), Blocked> {
    let mut copies: Vec<(PathBuf, bool)> = Vec::new();
    match backup_names(backup_dir) {
        Ok(names) => copies.extend(names.into_iter().map(|n| (backup_dir.join(n), true))),
        Err(err) => {
            tracing::warn!("the trash is not purged: the backups cannot be listed: {err}");
            return Err(Blocked);
        }
    }
    let db_name = crate::storage::DB_FILE_NAME;
    match names_in(data_dir) {
        Ok(names) => {
            for name in names {
                let Some(rest) = name.strip_prefix(db_name).and_then(|r| r.strip_prefix('.'))
                else {
                    continue;
                };
                if ["-wal", "-shm", "-journal"]
                    .iter()
                    .any(|s| rest.ends_with(s))
                {
                    continue;
                }
                copies.push((data_dir.join(&name), rest == "restore.partial"));
            }
        }
        Err(err) => {
            tracing::warn!("the trash is not purged: the data folder cannot be listed: {err}");
            return Err(Blocked);
        }
    }
    for (path, required) in copies {
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            // Pruned or moved since it was listed: nothing to restore.
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                tracing::warn!(path = %path.display(), "the trash is not purged: {err}");
                return Err(Blocked);
            }
        };
        // Restore reads only regular files.
        if !meta.file_type().is_file() {
            continue;
        }
        let key = (path.clone(), meta.len(), meta.ctime(), meta.ctime_nsec());
        if read.contains(&key) {
            continue;
        }
        match copy_hashes(&path) {
            Ok(hashes) => {
                protected.extend(hashes);
                read.insert(key);
            }
            // A copy that is no Rappel database at all (damaged, another
            // file under the name, as `.moving-aside` is) protects nothing;
            // any other failure (IO, busy, cannot open) might hide what it
            // uses, so it blocks the purge like an unreadable backup.
            Err(err) if !required && not_a_database(&err) => {
                tracing::info!(path = %path.display(), "a database copy is not read: {err}");
                read.insert(key);
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    "the trash is not purged: a database copy cannot be read: {err}"
                );
                return Err(Blocked);
            }
        }
    }
    Ok(())
}

/// Whether `err`, from reading a database copy, says the file is not a
/// Rappel database at all: not an SQLite file, damaged, or without a
/// `cards` table.
fn not_a_database(err: &Error) -> bool {
    match err {
        Error::Sqlite(rusqlite::Error::SqliteFailure(failure, message)) => {
            matches!(
                failure.code,
                rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
            ) || (failure.code == rusqlite::ErrorCode::Unknown
                && message
                    .as_deref()
                    .is_some_and(|m| m.starts_with("no such table")))
        }
        _ => false,
    }
}

/// The hashes the database file at `path` uses, read without writing to
/// it (read-only, `query_only`).
fn copy_hashes(path: &Path) -> Result<Hashes> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "query_only", true)?;
    conn.pragma_update(None, "trusted_schema", false)?;
    used_hashes(&conn)
}

/// A file of the trash: what it is, its size and where.
type TrashFile = (MediaRef, u64, PathBuf);

/// The files of the trash folders old enough to go ([`purgeable`]), and
/// those folders.
fn purge_candidates(
    trash: &Path,
    now: i64,
    latest: Option<i64>,
) -> Result<(Vec<PathBuf>, Vec<TrashFile>)> {
    let mut folders = Vec::new();
    let mut files = Vec::new();
    for folder in trash_folders(trash)? {
        if !folder.day.is_some_and(|day| purgeable(day, now, latest)) {
            continue;
        }
        for (media, bytes) in media_files(&folder.path)? {
            files.push((media, bytes, folder.path.join(media.file_name())));
        }
        folders.push(folder.path);
    }
    Ok((folders, files))
}

/// Deletes the files that have been in the trash long enough
/// ([`purgeable`]) and that no backup, no copy of the database in the data
/// folder and not the database itself uses; then the folders left empty.
///
/// The backups are read first, without a lock; then, holding the
/// database's write lock (so no clean-up, add, import or card write runs
/// meanwhile), backups made since are read too, the database's own use is
/// read, and the files are deleted. The `fs::remove_file` here is the only
/// deletion of a user's media file in Rappel (ADR 0057).
fn purge(store: &mut Store, now: i64, before_lock: &dyn Fn()) -> Result<TrashTidied> {
    let mut tidied = TrashTidied::default();
    let (Some(media_dir), Some(backup_dir)) = (store.media_dir.clone(), store.backup_dir.clone())
    else {
        return Err(Error::NoMediaDir);
    };
    let (Some(trash), Some(data_dir)) = (trash_of(&media_dir), media_dir.parent()) else {
        return Err(Error::NoMediaDir);
    };
    let (folders, files) = purge_candidates(&trash, now, latest_time(&store.conn)?)?;
    if folders.is_empty() {
        return Ok(tidied);
    }
    let mut read = HashSet::new();
    let mut protected = Hashes::new();
    if !files.is_empty() && read_copies(data_dir, &backup_dir, &mut read, &mut protected).is_err() {
        tidied.blocked = true;
        return Ok(tidied);
    }
    before_lock();
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Listed again under the lock: a file brought back meanwhile is gone.
    let (folders, files) = purge_candidates(&trash, now, latest_time(&tx)?)?;
    if !files.is_empty() {
        if read_copies(data_dir, &backup_dir, &mut read, &mut protected).is_err() {
            tidied.blocked = true;
            return Ok(tidied);
        }
        protected.extend(used_hashes(&tx)?);
    }
    for (media, bytes, path) in files {
        if protected.contains(media.hash()) {
            tidied.kept += 1;
            continue;
        }
        // The only deletion of a user's media file in Rappel (ADR 0057):
        // a regular file with a media name, in a dated trash folder 31 days
        // old, that no backup, database copy or the database uses.
        match fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(hash = %media.hex(), "media file purged from the trash");
                tidied.purged += 1;
                tidied.purged_bytes += bytes;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(hash = %media.hex(), "cannot purge a media file: {err}");
            }
        }
    }
    for folder in folders {
        // Only an empty folder goes; one holding anything else stays.
        if fs::remove_dir(&folder).is_ok() {
            tracing::debug!(folder = %folder.display(), "empty trash folder removed");
        }
    }
    drop(tx);
    Ok(tidied)
}
