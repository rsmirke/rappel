//! The reader import had before it streamed (ADR 0053), kept as a test
//! oracle: the whole `cards.json` parsed into one document, checked table
//! by table, then compared with the database. The streaming reader must
//! give exactly its summary, or exactly its refusal, for any file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::super::format::{
    CardRecord, DeckRecord, Document, EARLY_TEST_VERSION, EventRecord, FORMAT_VERSION, Header,
    SettingsRecord,
};
use super::super::import::{Outcome, compare, stored_of};
use super::super::{Conflict, FieldProblem, ImportProblem, ImportSummary, Limits, SettingsImport};
use crate::domain::{Grade, Settings, deck_name_key, has_nul, name_key};
use crate::error::Result as DbResult;
use crate::id::uuid_v7_ms;
use crate::storage::{rebuild_cards, replace_settings, scheduler_in, stored_settings};

/// The old `apply_import` after the backup, in one transaction: classify,
/// revive, insert, apply the settings, rebuild the schedules. `Ok(false)`
/// if the file is refused or has conflicts (nothing written).
pub(super) fn apply(conn: &mut Connection, json: &[u8], limits: &Limits) -> DbResult<bool> {
    let Ok(doc) = parse(json, limits) else {
        return Ok(false);
    };
    let tx = conn.transaction()?;
    let rows = classify(&tx, &doc)?;
    if !rows.summary.conflicts.is_empty() {
        return Ok(false);
    }
    revive_rows(&tx, &doc, &rows)?;
    let reviewed = insert_rows(&tx, &doc, &rows)?;
    if rows.summary.settings == SettingsImport::Applied {
        replace_settings(&tx, &stored_of(&doc.settings))?;
    }
    rebuild_cards(&tx, &scheduler_in(&tx)?, reviewed)?;
    tx.commit()?;
    Ok(true)
}

/// The old `plan_import` after the archive: parse, check, compare.
pub(super) fn plan(
    conn: &Connection,
    json: &[u8],
    limits: &Limits,
) -> DbResult<Result<ImportSummary, ImportProblem>> {
    let document = match parse(json, limits) {
        Ok(document) => document,
        Err(problem) => return Ok(Err(problem)),
    };
    Ok(Ok(classify(conn, &document)?.summary))
}

/// 9999-12-31T23:59:59.999Z, the latest timestamp a file may hold.
const MAX_TIMESTAMP: i64 = 253_402_300_799_999;

/// Parses `json` into a checked [`Document`].
pub(super) fn parse(json: &[u8], limits: &Limits) -> Result<Document, ImportProblem> {
    let header: Header = serde_json::from_slice(json).map_err(|e| json_problem(&e))?;
    if header.format_version == EARLY_TEST_VERSION {
        return Err(ImportProblem::EarlyTestVersion);
    }
    if header.format_version != i64::from(FORMAT_VERSION) {
        return Err(ImportProblem::UnsupportedFormatVersion {
            found: header.format_version,
        });
    }
    let document: Document = serde_json::from_slice(json).map_err(|e| json_problem(&e))?;
    check(&document, limits)?;
    Ok(document)
}

/// Only the kind of error and its place: serde's message can quote any
/// amount of the file's text (a string where a number belongs).
fn json_problem(err: &serde_json::Error) -> ImportProblem {
    let kind = match err.classify() {
        serde_json::error::Category::Io => "cannot be read",
        serde_json::error::Category::Syntax => "invalid JSON",
        serde_json::error::Category::Data => "unexpected field, value or type",
        serde_json::error::Category::Eof => "ends too early",
    };
    ImportProblem::Json(format!(
        "{kind} at line {}, column {}",
        err.line(),
        err.column()
    ))
}

/// Every rule a file must follow, in file order: top level, decks, cards,
/// tags, card tags, review events, then settings. The first broken rule is
/// the one reported.
pub(super) fn check(doc: &Document, limits: &Limits) -> Result<(), ImportProblem> {
    check_top_level(doc, limits)?;
    let decks = check_decks(doc, limits)?;
    let cards = check_cards(doc, &decks, limits)?;
    let tags = check_tags(doc, limits)?;
    check_card_tags(doc, &cards, &tags)?;
    check_review_events(doc, &cards)?;
    check_settings(doc)
}

