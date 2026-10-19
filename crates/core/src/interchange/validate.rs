//! The checks on `cards.json` that need neither the database nor other
//! rows: each record's own fields, the top level and the settings. The
//! checks across rows (uniqueness, references, undo targets) are in
//! [`super::engine`], over compact notes (ADR 0053). The file is
//! untrusted, so everything is checked before anything is compared or
//! written.
//!
//! Each function runs its checks in the order they have always run, so the
//! first broken rule of a row is the one reported.

use super::format::{
    CardRecord, DeckRecord, EARLY_TEST_VERSION, EventRecord, FORMAT_VERSION, MediaRecord,
    SettingsRecord, TagRecord,
};
use super::{FieldProblem, ImportProblem, Limits};
use crate::domain::{Grade, Settings, has_nul};
use crate::id::uuid_v7_value;
use crate::media::{MediaKind, MediaRef, parse_hex};

/// 9999-12-31T23:59:59.999Z, the latest timestamp a file may hold.
pub(super) const MAX_TIMESTAMP: i64 = 253_402_300_799_999;

/// What is wrong with a row: the field and the problem. The caller knows
/// the table and the row.
pub(super) type FieldError = (&'static str, FieldProblem);

/// A file of a version this build does not read.
pub(super) fn version_problem(found: i64) -> Option<ImportProblem> {
    if found == EARLY_TEST_VERSION {
        return Some(ImportProblem::EarlyTestVersion);
    }
    (found != i64::from(FORMAT_VERSION))
        .then_some(ImportProblem::UnsupportedFormatVersion { found })
}

pub(super) fn invalid(
    table: &'static str,
    index: usize,
    field: &'static str,
    reason: FieldProblem,
) -> ImportProblem {
    ImportProblem::InvalidField {
        table,
        index,
        field,
        reason,
    }
}

/// A row's own id, as its 122-bit value.
///
/// The id's own timestamp must be in range too: `IdGen` issues every new
/// id above the largest stored one, so a stored id near the top of the
/// `UUIDv7` range (year 10889) would leave it no room and make every later
/// write fail with `IdsExhausted`, which nothing could repair because rows
/// are never removed. Up to the year 9999 leaves centuries of room.
///
/// A reference that is not a valid id cannot resolve either, so only row
/// ids are checked for their form; references are checked by lookup.
pub(super) fn check_id(id: &str) -> Result<u128, FieldProblem> {
    match uuid_v7_value(id) {
        None => Err(FieldProblem::NotUuidV7),
        Some((_, ms)) if !timestamp_ok(ms) => Err(FieldProblem::TimestampOutOfRange),
        Some((value, _)) => Ok(value),
    }
}

/// The value a reference names, or `None` if no row can have it.
pub(super) fn reference(id: &str) -> Option<u128> {
    uuid_v7_value(id).map(|(value, _)| value)
}

pub(super) fn timestamp_ok(ms: i64) -> bool {
    (0..=MAX_TIMESTAMP).contains(&ms)
}

/// The first of the three timestamps that is out of range, by field name.
fn check_times(created: i64, updated: i64, deleted: Option<i64>) -> Result<(), &'static str> {
    if !timestamp_ok(created) {
        return Err("created_at");
    }
    if !timestamp_ok(updated) {
        return Err("updated_at");
    }
    if deleted.is_some_and(|d| !timestamp_ok(d)) {
        return Err("deleted_at");
    }
    Ok(())
}

fn times(created: i64, updated: i64, deleted: Option<i64>) -> Result<(), FieldError> {
    check_times(created, updated, deleted).map_err(|f| (f, FieldProblem::TimestampOutOfRange))
}

/// A deck's name and times (after its id, before its name's uniqueness).
pub(super) fn deck_fields(d: &DeckRecord, limits: &Limits) -> Result<(), FieldError> {
    if d.name.trim().is_empty() {
        return Err(("name", FieldProblem::Empty));
    }
    if d.name.trim() != d.name {
        return Err(("name", FieldProblem::NotTrimmed));
    }
    if d.name.len() > limits.deck_name {
        return Err(("name", FieldProblem::TooLong));
    }
    if has_nul(&d.name) {
        return Err(("name", FieldProblem::ContainsNul));
    }
    times(d.created_at, d.updated_at, d.deleted_at)
}

/// A card's sides and times (after its id and deck).
pub(super) fn card_fields(c: &CardRecord, limits: &Limits) -> Result<(), FieldError> {
    if c.front_md.trim().is_empty() {
        return Err(("front_md", FieldProblem::Empty));
    }
    if c.front_md.len() > limits.card_text {
        return Err(("front_md", FieldProblem::TooLong));
    }
    if c.back_md.len() > limits.card_text {
        return Err(("back_md", FieldProblem::TooLong));
    }
    if has_nul(&c.front_md) {
        return Err(("front_md", FieldProblem::ContainsNul));
    }
    if has_nul(&c.back_md) {
        return Err(("back_md", FieldProblem::ContainsNul));
    }
    times(c.created_at, c.updated_at, c.deleted_at)
}

