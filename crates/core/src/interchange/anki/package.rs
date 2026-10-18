//! Reading an Anki package: the zip archive (through the import's own
//! reader and its limits, ADR 0053), its version in `meta`, its media list,
//! its collection written to a temporary file inside the data directory,
//! and its media files, each decompressed (zstd in the current format)
//! with a hard cap on what comes out (ADR 0061).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use ruzstd::decoding::{FrameDecoder, StreamingDecoder};
use sha2::{Digest, Sha256};

use super::proto::{Fields, Value, varint_field};
use super::{AnkiFormat, AnkiProblem};
use crate::error::{Error, Result};
use crate::interchange::archive::{AnkiName, Archive, Entry, EntryReader, file_number};
use crate::interchange::{ImportProblem, Limits};
use crate::media::hex;

/// The prefix of the temporary copy of a package's collection in the data
/// directory. Never a database name Rappel reads (backups, restore copies
/// and the media trash's database copies all start with `rappel.`).
pub(super) const TEMP_PREFIX: &str = ".anki-import-";

/// The limits of an Anki import, besides the archive's own ([`Limits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnkiLimits {
    /// The archive: its size, entries and central directory.
    pub archive: Limits,
    /// The collection, decompressed (written to a temporary file).
    pub collection: u64,
    /// The media list, decompressed.
    pub media_list: u64,
    /// `meta`.
    pub meta: u64,
    /// The largest window a zstd frame may ask for (the decoder's memory).
    pub zstd_window: u64,
    /// Media files in the media list.
    pub media_files: usize,
    /// A media file's name, in bytes.
    pub media_name: usize,
    /// A note's fields, all together, in bytes.
    pub note: usize,
    /// A note's tags, in bytes.
    pub note_tags: usize,
    /// Note types.
    pub notetypes: usize,
    /// Fields or templates of one note type.
    pub notetype_parts: usize,
    /// Fields and templates of all note types together.
    pub notetype_parts_total: usize,
    /// The names of decks, note types, fields and templates together, in
    /// bytes.
    pub names: usize,
    /// One card template (question or answer), in bytes.
    pub template: usize,
    /// Decks.
    pub decks: usize,
    /// The JSON of a legacy collection's note types or decks, in bytes.
    pub legacy_json: usize,
}

impl AnkiLimits {
    pub(crate) const DEFAULT: Self = Self {
        archive: Limits::DEFAULT,
        collection: 2 << 30,
        media_list: 64 << 20,
        meta: 1 << 10,
        zstd_window: 32 << 20,
        media_files: 100_000,
        media_name: 1 << 10,
        note: 8 << 20,
        note_tags: 64 << 10,
        notetypes: 10_000,
        notetype_parts: 1_000,
        notetype_parts_total: 200_000,
        names: 16 << 20,
        template: 256 << 10,
        decks: 100_000,
        legacy_json: 64 << 20,
    };
}

/// An entry of the media list: the file's name in the package (what card
/// fields name) and the number of its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListedFile {
    pub(super) name: String,
    pub(super) file: u32,
}

/// An opened Anki package.
pub(super) struct Package<R> {
    archive: Archive<R>,
    pub(super) format: AnkiFormat,
    limits: AnkiLimits,
}

impl<R> std::fmt::Debug for Package<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Package")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// The archive reader's problems, as an Anki import says them.
pub(super) fn from_archive(problem: ImportProblem) -> AnkiProblem {
    match problem {
        ImportProblem::FileTooLarge { limit } => AnkiProblem::FileTooLarge { limit },
        ImportProblem::NotAFile => AnkiProblem::NotAFile,
        ImportProblem::TooManyEntries { limit } => AnkiProblem::TooManyEntries { limit },
        ImportProblem::MissingEntry(_) => AnkiProblem::NoCollection,
        ImportProblem::UnsupportedEntry(entry) => AnkiProblem::UnsupportedEntry(entry),
        ImportProblem::EntryTooLarge { entry, limit } => {
            AnkiProblem::EntryTooLarge { entry, limit }
        }
        ImportProblem::InvalidArchive(why) => AnkiProblem::InvalidArchive(why),
        other => AnkiProblem::InvalidArchive(other.to_string()),
    }
}

