//! Export and import of the whole database as a `.rappel` file (SPEC §4.1,
//! ADR 0017).
//!
//! A `.rappel` file is a zip archive holding `cards.json` (decks, cards,
//! tags, the review log with its resets and undos, deleted rows included,
//! the settings and the list of media files), the `media/` directory and
//! one entry `media/<sha256>.<ext>` per media file (ADR 0054). The same
//! database exported at the same time always gives the same bytes.
//!
//! Import is two steps. [`Store::plan_import`](crate::storage::Store::plan_import)
//! reads and checks the file and compares it with the database without
//! writing anything; [`Store::apply_import`](crate::storage::Store::apply_import)
//! backs the database up, then reads the file again and adds the new rows
//! and brings back deleted rows that are live in the file, in one
//! transaction. Both read the file as a stream and keep only compact notes
//! (ADR 0053). The file is untrusted: anything unexpected refuses the
//! whole import.

mod anki;
mod archive;
mod engine;
mod export;
mod format;
mod import;
mod media;
mod notes;
mod source;
mod stream;
mod validate;

use std::fmt;
use std::path::PathBuf;

pub use anki::{
    ANKI_EXTENSIONS, AnkiFormat, AnkiPlan, AnkiProblem, AnkiReport, AnkiSummary, FileKind, Skip,
    Skipped, file_kind,
};
pub(crate) use format::FORMAT_VERSION;

/// File name extension of an export, without the dot.
pub const FILE_EXTENSION: &str = "rappel";

/// File name extensions an import offers, without the dot. `cards` is what
/// exports were called before the rename to Rappel (ADR 0020). Those files
/// have `format_version` 1, which only early test versions wrote, so they
/// are refused by their version ([`ImportProblem::EarlyTestVersion`]); they
/// stay in the file chooser so the user gets that message instead of not
/// seeing the file. Import reads the content, never the extension.
pub const IMPORT_EXTENSIONS: [&str; 2] = [FILE_EXTENSION, "cards"];

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportSummary {
    /// Decks written, deleted ones included.
    pub decks: usize,
    /// Cards written, deleted ones included.
    pub cards: usize,
    /// Tags written.
    pub tags: usize,
    /// Log entries written (reviews, resets and undos).
    pub review_events: usize,
    /// Media files written.
    pub media: usize,
    /// Media files the database lists but that are missing from the media
    /// folder or damaged (not the size, kind or hash recorded): left out of
    /// the export, with their rows, so the file stays importable. Cards
    /// that show them show them as missing, as before the export.
    pub missing_media: usize,
}

/// How the rows of one table in a file compare with the database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TableCounts {
    /// Rows whose id is not in the database: they would be added.
    pub new: usize,
    /// Rows already in the database with the same content: identical, or
    /// deleted in the file but live in the database (an import never
    /// deletes). A media file is present when the database has its hash
    /// with the same type and size, whenever it was added.
    pub present: usize,
    /// Deleted rows of the database that are live in the file with
    /// otherwise the same content: they would be brought back. For cards,
    /// this includes the cards deleted with a revived deck that the file
    /// does not have. Always 0 for tags, card tags and review events.
    pub revived: usize,
}

/// What an import does with the file's settings (ADR 0027).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsImport {
    /// The file has the same stored settings as the database.
    #[default]
    Unchanged,
    /// The database has no decks yet (as on a new machine), so the file's
    /// settings replace its own: keys the file does not have go back to
    /// their defaults.
    Applied,
    /// The database already has decks, so it keeps its own settings: a
    /// merge never changes the retention, which would reschedule every
    /// card.
    Kept,
}

