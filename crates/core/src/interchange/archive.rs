//! Writing and reading the `.rappel` zip archive.
//!
//! An export holds `cards.json`, the `media/` directory and one entry
//! `media/<sha256>.<ext>` per media file, stored as they are (ADR 0054). It
//! is written with the `zip` crate. It is read here, not by the `zip` crate,
//! whose reader keeps every central record in memory and, when one end
//! record does not work out, tries every earlier one it can find
//! (ADR 0053): nothing is ever extracted to disk, nothing is read into
//! memory whole, and every read is bounded by the limits. The end record
//! must end the file; the central directory it names is walked record by
//! record, exactly as many as it declares and within the size it declares;
//! each name is compared byte for byte with the names Rappel writes (a
//! media name is parsed as `media/`, 64 lowercase hex digits, a dot and a
//! known extension) and any other entry refuses the file, so paths in
//! entry names (zip-slip) cannot matter; an entry's data is streamed
//! (Stored, or Deflate through `flate2`) with a hard cap on the bytes it
//! decompresses to, its CRC and size checked at its end, and hashed as it
//! goes.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;

use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};
use zip::result::ZipResult;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::{ImportProblem, Limits};
use crate::media::MediaRef;

/// The JSON document's entry name.
pub(super) const JSON_ENTRY: &str = "cards.json";
/// The media directory's entry name (empty until media exist).
pub(super) const MEDIA_DIR: &str = "media/";

/// Length of a local file header without its name and extra field.
const LOCAL_HEADER_LEN: usize = 30;
const LOCAL_SIGNATURE: [u8; 4] = *b"PK\x03\x04";
/// Length of a central record without its name, extra field and comment.
const CENTRAL_LEN: usize = 46;
const CENTRAL_SIGNATURE: [u8; 4] = *b"PK\x01\x02";
/// The extra field that holds 64-bit sizes and offsets.
const ZIP64_EXTRA: u16 = 0x0001;
/// Compression methods: Stored and Deflate, the only ones read.
const STORED: u16 = 0;
const DEFLATED: u16 = 8;

/// The end of central directory record, without a comment.
const EOCD_LEN: usize = 22;
const EOCD_SIGNATURE: [u8; 4] = *b"PK\x05\x06";
/// The zip64 end of central directory locator, just before the record.
const ZIP64_LOCATOR_LEN: usize = 20;
const ZIP64_LOCATOR_SIGNATURE: [u8; 4] = *b"PK\x06\x07";
/// The zip64 end of central directory record without extensible data.
const ZIP64_EOCD_LEN: usize = 56;
const ZIP64_EOCD_SIGNATURE: [u8; 4] = *b"PK\x06\x06";

/// Writes an archive: `cards.json` (deflated), the `media/` directory,
/// then each media file the caller adds, in the caller's order, stored as
/// it is (images and audio are compressed already). Deterministic: the zip
/// format's earliest timestamp (1980-01-01) instead of the clock, fixed
/// permissions, no extra fields and no comment.
pub(super) struct ArchiveWriter<W: Write + Seek> {
    zip: ZipWriter<W>,
}

impl<W: Write + Seek> ArchiveWriter<W> {
    /// An archive in `out` holding `json` and the `media/` directory.
    pub(super) fn new(out: W, json: &[u8]) -> ZipResult<Self> {
        // `DEFAULT` (not `default()`) has the fixed 1980 timestamp.
        let file = SimpleFileOptions::DEFAULT
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o600);
        let dir = SimpleFileOptions::DEFAULT.unix_permissions(0o700);
        let mut zip = ZipWriter::new(out);
        zip.start_file(JSON_ENTRY, file)?;
        zip.write_all(json)?;
        zip.add_directory(MEDIA_DIR, dir)?;
        Ok(Self { zip })
    }

    /// Starts the entry `media/<file_name>`; its data is what is then
    /// written to the returned writer.
    pub(super) fn start_media(&mut self, file_name: &str) -> ZipResult<&mut ZipWriter<W>> {
        let stored = SimpleFileOptions::DEFAULT
            .compression_method(CompressionMethod::Stored)
            .unix_permissions(0o600);
        self.zip
            .start_file(format!("{MEDIA_DIR}{file_name}"), stored)?;
        Ok(&mut self.zip)
    }

    /// Writes the central directory and returns `out`.
    pub(super) fn finish(self) -> ZipResult<W> {
        self.zip.finish()
    }
}

/// The archive bytes for `json`, without media.
#[cfg(test)]
pub(super) fn write_archive(json: &[u8]) -> ZipResult<Vec<u8>> {
    Ok(ArchiveWriter::new(io::Cursor::new(Vec::new()), json)?
        .finish()?
        .into_inner())
}

