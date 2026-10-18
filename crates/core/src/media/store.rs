//! The media folder: `<data dir>/media/<sha256>.<ext>`, the folder mode
//! `0700`, the files `0600` (ADR 0054).
//!
//! A file is streamed into a new temporary file in the folder, hashed as
//! it goes and capped by its kind's limit, synced, then given its final
//! name by a hard link, which never replaces a file: if the name exists,
//! that file is hashed and kept if it has the same content; otherwise it is
//! damaged, kept under a second name, and the good copy takes the name.
//! Nothing here deletes a media file. The temporary name is removed once the file has
//! its name, or after a failure; a crash can leave one behind, named
//! `.incoming-…`, which is never a media name, and which a clean-up leaves
//! alone (ADR 0057).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::{DETECT_BYTES, MediaKind, MediaProblem, MediaRef, check_structure, hex};
use crate::data_dir::ensure_data_dir;
use crate::error::{Error, Result};
use crate::storage::Store;
use crate::time::now_ms;

/// The media folder's name inside the data directory.
pub(crate) const MEDIA_DIR_NAME: &str = "media";

const FILE_MODE: u32 = 0o600;

/// A file put in the media folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ingested {
    pub(crate) media: MediaRef,
    /// Its size.
    pub(crate) bytes: u64,
}

impl Store {
    /// Copies the file `reader` gives into the media folder and records it,
    /// and returns how card Markdown refers to it
    /// ([`MediaRef::reference`]).
    ///
    /// The kind comes from the first bytes, never from a name: a JPEG
    /// called `x.png` is stored as `.jpg`. Only the kinds of
    /// [`MediaKind`] are accepted, each up to its limit (10 MB for an
    /// image, 20 MB for audio). Adding a file that is already there
    /// changes nothing and gives the same reference.
    ///
    /// # Errors
    /// [`Error::MediaRefused`] (empty, not a supported kind, too large),
    /// [`Error::MediaRead`] if `reader` fails, [`Error::NoMediaDir`] for a
    /// store without a data directory, IO and SQLite errors. Nothing is
    /// recorded on error; a file may be left in the folder under its own
    /// hash, unused.
    pub fn add_media(&self, mut reader: impl Read) -> Result<MediaRef> {
        self.add_media_at(&mut reader, now_ms())
    }

    /// [`Self::add_media`] for bytes in memory (a pasted image).
    ///
    /// # Errors
    /// As [`Self::add_media`].
    pub fn add_media_bytes(&self, bytes: &[u8]) -> Result<MediaRef> {
        self.add_media(bytes)
    }

    /// [`Self::add_media`] for an image only (the editor's Insert Image…
    /// and a pasted picture): a file of an audio kind is refused as
    /// [`MediaProblem::NotImage`] before anything is written.
    ///
    /// # Errors
    /// As [`Self::add_media`].
    pub fn add_image(&self, reader: impl Read) -> Result<MediaRef> {
        self.add_media_if(reader, MediaKind::is_image, MediaProblem::NotImage)
    }

    /// [`Self::add_media`] for an audio file only (the editor's Insert
    /// Audio…): a file of an image kind is refused as
    /// [`MediaProblem::NotAudio`] before anything is written.
    ///
    /// # Errors
    /// As [`Self::add_media`].
    pub fn add_audio(&self, reader: impl Read) -> Result<MediaRef> {
        self.add_media_if(reader, |kind| !kind.is_image(), MediaProblem::NotAudio)
    }

    /// [`Self::add_media`] if the first bytes are of a kind `wanted`
    /// accepts, else `refused` before anything is written.
    fn add_media_if(
        &self,
        mut reader: impl Read,
        wanted: fn(MediaKind) -> bool,
        refused: MediaProblem,
    ) -> Result<MediaRef> {
        let mut head = [0; DETECT_BYTES];
        let n = read_full(&mut reader, &mut head).map_err(Error::MediaRead)?;
        if let Some(kind) = MediaKind::detect(&head[..n])
            && !wanted(kind)
        {
            return Err(Error::MediaRefused(refused));
        }
        // Empty and unknown files are refused by `add_media` itself.
        self.add_media(io::Cursor::new(&head[..n]).chain(reader))
    }

    /// The media folder, inside the data directory; `None` for a store
    /// without one. It may not exist yet: it is made with the first file.
    #[must_use]
    pub fn media_folder(&self) -> Option<&Path> {
        self.media_dir.as_deref()
    }

    /// [`Self::add_media`] at time `now`, the row's `created_at` the first
    /// time the file is added.
    pub(crate) fn add_media_at(&self, reader: &mut impl Read, now: i64) -> Result<MediaRef> {
        let dir = self.media_dir.as_deref().ok_or(Error::NoMediaDir)?;
        // The write lock from before the file gets its name until its row
        // is committed, so a clean-up (which takes it too) never sees the
        // file without its row and moves it to the trash (ADR 0057). Taken
        // once the copy is complete, so a slow input never holds it.
        let (got, tx) = ingest_then(dir, reader, Sync::Folder, || {
            Ok(Transaction::new_unchecked(
                &self.conn,
                TransactionBehavior::Immediate,
            )?)
        })?;
        insert_media_row(&tx, &got, now)?;
        tx.commit()?;
        Ok(got.media)
    }