fn check_top_level(doc: &Document, limits: &Limits) -> Result<(), ImportProblem> {
    if !timestamp_ok(doc.exported_at) {
        return Err(ImportProblem::InvalidTopLevel {
            field: "exported_at",
            reason: FieldProblem::TimestampOutOfRange,
        });
    }
    // The media records on their own, then against the archive's media
    // entries, which the oracle's archives never have (ADR 0054).
    let mut previous = None;
    for (index, m) in doc.media.iter().enumerate() {
        let (media, _) = super::super::validate::media_fields(m, previous.as_ref(), limits)
            .map_err(|(field, reason)| invalid("media", index, field, reason))?;
        previous = Some(*media.hash());
    }
    if !doc.media.is_empty() {
        return Err(invalid("media", 0, "hash", FieldProblem::NoMediaFile));
    }
    Ok(())
}

/// The decks' rules. Returns each deck id with whether the deck is deleted.
fn check_decks<'d>(
    doc: &'d Document,
    limits: &Limits,
) -> Result<HashMap<&'d str, bool>, ImportProblem> {
    let mut decks: HashMap<&str, bool> = HashMap::with_capacity(doc.decks.len());
    let mut live_names: HashSet<String> = HashSet::new();
    for (index, d) in doc.decks.iter().enumerate() {
        let at = |field, reason| invalid("decks", index, field, reason);
        check_id(&d.id).map_err(|r| at("id", r))?;
        if decks.insert(&d.id, d.deleted_at.is_some()).is_some() {
            return Err(at("id", FieldProblem::DuplicateId));
        }
        if d.name.trim().is_empty() {
            return Err(at("name", FieldProblem::Empty));
        }
        if d.name.trim() != d.name {
            return Err(at("name", FieldProblem::NotTrimmed));
        }
        if d.name.len() > limits.deck_name {
            return Err(at("name", FieldProblem::TooLong));
        }
        if has_nul(&d.name) {
            return Err(at("name", FieldProblem::ContainsNul));
        }
        check_times(d.created_at, d.updated_at, d.deleted_at)
            .map_err(|f| at(f, FieldProblem::TimestampOutOfRange))?;
        if d.deleted_at.is_none() && !live_names.insert(deck_name_key(&d.name)) {
            return Err(at("name", FieldProblem::DuplicateDeckName));
        }
    }
    Ok(decks)
}

/// The cards' rules, given the decks from [`check_decks`]. Returns the card
/// ids.
fn check_cards<'d>(
    doc: &'d Document,
    decks: &HashMap<&str, bool>,
    limits: &Limits,
) -> Result<HashSet<&'d str>, ImportProblem> {
    let mut cards: HashSet<&str> = HashSet::with_capacity(doc.cards.len());
    for (index, c) in doc.cards.iter().enumerate() {
        let at = |field, reason| invalid("cards", index, field, reason);
        check_id(&c.id).map_err(|r| at("id", r))?;
        if !cards.insert(&c.id) {
            return Err(at("id", FieldProblem::DuplicateId));
        }
        let Some(&deck_deleted) = decks.get(c.deck_id.as_str()) else {
            return Err(at("deck_id", FieldProblem::UnknownReference));
        };
        if deck_deleted && c.deleted_at.is_none() {
            return Err(at("deleted_at", FieldProblem::LiveCardInDeletedDeck));
        }
        if c.front_md.trim().is_empty() {
            return Err(at("front_md", FieldProblem::Empty));
        }
        if c.front_md.len() > limits.card_text {
            return Err(at("front_md", FieldProblem::TooLong));
        }
        if c.back_md.len() > limits.card_text {
            return Err(at("back_md", FieldProblem::TooLong));
        }
        if has_nul(&c.front_md) {
            return Err(at("front_md", FieldProblem::ContainsNul));
        }
        if has_nul(&c.back_md) {
            return Err(at("back_md", FieldProblem::ContainsNul));
        }
        check_times(c.created_at, c.updated_at, c.deleted_at)
            .map_err(|f| at(f, FieldProblem::TimestampOutOfRange))?;
    }
    Ok(cards)
}