impl<R: Read + Seek> Package<R> {
    /// Opens the package in the first `len` bytes of `input`: its
    /// archive, then its version.
    pub(super) fn open(
        input: R,
        len: u64,
        limits: &AnkiLimits,
    ) -> std::result::Result<Self, AnkiProblem> {
        let mut archive = Archive::open_anki(input, len, &limits.archive).map_err(from_archive)?;
        let has = |a: &Archive<R>, name| a.anki_entry(name).is_some();
        let format = match archive.anki_entry(AnkiName::Meta) {
            Some(entry) => {
                let meta = read_whole(&mut archive, &entry, false, limits.meta, limits)?;
                let version = varint_field(&meta, 1)
                    .map_err(|_| AnkiProblem::Damaged("its version cannot be read".to_owned()))?
                    .unwrap_or(0);
                match version {
                    1 => AnkiFormat::Anki20,
                    2 => AnkiFormat::Anki21,
                    3 => AnkiFormat::Current,
                    other => return Err(AnkiProblem::NewerVersion(other)),
                }
            }
            None if has(&archive, AnkiName::Anki21) => AnkiFormat::Anki21,
            None => AnkiFormat::Anki20,
        };
        if !has(&archive, format.collection()) {
            return Err(AnkiProblem::NoCollection);
        }
        Ok(Self {
            archive,
            format,
            limits: *limits,
        })
    }

    /// The media list, in order. Names are checked by the caller.
    pub(super) fn media_list(&mut self) -> std::result::Result<Vec<ListedFile>, AnkiProblem> {
        let Some(entry) = self.archive.anki_entry(AnkiName::MediaMap) else {
            return Ok(Vec::new());
        };
        let zstd = self.format == AnkiFormat::Current;
        let bytes = read_whole(
            &mut self.archive,
            &entry,
            zstd,
            self.limits.media_list,
            &self.limits,
        )?;
        let damaged = || AnkiProblem::Damaged("its list of media files cannot be read".to_owned());
        let mut files = Vec::new();
        if zstd {
            // MediaEntries { repeated MediaEntry entries = 1; }
            // MediaEntry { string name = 1; uint32 size = 2; bytes sha1 = 3;
            //              optional uint32 legacy_zip_filename = 255; }
            // The files are numbered by their place among the entries
            // (field 1) only.
            let mut index = 0_u32;
            for field in Fields::new(&bytes) {
                let (number, value) = field.map_err(|_| damaged())?;
                let (1, Value::Bytes(entry)) = (number, value) else {
                    continue;
                };
                let mut name = None;
                let mut file = index;
                index = index.checked_add(1).ok_or_else(damaged)?;
                for field in Fields::new(entry) {
                    match field.map_err(|_| damaged())? {
                        (1, Value::Bytes(b)) => {
                            name = Some(String::from_utf8_lossy(b).into_owned());
                        }
                        (255, Value::Varint(n)) => {
                            file = u32::try_from(n).map_err(|_| damaged())?;
                        }
                        _ => {}
                    }
                }
                files.push(ListedFile {
                    name: name.unwrap_or_default(),
                    file,
                });
                if files.len() > self.limits.media_files {
                    return Err(AnkiProblem::TooManyMediaFiles {
                        limit: self.limits.media_files,
                    });
                }
            }
        } else {
            // {"0": "name.png", ...}, read with bounds: a name too long
            // is kept empty, which is not a usable name.
            let list =
                super::json::media_list(&bytes, self.limits.media_files, self.limits.media_name)
                    .map_err(|refused| {
                        if refused.1 {
                            AnkiProblem::TooManyMediaFiles {
                                limit: self.limits.media_files,
                            }
                        } else {
                            damaged()
                        }
                    })?;
            for (key, name) in list {
                let file = key
                    .kept()
                    .and_then(|k| file_number(k.as_bytes()))
                    .ok_or_else(damaged)?;
                let name = match name {
                    super::json::Text::Kept(name) => name,
                    super::json::Text::TooLong => String::new(),
                    super::json::Text::Other => return Err(damaged()),
                };
                files.push(ListedFile { name, file });
            }
            files.sort_by_key(|f| f.file);
        }
        Ok(files)
    }

