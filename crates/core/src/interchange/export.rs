//! Exporting the whole database to a `.rappel` file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Seek, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::ExportSummary;
use super::archive::ArchiveWriter;
use super::format::{
    CardRecord, CardTagRecord, DeckRecord, Document, EventRecord, FORMAT_VERSION, MediaRecord,
    SettingsRecord, TagRecord,
};
use crate::error::{Error, Result};
use crate::media::{CopyFailure, MediaKind, MediaRef, copy_capped, parse_hex};
use crate::storage::{Store, stored_settings};
use crate::time::now_ms;

impl Store {
    /// Writes every deck, card, tag, card tag and log entry, deleted ones
    /// included, the stored settings and every media file, to the
    /// `.rappel` file `path`, replacing it if it exists.
    ///
    /// The file is written next to `path` with mode `0600`, synced, and
    /// only then renamed, so `path` is never a partial export. Media files
    /// are streamed from the media folder, never held in memory. A media
    /// file that is missing or damaged is left out with its record and
    /// counted in [`ExportSummary::missing_media`]. The same database
    /// exported at the same time gives the same bytes (ADR 0017, 0054).
    ///
    /// # Errors
    /// SQLite and IO errors, or [`Error::ExportFailed`]. On error `path`
    /// is left as it was.
    pub fn export_to(&self, path: &Path) -> Result<ExportSummary> {
        write_file_atomically(path, |file| {
            let mut out = BufWriter::with_capacity(64 << 10, file);
            let summary = self.write_export(&mut out, now_ms())?;
            out.flush().map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(summary)
        })
    }

    /// The archive bytes of an export made at `now` (UTC milliseconds).
    #[cfg(test)]
    pub(crate) fn export_bytes_at(&self, now: i64) -> Result<(Vec<u8>, ExportSummary)> {
        let mut out = io::Cursor::new(Vec::new());
        let summary = self.write_export(&mut out, now)?;
        Ok((out.into_inner(), summary))
    }

    /// Writes the export made at `now` to `out`.
    fn write_export<W: Write + Seek>(&self, out: W, now: i64) -> Result<ExportSummary> {
        let (mut document, rows) = read_document(&self.conn, now)?;
        // Only the files that are there and whole go in, with their
        // records, so the export stays importable.
        let mut files = Vec::with_capacity(rows.len());
        let mut missing_media = 0;
        for (record, media) in rows {
            match self.media_file(&media, &record) {
                Some(path) => {
                    document.media.push(record);
                    files.push((media, path));
                }
                None => missing_media += 1,
            }
        }
        if missing_media > 0 {
            tracing::warn!(
                missing_media,
                "media files missing or damaged, left out of the export"
            );
        }
        let summary = ExportSummary {
            decks: document.decks.len(),
            cards: document.cards.len(),
            tags: document.tags.len(),
            review_events: document.review_events.len(),
            media: files.len(),
            missing_media,
        };
        let mut json =
            serde_json::to_vec_pretty(&document).map_err(|e| Error::ExportFailed(Box::new(e)))?;
        json.push(b'\n');
        drop(document);
        let failed = |e: zip::result::ZipError| Error::ExportFailed(Box::new(e));
        let mut archive = ArchiveWriter::new(out, &json).map_err(failed)?;
        drop(json);
        for (media, path) in &files {
            let entry = archive.start_media(&media.file_name()).map_err(failed)?;
            copy_media(path, media, entry)?;
        }
        archive.finish().map_err(failed)?;
        Ok(summary)
    }

    /// The path of `media`'s file if it is a regular file whose size, kind
    /// and SHA-256 are those recorded (read once to check); else `None`,
    /// and it is left out of the export.
    fn media_file(&self, media: &MediaRef, record: &MediaRecord) -> Option<PathBuf> {
        let path = self.media_path(media)?;
        let mut file = open_regular(&path).ok()?;
        let copied = copy_capped(&mut file, &mut io::sink()).ok()?;
        let whole =
            copied.media == *media && i64::try_from(copied.bytes).ok() == Some(record.bytes);
        whole.then_some(path)
    }
}

/// Opens `path` for reading if it is a regular file, never through a
/// symbolic link (the media folder holds only Rappel's own files).
fn open_regular(path: &Path) -> io::Result<File> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

/// Streams the media file at `path` into the archive entry `out`, hashing
/// it again: a file that changed since it was checked fails the export
/// (the entry is written already, so it cannot be left out any more).
fn copy_media(path: &Path, media: &MediaRef, out: &mut impl Write) -> Result<()> {
    let io_err = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = open_regular(path).map_err(io_err)?;
    match copy_capped(&mut file, out) {
        Ok(copied) if copied.media == *media => Ok(()),
        Ok(_) | Err(CopyFailure::Refused(_)) => Err(Error::ExportFailed(
            "a media file changed while it was exported".into(),
        )),
        Err(CopyFailure::Read(source) | CopyFailure::Write(source)) => Err(io_err(source)),
    }
}

