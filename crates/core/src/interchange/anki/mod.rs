//! Import of Anki packages, `.apkg` and `.colpkg`, legacy and current
//! (SPEC §4.11, ADR 0061). One way: Anki's cards become plain Rappel
//! cards, with their tags, media and review history; note types,
//! templates and settings are not kept.
//!
//! As for a `.rappel` file, the import is two steps.
//! [`Store::plan_anki_import`](crate::storage::Store::plan_anki_import)
//! reads the package and converts every card without writing anything,
//! and counts what would be added and what is left out, with reasons;
//! [`Store::apply_anki_import`](crate::storage::Store::apply_anki_import)
//! backs the database up, converts the package again and writes it in one
//! transaction, media files first. Both can be cancelled. The package is
//! untrusted: its archive, its SQLite collection, its HTML and its media
//! are read with limits, and nothing in it is ever run, rendered as HTML
//! or fetched.

mod cloze;
mod collection;
mod convert;
mod entities;
mod html;
mod import;
mod markdown;
mod package;
mod proto;
mod template;

#[cfg(test)]
mod tests;

use std::fmt;
use std::path::PathBuf;

use crate::interchange::archive::AnkiName;

/// File name extensions of Anki packages, without the dot: a deck package
/// and a collection package (Anki's whole collection, the same format).
pub const ANKI_EXTENSIONS: [&str; 2] = ["apkg", "colpkg"];

/// How many examples a reason keeps.
pub const MAX_EXAMPLES: usize = 3;

/// The Anki format of a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnkiFormat {
    /// Anki 2.0: `collection.anki2`, schema 11.
    Anki20,
    /// Anki 2.1 before 2.1.50, or a "legacy" export: `collection.anki21`,
    /// schema 11.
    Anki21,
    /// Anki 2.1.50 and later: `collection.anki21b` and media compressed
    /// with zstd, schema 18.
    Current,
}

impl AnkiFormat {
    pub(crate) fn collection(self) -> AnkiName {
        match self {
            Self::Anki20 => AnkiName::Anki2,
            Self::Anki21 => AnkiName::Anki21,
            Self::Current => AnkiName::Anki21b,
        }
    }
}

/// Why something in a package is left out, or kept differently. The order
/// is the order of the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Skip {
    /// Cards: a card identical to a live card of Rappel (same deck, front
    /// and back), such as one imported before. Not added again.
    AlreadyInRappel,
    /// Cards: the front is empty once converted (Anki would not show the
    /// card either). Not added.
    EmptyFront,
    /// Cards: the note is missing, or its note type is, or does not match
    /// it (its number of fields, its templates). Not added.
    BrokenNote,
    /// Cards: the note type's template cannot be read (unbalanced
    /// sections, too deep, too large). Not added.
    BadTemplate,
    /// Cards: a side longer than Rappel allows (1 MiB). Not added.
    TooLong,
    /// Cards, kept: a template filter Rappel does not know; the field is
    /// shown as it is. Examples are the filters.
    UnknownFilter,
    /// Cards, kept: a template names a field the note type does not have;
    /// it shows nothing. Examples are the fields.
    UnknownField,
    /// Cards, kept: math Rappel cannot draw, kept as its source text.
    MathAsText,
    /// Cards, kept: an image that is not in the package (a web address or
    /// an embedded image), shown as text and never fetched. Examples are
    /// the addresses.
    RemoteImage,
    /// Media: a file cards name that the package does not have; the name
    /// is shown instead. Examples are the names.
    MissingMedia,
    /// Media: a file of a kind Rappel does not keep (SVG, video, PDF…) or
    /// used as another kind (an image as sound), never rendered; the name
    /// is shown instead.
    UnsupportedMedia,
    /// Media: a file larger than Rappel allows (10 MB for an image, 20 MB
    /// for audio); the name is shown instead.
    MediaTooLarge,
    /// Media: a damaged file (or an audio file with more than plain
    /// audio); the name is shown instead.
    DamagedMedia,
    /// Media: a name that cannot be a file name (with `/`, `..`, a NUL,
    /// too long) or used twice. Never used.
    BadMediaName,
    /// Media: a file no imported card uses. Not imported.
    UnusedMedia,
    /// Tags: a tag Rappel cannot have (with a comma, too long). Examples
    /// are the tags.
    InvalidTag,
    /// Tags: a note with more than 100 tags; the rest are left out.
    TooManyTags,
    /// Decks: a deck name longer than Rappel allows, shortened.
    DeckNameShortened,
    /// History: a manual entry (a reschedule, "Set Due Date", "Forget"),
    /// which is not a review. A forgotten card becomes new again by a
    /// reset instead.
    ManualEntry,
    /// History: a review in a filtered deck that did not reschedule the
    /// card (cramming, previews).
    FilteredReview,
    /// History: an entry with an unknown button or kind, or a time out of
    /// range.
    BadReview,
}