    /// Writes the collection, decompressed, into a new temporary file in
    /// `data_dir` (mode `0600`, removed when the returned value is
    /// dropped), and returns it with the SHA-256 of its bytes. A
    /// collection whose header says WAL is set to the rollback journal, so
    /// it opens read-only without a `-wal` or `-shm` file.
    pub(super) fn extract_collection(&mut self, data_dir: &Path) -> Result<TempCollection> {
        let name = self.format.collection();
        let entry = self
            .archive
            .anki_entry(name)
            .ok_or(Error::AnkiRefused(AnkiProblem::NoCollection))?;
        let (mut temp, mut file) = TempCollection::create(data_dir)?;
        let mut out = HashingWriter {
            file: &mut file,
            hasher: Sha256::new(),
        };
        let zstd = self.format == AnkiFormat::Current;
        copy_data(
            &mut self.archive,
            &entry,
            zstd,
            self.limits.collection,
            &self.limits,
            &mut out,
        )
        .map_err(|failure| failure.into_error(name.name(), &temp.path))?;
        temp.sha256 = out.hasher.finalize().into();
        set_rollback_journal(&file).map_err(|source| Error::Io {
            path: temp.path.clone(),
            source,
        })?;
        drop(file);
        Ok(temp)
    }

    /// The media file number `file`, decompressed, as a reader that fails
    /// past `cap` bytes; `None` if the package has no such entry.
    pub(super) fn media_reader(
        &mut self,
        file: u32,
        cap: u64,
    ) -> Option<std::result::Result<DataReader<'_, R>, AnkiProblem>> {
        let entry = self.archive.anki_file(file)?;
        let zstd = self.format == AnkiFormat::Current;
        Some(DataReader::open(
            &mut self.archive,
            &entry,
            zstd,
            cap,
            &self.limits,
        ))
    }
}

/// The temporary copy of a package's collection, removed when dropped.
#[derive(Debug)]
pub(super) struct TempCollection {
    pub(super) path: PathBuf,
    /// The SHA-256 of the decompressed collection.
    pub(super) sha256: [u8; 32],
}

impl TempCollection {
    /// A new, empty file `<data_dir>/.anki-import-<random>`, mode `0600`,
    /// and its handle (the file made, never another one opened by name).
    fn create(data_dir: &Path) -> Result<(Self, File)> {
        crate::data_dir::ensure_data_dir(data_dir)?;
        let mut random = [0; 8];
        getrandom::fill(&mut random).map_err(|err| Error::Random(Box::new(err)))?;
        let path = data_dir.join(format!("{TEMP_PREFIX}{}", hex(&random)));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        Ok((
            Self {
                path,
                sha256: [0; 32],
            },
            file,
        ))
    }
}

impl Drop for TempCollection {
    fn drop(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("cannot remove an Anki import's temporary file: {err}"),
        }
    }
}

/// Removes the temporary collections an earlier import left in `data_dir`
/// (only after a crash: each import removes its own). Only names with
/// [`TEMP_PREFIX`], regular files; a failure is only logged.
pub(super) fn remove_stale_temps(data_dir: &Path) {
    let Ok(entries) = fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry.file_name().to_str().is_some_and(|n| {
            n.strip_prefix(TEMP_PREFIX)
                .is_some_and(|rest| rest.len() == 16 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
        });
        if stale
            && entry.file_type().is_ok_and(|t| t.is_file())
            && let Err(err) = fs::remove_file(entry.path())
        {
            tracing::warn!("cannot remove an old Anki import's temporary file: {err}");
        }
    }
}