/// The tags' rules. Returns the tag ids.
fn check_tags<'d>(doc: &'d Document, limits: &Limits) -> Result<HashSet<&'d str>, ImportProblem> {
    let mut tags: HashSet<&str> = HashSet::with_capacity(doc.tags.len());
    let mut keys: HashSet<String> = HashSet::with_capacity(doc.tags.len());
    for (index, t) in doc.tags.iter().enumerate() {
        let at = |field, reason| invalid("tags", index, field, reason);
        check_id(&t.id).map_err(|r| at("id", r))?;
        if !tags.insert(&t.id) {
            return Err(at("id", FieldProblem::DuplicateId));
        }
        // The rules of `check_tag_name`, each with its own problem.
        if t.name.trim().is_empty() {
            return Err(at("name", FieldProblem::Empty));
        }
        if t.name.trim() != t.name {
            return Err(at("name", FieldProblem::NotTrimmed));
        }
        if t.name.len() > limits.tag_name {
            return Err(at("name", FieldProblem::TooLong));
        }
        if has_nul(&t.name) {
            return Err(at("name", FieldProblem::ContainsNul));
        }
        if t.name.contains(',') {
            return Err(at("name", FieldProblem::ContainsComma));
        }
        if !keys.insert(name_key(&t.name)) {
            return Err(at("name", FieldProblem::DuplicateTagName));
        }
    }
    Ok(tags)
}

/// The card tags' rules, given the card and tag ids. The rows must come
/// in the order an export writes them, by card id then tag id, so a
/// repeated row is the one just before: no set of pairs is held, which
/// keeps the smallest rows of the format within the memory budget
/// (ADR 0025, row 9).
fn check_card_tags(
    doc: &Document,
    cards: &HashSet<&str>,
    tags: &HashSet<&str>,
) -> Result<(), ImportProblem> {
    let mut previous: Option<(&str, &str)> = None;
    for (index, ct) in doc.card_tags.iter().enumerate() {
        let at = |field, reason| invalid("card_tags", index, field, reason);
        if !cards.contains(ct.card_id.as_str()) {
            return Err(at("card_id", FieldProblem::UnknownReference));
        }
        if !tags.contains(ct.tag_id.as_str()) {
            return Err(at("tag_id", FieldProblem::UnknownReference));
        }
        let pair = (ct.card_id.as_str(), ct.tag_id.as_str());
        match previous {
            Some(p) if p == pair => return Err(at("tag_id", FieldProblem::DuplicateRow)),
            Some(p) if p > pair => return Err(at("card_id", FieldProblem::OutOfOrder)),
            _ => {}
        }
        previous = Some(pair);
    }
    Ok(())
}

/// The review events' rules, given the card ids from [`check_cards`]:
/// each row on its own first, then each undo against the entry it
/// cancels (ADR 0028), which may come anywhere in the list. Only the
/// entries an undo names are kept in a map, so a log of reviews needs no
/// more memory than before undos existed (ADR 0025, row 9).
fn check_review_events(doc: &Document, cards: &HashSet<&str>) -> Result<(), ImportProblem> {
    let mut events: HashSet<&str> = HashSet::with_capacity(doc.review_events.len());
    // Each named entry, filled in below; a name used twice is refused.
    let mut targets: HashMap<&str, Option<&EventRecord>> = HashMap::new();
    for (index, e) in doc.review_events.iter().enumerate() {
        let at = |field, reason| invalid("review_events", index, field, reason);
        check_id(&e.id).map_err(|r| at("id", r))?;
        if !events.insert(&e.id) {
            return Err(at("id", FieldProblem::DuplicateId));
        }
        if !cards.contains(e.card_id.as_str()) {
            return Err(at("card_id", FieldProblem::UnknownReference));
        }
        let (grade, undoes) = match e.kind.as_str() {
            "review" => (true, false),
            "reset" => (false, false),
            "undo" => (false, true),
            _ => return Err(at("kind", FieldProblem::InvalidKind)),
        };
        if !timestamp_ok(e.reviewed_at) {
            return Err(at("reviewed_at", FieldProblem::TimestampOutOfRange));
        }
        match (&e.grade, grade) {
            (Some(g), true) if g.parse::<Grade>().is_err() => {
                return Err(at("grade", FieldProblem::InvalidGrade));
            }
            (None, true) => return Err(at("grade", FieldProblem::Missing)),
            (Some(_), false) => return Err(at("grade", FieldProblem::NotAllowed)),
            _ => {}
        }
        if e.duration_ms < 0 {
            return Err(at("duration_ms", FieldProblem::Negative));
        }
        if !grade && e.duration_ms != 0 {
            return Err(at("duration_ms", FieldProblem::NotAllowed));
        }
        match (&e.undoes, undoes) {
            (None, true) => return Err(at("undoes", FieldProblem::Missing)),
            (Some(_), false) => return Err(at("undoes", FieldProblem::NotAllowed)),
            (Some(target), true) if targets.insert(target, None).is_some() => {
                return Err(at("undoes", FieldProblem::AlreadyUndone));
            }
            _ => {}
        }
    }
    if targets.is_empty() {
        return Ok(());
    }
    for e in &doc.review_events {
        if let Some(slot) = targets.get_mut(e.id.as_str()) {
            *slot = Some(e);
        }
    }
    for (index, e) in doc.review_events.iter().enumerate() {
        let Some(target_id) = &e.undoes else { continue };
        // An earlier review or reset of the same card: dated no later and
        // with a smaller id, so it is replayed (and inserted) first, as
        // the schema requires.
        let cancels = targets
            .get(target_id.as_str())
            .copied()
            .flatten()
            .is_some_and(|t| {
                t.card_id == e.card_id
                    && t.kind != "undo"
                    && t.reviewed_at <= e.reviewed_at
                    && t.id < e.id
            });
        if !cancels {
            return Err(invalid(
                "review_events",
                index,
                "undoes",
                FieldProblem::InvalidUndo,
            ));
        }
    }
    Ok(())
}