/// One reason with how many it concerns and a few examples (escaped
/// nowhere: they are the package's text, to be shown as plain text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub reason: Skip,
    /// Cards, files, tags, entries or decks, as the reason says.
    pub count: usize,
    /// At most [`MAX_EXAMPLES`], each at most 80 characters.
    pub examples: Vec<String>,
}

/// What an Anki import would do, from [`AnkiPlan::summary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnkiSummary {
    pub format: AnkiFormat,
    /// Decks that would be created.
    pub new_decks: usize,
    /// Decks of Rappel the cards would go into (same name).
    pub existing_decks: usize,
    /// Cards that would be added.
    pub cards: usize,
    /// Of those, the suspended ones.
    pub suspended: usize,
    /// Reviews that would be added to their log.
    pub reviews: usize,
    /// Media files that would be added to the media folder.
    pub new_media: usize,
    /// Media files the cards use that Rappel already has.
    pub present_media: usize,
    /// Different tags on the cards.
    pub tags: usize,
    /// What is left out or kept differently, in [`Skip`] order, only the
    /// reasons that occur.
    pub skipped: Vec<Skipped>,
}

impl AnkiSummary {
    /// Whether applying would add nothing.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.cards == 0
    }
}

/// A checked package and what importing it would do, from
/// [`Store::plan_anki_import`](crate::storage::Store::plan_anki_import).
/// Holds no content of the package.
#[derive(Clone)]
pub struct AnkiPlan {
    source: crate::interchange::source::Source,
    stamp: Option<crate::interchange::source::Stamp>,
    limits: package::AnkiLimits,
    /// The SHA-256 of the collection.
    collection: [u8; 32],
    /// The digest of every row the apply writes.
    digest: [u8; 32],
    summary: AnkiSummary,
}

impl fmt::Debug for AnkiPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnkiPlan")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

impl AnkiPlan {
    #[must_use]
    pub fn summary(&self) -> &AnkiSummary {
        &self.summary
    }
}

/// What [`Store::apply_anki_import`](crate::storage::Store::apply_anki_import)
/// did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnkiReport {
    pub new_decks: usize,
    pub cards: usize,
    pub suspended: usize,
    pub reviews: usize,
    pub new_media: usize,
    /// What was left out or kept differently (the plan's).
    pub skipped: Vec<Skipped>,
    /// The backup made just before writing; `None` if nothing was added.
    pub backup: Option<PathBuf>,
}

/// Why a package was refused. The messages never quote the package's
/// text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnkiProblem {
    #[error("the file is larger than {limit} bytes")]
    FileTooLarge { limit: u64 },
    #[error("it is not a regular file (a folder, a pipe or a device)")]
    NotAFile,
    /// Not a zip archive, or a damaged one.
    #[error("not a valid Anki package: {0}")]
    InvalidArchive(String),
    #[error("the archive has more than {limit} entries")]
    TooManyEntries { limit: u64 },
    /// No collection in the package (not an Anki package).
    #[error("the archive has no Anki collection")]
    NoCollection,
    #[error("archive entry {0} is not stored in a supported way")]
    UnsupportedEntry(&'static str),
    #[error("{entry} is larger than {limit} bytes")]
    EntryTooLarge { entry: &'static str, limit: u64 },
    #[error("the package lists more than {limit} media files")]
    TooManyMediaFiles { limit: usize },
    /// `meta` names a package version this Rappel does not know.
    #[error("the package was made by a newer version of Anki (package version {0})")]
    NewerVersion(u64),
    /// The collection's schema version is not one Rappel reads (11, or
    /// 15 to 18).
    #[error("the collection's version {0} is not supported")]
    UnsupportedSchema(i64),
    /// Something in the package cannot be read (says what, not the text).
    #[error("the package is damaged: {0}")]
    Damaged(String),
}

/// Whether a file is a Rappel export or an Anki package, by its name's
/// extension (`.rappel` and `.cards`, or `.apkg` and `.colpkg`), else by
/// its content: a zip archive with `cards.json` or with an Anki
/// collection. `None` if neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Rappel,
    Anki,
}

/// The kind of the file at `path` ([`FileKind`]); `None` for anything
/// else, which the `.rappel` import then refuses with its own message.
#[must_use]
pub fn file_kind(path: &std::path::Path) -> Option<FileKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if let Some(ext) = ext.as_deref() {
        if ANKI_EXTENSIONS.contains(&ext) {
            return Some(FileKind::Anki);
        }
        if crate::interchange::IMPORT_EXTENSIONS.contains(&ext) {
            return Some(FileKind::Rappel);
        }
    }
    import::sniff(path)
}