/// A SQLite header's write and read versions (bytes 18 and 19) of 2 mean
/// WAL; 1 is the rollback journal, which a read-only connection can open
/// without making a `-shm` file. Only the copy is changed.
fn set_rollback_journal(file: &File) -> io::Result<()> {
    let mut versions = [0; 2];
    match file.read_exact_at(&mut versions, 18) {
        Ok(()) => {}
        // Too short to be a database: SQLite says so when it opens it.
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(err) => return Err(err),
    }
    if versions.contains(&2) {
        file.write_all_at(&[1, 1], 18)?;
    }
    Ok(())
}

struct HashingWriter<'a> {
    file: &'a mut File,
    hasher: Sha256,
}

impl Write for HashingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.file.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Why reading an entry's data failed.
#[derive(Debug)]
pub(super) enum ReadFailure {
    /// The data is damaged (the zip entry or its zstd frame).
    Damaged(String),
    /// More than the cap came out.
    TooLarge(u64),
    /// Writing the copy failed.
    Write(io::Error),
}

impl ReadFailure {
    /// As the error of an import that read entry `entry` into `path`.
    fn into_error(self, entry: &'static str, path: &Path) -> Error {
        match self {
            Self::Damaged(why) => Error::AnkiRefused(AnkiProblem::Damaged(why)),
            Self::TooLarge(limit) => {
                Error::AnkiRefused(AnkiProblem::EntryTooLarge { entry, limit })
            }
            Self::Write(source) => Error::Io {
                path: path.to_path_buf(),
                source,
            },
        }
    }

    pub(super) fn into_problem(self, entry: &'static str) -> AnkiProblem {
        match self {
            Self::Damaged(why) => AnkiProblem::Damaged(why),
            Self::TooLarge(limit) => AnkiProblem::EntryTooLarge { entry, limit },
            Self::Write(err) => AnkiProblem::Damaged(err.to_string()),
        }
    }
}

/// The decompressed data of an entry, whole, at most `cap` bytes.
fn read_whole<R: Read + Seek>(
    archive: &mut Archive<R>,
    entry: &Entry,
    zstd: bool,
    cap: u64,
    limits: &AnkiLimits,
) -> std::result::Result<Vec<u8>, AnkiProblem> {
    let mut out = Vec::new();
    copy_data(archive, entry, zstd, cap, limits, &mut out)
        .map_err(|f| f.into_problem(entry.role.name()))?;
    Ok(out)
}

/// Streams the decompressed data of `entry` to `out`, at most `cap` bytes.
fn copy_data<R: Read + Seek>(
    archive: &mut Archive<R>,
    entry: &Entry,
    zstd: bool,
    cap: u64,
    limits: &AnkiLimits,
    out: &mut impl Write,
) -> std::result::Result<u64, ReadFailure> {
    let mut reader = DataReader::open(archive, entry, zstd, cap, limits).map_err(|p| match p {
        AnkiProblem::EntryTooLarge { limit, .. } => ReadFailure::TooLarge(limit),
        other => ReadFailure::Damaged(other.to_string()),
    })?;
    let mut buf = vec![0; 64 << 10];
    let mut total = 0_u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.write_all(&buf[..n]).map_err(ReadFailure::Write)?;
                total += n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    reader.finish()?;
    Ok(total)
}

/// How much an entry's stored data may exceed what it decompresses to: a
/// zstd frame of incompressible data is a little larger than the data.
fn zip_cap(cap: u64, zstd: bool) -> u64 {
    if zstd {
        cap.saturating_add(cap / 8).saturating_add(64 << 10)
    } else {
        cap
    }
}

/// An entry's data, decompressed by the zip reader and, in the current
/// format, by zstd; it fails once more than its cap comes out. After the
/// last byte, [`Self::finish`] says whether the data was whole: the zstd
/// frame complete with nothing after it, the entry's CRC and size right.
pub(super) struct DataReader<'a, R: Read> {
    data: Data<'a, R>,
    cap: u64,
    count: u64,
    failure: Option<ReadFailure>,
}