/// The decompressed `cards.json` of the archive `bytes`, whole, for tests
/// that look at or change the JSON of an export.
#[cfg(test)]
pub(super) fn read_cards_json(bytes: &[u8], limits: &Limits) -> Result<Vec<u8>, ImportProblem> {
    let mut archive = Archive::open(io::Cursor::new(bytes), bytes.len() as u64, limits)?;
    let entry = archive.json_entry()?;
    let mut reader = archive.open_entry(&entry, limits.json)?;
    let mut json = Vec::new();
    let read = reader.read_to_end(&mut json);
    if let Some(problem) = reader.failure() {
        return Err(problem);
    }
    read.map_err(|e| io_problem(&e))?;
    Ok(json)
}

/// Which names an archive may have: a Rappel export's, or an Anki
/// package's (ADR 0061).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    /// `cards.json`, `media/` and `media/<sha256>.<ext>`; any other name
    /// refuses the file.
    Rappel,
    /// `meta`, `collection.anki2`, `collection.anki21`,
    /// `collection.anki21b`, `media` and numbered media files; other names
    /// are passed over (never read, never extracted), so a later Anki
    /// adding an entry does not make its packages unreadable.
    Anki,
}

/// An entry of an Anki package, by its central name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AnkiName {
    /// `meta`: the package version (protobuf), in packages since 2.1.50.
    Meta,
    /// `collection.anki2`: the collection of Anki 2.0 (schema 11), or a
    /// placeholder in later packages.
    Anki2,
    /// `collection.anki21`: the collection of Anki 2.1 (schema 11).
    Anki21,
    /// `collection.anki21b`: the current collection (schema 18), zstd.
    Anki21b,
    /// `media`: the media list (JSON, or zstd protobuf in the current
    /// format).
    MediaMap,
    /// A media file, named by its number in the media list.
    File(u32),
}

impl AnkiName {
    /// The entry named `name`, if it is one an Anki package has.
    fn of(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"meta" => Self::Meta,
            b"collection.anki2" => Self::Anki2,
            b"collection.anki21" => Self::Anki21,
            b"collection.anki21b" => Self::Anki21b,
            b"media" => Self::MediaMap,
            _ => Self::File(file_number(name)?),
        })
    }

    /// The name, for messages.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Anki2 => "collection.anki2",
            Self::Anki21 => "collection.anki21",
            Self::Anki21b => "collection.anki21b",
            Self::MediaMap => "media",
            Self::File(_) => "a media file",
        }
    }
}

/// A media file's number: decimal digits without a leading zero (except
/// `0` itself), at most 9 of them, as Anki names them.
pub(crate) fn file_number(name: &[u8]) -> Option<u32> {
    if name.is_empty() || name.len() > 9 || (name.len() > 1 && name[0] == b'0') {
        return None;
    }
    if !name.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(name).ok()?.parse().ok()
}

/// What an entry of the archive is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// `cards.json`.
    Json,
    /// The `media/` directory.
    MediaDir,
    /// `media/<sha256>.<ext>`, a media file, by its central name only.
    Media(MediaRef),
    /// An entry of an Anki package.
    Anki(AnkiName),
}

impl Role {
    /// The entry's name, for messages. A media file is not named: its name
    /// is the file's.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Json => JSON_ENTRY,
            Self::MediaDir => MEDIA_DIR,
            Self::Media(_) => "a media file",
            Self::Anki(name) => name.name(),
        }
    }

    /// The role of an entry named `name` (its central record's name) in an
    /// archive of `layout`, or `None` for a name it never has.
    fn of(name: &[u8], layout: Layout) -> Option<Self> {
        if layout == Layout::Anki {
            return AnkiName::of(name).map(Self::Anki);
        }
        if name == JSON_ENTRY.as_bytes() {
            return Some(Self::Json);
        }
        if name == MEDIA_DIR.as_bytes() {
            return Some(Self::MediaDir);
        }
        let file = name.strip_prefix(MEDIA_DIR.as_bytes())?;
        MediaRef::from_name_bytes(file).map(Self::Media)
    }
}

/// One entry, from its central record and its local header. Nothing of
/// its data has been read.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub(super) role: Role,
    /// Stored or Deflate.
    method: u16,
    /// The CRC-32 of its data, checked once the data is read.
    crc: u32,
    /// Where its data starts and how many bytes it takes in the file.
    data_start: u64,
    compressed: u64,
    /// The size the central record declares, decompressed. Not trusted:
    /// reading stops at the caller's cap whatever it says, and a size that
    /// differs from what the data gives refuses the entry.
    pub(super) size: u64,
    /// The bytes it takes, from its local header to the end of its data.
    pub(super) span: Range<u64>,
}

