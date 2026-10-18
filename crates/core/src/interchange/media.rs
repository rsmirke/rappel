//! The media of an import (ADR 0054): the `media` records of `cards.json`
//! checked as they stream by, matched with the archive's `media/<sha256>.<ext>`
//! entries, each entry's content checked (SHA-256 and first bytes) before
//! anything is written, and, when applying, each new file streamed into the
//! media folder.
//!
//! The records keep a note each (a hash, a size, a time: 56 bytes), and no
//! more notes than the archive has media entries (at most 100,000), since
//! a record without an entry refuses the file anyway.

use std::io::{self, Read, Seek, Write};
use std::path::Path;

use rusqlite::Connection;

use super::archive::{Archive, CopyError, Role};
use super::format::MediaRecord;
use super::validate::{invalid, media_fields};
use super::{FieldProblem, ImportProblem, Limits};
use crate::error::{Error, Result};
use crate::media::{
    DETECT_BYTES, Ingested, MediaKind, MediaRef, Sync, check_structure, ingest, insert_media_row,
    sync_folder,
};

/// What the import keeps of one media record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MediaNote {
    pub(super) media: MediaRef,
    pub(super) bytes: u64,
    pub(super) created_at: i64,
    /// The database does not have it: the apply adds it.
    pub(super) new: bool,
}

/// The media records read so far.
#[derive(Debug, Default)]
pub(super) struct MediaList {
    notes: Vec<MediaNote>,
    /// How many notes are kept at most: the archive's media entries.
    room: usize,
    /// Records read, kept or not.
    count: usize,
    /// The hash of the last record read past `room`.
    overflow: Option<[u8; 32]>,
    /// The first record's problem, in row order; later records are not
    /// looked at.
    problem: Option<ImportProblem>,
}

impl MediaList {
    /// A list for an archive with `room` media entries.
    pub(super) fn new(room: usize) -> Self {
        Self {
            room,
            ..Self::default()
        }
    }

    /// Checks record `index` on its own and against the one before it.
    pub(super) fn push(&mut self, index: usize, record: &MediaRecord, limits: &Limits) {
        if self.problem.is_some() {
            return;
        }
        let previous = self.last_hash();
        match media_fields(record, previous.as_ref(), limits) {
            Err((field, reason)) => {
                self.problem = Some(invalid("media", index, field, reason));
            }
            Ok((media, bytes)) => {
                self.count += 1;
                if self.notes.len() < self.room {
                    self.notes.push(MediaNote {
                        media,
                        bytes,
                        created_at: record.created_at,
                        new: false,
                    });
                } else {
                    // Past the entries there is no file for it: refused by
                    // [`check_entries`]; only its hash is needed for the
                    // order of the next one.
                    self.overflow = Some(*media.hash());
                }
            }
        }
    }

    fn last_hash(&self) -> Option<[u8; 32]> {
        self.overflow
            .or_else(|| self.notes.last().map(|n| *n.media.hash()))
    }

    /// The first record that breaks a rule of its own.
    pub(super) fn problem(&self) -> Option<ImportProblem> {
        self.problem.clone()
    }

    pub(super) fn notes_mut(&mut self) -> &mut [MediaNote] {
        &mut self.notes
    }

    /// How many records are new.
    pub(super) fn new_count(&self) -> usize {
        self.notes.iter().filter(|n| n.new).count()
    }
}

/// An entry's name for a message: built from its checked hash and kind.
fn entry_name(media: &MediaRef) -> String {
    format!("\"media/{}\"", media.file_name())
}

/// Each record has exactly one entry, `media/<hash>.<ext>` with the
/// extension of its type and the size it says, and each media entry has a
/// record. Both are sorted by hash, so they are walked side by side; the
/// first mismatch in hash order is reported.
pub(super) fn check_entries<R: Read + Seek>(
    list: &MediaList,
    archive: &Archive<R>,
) -> std::result::Result<(), ImportProblem> {
    let mut entries = archive.media_entries().peekable();
    let entry_media = |role: Role| match role {
        Role::Media(media) => Some(media),
        Role::Json | Role::MediaDir | Role::Anki(_) => None,
    };
    for (index, note) in list.notes.iter().enumerate() {
        let no_file = || invalid("media", index, "hash", FieldProblem::NoMediaFile);
        // An entry before this record's hash has no record.
        if let Some(media) = entries.peek().and_then(|e| entry_media(e.role))
            && media.hash() < note.media.hash()
        {
            return Err(ImportProblem::UnexpectedEntry(entry_name(&media)));
        }
        let Some(entry) =
            entries.next_if(|e| entry_media(e.role).is_some_and(|m| m.hash() == note.media.hash()))
        else {
            return Err(no_file());
        };
        if entry.role != Role::Media(note.media) {
            // The same hash under another extension: not this record's name.
            return Err(no_file());
        }
        if entry.size != note.bytes {
            return Err(invalid("media", index, "bytes", FieldProblem::SizeDiffers));
        }
    }
    if list.count > list.notes.len() {
        return Err(invalid(
            "media",
            list.notes.len(),
            "hash",
            FieldProblem::NoMediaFile,
        ));
    }
    if let Some(media) = entries.next().and_then(|e| entry_media(e.role)) {
        return Err(ImportProblem::UnexpectedEntry(entry_name(&media)));
    }
    Ok(())
}