/// Each setting present is within its range.
fn check_settings(doc: &Document) -> Result<(), ImportProblem> {
    let s = &doc.settings;
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

fn invalid(
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

/// A reference that is not a valid id cannot resolve either, so only row
/// ids are checked for their form; references are checked by lookup.
///
/// The id's own timestamp must be in range too: `IdGen` issues every new
/// id above the largest stored one, so a stored id near the top of the
/// `UUIDv7` range (year 10889) would leave it no room and make every later
/// write fail with `IdsExhausted`, which nothing could repair because rows
/// are never removed. Up to the year 9999 leaves centuries of room.
fn check_id(id: &str) -> Result<(), FieldProblem> {
    match uuid_v7_ms(id) {
        None => Err(FieldProblem::NotUuidV7),
        Some(ms) if !timestamp_ok(ms) => Err(FieldProblem::TimestampOutOfRange),
        Some(_) => Ok(()),
    }
}

fn timestamp_ok(ms: i64) -> bool {
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

/// How a document compares with the database: the summary, the positions
/// of the rows to add and to bring back, the ids of the cards that come
/// back with a revived deck but are not in the file, and the file's tags
/// that are mapped onto a database tag of the same name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Classified {
    pub(super) summary: ImportSummary,
    new_decks: Vec<usize>,
    new_cards: Vec<usize>,
    new_tags: Vec<usize>,
    new_card_tags: Vec<usize>,
    new_events: Vec<usize>,
    revived_decks: Vec<usize>,
    revived_cards: Vec<usize>,
    batch_cards: Vec<String>,
    /// File tag id → the database's tag id with the same name key.
    mapped_tags: BTreeMap<String, String>,
}

impl Classified {
    /// The database tag id a file tag id stands for.
    fn tag_id<'a>(&'a self, file_id: &'a str) -> &'a str {
        self.mapped_tags
            .get(file_id)
            .map_or(file_id, String::as_str)
    }
}

/// Unknown id: new; otherwise as [`compare`] says. A revived deck also
/// revives the database's cards of the same delete batch (same deck, same
/// `deleted_at`, as [`Store::undelete_deck`]) that the file does not have;
/// those the file has are decided by their own row. Then live deck names
/// against the database's other live decks, which covers revived decks.
/// Conflicts come in file order: decks, cards, tags, events, then deck
/// names. Last, the settings.
fn classify(conn: &Connection, doc: &Document) -> DbResult<Classified> {
    let mut rows = Classified::default();
    let batches = classify_decks(conn, doc, &mut rows)?;
    classify_cards(conn, doc, &mut rows)?;
    add_batch_cards(conn, doc, &batches, &mut rows)?;
    classify_tags(conn, doc, &mut rows)?;
    classify_card_tags(conn, doc, &mut rows)?;
    classify_events(conn, doc, &mut rows)?;
    check_deck_names(conn, doc, &mut rows.summary)?;
    rows.summary.settings = classify_settings(conn, &doc.settings)?;
    Ok(rows)
}

/// A tag with the same id is present if it has the same name, else a
/// conflict. A tag whose id is not here but whose name key a database tag
/// has is that tag (two machines that each created "maths"): it is mapped
/// onto the database's id, counted as present, and the file's card tags
/// that name it attach to the database's tag. The database keeps its
/// own spelling. Else the tag is new.
fn classify_tags(conn: &Connection, doc: &Document, rows: &mut Classified) -> DbResult<()> {
    let mut by_id = conn.prepare("SELECT name FROM tags WHERE id = ?1")?;
    let mut by_key = conn.prepare("SELECT id FROM tags WHERE name_key = ?1")?;
    for (i, t) in doc.tags.iter().enumerate() {
        let stored: Option<String> = by_id.query_row([&t.id], |r| r.get(0)).optional()?;
        match stored {
            Some(name) if name == t.name => rows.summary.tags.present += 1,
            Some(_) => rows
                .summary
                .conflicts
                .push(Conflict::TagDiffers(t.id.clone())),
            None => {
                let existing: Option<String> = by_key
                    .query_row([name_key(&t.name)], |r| r.get(0))
                    .optional()?;
                if let Some(existing_id) = existing {
                    rows.summary.tags.present += 1;
                    rows.mapped_tags.insert(t.id.clone(), existing_id);
                } else {
                    rows.summary.tags.new += 1;
                    rows.new_tags.push(i);
                }
            }
        }
    }
    Ok(())
}

/// A card tag is present or new, once its tag is mapped
/// ([`classify_tags`]); it has no content to differ.
fn classify_card_tags(conn: &Connection, doc: &Document, rows: &mut Classified) -> DbResult<()> {
    let mut stmt = conn.prepare("SELECT 1 FROM card_tags WHERE card_id = ?1 AND tag_id = ?2")?;
    for (i, ct) in doc.card_tags.iter().enumerate() {
        if stmt.exists(params![ct.card_id, rows.tag_id(&ct.tag_id)])? {
            rows.summary.card_tags.present += 1;
        } else {
            rows.summary.card_tags.new += 1;
            rows.new_card_tags.push(i);
        }
    }
    Ok(())
}

/// The file's settings go in only when the database has no decks at all,
/// deleted ones included (ADR 0027): moving to a new machine brings them,
/// a merge never changes them. Applied, they replace the stored ones, so
/// the database ends with the settings the file's database had, an
/// empty `settings` (all defaults) included.
fn classify_settings(conn: &Connection, file: &SettingsRecord) -> DbResult<SettingsImport> {
    if stored_of(file) == stored_settings(conn)? {
        return Ok(SettingsImport::Unchanged);
    }
    let has_decks: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM decks)", [], |r| r.get(0))?;
    Ok(if has_decks {
        SettingsImport::Kept
    } else {
        SettingsImport::Applied
    })
}

/// Classifies the decks. Returns each revived deck's id with its
/// `deleted_at` here, the stamp of its delete batch.
fn classify_decks<'d>(
    conn: &Connection,
    doc: &'d Document,
    rows: &mut Classified,
) -> DbResult<Vec<(&'d str, i64)>> {
    let mut batches = Vec::new();
    let mut stmt = conn
        .prepare("SELECT id, name, created_at, updated_at, deleted_at FROM decks WHERE id = ?1")?;
    for (i, d) in doc.decks.iter().enumerate() {
        let stored = stmt
            .query_row([&d.id], |r| {
                Ok(DeckRecord {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                    updated_at: r.get(3)?,
                    deleted_at: r.get(4)?,
                })
            })
            .optional()?;
        let Some(s) = stored else {
            rows.summary.decks.new += 1;
            rows.new_decks.push(i);
            continue;
        };
        let stored_deleted = s.deleted_at;
        let rest_equal = DeckRecord {
            deleted_at: d.deleted_at,
            ..s
        } == *d;
        match compare(rest_equal, stored_deleted, d.deleted_at) {
            Outcome::Present => rows.summary.decks.present += 1,
            Outcome::Revive(stamp) => {
                rows.summary.decks.revived += 1;
                rows.revived_decks.push(i);
                batches.push((d.id.as_str(), stamp));
            }
            Outcome::Conflict => rows
                .summary
                .conflicts
                .push(Conflict::DeckDiffers(d.id.clone())),
        }
    }
    Ok(batches)
}