/// A reader that sees only the first `len` bytes of `inner`, whatever the
/// file grows to while it is read: every part of the import sees the same
/// end of the file, and nothing past the size checked against the limit
/// is ever read.
#[derive(Debug)]
pub(super) struct Window<R> {
    inner: R,
    len: u64,
    pos: u64,
}

impl<R: Read + Seek> Window<R> {
    /// `inner`, positioned at its start, cut to `len` bytes.
    pub(super) fn new(mut inner: R, len: u64) -> io::Result<Self> {
        inner.seek(SeekFrom::Start(0))?;
        Ok(Self { inner, len, pos: 0 })
    }
}

impl<R: Read> Read for Window<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        let room = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let n = self.inner.read(&mut buf[..room])?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// The longest forward seek done by reading (half the import's read
/// buffer).
const SKIP_BY_READING: u64 = 32 << 10;

impl<R: Read + Seek> Seek for Window<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        // A seek to where the reader already is costs nothing, and a short
        // one forward reads past the bytes: a seek would empty a
        // `BufReader`'s buffer, and an archive of 100,000 small media files
        // has its local headers a few bytes apart, read in order.
        if target > self.pos && target - self.pos <= SKIP_BY_READING {
            let gap = target - self.pos;
            let skipped = io::copy(&mut (&mut *self).take(gap), &mut io::sink())?;
            if skipped == gap {
                return Ok(target);
            }
        }
        if target != self.pos {
            self.inner.seek(SeekFrom::Start(target))?;
            self.pos = target;
        }
        Ok(target)
    }
}

/// A checked archive: its entries are exactly the ones a Rappel export
/// has (or, for an Anki package, the ones Anki writes, others passed
/// over), each stored in a supported way, and they do not overlap.
pub(super) struct Archive<R> {
    reader: Window<R>,
    entries: Vec<Entry>,
    /// `cards.json` (a Rappel export only).
    json: Option<Entry>,
    /// The media entries' positions in `entries`, sorted by hash (a Rappel
    /// export), or by number (an Anki package).
    media: Vec<u32>,
}

impl<R> std::fmt::Debug for Archive<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> Archive<R> {
    /// Reads the end record and the central directory of the archive in
    /// the first `len` bytes of `reader`, and checks its entries. Reads no
    /// entry's data.
    ///
    /// The entry count and the size of the central directory are checked
    /// against `limits` from the end record, before the central directory
    /// is read, so a file declaring millions of entries costs nothing; the
    /// walk reads at most that size, keeps no name, and stops at the first
    /// record that is not one Rappel writes.
    pub(super) fn open(reader: R, len: u64, limits: &Limits) -> Result<Self, ImportProblem> {
        let (reader, entries, directory_offset) = Self::walk(reader, len, limits, Layout::Rappel)?;
        if !entries.iter().any(|e| e.role == Role::Json) {
            return Err(ImportProblem::MissingEntry(JSON_ENTRY));
        }
        if !entries.iter().any(|e| e.role == Role::MediaDir) {
            return Err(ImportProblem::MissingEntry(MEDIA_DIR));
        }
        check_layout(&entries, directory_offset)?;
        let json = entries.iter().find(|e| e.role == Role::Json).cloned();
        let media = media_by_hash(&entries)?;
        Ok(Self {
            reader,
            entries,
            json,
            media,
        })
    }

    /// [`Self::open`] for an Anki package: its entries are those Anki
    /// writes, other names passed over, each name once, and at least one
    /// collection. Reads no entry's data.
    pub(super) fn open_anki(reader: R, len: u64, limits: &Limits) -> Result<Self, ImportProblem> {
        let (reader, entries, directory_offset) = Self::walk(reader, len, limits, Layout::Anki)?;
        let has = |name| entries.iter().any(|e| e.role == Role::Anki(name));
        if !(has(AnkiName::Anki2) || has(AnkiName::Anki21) || has(AnkiName::Anki21b)) {
            return Err(ImportProblem::MissingEntry("collection.anki2"));
        }
        check_layout(&entries, directory_offset)?;
        let media = files_by_number(&entries)?;
        Ok(Self {
            reader,
            entries,
            json: None,
            media,
        })
    }