/// One reason an import cannot be applied. Ids are the file's own, already
/// checked to be `UUIDv7`; no card text is included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// The database has a deck with this id but different content (apart
    /// from one side being deleted and the other live).
    DeckDiffers(String),
    /// The database has a card with this id but different content (apart
    /// from one side being deleted and the other live).
    CardDiffers(String),
    /// The database has a review event with this id but different content.
    EventDiffers(String),
    /// The database has a tag with this id but another name. (A tag with
    /// another id but the same name is not a conflict: it is mapped onto
    /// the database's tag, ADR 0029.)
    TagDiffers(String),
    /// The database has a media file with this hash (64 hex digits) but
    /// another type or size, which a damaged database could have.
    MediaDiffers(String),
    /// An undo in the file cancels an entry that another undo in the
    /// database already cancels (ADR 0028).
    AlreadyUndone {
        /// The undo in the file.
        event_id: String,
        /// The database's undo of the same entry.
        existing_id: String,
    },
    /// A live deck in the file has the name (by
    /// [`deck_name_key`](crate::domain::deck_name_key)) of another live
    /// deck in the database.
    DeckNameTaken {
        /// The deck in the file.
        deck_id: String,
        /// The database's deck with that name.
        existing_id: String,
    },
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeckDiffers(id) => write!(f, "deck {id} differs from the one in the database"),
            Self::CardDiffers(id) => write!(f, "card {id} differs from the one in the database"),
            Self::EventDiffers(id) => {
                write!(f, "review {id} differs from the one in the database")
            }
            Self::DeckNameTaken {
                deck_id,
                existing_id,
            } => write!(
                f,
                "deck {deck_id} has the same name as deck {existing_id} in the database"
            ),
            Self::TagDiffers(id) => write!(f, "tag {id} differs from the one in the database"),
            Self::MediaDiffers(hash) => {
                write!(f, "media file {hash} differs from the one in the database")
            }
            Self::AlreadyUndone {
                event_id,
                existing_id,
            } => write!(
                f,
                "undo {event_id} cancels an entry that undo {existing_id} in the database already cancels"
            ),
        }
    }
}

/// What an import would do, from [`ImportPlan::summary`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Decks, deleted ones included.
    pub decks: TableCounts,
    /// Cards, deleted ones included.
    pub cards: TableCounts,
    /// Tags. A tag of the file with the name of a database tag (another
    /// id) is mapped onto it and counted as present.
    pub tags: TableCounts,
    /// Card tags (a card having a tag).
    pub card_tags: TableCounts,
    /// Log entries (reviews, resets and undos).
    pub review_events: TableCounts,
    /// Media files.
    pub media: TableCounts,
    /// What happens to the file's settings.
    pub settings: SettingsImport,
    /// Every conflict, in file order (decks, cards, tags, events, media,
    /// then deck names). Any conflict refuses the whole import.
    pub conflicts: Vec<Conflict>,
}

impl ImportSummary {
    /// Whether applying would change nothing (every row is already
    /// present: nothing to add, nothing to bring back).
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.decks.new == 0
            && self.cards.new == 0
            && self.tags.new == 0
            && self.card_tags.new == 0
            && self.review_events.new == 0
            && self.media.new == 0
            && self.decks.revived == 0
            && self.cards.revived == 0
            && self.settings != SettingsImport::Applied
    }
}

/// A checked file and how it compares with the database, from
/// [`Store::plan_import`](crate::storage::Store::plan_import). Pass it to
/// [`Store::apply_import`](crate::storage::Store::apply_import) to write.
///
/// It holds no content of the file, only where it is and what identifies
/// it, so it stays small whatever the file holds (ADR 0053).
#[derive(Clone)]
pub struct ImportPlan {
    /// The file, read again by the apply.
    source: source::Source,
    /// The limits it was checked with.
    limits: Limits,
    /// Its size, time and identity when it was opened (none for bytes in
    /// memory).
    stamp: Option<source::Stamp>,
    /// How it was read.
    mode: Mode,
    /// The SHA-256 of its `cards.json`.
    sha256: [u8; 32],
    /// The digest of every decision, in order: exactly which rows the
    /// apply writes.
    digest: [u8; 32],
    summary: ImportSummary,
}

/// How a file was read: in one pass (a file as an export writes it), or
/// one step at a time (its keys in another order). The apply reads it the
/// same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    OnePass,
    StepByStep,
}

/// Only the summary: the path is the user's, and nothing of the file is
/// held (SPEC §2).
impl fmt::Debug for ImportPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportPlan")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

impl ImportPlan {
    /// What the import would add, what it would bring back, what is
    /// already there, and the conflicts.
    #[must_use]
    pub fn summary(&self) -> &ImportSummary {
        &self.summary
    }
}

/// What [`Store::apply_import`](crate::storage::Store::apply_import) did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Decks added.
    pub decks: usize,
    /// Cards added.
    pub cards: usize,
    /// Tags added.
    pub tags: usize,
    /// Card tags added.
    pub card_tags: usize,
    /// Log entries added.
    pub review_events: usize,
    /// Media files added.
    pub media: usize,
    /// Whether the file's settings replaced the database's.
    pub settings_applied: bool,
    /// Deleted decks brought back.
    pub revived_decks: usize,
    /// Deleted cards brought back.
    pub revived_cards: usize,
    /// The backup made just before writing; `None` if nothing was added or
    /// brought back (then nothing was written and no backup was needed).
    pub backup: Option<PathBuf>,
}