fn classify_cards(conn: &Connection, doc: &Document, rows: &mut Classified) -> DbResult<()> {
    let mut stmt = conn.prepare(
        "SELECT id, deck_id, front_md, back_md, created_at, updated_at, suspended, deleted_at
         FROM cards WHERE id = ?1",
    )?;
    for (i, c) in doc.cards.iter().enumerate() {
        let stored = stmt
            .query_row([&c.id], |r| {
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
            })
            .optional()?;
        let Some(s) = stored else {
            rows.summary.cards.new += 1;
            rows.new_cards.push(i);
            continue;
        };
        let stored_deleted = s.deleted_at;
        // The database keeps its own `suspended`: an import never changes
        // how a live card is studied (ADR 0035).
        let rest_equal = CardRecord {
            deleted_at: c.deleted_at,
            suspended: c.suspended,
            ..s
        } == *c;
        match compare(rest_equal, stored_deleted, c.deleted_at) {
            Outcome::Present => rows.summary.cards.present += 1,
            // The stamp matters only for a deck, where it marks the batch.
            Outcome::Revive(_) => {
                rows.summary.cards.revived += 1;
                rows.revived_cards.push(i);
            }
            Outcome::Conflict => rows
                .summary
                .conflicts
                .push(Conflict::CardDiffers(c.id.clone())),
        }
    }
    Ok(())
}