    /// The end record and the central directory, walked; each entry's
    /// local header read. Returns the reader, the entries and where the
    /// central directory starts.
    fn walk(
        reader: R,
        len: u64,
        limits: &Limits,
        layout: Layout,
    ) -> Result<(Window<R>, Vec<Entry>, u64), ImportProblem> {
        if len > limits.archive {
            return Err(ImportProblem::FileTooLarge {
                limit: limits.archive,
            });
        }
        let mut reader = Window::new(reader, len).map_err(|e| io_problem(&e))?;
        let end = end_record(&mut reader, len)?;
        if end.entries > limits.entries {
            return Err(ImportProblem::TooManyEntries {
                limit: limits.entries,
            });
        }
        if end.directory_size > limits.central_directory {
            return Err(ImportProblem::InvalidArchive(format!(
                "the list of entries is larger than {} bytes",
                limits.central_directory
            )));
        }
        // The central directory ends before the end records start.
        end.directory_offset
            .checked_add(end.directory_size)
            .filter(|e| *e <= end.end_start)
            .ok_or_else(not_layout)?;
        let mut entries = walk_directory(&mut reader, &end, limits, layout)?;
        for entry in &mut entries {
            locate_data(&mut reader, entry)?;
        }
        Ok((reader, entries, end.directory_offset))
    }

    /// The entries, in archive order.
    #[cfg(test)]
    pub(super) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The media entries, sorted by hash (each hash once).
    pub(super) fn media_entries(&self) -> impl ExactSizeIterator<Item = &Entry> {
        self.media.iter().map(|&i| &self.entries[i as usize])
    }

    /// The media entry `n` of [`Self::media_entries`].
    pub(super) fn media_entry(&self, n: usize) -> Option<Entry> {
        let i = *self.media.get(n)?;
        self.entries.get(i as usize).cloned()
    }

    /// `cards.json`.
    pub(super) fn json_entry(&self) -> Result<Entry, ImportProblem> {
        self.json
            .clone()
            .ok_or(ImportProblem::MissingEntry(JSON_ENTRY))
    }

    /// The Anki package's entry `name` (not a media file), if it has one.
    pub(super) fn anki_entry(&self, name: AnkiName) -> Option<Entry> {
        self.entries
            .iter()
            .find(|e| e.role == Role::Anki(name))
            .cloned()
    }

    /// The Anki package's media file number `n`, if it has one.
    pub(super) fn anki_file(&self, n: u32) -> Option<Entry> {
        let number = |i: &u32| match self.entries[*i as usize].role {
            Role::Anki(AnkiName::File(n)) => n,
            _ => u32::MAX,
        };
        let at = self.media.binary_search_by_key(&n, number).ok()?;
        self.entries.get(self.media[at] as usize).cloned()
    }

    /// A reader of `entry`'s decompressed data that fails once more than
    /// `cap` bytes come out, whatever the entry declares, and hashes what
    /// it reads. At the end of the data, its CRC and size are checked.
    pub(super) fn open_entry(
        &mut self,
        entry: &Entry,
        cap: u64,
    ) -> Result<EntryReader<'_, R>, ImportProblem> {
        self.reader
            .seek(SeekFrom::Start(entry.data_start))
            .map_err(|e| io_problem(&e))?;
        let raw = (&mut self.reader).take(entry.compressed);
        let data = if entry.method == DEFLATED {
            Data::Deflated(DeflateDecoder::new(raw))
        } else {
            Data::Stored(raw)
        };
        Ok(EntryReader {
            data,
            entry: entry.role.name(),
            cap,
            declared: entry.size,
            expected_crc: entry.crc,
            count: 0,
            crc: crc32fast::Hasher::new(),
            hasher: Sha256::new(),
            failure: None,
        })
    }

    /// Streams `entry`'s data to `out`, at most `cap` bytes once
    /// decompressed, and returns how many bytes it had and their SHA-256.
    /// Nothing is kept in memory but a buffer.
    pub(super) fn copy_entry(
        &mut self,
        entry: &Entry,
        cap: u64,
        out: &mut impl Write,
    ) -> Result<Copied, CopyError> {
        let mut reader = self.open_entry(entry, cap).map_err(CopyError::Archive)?;
        let mut buf = vec![0; 64 << 10];
        loop {
            match reader.read(&mut buf) {
                // A failure is kept by the reader, and reported below.
                Ok(0) | Err(_) => break,
                Ok(n) => out.write_all(&buf[..n]).map_err(CopyError::Write)?,
            }
        }
        if let Some(problem) = reader.failure() {
            return Err(CopyError::Archive(problem));
        }
        let (bytes, sha256) = reader.finish();
        Ok(Copied { bytes, sha256 })
    }
}

/// What [`Archive::copy_entry`] copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Copied {
    /// Bytes written.
    pub(super) bytes: u64,
    /// Their SHA-256.
    pub(super) sha256: [u8; 32],
}