enum Data<'a, R: Read> {
    Plain(Box<EntryReader<'a, R>>),
    Zstd(Box<StreamingDecoder<EntryReader<'a, R>, FrameDecoder>>),
}

impl<R: Read> std::fmt::Debug for DataReader<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataReader")
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl<'a, R: Read + Seek> DataReader<'a, R> {
    fn open(
        archive: &'a mut Archive<R>,
        entry: &Entry,
        zstd: bool,
        cap: u64,
        limits: &AnkiLimits,
    ) -> std::result::Result<Self, AnkiProblem> {
        let inner = archive
            .open_entry(entry, zip_cap(cap, zstd))
            .map_err(from_archive)?;
        let data = if zstd {
            let mut decoder = FrameDecoder::new();
            decoder.set_max_window_size(limits.zstd_window);
            match StreamingDecoder::new_with_decoder(inner, decoder) {
                Ok(d) => Data::Zstd(Box::new(d)),
                Err(err) => {
                    return Err(AnkiProblem::Damaged(format!(
                        "{} cannot be decompressed: {err}",
                        entry.role.name()
                    )));
                }
            }
        } else {
            Data::Plain(Box::new(inner))
        };
        Ok(Self {
            data,
            cap,
            count: 0,
            failure: None,
        })
    }

    /// After the data is read: whether it was whole and within the cap.
    pub(super) fn finish(mut self) -> std::result::Result<(), ReadFailure> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        let inner = match &mut self.data {
            Data::Plain(inner) => &mut **inner,
            Data::Zstd(decoder) => {
                // A frame with a checksum must have the one of what came
                // out (xxHash64's low 32 bits, computed as it decoded).
                let frame = &decoder.decoder;
                if let Some(expected) = frame.get_checksum_from_data()
                    && frame.get_calculated_checksum() != Some(expected)
                {
                    return Err(ReadFailure::Damaged(
                        "the compressed data's checksum is wrong".to_owned(),
                    ));
                }
                // The frame ended; nothing may follow it in the entry.
                let inner = decoder.get_mut();
                let mut rest = [0; 1];
                if inner
                    .read(&mut rest)
                    .map_err(|e| ReadFailure::Damaged(e.to_string()))?
                    != 0
                {
                    return Err(ReadFailure::Damaged(
                        "more data follows the compressed data".to_owned(),
                    ));
                }
                inner
            }
        };
        inner.drain();
        match inner.failure() {
            None => Ok(()),
            Some(ImportProblem::EntryTooLarge { .. }) => Err(ReadFailure::TooLarge(self.cap)),
            Some(problem) => Err(ReadFailure::Damaged(problem.to_string())),
        }
    }
}

impl<R: Read + Seek> Read for DataReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other("the entry cannot be read"));
        }
        // One byte past the cap is enough to know it is too large.
        let left = (self.cap - self.count.min(self.cap)).saturating_add(1);
        let room = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let read = match &mut self.data {
            Data::Plain(r) => r.read(&mut buf[..room]),
            Data::Zstd(r) => r.read(&mut buf[..room]),
        };
        let n = match read {
            Ok(n) => n,
            Err(err) => {
                let inner = match &self.data {
                    Data::Plain(r) => r.failure(),
                    Data::Zstd(r) => r.get_ref().failure(),
                };
                self.failure = Some(match inner {
                    Some(ImportProblem::EntryTooLarge { .. }) => ReadFailure::TooLarge(self.cap),
                    Some(problem) => ReadFailure::Damaged(problem.to_string()),
                    None => ReadFailure::Damaged(err.to_string()),
                });
                return Err(io::Error::other("the entry cannot be read"));
            }
        };
        self.count += n as u64;
        if self.count > self.cap {
            self.failure = Some(ReadFailure::TooLarge(self.cap));
            return Err(io::Error::other("the entry is too large"));
        }
        Ok(n)
    }
}