/// Adds the cards deleted with each revived deck (same deck, same stamp)
/// that are not in the file.
fn add_batch_cards(
    conn: &Connection,
    doc: &Document,
    batches: &[(&str, i64)],
    rows: &mut Classified,
) -> DbResult<()> {
    let in_file: HashSet<&str> = doc.cards.iter().map(|c| c.id.as_str()).collect();
    let mut stmt =
        conn.prepare("SELECT id FROM cards WHERE deck_id = ?1 AND deleted_at = ?2 ORDER BY id")?;
    for &(deck_id, stamp) in batches {
        let ids = stmt.query_map(params![deck_id, stamp], |r| r.get::<_, String>(0))?;
        for id in ids {
            let id = id?;
            if !in_file.contains(id.as_str()) {
                rows.summary.cards.revived += 1;
                rows.batch_cards.push(id);
            }
        }
    }
    Ok(())
}

/// An entry with the same id is present if it is the same, else a
/// conflict. A new undo of an entry that the database has already undone
/// (with another undo) is a conflict too: an entry is undone once.
fn classify_events(conn: &Connection, doc: &Document, rows: &mut Classified) -> DbResult<()> {
    let mut stmt = conn.prepare(
        "SELECT id, card_id, kind, reviewed_at, grade, duration_ms, undoes
         FROM review_events WHERE id = ?1",
    )?;
    let mut undone_by = conn.prepare("SELECT id FROM review_events WHERE undoes = ?1")?;
    for (i, e) in doc.review_events.iter().enumerate() {
        let stored = stmt
            .query_row([&e.id], |r| {
                Ok(EventRecord {
                    id: r.get(0)?,
                    card_id: r.get(1)?,
                    kind: r.get(2)?,
                    reviewed_at: r.get(3)?,
                    grade: r.get(4)?,
                    duration_ms: r.get(5)?,
                    undoes: r.get(6)?,
                })
            })
            .optional()?;
        match stored {
            None => {
                let existing: Option<String> = match &e.undoes {
                    Some(target) => undone_by.query_row([target], |r| r.get(0)).optional()?,
                    None => None,
                };
                if let Some(existing_id) = existing {
                    rows.summary.conflicts.push(Conflict::AlreadyUndone {
                        event_id: e.id.clone(),
                        existing_id,
                    });
                } else {
                    rows.summary.review_events.new += 1;
                    rows.new_events.push(i);
                }
            }
            Some(s) if s == *e => rows.summary.review_events.present += 1,
            Some(_) => rows
                .summary
                .conflicts
                .push(Conflict::EventDiffers(e.id.clone())),
        }
    }
    Ok(())
}