/// Why [`Archive::copy_entry`] failed.
#[derive(Debug)]
pub(super) enum CopyError {
    /// The entry is damaged or larger than the cap.
    Archive(ImportProblem),
    /// Writing the copy failed.
    Write(io::Error),
}

/// Why an entry reader stopped early.
#[derive(Debug, Clone)]
enum Failure {
    /// The data is damaged: it cannot be inflated, or its CRC or size is
    /// not the one its central record declares.
    Damaged(String),
    /// More than the cap came out.
    TooLarge,
}

/// An entry's data as it is in the file.
enum Data<'a, R> {
    Stored(io::Take<&'a mut Window<R>>),
    Deflated(DeflateDecoder<io::Take<&'a mut Window<R>>>),
}

impl<R: Read> Read for Data<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Stored(r) => r.read(buf),
            Self::Deflated(r) => r.read(buf),
        }
    }
}

/// The decompressed data of one entry, counted against a cap, hashed, and
/// checked against its CRC and size at its end. After a failure every
/// read fails, and [`Self::failure`] says why.
pub(super) struct EntryReader<'a, R> {
    data: Data<'a, R>,
    entry: &'static str,
    cap: u64,
    declared: u64,
    expected_crc: u32,
    count: u64,
    crc: crc32fast::Hasher,
    hasher: Sha256,
    failure: Option<Failure>,
}

impl<R> std::fmt::Debug for EntryReader<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryReader")
            .field("entry", &self.entry)
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl<R: Read> EntryReader<'_, R> {
    /// Reads what is left, so the count, the hash and the CRC check cover
    /// the whole entry (or stop at the first failure).
    pub(super) fn drain(&mut self) {
        let mut buf = [0; 16 << 10];
        while let Ok(n) = self.read(&mut buf) {
            if n == 0 {
                break;
            }
        }
    }

    /// Why reading failed, as the import reports it.
    pub(super) fn failure(&self) -> Option<ImportProblem> {
        self.failure.as_ref().map(|f| match f {
            Failure::Damaged(message) => ImportProblem::InvalidArchive(message.clone()),
            Failure::TooLarge => ImportProblem::EntryTooLarge {
                entry: self.entry,
                limit: self.cap,
            },
        })
    }

    /// The bytes read and their SHA-256.
    pub(super) fn finish(self) -> (u64, [u8; 32]) {
        (self.count, self.hasher.finalize().into())
    }

    fn fail(&mut self, failure: Failure) -> io::Error {
        self.failure = Some(failure);
        io::Error::other("the entry cannot be read")
    }
}

impl<R: Read> Read for EntryReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other("the entry cannot be read"));
        }
        // One byte past the cap is enough to know it is too large.
        let left = (self.cap - self.count.min(self.cap)).saturating_add(1);
        let room = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let n = match self.data.read(&mut buf[..room]) {
            Ok(n) => n,
            Err(err) => return Err(self.fail(Failure::Damaged(err.to_string()))),
        };
        if n == 0 && room > 0 {
            // The end of the data: it must be what the central record says.
            let crc = self.crc.clone().finalize();
            if self.count != self.declared {
                return Err(self.fail(Failure::Damaged(format!(
                    "{} is not the size its entry declares",
                    self.entry
                ))));
            }
            if crc != self.expected_crc {
                return Err(self.fail(Failure::Damaged("invalid checksum".to_owned())));
            }
            return Ok(0);
        }
        self.count += n as u64;
        if self.count > self.cap {
            return Err(self.fail(Failure::TooLarge));
        }
        // More than it declares is refused at once, not at its end: a
        // bomb declaring 100 bytes stops there, not at the cap (ADR 0063).
        if self.count > self.declared {
            return Err(self.fail(Failure::Damaged(format!(
                "{} is not the size its entry declares",
                self.entry
            ))));
        }
        self.crc.update(&buf[..n]);
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// What the end record says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EndRecord {
    entries: u64,
    directory_size: u64,
    directory_offset: u64,
    /// Where the end records start (the zip64 one, if any): the central
    /// directory must end before.
    end_start: u64,
}