    /// Where the file `media` is: always inside the media folder, built
    /// from its checked hash and fixed extension. `None` for a store
    /// without a data directory. The file may be missing (a reference to a
    /// file that was never added, or lost).
    #[must_use]
    pub fn media_path(&self, media: &MediaRef) -> Option<PathBuf> {
        self.media_dir
            .as_deref()
            .map(|dir| dir.join(media.file_name()))
    }
}

/// Records `got` in the `media` table; a hash already there keeps its row
/// (and its first `created_at`).
pub(crate) fn insert_media_row(conn: &Connection, got: &Ingested, created_at: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO media (hash, mime, bytes, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (hash) DO NOTHING",
        params![
            got.media.hex(),
            got.media.kind().mime(),
            i64::try_from(got.bytes).unwrap_or(i64::MAX),
            created_at
        ],
    )?;
    Ok(())
}

/// What [`ingest`] syncs besides the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sync {
    /// The folder too, so the new name is on disk.
    Folder,
    /// Only the file: the caller syncs the folder once after many files
    /// ([`sync_folder`]).
    FileOnly,
}

/// Streams `reader` into the media folder `dir` (created with mode `0700`
/// if needed) under its hash and kind, and returns them. A file already
/// under that name is kept if it has that content, and set aside if not
/// ([`give_name`]).
pub(crate) fn ingest(dir: &Path, reader: &mut impl Read, sync: Sync) -> Result<Ingested> {
    ingest_then(dir, reader, sync, || Ok(())).map(|(got, ())| got)
}

/// [`ingest`], calling `before_naming` once the copy is complete and synced
/// and before the file gets its name, and returning its value too; its
/// error stops the add before the file is named.
pub(crate) fn ingest_then<T>(
    dir: &Path,
    reader: &mut impl Read,
    sync: Sync,
    before_naming: impl FnOnce() -> Result<T>,
) -> Result<(Ingested, T)> {
    ensure_data_dir(dir)?;
    let (temp, file) = create_temp(dir)?;
    let result = copy_and_name(dir, &temp, file, reader, sync, before_naming);
    // The temporary name goes in every case: on success the file has its
    // own name (or was already there), on failure it is incomplete.
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!("cannot remove a temporary media file: {err}"),
    }
    result
}

/// The body of [`ingest_then`], with the temporary file made.
fn copy_and_name<T>(
    dir: &Path,
    temp: &Path,
    mut file: File,
    reader: &mut impl Read,
    sync: Sync,
    before_naming: impl FnOnce() -> Result<T>,
) -> Result<(Ingested, T)> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    let got = copy_capped(reader, &mut file).map_err(|e| match e {
        CopyFailure::Refused(problem) => Error::MediaRefused(problem),
        CopyFailure::Read(err) => Error::MediaRead(err),
        CopyFailure::Write(err) => io_err(temp)(err),
    })?;
    file.sync_all().map_err(io_err(temp))?;
    drop(file);
    let value = before_naming()?;
    let target = dir.join(got.media.file_name());
    give_name(&Disk, temp, &target, &got)?;
    if sync == Sync::Folder {
        sync_folder(dir)?;
    }
    Ok((got, value))
}

/// A new, empty temporary file in `dir`, mode `0600`. `create_new` never
/// follows a symbolic link or opens an existing file.
fn create_temp(dir: &Path) -> Result<(PathBuf, File)> {
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|err| Error::Random(Box::new(err)))?;
    let path = dir.join(format!(".incoming-{}", hex(&random)));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(&path)
        .map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
    Ok((path, file))
}

/// Why [`copy_capped`] stopped.
#[derive(Debug)]
pub(crate) enum CopyFailure {
    /// The content is not accepted.
    Refused(MediaProblem),
    /// Reading the input failed.
    Read(io::Error),
    /// Writing the copy failed.
    Write(io::Error),
}