/// Each live deck of the file (new, present or revived) against the
/// database's other live decks.
fn check_deck_names(
    conn: &Connection,
    doc: &Document,
    summary: &mut ImportSummary,
) -> DbResult<()> {
    let mut stmt = conn
        .prepare("SELECT id FROM decks WHERE name_key = ?1 AND deleted_at IS NULL AND id <> ?2")?;
    for d in doc.decks.iter().filter(|d| d.deleted_at.is_none()) {
        let existing: Option<String> = stmt
            .query_row(params![deck_name_key(&d.name), d.id], |r| r.get(0))
            .optional()?;
        if let Some(existing_id) = existing {
            summary.conflicts.push(Conflict::DeckNameTaken {
                deck_id: d.id.clone(),
                existing_id,
            });
        }
    }
    Ok(())
}

/// Brings back the revived decks, then the revived cards. `updated_at` is
/// not touched, as deleting did not touch it.
fn revive_rows(conn: &Connection, doc: &Document, rows: &Classified) -> DbResult<()> {
    let mut deck = conn
        .prepare("UPDATE decks SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL")?;
    for d in rows.revived_decks.iter().map(|&i| &doc.decks[i]) {
        deck.execute([&d.id])?;
    }
    let mut card = conn
        .prepare("UPDATE cards SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL")?;
    let from_file = rows.revived_cards.iter().map(|&i| doc.cards[i].id.as_str());
    for id in from_file.chain(rows.batch_cards.iter().map(String::as_str)) {
        card.execute([id])?;
    }
    Ok(())
}

/// Inserts the rows at the given positions of `doc`.
fn insert_rows<'d>(
    conn: &Connection,
    doc: &'d Document,
    rows: &Classified,
) -> DbResult<BTreeSet<&'d str>> {
    let mut deck = conn.prepare(
        "INSERT INTO decks (id, name, name_key, created_at, updated_at, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for d in rows.new_decks.iter().map(|&i| &doc.decks[i]) {
        deck.execute(params![
            d.id,
            d.name,
            deck_name_key(&d.name),
            d.created_at,
            d.updated_at,
            d.deleted_at
        ])?;
    }
    let mut card = conn.prepare(
        "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at, suspended, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for c in rows.new_cards.iter().map(|&i| &doc.cards[i]) {
        card.execute(params![
            c.id,
            c.deck_id,
            c.front_md,
            c.back_md,
            c.created_at,
            c.updated_at,
            c.suspended,
            c.deleted_at
        ])?;
        crate::storage::index_card_in(conn, &c.id, &c.front_md, &c.back_md)?;
    }
    let mut tag = conn.prepare("INSERT INTO tags (id, name, name_key) VALUES (?1, ?2, ?3)")?;
    for t in rows.new_tags.iter().map(|&i| &doc.tags[i]) {
        tag.execute(params![t.id, t.name, name_key(&t.name)])?;
    }
    let mut card_tag = conn.prepare("INSERT INTO card_tags (card_id, tag_id) VALUES (?1, ?2)")?;
    for ct in rows.new_card_tags.iter().map(|&i| &doc.card_tags[i]) {
        card_tag.execute(params![ct.card_id, rows.tag_id(&ct.tag_id)])?;
    }
    // In file order, which is by id: an undo's target (smaller id, checked
    // by validation) is inserted before it, or is already here.
    let mut event = conn.prepare(
        "INSERT INTO review_events (id, card_id, kind, reviewed_at, grade, duration_ms, undoes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut reviewed = BTreeSet::new();
    let mut new_events: Vec<&EventRecord> = rows
        .new_events
        .iter()
        .map(|&i| &doc.review_events[i])
        .collect();
    // The file's list should be sorted by id; sorting again costs little
    // and does not trust that.
    new_events.sort_by(|a, b| a.id.cmp(&b.id));
    for e in new_events {
        event.execute(params![
            e.id,
            e.card_id,
            e.kind,
            e.reviewed_at,
            e.grade,
            e.duration_ms,
            e.undoes
        ])?;
        reviewed.insert(e.card_id.as_str());
    }
    Ok(reviewed)
}