/// The entry count and the central directory's size from the archive's
/// end record. The record must end the file (no archive comment, which
/// Rappel never writes) and describe a single-disk archive, so it cannot
/// be confused with bytes inside an entry. A count, size or offset that
/// does not fit the classic record (more than 65,534 entries) is in a
/// zip64 record, which must be the 56 bytes just before its locator, and
/// the locator the 20 bytes just before the classic record.
fn end_record<R: Read + Seek>(reader: &mut R, len: u64) -> Result<EndRecord, ImportProblem> {
    let not_cards = || ImportProblem::InvalidArchive("no end of archive record".to_owned());
    let start = len.checked_sub(EOCD_LEN as u64).ok_or_else(not_cards)?;
    let mut eocd = [0; EOCD_LEN];
    read_at(reader, start, &mut eocd)?;
    let (disk, cd_disk, on_disk, total) = (
        u16_at(&eocd, 4),
        u16_at(&eocd, 6),
        u16_at(&eocd, 8),
        u16_at(&eocd, 10),
    );
    let (size, offset, comment) = (u32_at(&eocd, 12), u32_at(&eocd, 16), u16_at(&eocd, 20));
    if eocd[..4] != EOCD_SIGNATURE || disk != 0 || cd_disk != 0 || on_disk != total || comment != 0
    {
        return Err(not_cards());
    }
    let zip64 = total == u16::MAX || size == u32::MAX || offset == u32::MAX;
    if !zip64 {
        return Ok(EndRecord {
            entries: u64::from(total),
            directory_size: u64::from(size),
            directory_offset: u64::from(offset),
            end_start: start,
        });
    }

    let locator_start = start
        .checked_sub(ZIP64_LOCATOR_LEN as u64)
        .ok_or_else(not_cards)?;
    let record_start = locator_start
        .checked_sub(ZIP64_EOCD_LEN as u64)
        .ok_or_else(not_cards)?;
    let mut locator = [0; ZIP64_LOCATOR_LEN];
    read_at(reader, locator_start, &mut locator)?;
    if locator[..4] != ZIP64_LOCATOR_SIGNATURE
        || u32_at(&locator, 4) != 0
        || u64_at(&locator, 8) != record_start
        || u32_at(&locator, 16) != 1
    {
        return Err(not_cards());
    }
    let mut record = [0; ZIP64_EOCD_LEN];
    read_at(reader, record_start, &mut record)?;
    let entries = u64_at(&record, 32);
    let directory_size = u64_at(&record, 40);
    let directory_offset = u64_at(&record, 48);
    // No extensible data, one disk, and each classic field either its
    // "see zip64" marker or the same value.
    if record[..4] != ZIP64_EOCD_SIGNATURE
        || u64_at(&record, 4) != (ZIP64_EOCD_LEN - 12) as u64
        || u32_at(&record, 16) != 0
        || u32_at(&record, 20) != 0
        || u64_at(&record, 24) != entries
        || (total != u16::MAX && u64::from(total) != entries)
        || (size != u32::MAX && u64::from(size) != directory_size)
        || (offset != u32::MAX && u64::from(offset) != directory_offset)
    {
        return Err(not_cards());
    }
    Ok(EndRecord {
        entries,
        directory_size,
        directory_offset,
        end_start: record_start,
    })
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, buf: &mut [u8]) -> Result<(), ImportProblem> {
    reader
        .seek(SeekFrom::Start(at))
        .and_then(|_| reader.read_exact(buf))
        .map_err(|e| io_problem(&e))
}

fn u16_at(bytes: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([bytes[i], bytes[i + 1]])
}

fn u32_at(bytes: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
}

fn u64_at(bytes: &[u8], i: usize) -> u64 {
    let mut b = [0; 8];
    b.copy_from_slice(&bytes[i..i + 8]);
    u64::from_le_bytes(b)
}