/// Why a file was refused by
/// [`Store::plan_import`](crate::storage::Store::plan_import). The
/// messages never quote the file's text, only places in it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportProblem {
    /// The file is larger than an import may be.
    #[error("the file is larger than {limit} bytes")]
    FileTooLarge {
        /// The limit, in bytes.
        limit: u64,
    },
    /// The path is not a regular file (a directory, a pipe, a device).
    #[error("it is not a regular file (a folder, a pipe or a device)")]
    NotAFile,
    /// The file is not a zip archive, or a damaged one (`0` says what the
    /// zip reader found).
    #[error("not a valid Rappel export: {0}")]
    InvalidArchive(String),
    /// The archive's end record declares more entries than an import
    /// reads (refused before the list of entries is read).
    #[error("the archive has more than {limit} entries")]
    TooManyEntries {
        /// The limit.
        limit: u64,
    },
    /// The archive has an entry other than `cards.json`, `media/` and
    /// `media/<sha256>.<ext>` (64 lowercase hex digits, a known
    /// extension), one of them twice, or a media file the list of media
    /// does not have. The name is shown escaped and shortened.
    #[error("unexpected entry {0} in the archive")]
    UnexpectedEntry(String),
    /// A required entry is missing.
    #[error("the archive has no {0} entry")]
    MissingEntry(&'static str),
    /// An entry is encrypted, compressed with a method other than Stored
    /// or Deflate, or of the wrong kind (a file where a directory belongs).
    #[error("archive entry {0} is not stored in a supported way")]
    UnsupportedEntry(&'static str),
    /// `cards.json` or a media file is larger than allowed once
    /// decompressed.
    #[error("{entry} is larger than {limit} bytes")]
    EntryTooLarge {
        /// The entry.
        entry: &'static str,
        /// The limit, in bytes.
        limit: u64,
    },
    /// `cards.json` is not valid JSON, does not have the expected shape,
    /// or has a string longer than any field can be (`0` says which, with
    /// line and column).
    #[error("cards.json is malformed: {0}")]
    Json(String),
    /// The file was made by an early test version of Rappel (format
    /// version 1, 0.2.0-alpha), whose files are not imported (ADR 0017).
    #[error("the file was made by an early test version of Rappel and cannot be imported")]
    EarlyTestVersion,
    /// The file was written by a newer version of Rappel, or has a version
    /// that never existed.
    #[error(
        "format version {found} is not supported (this version of Rappel reads up to {FORMAT_VERSION})"
    )]
    UnsupportedFormatVersion {
        /// The file's `format_version`.
        found: i64,
    },
    /// A field of one row is invalid.
    #[error("{table}[{index}].{field}: {reason}")]
    InvalidField {
        /// `decks`, `cards`, `tags`, `card_tags`, `review_events` or
        /// `media`.
        table: &'static str,
        /// The row's position in its list, from 0.
        index: usize,
        /// The field.
        field: &'static str,
        /// What is wrong with it.
        reason: FieldProblem,
    },
    /// A top-level field is invalid (`exported_at`, or a setting such as
    /// `settings.desired_retention`).
    #[error("{field}: {reason}")]
    InvalidTopLevel {
        /// The field.
        field: &'static str,
        /// What is wrong with it.
        reason: FieldProblem,
    },
}