/// Every row and the stored settings, read in one transaction so the
/// tables agree, each table sorted by id (`card_tags` by card, then tag),
/// and the media rows ([`media_rows`]). The document's `media` is empty:
/// the caller adds the files it finds.
fn read_document(conn: &Connection, now: i64) -> Result<(Document, Vec<(MediaRecord, MediaRef)>)> {
    let tx = conn.unchecked_transaction()?;
    let decks = tx
        .prepare("SELECT id, name, created_at, updated_at, deleted_at FROM decks ORDER BY id")?
        .query_map([], |r| {
            Ok(DeckRecord {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                updated_at: r.get(3)?,
                deleted_at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let cards = tx
        .prepare(
            "SELECT id, deck_id, front_md, back_md, created_at, updated_at, suspended, deleted_at
             FROM cards ORDER BY id",
        )?
        .query_map([], |r| {
            Ok(CardRecord {
                id: r.get(0)?,
                deck_id: r.get(1)?,
                front_md: r.get(2)?,
                back_md: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
                suspended: r.get(6)?,
                deleted_at: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let tags = tx
        .prepare("SELECT id, name FROM tags ORDER BY id")?
        .query_map([], |r| {
            Ok(TagRecord {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let card_tags = tx
        .prepare("SELECT card_id, tag_id FROM card_tags ORDER BY card_id, tag_id")?
        .query_map([], |r| {
            Ok(CardTagRecord {
                card_id: r.get(0)?,
                tag_id: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let review_events = tx
        .prepare(
            "SELECT id, card_id, kind, reviewed_at, grade, duration_ms, undoes
             FROM review_events ORDER BY id",
        )?
        .query_map([], |r| {
            Ok(EventRecord {
                id: r.get(0)?,
                card_id: r.get(1)?,
                kind: r.get(2)?,
                reviewed_at: r.get(3)?,
                grade: r.get(4)?,
                duration_ms: r.get(5)?,
                undoes: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let media = media_rows(&tx)?;
    let stored = stored_settings(&tx)?;
    let settings = SettingsRecord {
        desired_retention: stored.desired_retention,
        max_new_per_day: stored.max_new_per_day,
        max_reviews_per_day: stored.max_reviews_per_day,
        day_start_hour: stored.day_start_hour,
    };
    // Read only: ending the transaction cannot lose anything.
    tx.finish()?;
    Ok((
        Document {
            format_version: FORMAT_VERSION,
            exported_at: now,
            decks,
            cards,
            tags,
            card_tags,
            review_events,
            settings,
            media: Vec::new(),
        },
        media,
    ))
}

/// The media rows, sorted by hash, each with its file; a row whose hash or
/// type the schema let through but Rappel cannot name is left out.
fn media_rows(conn: &Connection) -> Result<Vec<(MediaRecord, MediaRef)>> {
    let rows = conn
        .prepare("SELECT hash, mime, bytes, created_at FROM media ORDER BY hash")?
        .query_map([], |r| {
            Ok(MediaRecord {
                hash: r.get(0)?,
                mime: r.get(1)?,
                bytes: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|record| {
            let hash = parse_hex(record.hash.as_bytes())?;
            let kind = MediaKind::from_mime(&record.mime)?;
            Some((record, MediaRef::new(hash, kind)))
        })
        .collect())
}

/// Writes `<path>.partial` (mode `0600`) with `write`, syncs it, renames
/// it to `path` and syncs the directory. The partial file is removed on
/// error.
fn write_file_atomically<T>(path: &Path, write: impl FnOnce(&mut File) -> Result<T>) -> Result<T> {
    let io_err = |p: &Path| {
        let path = p.to_path_buf();
        move |source| Error::Io { path, source }
    };
    let partial = partial_path(path);
    // A leftover from a crashed export is removed, and the file is then
    // created anew: an existing file would keep its mode, and a symlink
    // there would be followed.
    match fs::remove_file(&partial) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(&partial)(e)),
    }
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(io_err(&partial))?;
        let value = write(&mut file)?;
        file.sync_all().map_err(io_err(&partial))?;
        fs::rename(&partial, path).map_err(io_err(path))?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            File::open(dir)
                .and_then(|d| d.sync_all())
                .map_err(io_err(dir))?;
        }
        Ok(value)
    })();
    if result.is_err() {
        // Best effort: the error being returned matters more.
        let _ = fs::remove_file(&partial);
    }
    result
}

fn partial_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}
