//! The `cards.json` document (format version 2), as serde types.
//!
//! Field order here is the order in the file, and every list is written
//! sorted by id (`card_tags` by card id, then tag id), so a database always
//! gives the same bytes (ADR 0017). Reading denies unknown fields: a file
//! that does not match exactly is refused rather than half understood.

use serde::{Deserialize, Serialize};

/// The format version this build writes, and the only one it reads.
/// Version 1 was written by early test versions only (0.2.0-alpha) and is
/// refused (ADR 0017). From 1.0 on, every released version must stay
/// importable.
pub const FORMAT_VERSION: u32 = 2;

/// The version written by early test versions of Rappel, refused with a
/// message of its own.
pub(super) const EARLY_TEST_VERSION: i64 = 1;

/// Only the version, read before the rest so that a file from another
/// version is refused as such, not for its unknown fields.
#[derive(Debug, Deserialize)]
pub(super) struct Header {
    pub format_version: i64,
}

/// The whole `cards.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    pub format_version: u32,
    /// When the export was made, UTC milliseconds. The only field that
    /// differs between two exports of the same database.
    pub exported_at: i64,
    pub decks: Vec<DeckRecord>,
    pub cards: Vec<CardRecord>,
    pub tags: Vec<TagRecord>,
    pub card_tags: Vec<CardTagRecord>,
    pub review_events: Vec<EventRecord>,
    /// The settings the user changed; a key that is not here has its
    /// default (ADR 0027).
    pub settings: SettingsRecord,
    /// One record per file in the archive's `media/` folder, sorted by
    /// hash (ADR 0054).
    pub media: Vec<MediaRecord>,
}

/// A `decks` row, deleted ones included. `name_key` is not exported: it
/// is derived from `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeckRecord {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Required, though it may be `null`: serde would otherwise read a
    /// missing field as `null`, which would make a deleted row live.
    #[serde(deserialize_with = "Option::deserialize")]
    pub deleted_at: Option<i64>,
}

/// A `cards` row, deleted ones included. `seq` is internal and not
/// exported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CardRecord {
    pub id: String,
    pub deck_id: String,
    pub front_md: String,
    pub back_md: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Excluded from study (ADR 0035). Always written; a file without it
    /// (made before suspension existed) reads as `false`.
    #[serde(default)]
    pub suspended: bool,
    /// Required, though it may be `null`: serde would otherwise read a
    /// missing field as `null`, which would make a deleted row live.
    #[serde(deserialize_with = "Option::deserialize")]
    pub deleted_at: Option<i64>,
}

/// A `tags` row. `name_key` is not exported: it is derived from `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TagRecord {
    pub id: String,
    pub name: String,
}

/// A `card_tags` row: card `card_id` has tag `tag_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CardTagRecord {
    pub card_id: String,
    pub tag_id: String,
}

/// A `review_events` row: a review, a reset or an undo (ADR 0028). Kind
/// and grade stay strings here and are checked by validation, so a bad
/// value is reported with its place in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EventRecord {
    pub id: String,
    pub card_id: String,
    /// `review`, `reset` or `undo`.
    pub kind: String,
    pub reviewed_at: i64,
    /// Required, though `null` for a reset or an undo.
    #[serde(deserialize_with = "Option::deserialize")]
    pub grade: Option<String>,
    pub duration_ms: i64,
    /// Required, though `null` for a review or a reset: the id of the
    /// entry an undo cancels.
    #[serde(deserialize_with = "Option::deserialize")]
    pub undoes: Option<String>,
}

/// The stored settings: only the keys the user changed are present, in
/// this order. Typed, so a value of the wrong type is a JSON error; ranges
/// are checked by validation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsRecord {
    /// Written as an `f32`, so 0.85 reads `0.85`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_retention: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_new_per_day: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reviews_per_day: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_start_hour: Option<u8>,
}

/// A `media` row: a file of the archive's `media/` folder, which is named
/// `media/<hash>.<ext>` with the extension of `mime` (ADR 0054). `bytes`
/// is signed so that a negative size is reported as a field, not as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MediaRecord {
    pub hash: String,
    pub mime: String,
    pub bytes: i64,
    pub created_at: i64,
}