/// Copies `reader` to `out`: its kind from the first bytes, at most its
/// kind's limit, hashed as it goes. An audio file is also kept in memory
/// (at most 20 MB) and must be plain audio of its kind over its whole
/// length ([`check_structure`], ADR 0056), else it is refused as
/// [`MediaProblem::Malformed`] or [`MediaProblem::NotPlainAudio`] (what was written to `out` is then the
/// caller's to drop).
pub(crate) fn copy_capped(
    reader: &mut impl Read,
    out: &mut impl Write,
) -> std::result::Result<Ingested, CopyFailure> {
    let mut head = [0; DETECT_BYTES];
    let n = read_full(reader, &mut head).map_err(CopyFailure::Read)?;
    if n == 0 {
        return Err(CopyFailure::Refused(MediaProblem::Empty));
    }
    let kind = MediaKind::detect(&head[..n]).ok_or(CopyFailure::Refused(MediaProblem::NotMedia))?;
    let limit = kind.max_bytes();
    let mut hasher = Sha256::new();
    hasher.update(&head[..n]);
    out.write_all(&head[..n]).map_err(CopyFailure::Write)?;
    let mut whole = (!kind.is_image()).then(|| head[..n].to_vec());
    let mut total = n as u64;
    let mut buf = vec![0; 64 << 10];
    loop {
        // One byte past the limit is enough to know it is too large.
        let left = (limit - total.min(limit)).saturating_add(1);
        let room = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let n = match reader.read(&mut buf[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(CopyFailure::Read(err)),
        };
        total += n as u64;
        if total > limit {
            return Err(CopyFailure::Refused(MediaProblem::TooLarge { kind, limit }));
        }
        hasher.update(&buf[..n]);
        if let Some(whole) = whole.as_mut() {
            whole.extend_from_slice(&buf[..n]);
        }
        out.write_all(&buf[..n]).map_err(CopyFailure::Write)?;
    }
    if let Some(whole) = whole
        && let Err(why) = check_structure(kind, &whole)
    {
        tracing::debug!("an audio file is refused: {}", why.why);
        return Err(CopyFailure::Refused(if why.extra {
            MediaProblem::NotPlainAudio
        } else {
            MediaProblem::Malformed
        }));
    }
    Ok(Ingested {
        media: MediaRef::new(hasher.finalize().into(), kind),
        bytes: total,
    })
}

/// Reads until `buf` is full or the input ends; how many bytes came.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// The file-system calls that name a media file, so tests can make them
/// fail as some file systems do (no hard links).
pub(super) trait Namer {
    /// A second name for `from` at `to`; fails if `to` exists.
    fn link(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// `from` renamed to `to` (replacing it).
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// The real file system.
pub(super) struct Disk;

impl Namer for Disk {
    fn link(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::hard_link(from, to)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
}

/// Gives the complete file `temp` (whose content is `expected`) the name
/// `target`, never replacing a file with the same content and never losing
/// one. A hard link fails if the name exists; where the file system
/// refuses hard links, a rename after checking that the name is free (the
/// folder is the app's own). A regular file already under the name is
/// hashed: the same content is kept; other content is damaged, so it is
/// first kept under a second name (`<name>.damaged-<random>`, by a link or
/// else a copy to a new file), and only then does the good copy take the
/// name.
pub(super) fn give_name(
    namer: &impl Namer,
    temp: &Path,
    target: &Path,
    expected: &Ingested,
) -> Result<()> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    match namer.link(temp, target) {
        Ok(()) => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) if no_links(&err) => match fs::symlink_metadata(target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return namer.rename(temp, target).map_err(io_err(target));
            }
            Err(e) => return Err(io_err(target)(e)),
            Ok(_) => {}
        },
        Err(err) => return Err(io_err(target)(err)),
    }
    let meta = fs::symlink_metadata(target).map_err(io_err(target))?;
    if !meta.is_file() {
        // A folder or a link under a media name was not made by Rappel.
        return Err(io_err(target)(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "something that is not a media file has this name",
        )));
    }
    if meta.len() == expected.bytes && has_content(target, expected).map_err(io_err(target))? {
        return Ok(());
    }
    // Damaged: kept under a second name, then the good copy takes the name.
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|err| Error::Random(Box::new(err)))?;
    let mut aside = target.as_os_str().to_owned();
    aside.push(format!(".damaged-{}", hex(&random)));
    let aside = PathBuf::from(aside);
    match namer.link(target, &aside) {
        Ok(()) => {}
        Err(err) if no_links(&err) => copy_to_new(target, &aside).map_err(io_err(&aside))?,
        Err(err) => return Err(io_err(&aside)(err)),
    }
    tracing::warn!(
        kept_as = %aside.file_name().unwrap_or_default().to_string_lossy(),
        "a damaged media file was set aside"
    );
    // The second name on disk before the first is given to the good copy,
    // so the damaged file is never only under a name being replaced.
    if let Some(dir) = target.parent() {
        sync_folder(dir)?;
    }
    namer.rename(temp, target).map_err(io_err(target))
}

/// Whether a link failed because the file system has no hard links.
/// `PermissionDenied` can also come from `protected_hardlinks` on a file
/// the user does not own, which cannot be in the private (`0700`) folder.
fn no_links(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
    )
}

/// Whether the file at `path` has exactly the content `expected` (its
/// hash and kind), read once, at most its size plus one byte.
fn has_content(path: &Path, expected: &Ingested) -> io::Result<bool> {
    let file = File::open(path)?;
    let mut capped = file.take(expected.bytes.saturating_add(1));
    match copy_capped(&mut capped, &mut io::sink()) {
        Ok(found) => Ok(found == *expected),
        Err(CopyFailure::Refused(_)) => Ok(false),
        Err(CopyFailure::Read(err) | CopyFailure::Write(err)) => Err(err),
    }
}

/// Copies `from` to `to`, a new file (mode `0600`, never an existing one),
/// synced.
fn copy_to_new(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = File::open(from)?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(to)?;
    io::copy(&mut source, &mut out)?;
    out.sync_all()
}

/// Syncs the folder `dir`, so the names made in it are on disk.
pub(crate) fn sync_folder(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })
}