/// A tag's name: the rules of `check_tag_name`, each with its own
/// problem (after its id, before its name's uniqueness).
pub(super) fn tag_fields(t: &TagRecord, limits: &Limits) -> Result<(), FieldError> {
    if t.name.trim().is_empty() {
        return Err(("name", FieldProblem::Empty));
    }
    if t.name.trim() != t.name {
        return Err(("name", FieldProblem::NotTrimmed));
    }
    if t.name.len() > limits.tag_name {
        return Err(("name", FieldProblem::TooLong));
    }
    if has_nul(&t.name) {
        return Err(("name", FieldProblem::ContainsNul));
    }
    if t.name.contains(',') {
        return Err(("name", FieldProblem::ContainsComma));
    }
    Ok(())
}

/// The kind of a log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Review,
    Reset,
    Undo,
}

/// A log entry's kind and the fields each kind allows (after its id and
/// card, before whether another undo cancels the same entry).
pub(super) fn event_fields(e: &EventRecord) -> Result<Kind, FieldError> {
    let kind = match e.kind.as_str() {
        "review" => Kind::Review,
        "reset" => Kind::Reset,
        "undo" => Kind::Undo,
        _ => return Err(("kind", FieldProblem::InvalidKind)),
    };
    let graded = kind == Kind::Review;
    if !timestamp_ok(e.reviewed_at) {
        return Err(("reviewed_at", FieldProblem::TimestampOutOfRange));
    }
    match (&e.grade, graded) {
        (Some(g), true) if g.parse::<Grade>().is_err() => {
            return Err(("grade", FieldProblem::InvalidGrade));
        }
        (None, true) => return Err(("grade", FieldProblem::Missing)),
        (Some(_), false) => return Err(("grade", FieldProblem::NotAllowed)),
        _ => {}
    }
    if e.duration_ms < 0 {
        return Err(("duration_ms", FieldProblem::Negative));
    }
    if !graded && e.duration_ms != 0 {
        return Err(("duration_ms", FieldProblem::NotAllowed));
    }
    match (&e.undoes, kind == Kind::Undo) {
        (None, true) => Err(("undoes", FieldProblem::Missing)),
        (Some(_), false) => Err(("undoes", FieldProblem::NotAllowed)),
        _ => Ok(kind),
    }
}

/// `exported_at`, checked before any list (then the media records, which
/// [`super::media::MediaList`] checks as they are read).
pub(super) fn check_top_level(exported_at: i64) -> Result<(), ImportProblem> {
    if !timestamp_ok(exported_at) {
        return Err(ImportProblem::InvalidTopLevel {
            field: "exported_at",
            reason: FieldProblem::TimestampOutOfRange,
        });
    }
    Ok(())
}

/// A media record's own fields, in this order: the hash (64 lowercase hex
/// digits, and after `previous`, the record before: the list is sorted by
/// hash, each once), the type (one Rappel keeps), the size (above zero,
/// within the kind's limit) and `created_at`.
pub(super) fn media_fields(
    m: &MediaRecord,
    previous: Option<&[u8; 32]>,
    limits: &Limits,
) -> Result<(MediaRef, u64), FieldError> {
    let hash = parse_hex(m.hash.as_bytes()).ok_or(("hash", FieldProblem::NotHash))?;
    match previous {
        Some(p) if *p == hash => return Err(("hash", FieldProblem::DuplicateId)),
        Some(p) if *p > hash => return Err(("hash", FieldProblem::OutOfOrder)),
        _ => {}
    }
    let kind = MediaKind::from_mime(&m.mime).ok_or(("mime", FieldProblem::UnknownMediaType))?;
    let bytes = match u64::try_from(m.bytes) {
        Err(_) => return Err(("bytes", FieldProblem::Negative)),
        Ok(0) => return Err(("bytes", FieldProblem::Empty)),
        Ok(n) if n > limits.media(kind) => return Err(("bytes", FieldProblem::TooLarge)),
        Ok(n) => n,
    };
    if !timestamp_ok(m.created_at) {
        return Err(("created_at", FieldProblem::TimestampOutOfRange));
    }
    Ok((MediaRef::new(hash, kind), bytes))
}

/// Each setting present is within its range; checked last.
pub(super) fn check_settings(s: &SettingsRecord) -> Result<(), ImportProblem> {
    let out_of_range = |field| ImportProblem::InvalidTopLevel {
        field,
        reason: FieldProblem::OutOfRange,
    };
    if s.desired_retention
        .is_some_and(|v| Settings::check_retention(v).is_err())
    {
        return Err(out_of_range("settings.desired_retention"));
    }
    if s.max_new_per_day
        .is_some_and(|v| Settings::check_max_new(v).is_err())
    {
        return Err(out_of_range("settings.max_new_per_day"));
    }
    if s.max_reviews_per_day
        .is_some_and(|v| Settings::check_max_reviews(v).is_err())
    {
        return Err(out_of_range("settings.max_reviews_per_day"));
    }
    if s.day_start_hour
        .is_some_and(|v| Settings::check_day_start_hour(v).is_err())
    {
        return Err(out_of_range("settings.day_start_hour"));
    }
    Ok(())
}