/// The central directory, walked record by record: exactly as many
/// records as the end record declares, each inside the size it declares,
/// which they must fill. Only what each entry needs is kept, never its name
/// or comment, and the first name Rappel does not write refuses the file,
/// so the walk costs at most the declared size in reading and nothing
/// that grows with it in memory.
fn walk_directory<R: Read + Seek>(
    reader: &mut Window<R>,
    end: &EndRecord,
    limits: &Limits,
    layout: Layout,
) -> Result<Vec<Entry>, ImportProblem> {
    let short = || ImportProblem::InvalidArchive("the list of entries is cut short".to_owned());
    reader
        .seek(SeekFrom::Start(end.directory_offset))
        .map_err(|e| io_problem(&e))?;
    let mut directory = io::BufReader::with_capacity(64 << 10, reader.take(end.directory_size));
    let capacity = usize::try_from(end.entries).unwrap_or(0).min(16);
    let mut entries: Vec<Entry> = Vec::with_capacity(capacity);
    let mut name = Vec::new();
    let mut extra = Vec::new();
    let mut seen = Seen::default();
    for _ in 0..end.entries {
        let mut record = [0; CENTRAL_LEN];
        directory.read_exact(&mut record).map_err(|_| short())?;
        if record[..4] != CENTRAL_SIGNATURE {
            return Err(ImportProblem::InvalidArchive(
                "an entry of the list is damaged".to_owned(),
            ));
        }
        // Only these fields are read. The disk-start field is not checked
        // (the end record already says one disk), and the local header's
        // name is never compared with the central record's: nothing may
        // rely on either. An entry is what its central name says (the
        // media import names media entries by that name only).
        let flags = u16_at(&record, 8);
        let method = u16_at(&record, 10);
        let crc = u32_at(&record, 16);
        let (name_len, extra_len, comment_len) = (
            usize::from(u16_at(&record, 28)),
            usize::from(u16_at(&record, 30)),
            u64::from(u16_at(&record, 32)),
        );
        name.resize(name_len, 0);
        directory.read_exact(&mut name).map_err(|_| short())?;
        extra.resize(extra_len, 0);
        directory.read_exact(&mut extra).map_err(|_| short())?;
        let skipped = io::copy(&mut (&mut directory).take(comment_len), &mut io::sink())
            .map_err(|_| short())?;
        if skipped != comment_len {
            return Err(short());
        }
        let Some(role) = Role::of(&name, layout) else {
            if layout == Layout::Anki {
                // Never read: only the entries Anki writes are.
                continue;
            }
            return Err(ImportProblem::UnexpectedEntry(shown_name(&name)));
        };
        // A name used twice would make the archive mean two things. Media
        // names are compared once the walk is done ([`media_by_hash`]), by
        // sorting, so the check stays quick for 100,000 of them.
        let repeated = seen.repeated(role);
        if repeated {
            return Err(twice());
        }
        let [compressed, size, local] = zip64_values(
            [
                u32_at(&record, 20),
                u32_at(&record, 24),
                u32_at(&record, 42),
            ],
            &extra,
        )?;
        // Bit 0: encrypted; bit 6: strong encryption.
        let encrypted = flags & 0x41 != 0;
        let supported = match role {
            Role::Json | Role::Media(_) | Role::Anki(_) => matches!(method, STORED | DEFLATED),
            Role::MediaDir => size == 0 && matches!(method, STORED | DEFLATED),
        };
        if encrypted || !supported {
            return Err(ImportProblem::UnsupportedEntry(role.name()));
        }
        if let Role::Media(media) = role {
            // Refused from the size it declares; reading it is capped at
            // the same limit whatever it declares.
            if size == 0 {
                return Err(ImportProblem::InvalidArchive(
                    "a media file is empty".to_owned(),
                ));
            }
            let limit = limits.media(media.kind());
            if size > limit {
                return Err(ImportProblem::EntryTooLarge {
                    entry: role.name(),
                    limit,
                });
            }
        }
        entries.push(Entry {
            role,
            method,
            crc,
            data_start: local,
            compressed,
            size,
            span: local..local,
        });
    }
    // The records must fill the declared size, no more and no less.
    let mut rest = [0];
    if directory.read(&mut rest).map_err(|e| io_problem(&e))? != 0 {
        return Err(ImportProblem::InvalidArchive(
            "the list of entries holds more than it declares".to_owned(),
        ));
    }
    Ok(entries)
}

/// The fixed names a walk has seen, one bit each.
#[derive(Debug, Default)]
struct Seen(u8);

impl Seen {
    /// Whether `role`'s name was seen before (never for numbered or hashed
    /// media, which are compared by sorting once the walk is done).
    fn repeated(&mut self, role: Role) -> bool {
        let bit: u8 = match role {
            Role::Media(_) | Role::Anki(AnkiName::File(_)) => return false,
            Role::Json => 1,
            Role::MediaDir => 2,
            Role::Anki(AnkiName::Meta) => 4,
            Role::Anki(AnkiName::Anki2) => 8,
            Role::Anki(AnkiName::Anki21) => 16,
            Role::Anki(AnkiName::Anki21b) => 32,
            Role::Anki(AnkiName::MediaMap) => 64,
        };
        let seen = self.0 & bit != 0;
        self.0 |= bit;
        seen
    }
}

/// A name used twice.
fn twice() -> ImportProblem {
    ImportProblem::UnexpectedEntry("(a name used twice)".to_owned())
}

/// The positions of the media entries, sorted by hash. Two entries with
/// the same hash (the same name, or the same hash with two extensions)
/// refuse the file: one file, one entry.
fn media_by_hash(entries: &[Entry]) -> Result<Vec<u32>, ImportProblem> {
    let hash = |i: u32| match entries[i as usize].role {
        Role::Media(media) => Some(*media.hash()),
        Role::Json | Role::MediaDir | Role::Anki(_) => None,
    };
    let mut media: Vec<u32> = (0..entries.len())
        .filter_map(|i| u32::try_from(i).ok())
        .filter(|&i| hash(i).is_some())
        .collect();
    media.sort_unstable_by_key(|&i| hash(i));
    if media.windows(2).any(|w| hash(w[0]) == hash(w[1])) {
        return Err(twice());
    }
    Ok(media)
}