/// Keeps the first bytes written to it, or all of them for an audio file
/// (at most its 20 MB cap, which the archive reader enforces), whose whole
/// structure is checked (`check_structure`, ADR 0056).
#[derive(Debug, Default)]
struct Head {
    bytes: Vec<u8>,
    whole: bool,
}

impl Write for Head {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let room = if self.whole {
            buf.len()
        } else {
            DETECT_BYTES.saturating_sub(self.bytes.len()).min(buf.len())
        };
        self.bytes.extend_from_slice(&buf[..room]);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Reads every media entry once, without writing it anywhere: its size and
/// CRC (checked by the archive reader, capped at its kind's limit), its
/// first bytes, which must be of the record's type (an audio file: all of
/// it plain audio of that type, ADR 0056), and its SHA-256, which
/// must be the record's hash. So a file whose media lie is refused before
/// anything is written. Run after [`check_entries`], which pairs the
/// entries with the notes in order.
pub(super) fn check_contents<R: Read + Seek>(
    list: &MediaList,
    archive: &mut Archive<R>,
    limits: &Limits,
) -> std::result::Result<(), ImportProblem> {
    for (index, note) in list.notes.iter().enumerate() {
        let kind = note.media.kind();
        let Some(entry) = archive.media_entry(index) else {
            return Err(invalid("media", index, "hash", FieldProblem::NoMediaFile));
        };
        let mut head = Head {
            bytes: Vec::new(),
            whole: !kind.is_image(),
        };
        let copied = match archive.copy_entry(&entry, limits.media(kind), &mut head) {
            Ok(copied) => copied,
            Err(CopyError::Archive(problem)) => return Err(problem),
            Err(CopyError::Write(err)) => {
                return Err(ImportProblem::InvalidArchive(err.to_string()));
            }
        };
        if MediaKind::detect(&head.bytes) != Some(kind)
            || check_structure(kind, &head.bytes).is_err()
        {
            return Err(invalid("media", index, "mime", FieldProblem::NotOfItsType));
        }
        if copied.sha256 != *note.media.hash() {
            return Err(invalid(
                "media",
                index,
                "hash",
                FieldProblem::MediaFileDiffers,
            ));
        }
    }
    Ok(())
}

/// Applying: streams each new media file of the archive into the media
/// folder `dir` and records it in `conn` (the apply's transaction), with
/// the record's `created_at`. The same code as [`crate::storage::Store::add_media`],
/// which hashes the file again: anything other than the plan's content
/// (another hash, kind or size, a damaged entry) is a changed file, and the
/// transaction is dropped. A file written before that stays in the folder
/// under its own hash, unused (slice 5 removes unused files); nothing is
/// deleted here.
pub(super) fn store_new_media<R: Read + Seek>(
    conn: &Connection,
    list: &MediaList,
    archive: &mut Archive<R>,
    limits: &Limits,
    dir: &Path,
) -> Result<usize> {
    let mut added = 0;
    for (index, note) in list.notes.iter().enumerate() {
        if !note.new {
            continue;
        }
        let entry = archive.media_entry(index).ok_or(Error::ImportFileChanged)?;
        let mut reader = archive
            .open_entry(&entry, limits.media(note.media.kind()))
            .map_err(|_| Error::ImportFileChanged)?;
        let got = ingest(dir, &mut reader, Sync::FileOnly);
        if reader.failure().is_some() {
            return Err(Error::ImportFileChanged);
        }
        let got: Ingested = got.map_err(|err| match err {
            Error::MediaRefused(_) | Error::MediaRead(_) => Error::ImportFileChanged,
            other => other,
        })?;
        if got.media != note.media || got.bytes != note.bytes {
            return Err(Error::ImportFileChanged);
        }
        insert_media_row(conn, &got, note.created_at)?;
        added += 1;
    }
    // Each file is synced; their names once, before the rows commit.
    if added > 0 {
        sync_folder(dir)?;
    }
    Ok(added)
}