/// What is wrong with one field, for [`ImportProblem::InvalidField`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FieldProblem {
    /// Not a lowercase hyphenated `UUIDv7` with the RFC variant (#12).
    #[error("not a valid id")]
    NotUuidV7,
    /// Another row of the same list has this id.
    #[error("the same id appears twice")]
    DuplicateId,
    /// No row of the referenced list has this id.
    #[error("refers to an id that is not in the file")]
    UnknownReference,
    /// Not `fail` or `pass`.
    #[error("not a grade")]
    InvalidGrade,
    /// A duration below zero.
    #[error("negative")]
    Negative,
    /// A timestamp before 1970 or after the year 9999.
    #[error("timestamp out of range")]
    TimestampOutOfRange,
    /// Empty, or only whitespace.
    #[error("empty")]
    Empty,
    /// Leading or trailing whitespace (deck names are stored trimmed).
    #[error("has leading or trailing whitespace")]
    NotTrimmed,
    /// Longer than allowed.
    #[error("too long")]
    TooLong,
    /// Has a NUL character (U+0000), which GTK cannot take.
    #[error("contains a NUL character")]
    ContainsNul,
    /// A live card whose deck is deleted, which the app never produces.
    #[error("a live card in a deleted deck")]
    LiveCardInDeletedDeck,
    /// Another live deck in the file has the same name key.
    #[error("another live deck in the file has the same name")]
    DuplicateDeckName,
    /// Another tag in the file has the same name key.
    #[error("another tag in the file has the same name")]
    DuplicateTagName,
    /// Has a comma, which separates tags in a list.
    #[error("contains a comma")]
    ContainsComma,
    /// Another row of the same list is the same.
    #[error("the same row appears twice")]
    DuplicateRow,
    /// Before the row above it: `card_tags` are sorted by card id, then
    /// tag id, as an export writes them.
    #[error("out of order")]
    OutOfOrder,
    /// Not `review`, `reset` or `undo`.
    #[error("not a kind of log entry")]
    InvalidKind,
    /// `null`, but this kind of entry needs it.
    #[error("missing for this kind of entry")]
    Missing,
    /// Set, but this kind of entry cannot have it.
    #[error("not allowed for this kind of entry")]
    NotAllowed,
    /// Does not name an earlier review or reset of the same card in the
    /// file.
    #[error("does not cancel an earlier review or reset of the same card")]
    InvalidUndo,
    /// Another undo in the file cancels the same entry.
    #[error("another undo in the file cancels the same entry")]
    AlreadyUndone,
    /// Outside the setting's range.
    #[error("out of range")]
    OutOfRange,
    /// Not a SHA-256 as 64 lowercase hex digits.
    #[error("not a SHA-256 in lowercase hex")]
    NotHash,
    /// Not the MIME type of a kind of media Rappel keeps.
    #[error("not a supported media type")]
    UnknownMediaType,
    /// Larger than the media file's kind allows.
    #[error("larger than allowed")]
    TooLarge,
    /// The archive's `media/` folder has no file for this record.
    #[error("has no file in the archive")]
    NoMediaFile,
    /// Not the size of the record's file in the archive.
    #[error("is not the size of its file")]
    SizeDiffers,
    /// The record's file in the archive has other content (another
    /// SHA-256).
    #[error("its file has other content")]
    MediaFileDiffers,
    /// The record's file in the archive is not of this type (its first
    /// bytes say otherwise).
    #[error("its file is not of this type")]
    NotOfItsType,
}

/// Size limits for reading a file (ADR 0053). The defaults are generous;
/// tests lower them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The whole `.rappel` file. Never read whole: media make it large.
    pub archive: u64,
    /// Entries in the archive, from its end record.
    pub entries: u64,
    /// The archive's central directory (the list of entries), from its end
    /// record: walked once, record by record, never kept.
    pub central_directory: u64,
    /// `cards.json`, decompressed. Bounds the notes an import keeps.
    pub json: u64,
    /// One image in `media/`, decompressed (SPEC §4.3).
    pub image: u64,
    /// One audio file in `media/`, decompressed (SPEC §4.9).
    pub audio: u64,
    /// One card side.
    pub card_text: usize,
    /// One deck name.
    pub deck_name: usize,
    /// One tag name.
    pub tag_name: usize,
}

impl Limits {
    /// The limits used by [`Store::plan_import`](crate::storage::Store::plan_import).
    pub(crate) const DEFAULT: Self = Self {
        archive: 2 << 30,
        entries: 100_000,
        // 100,000 central records with media names (about 122 bytes each)
        // fit with room to spare.
        central_directory: 16 << 20,
        json: 256 << 20,
        image: crate::media::IMAGE_MAX_BYTES,
        audio: crate::media::AUDIO_MAX_BYTES,
        card_text: crate::domain::MAX_CARD_SIDE_BYTES,
        deck_name: crate::domain::MAX_DECK_NAME_BYTES,
        tag_name: crate::domain::MAX_TAG_NAME_BYTES,
    };

    /// The largest file of `kind` an import reads.
    pub(crate) fn media(&self, kind: crate::media::MediaKind) -> u64 {
        if kind.is_image() {
            self.image
        } else {
            self.audio
        }
    }
}

#[cfg(test)]
mod tests;