/// The positions of an Anki package's numbered media files, sorted by
/// number. A number used twice refuses the file.
fn files_by_number(entries: &[Entry]) -> Result<Vec<u32>, ImportProblem> {
    let number = |i: u32| match entries[i as usize].role {
        Role::Anki(AnkiName::File(n)) => Some(n),
        _ => None,
    };
    let mut files: Vec<u32> = (0..entries.len())
        .filter_map(|i| u32::try_from(i).ok())
        .filter(|&i| number(i).is_some())
        .collect();
    files.sort_unstable_by_key(|&i| number(i));
    if files.windows(2).any(|w| number(w[0]) == number(w[1])) {
        return Err(twice());
    }
    Ok(files)
}

/// The compressed size, size and local header offset of a central record,
/// each from its 32-bit field or, when that holds the zip64 marker, from
/// the zip64 extra field, which has the marked ones in that order.
fn zip64_values(fields: [u32; 3], extra: &[u8]) -> Result<[u64; 3], ImportProblem> {
    let bad = || ImportProblem::InvalidArchive("an entry's sizes cannot be read".to_owned());
    let mut values = fields.map(u64::from);
    if !fields.contains(&u32::MAX) {
        return Ok(values);
    }
    // The extra field is a list of (id, length, data).
    let mut rest = extra;
    let data = loop {
        if rest.len() < 4 {
            return Err(bad());
        }
        let (id, len) = (u16_at(rest, 0), usize::from(u16_at(rest, 2)));
        let body = rest.get(4..4 + len).ok_or_else(bad)?;
        if id == ZIP64_EXTRA {
            break body;
        }
        rest = &rest[4 + len..];
    };
    // The order of the zip64 field: size, compressed size, offset.
    let mut at = 0;
    for i in [1, 0, 2] {
        if fields[i] == u32::MAX {
            let bytes = data.get(at..at + 8).ok_or_else(bad)?;
            values[i] = u64_at(bytes, 0);
            at += 8;
        }
    }
    Ok(values)
}

/// Reads `entry`'s local header for where its data starts, and the bytes
/// it takes. A data descriptor after the data is not counted.
fn locate_data<R: Read + Seek>(
    reader: &mut Window<R>,
    entry: &mut Entry,
) -> Result<(), ImportProblem> {
    let start = entry.data_start;
    let mut header = [0; LOCAL_HEADER_LEN];
    read_at(reader, start, &mut header)?;
    if header[..4] != LOCAL_SIGNATURE {
        return Err(ImportProblem::InvalidArchive(
            "an entry's header is missing".to_owned(),
        ));
    }
    let data = start
        .saturating_add(LOCAL_HEADER_LEN as u64)
        .saturating_add(u64::from(u16_at(&header, 26)))
        .saturating_add(u64::from(u16_at(&header, 28)));
    entry.data_start = data;
    entry.span = start..data.saturating_add(entry.compressed);
    Ok(())
}

/// Two central records may point at the same bytes (overlapping entries,
/// the trick of the "better" zip bombs). Rappel never writes that, so it
/// is refused as a malformed file: the entries must not overlap, and each
/// must end before the central directory starts. Sorted by start, each
/// entry must end before the next one starts.
fn check_layout(entries: &[Entry], cd_start: u64) -> Result<(), ImportProblem> {
    let mut spans: Vec<&Range<u64>> = entries.iter().map(|e| &e.span).collect();
    spans.sort_by_key(|s| s.start);
    let overlap = spans.windows(2).any(|w| w[0].end > w[1].start);
    if overlap || spans.iter().any(|s| s.end > cd_start) {
        return Err(not_layout());
    }
    Ok(())
}

/// Entries that overlap or are out of place.
fn not_layout() -> ImportProblem {
    ImportProblem::InvalidArchive("entries overlap or are out of place".to_owned())
}

fn io_problem(err: &io::Error) -> ImportProblem {
    ImportProblem::InvalidArchive(err.to_string())
}

/// An entry name for a message: escaped and at most 64 characters, since
/// it comes from the untrusted file.
fn shown_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    let mut shown: String = name
        .chars()
        .take(64)
        .collect::<String>()
        .escape_debug()
        .to_string();
    if name.chars().count() > 64 {
        shown.push('…');
    }
    format!("\"{shown}\"")
}
