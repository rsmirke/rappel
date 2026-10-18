//! Reading an Anki collection: an untrusted SQLite file, the temporary
//! copy [`super::package`] wrote (ADR 0061).
//!
//! It is opened read-only, with SQLite's defensive mode, no trusted
//! schema, no triggers and no views (so nothing in the file's schema runs),
//! `query_only`, no memory map, cell size checks and lowered length limits.
//! Before anything is read (the integrity check included), a schema with a
//! virtual table, a shadow table or a generated column is refused: a
//! virtual table's module or a generated column's expression would run
//! code chosen by the file. Each table read must be a real table. Every
//! value is read by its type as SQLite stores it, whatever the schema
//! says: an id that is not an integer, text that is not UTF-8 or a field
//! too large is the row's problem, never a panic, and large values are
//! measured with `octet_length` (which reads a value's size, not the
//! value) before they are fetched. The JSON of a legacy collection is read
//! with bounds ([`super::json`]).
//!
//! Two layouts: schema 11 (Anki 2.0 and 2.1 packages) keeps the note types
//! and decks as JSON in `col`; schema 15 to 18 (the current packages)
//! keeps them in tables, with protobuf configs.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, config::DbConfig};

use super::AnkiProblem;
use super::json::{self, Bounds, Text};
use super::package::AnkiLimits;
use super::proto::{text_field, varint_field};

/// How a collection keeps its note types and decks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Schema {
    /// Schema 11: JSON in `col.models` and `col.decks`.
    Json,
    /// Schema 15 to 18: the `notetypes`, `fields`, `templates` and `decks`
    /// tables.
    Tables,
}

/// An opened collection.
pub(super) struct Collection {
    conn: Connection,
    schema: Schema,
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collection")
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

/// A card template, or why it cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Template {
    pub(super) name: String,
    /// The question and answer formats; `None` if one is larger than the
    /// limit or not text.
    pub(super) formats: Option<(String, String)>,
}

/// A note type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Notetype {
    pub(super) name: String,
    pub(super) cloze: bool,
    /// The field names, by ord.
    pub(super) fields: Vec<String>,
    /// The templates, by ord.
    pub(super) templates: Vec<Template>,
}

/// One card with what its note has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CardRow {
    pub(super) id: i64,
    pub(super) did: i64,
    pub(super) odid: i64,
    pub(super) ord: i64,
    pub(super) queue: i64,
    pub(super) kind: i64,
    pub(super) note: Option<NoteRow>,
}

/// A text column of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Column {
    Kept(String),
    /// Larger than its limit (not fetched).
    TooLong,
    /// NULL.
    Missing,
}

/// A card's note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NoteRow {
    pub(super) mid: i64,
    /// When it was last changed, in seconds.
    pub(super) modified: i64,
    /// The fields joined by U+001F.
    pub(super) fields: Column,
    /// The tags, separated by spaces.
    pub(super) tags: Column,
}

/// One review log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RevRow {
    /// When, in UTC milliseconds.
    pub(super) id: i64,
    pub(super) cid: i64,
    /// The button: 1 Again, 2 Hard, 3 Good, 4 Easy; 0 for a manual entry.
    pub(super) ease: i64,
    /// Time taken, in milliseconds.
    pub(super) time: i64,
    /// 0 learn, 1 review, 2 relearn, 3 filtered (cram), 4 manual, 5
    /// rescheduled.
    pub(super) kind: i64,
    /// The interval after the entry (days, or negative seconds).
    pub(super) ivl: i64,
    /// The ease factor after the entry (0 when the entry did not schedule).
    pub(super) factor: i64,
}

fn damaged(why: impl std::fmt::Display) -> AnkiProblem {
    AnkiProblem::Damaged(format!("the collection cannot be read: {why}"))
}

fn sql(err: rusqlite::Error) -> AnkiProblem {
    damaged(err)
}

/// A limit as an SQL integer.
fn sql_n(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Text from a value of any type (lossy for invalid UTF-8); `None` for
/// NULL.
fn text_of(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(r) => Some(r.to_string()),
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
    }
}

/// An integer value; anything else is `None`.
fn int_of(value: ValueRef<'_>) -> Option<i64> {
    match value {
        ValueRef::Integer(i) => Some(i),
        _ => None,
    }
}

impl Collection {
    /// Opens the collection at `path` read-only and defensively, checks it
    /// and its version.
    pub(super) fn open(path: &Path, limits: &AnkiLimits) -> Result<Self, AnkiProblem> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql)?;
        for (config, on) in [
            (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
            (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
            (DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false),
            (DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
        ] {
            conn.set_db_config(config, on).map_err(sql)?;
        }
        conn.pragma_update(None, "query_only", true).map_err(sql)?;
        conn.pragma_update(None, "cell_size_check", true)
            .map_err(sql)?;
        conn.pragma_update(None, "mmap_size", 0).map_err(sql)?;
        // No value larger than the largest one read (the legacy JSON, a
        // note's fields), no statement or table of another size than ours:
        // a larger value is still measured by `octet_length`, never loaded.
        let largest = limits.legacy_json.max(limits.note * 2) + (1 << 20);
        for (limit, value) in [
            (Limit::SQLITE_LIMIT_LENGTH, largest),
            (Limit::SQLITE_LIMIT_SQL_LENGTH, 1 << 20),
            (Limit::SQLITE_LIMIT_COLUMN, 200),
        ] {
            conn.set_limit(limit, i32::try_from(value).unwrap_or(i32::MAX))
                .map_err(sql)?;
        }
        // The schema of a current collection sorts names with Anki's own
        // collation; without it, its tables cannot be read. Case-insensitive
        // here too: only the order of a few indexes depends on it.
        conn.create_collation("unicase", |a: &str, b: &str| {
            a.to_lowercase().cmp(&b.to_lowercase())
        })
        .map_err(sql)?;

        check_schema(&conn)?;
        let check: String = conn
            .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
            .map_err(|_| AnkiProblem::Damaged("the collection is not a database".to_owned()))?;
        if check != "ok" {
            return Err(AnkiProblem::Damaged(
                "the collection's integrity check failed".to_owned(),
            ));
        }
        let mut collection = Self {
            conn,
            schema: Schema::Json,
        };
        collection.require_tables(&["col", "notes", "cards", "revlog"])?;
        let version = collection
            .conn
            .query_row("SELECT ver FROM col LIMIT 1", [], |r| {
                Ok(int_of(r.get_ref(0)?))
            })
            .map_err(sql)?
            .ok_or_else(|| damaged("its version is not a number"))?;
        collection.schema = match version {
            11 => Schema::Json,
            15..=18 => Schema::Tables,
            other => return Err(AnkiProblem::UnsupportedSchema(other)),
        };
        if collection.schema == Schema::Tables {
            collection.require_tables(&["notetypes", "fields", "templates", "decks"])?;
        }
        Ok(collection)
    }

    /// Each of `names` must be an ordinary table of the main database
    /// (`PRAGMA table_list` type `table`: not a view, a virtual or a shadow
    /// table, nor missing).
    fn require_tables(&self, names: &[&str]) -> Result<(), AnkiProblem> {
        for name in names {
            let kind: Option<String> = self
                .conn
                .query_row(
                    "SELECT type FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                    [name],
                    |r| Ok(text_of(r.get_ref(0)?)),
                )
                .map_err(|_| damaged(format!("it has no {name} table")))?;
            if kind.as_deref() != Some("table") {
                return Err(damaged(format!("{name} is not a table")));
            }
        }
        Ok(())
    }

    /// The note types, by id.
    pub(super) fn notetypes(
        &self,
        limits: &AnkiLimits,
    ) -> Result<HashMap<i64, Notetype>, AnkiProblem> {
        match self.schema {
            Schema::Json => self.json_notetypes(limits),
            Schema::Tables => self.table_notetypes(limits),
        }
    }

    /// The deck names, by id, as Anki has them (components separated by
    /// `::` or U+001F).
    pub(super) fn decks(&self, limits: &AnkiLimits) -> Result<HashMap<i64, String>, AnkiProblem> {
        match self.schema {
            Schema::Json => {
                let decks = self.with_col_json("decks", limits, |json| {
                    json::decks(json, limits.decks, NAME_BYTES)
                })?;
                let mut budget = Budget::new(limits);
                let mut out = HashMap::new();
                for (id, name) in decks {
                    let (Some(id), Some(name)) =
                        (id.kept().and_then(|i| i.parse().ok()), name.kept())
                    else {
                        continue;
                    };
                    budget.take(name.len())?;
                    out.insert(id, name);
                }
                Ok(out)
            }
            Schema::Tables => {
                let mut stmt = self
                    .conn
                    .prepare(&format!(
                        "SELECT id, {} FROM decks LIMIT ?1",
                        name_column("name")
                    ))
                    .map_err(sql)?;
                let mut rows = stmt.query([sql_n(limits.decks + 1)]).map_err(sql)?;
                let mut decks = HashMap::new();
                let mut budget = Budget::new(limits);
                while let Some(row) = rows.next().map_err(sql)? {
                    if decks.len() == limits.decks {
                        return Err(damaged("it has too many decks"));
                    }
                    let (Some(id), Some(name)) = (
                        int_of(row.get_ref(0).map_err(sql)?),
                        text_of(row.get_ref(1).map_err(sql)?),
                    ) else {
                        continue;
                    };
                    budget.take(name.len())?;
                    decks.insert(id, name);
                }
                Ok(decks)
            }
        }
    }

    /// Runs `read` on `col.<column>`'s bytes (at most the legacy JSON
    /// limit), borrowed from SQLite, never copied.
    fn with_col_json<T>(
        &self,
        column: &str,
        limits: &AnkiLimits,
        read: impl FnOnce(&[u8]) -> Result<T, json::Refused>,
    ) -> Result<T, AnkiProblem> {
        let len: Option<i64> = self
            .conn
            .query_row(
                &format!("SELECT octet_length({column}) FROM col LIMIT 1"),
                [],
                |r| Ok(int_of(r.get_ref(0)?)),
            )
            .map_err(sql)?;
        if usize::try_from(len.unwrap_or(0)).map_or(true, |len| len > limits.legacy_json) {
            return Err(damaged(format!("its {column} are too large")));
        }
        let mut read = Some(read);
        let result = self
            .conn
            .query_row(&format!("SELECT {column} FROM col LIMIT 1"), [], |r| {
                let bytes: &[u8] = match r.get_ref(0)? {
                    ValueRef::Text(b) | ValueRef::Blob(b) => b,
                    _ => b"{}",
                };
                Ok(read.take().map(|read| read(bytes)))
            })
            .map_err(sql)?;
        result
            .unwrap_or_else(|| Err(json::Refused(String::new(), false)))
            .map_err(|e| damaged(format!("its {column}: {}", e.0)))
    }

    fn json_notetypes(&self, limits: &AnkiLimits) -> Result<HashMap<i64, Notetype>, AnkiProblem> {
        let bounds = Bounds {
            entries: limits.notetypes,
            items: limits.notetype_parts,
            items_total: limits.notetype_parts_total,
            name: NAME_BYTES,
            template: limits.template,
        };
        let models = self.with_col_json("models", limits, |json| json::models(json, bounds))?;
        let mut budget = Budget::new(limits);
        let mut out = HashMap::new();
        for (id, model) in models {
            let Some(id) = id.kept().and_then(|i| i.parse::<i64>().ok()) else {
                continue;
            };
            let mut name = |t: Text| -> Result<String, AnkiProblem> {
                let name = t.kept().unwrap_or_default();
                budget.take(name.len())?;
                Ok(name)
            };
            let mut fields = Vec::with_capacity(model.fields.len());
            for f in model.fields {
                fields.push((f.ord, name(f.name)?));
            }
            let mut templates = Vec::with_capacity(model.templates.len());
            for t in model.templates {
                let formats = match (t.qfmt, t.afmt) {
                    (Text::Kept(q), Text::Kept(a)) => Some((q, a)),
                    _ => None,
                };
                templates.push((
                    t.ord,
                    Template {
                        name: name(t.name)?,
                        formats,
                    },
                ));
            }
            let notetype_name = name(model.name.unwrap_or(Text::Other))?;
            // Fields and templates must be exactly ords 0 to n-1, each once.
            let (Some(fields), Some(templates)) =
                (by_ord(fields.into_iter()), by_ord(templates.into_iter()))
            else {
                continue;
            };
            out.insert(
                id,
                Notetype {
                    name: notetype_name,
                    cloze: model.kind == Some(1),
                    fields,
                    templates,
                },
            );
        }
        Ok(out)
    }

    fn table_notetypes(&self, limits: &AnkiLimits) -> Result<HashMap<i64, Notetype>, AnkiProblem> {
        let mut budget = Budget::new(limits);
        let mut out = self.notetype_rows(limits, &mut budget)?;
        let mut fields = self.field_rows(limits, &out, &mut budget)?;
        let mut templates = self.template_rows(limits, &out, &mut budget)?;
        out.retain(|id, notetype| {
            let parts = |n: usize| n <= limits.notetype_parts;
            let Some(f) = fields.remove(id).filter(|f| parts(f.len())) else {
                return false;
            };
            let Some(t) = templates.remove(id).filter(|t| parts(t.len())) else {
                return false;
            };
            let (Some(f), Some(t)) = (by_ord(f.into_iter()), by_ord(t.into_iter())) else {
                return false;
            };
            notetype.fields = f;
            notetype.templates = t;
            true
        });
        Ok(out)
    }

    /// The `notetypes` table: names and kinds, without fields or templates.
    fn notetype_rows(
        &self,
        limits: &AnkiLimits,
        budget: &mut Budget,
    ) -> Result<HashMap<i64, Notetype>, AnkiProblem> {
        let mut out = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "SELECT id, {},
                                CASE WHEN octet_length(config) <= ?2 THEN config END
                         FROM notetypes LIMIT ?1",
                    name_column("name")
                ))
                .map_err(sql)?;
            let mut rows = stmt
                .query((sql_n(limits.notetypes + 1), sql_n(limits.template)))
                .map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                if out.len() == limits.notetypes {
                    return Err(damaged("it has too many note types"));
                }
                let Some(id) = int_of(row.get_ref(0).map_err(sql)?) else {
                    continue;
                };
                let name = text_of(row.get_ref(1).map_err(sql)?).unwrap_or_default();
                budget.take(name.len())?;
                // Notetype.Config: kind = 1 (0 normal, 1 cloze).
                let cloze = match row.get_ref(2).map_err(sql)? {
                    ValueRef::Blob(config) => varint_field(config, 1).ok().flatten() == Some(1),
                    _ => false,
                };
                out.insert(
                    id,
                    Notetype {
                        name,
                        cloze,
                        fields: Vec::new(),
                        templates: Vec::new(),
                    },
                );
            }
        }
        Ok(out)
    }

    /// The `fields` table, by note type (only those of `notetypes`).
    fn field_rows(
        &self,
        limits: &AnkiLimits,
        out: &HashMap<i64, Notetype>,
        budget: &mut Budget,
    ) -> Result<Parts<String>, AnkiProblem> {
        let mut fields: Parts<String> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "SELECT ntid, ord, {} FROM fields",
                    name_column("name")
                ))
                .map_err(sql)?;
            let mut rows = stmt.query([]).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                let Some(ntid) = int_of(row.get_ref(0).map_err(sql)?) else {
                    continue;
                };
                let ord = int_of(row.get_ref(1).map_err(sql)?);
                if out.contains_key(&ntid) {
                    budget.part()?;
                    let name = text_of(row.get_ref(2).map_err(sql)?).unwrap_or_default();
                    budget.take(name.len())?;
                    let list = fields.entry(ntid).or_default();
                    if list.len() <= limits.notetype_parts {
                        list.push((ord, name));
                    }
                }
            }
        }
        Ok(fields)
    }

    /// The `templates` table, by note type (only those of `notetypes`).
    fn template_rows(
        &self,
        limits: &AnkiLimits,
        out: &HashMap<i64, Notetype>,
        budget: &mut Budget,
    ) -> Result<Parts<Template>, AnkiProblem> {
        let mut templates: Parts<Template> = HashMap::new();
        let mut total = 0_usize;
        {
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "SELECT ntid, ord, {},
                                CASE WHEN octet_length(config) <= ?1 THEN config END
                         FROM templates",
                    name_column("name")
                ))
                .map_err(sql)?;
            let mut rows = stmt.query([sql_n(limits.template * 2 + 64)]).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                let Some(ntid) = int_of(row.get_ref(0).map_err(sql)?) else {
                    continue;
                };
                if !out.contains_key(&ntid) {
                    continue;
                }
                budget.part()?;
                let ord = int_of(row.get_ref(1).map_err(sql)?);
                let name = text_of(row.get_ref(2).map_err(sql)?).unwrap_or_default();
                budget.take(name.len())?;
                // Template.Config: q_format = 1, a_format = 2.
                let formats = match row.get_ref(3).map_err(sql)? {
                    ValueRef::Blob(config) => {
                        match (text_field(config, 1), text_field(config, 2)) {
                            (Ok(q), Ok(a)) => {
                                let (q, a) = (q.unwrap_or(""), a.unwrap_or(""));
                                (q.len() <= limits.template && a.len() <= limits.template)
                                    .then(|| (q.to_owned(), a.to_owned()))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some((q, a)) = &formats {
                    total = total.saturating_add(q.len() + a.len());
                    if total > limits.legacy_json {
                        return Err(damaged("its card templates are too large"));
                    }
                }
                let list = templates.entry(ntid).or_default();
                if list.len() <= limits.notetype_parts {
                    list.push((ord, Template { name, formats }));
                }
            }
        }
        Ok(templates)
    }

    /// Every card with its note, in id order, given to `each`, which may
    /// stop the walk by returning an error. Each card's note is read with
    /// it; fields and tags larger than their limits are `None`.
    pub(super) fn each_card<E>(
        &self,
        limits: &AnkiLimits,
        mut each: impl FnMut(
            CardRow,
            &mut dyn FnMut(i64) -> Result<Vec<RevRow>, AnkiProblem>,
        ) -> Result<(), E>,
    ) -> Result<(), CardWalkError<E>> {
        let mut cards = self
            .conn
            .prepare(
                "SELECT c.id, c.did, c.odid, c.ord, c.queue, c.type, n.id, n.mid, n.mod,
                        CASE WHEN octet_length(n.flds) <= ?1 THEN n.flds END,
                        CASE WHEN octet_length(n.tags) <= ?2 THEN n.tags END,
                        octet_length(n.flds) IS NULL, octet_length(n.tags) IS NULL
                 FROM cards c LEFT JOIN notes n ON n.id = c.nid
                 ORDER BY c.id",
            )
            .map_err(|e| CardWalkError::Problem(sql(e)))?;
        let mut revlog = self
            .conn
            .prepare("SELECT id, cid, ease, time, type, ivl, factor FROM revlog ORDER BY cid, id")
            .map_err(|e| CardWalkError::Problem(sql(e)))?;
        let mut revs = revlog
            .query([])
            .map_err(|e| CardWalkError::Problem(sql(e)))?;
        // The next log entry not yet given to a card.
        let mut pending: Option<RevRow> = None;
        let mut exhausted = false;
        let mut take = |cid: i64| -> Result<Vec<RevRow>, AnkiProblem> {
            let mut out = Vec::new();
            loop {
                if pending.is_none() && !exhausted {
                    match revs.next().map_err(sql)? {
                        None => exhausted = true,
                        Some(row) => {
                            let get = |i| row.get_ref(i).map(int_of).map_err(sql);
                            let (Some(id), Some(rcid)) = (get(0)?, get(1)?) else {
                                // An entry without a card or a time: no card has it.
                                continue;
                            };
                            pending = Some(RevRow {
                                id,
                                cid: rcid,
                                ease: get(2)?.unwrap_or(-1),
                                time: get(3)?.unwrap_or(0),
                                kind: get(4)?.unwrap_or(-1),
                                ivl: get(5)?.unwrap_or(0),
                                factor: get(6)?.unwrap_or(0),
                            });
                        }
                    }
                }
                match pending {
                    // Entries of cards before this one (not in the
                    // package) are passed over.
                    Some(rev) if rev.cid < cid => pending = None,
                    Some(rev) if rev.cid == cid => {
                        out.push(rev);
                        pending = None;
                    }
                    _ => return Ok(out),
                }
            }
        };
        let mut rows = cards
            .query((sql_n(limits.note), sql_n(limits.note_tags)))
            .map_err(|e| CardWalkError::Problem(sql(e)))?;
        while let Some(row) = rows.next().map_err(|e| CardWalkError::Problem(sql(e)))? {
            let card = card_of(row).map_err(CardWalkError::Problem)?;
            each(card, &mut take).map_err(CardWalkError::Stopped)?;
        }
        Ok(())
    }

    /// Every note's fields (those within the limit), for the media names
    /// they use, and every template's.
    pub(super) fn each_note_fields(
        &self,
        limits: &AnkiLimits,
        mut each: impl FnMut(&str) -> bool,
    ) -> Result<bool, AnkiProblem> {
        let mut stmt = self
            .conn
            .prepare("SELECT CASE WHEN octet_length(flds) <= ?1 THEN flds END FROM notes")
            .map_err(sql)?;
        let mut rows = stmt.query([sql_n(limits.note)]).map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            if let Some(fields) = text_of(row.get_ref(0).map_err(sql)?)
                && !each(&fields)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The longest name kept (of a deck, note type, field or template), in
/// bytes; a longer one is read as empty.
const NAME_BYTES: usize = 4096;

/// `column`, a name, if it is at most [`NAME_BYTES`] long (measured, not
/// loaded), else NULL.
fn name_column(column: &str) -> String {
    format!("CASE WHEN octet_length({column}) <= {NAME_BYTES} THEN {column} END")
}

/// A note's text column: `value`, or why there is none.
fn column(value: ValueRef<'_>, null: bool) -> Column {
    match text_of(value) {
        Some(text) => Column::Kept(text),
        None if null => Column::Missing,
        None => Column::TooLong,
    }
}

/// What reading note types and decks may keep: the bytes of every name
/// together, and the fields and templates of every note type together.
pub(super) struct Budget {
    bytes: usize,
    parts: usize,
}

impl Budget {
    fn new(limits: &AnkiLimits) -> Self {
        Self {
            bytes: limits.names,
            parts: limits.notetype_parts_total,
        }
    }

    fn take(&mut self, bytes: usize) -> Result<(), AnkiProblem> {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .ok_or_else(|| damaged("its names are too large"))?;
        Ok(())
    }

    fn part(&mut self) -> Result<(), AnkiProblem> {
        self.parts = self
            .parts
            .checked_sub(1)
            .ok_or_else(|| damaged("it has too many fields or templates"))?;
        Ok(())
    }
}

/// Refuses a schema that would run code chosen by the file when a table is
/// read: a virtual table (its module), a shadow table, or a generated
/// column (its expression; trusted schema off does not stop built-in
/// functions such as `printf`). Read before anything else, the integrity
/// check included: only the schema's text is parsed, and parsing `CREATE
/// VIRTUAL TABLE` does not connect its module (SQLite connects it on first
/// use).
fn check_schema(conn: &Connection) -> Result<(), AnkiProblem> {
    let not_db = |_| AnkiProblem::Damaged("the collection is not a database".to_owned());
    let mut stmt = conn
        .prepare("SELECT type, name, sql FROM sqlite_schema")
        .map_err(not_db)?;
    let mut rows = stmt.query([]).map_err(not_db)?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next().map_err(not_db)? {
        let text = text_of(row.get_ref(2).map_err(sql)?).unwrap_or_default();
        let words: Vec<String> = text
            .split_whitespace()
            .take(3)
            .map(str::to_ascii_lowercase)
            .collect();
        if words.get(1).is_some_and(|w| w == "virtual") {
            return Err(damaged("it has a virtual table"));
        }
        if text_of(row.get_ref(0).map_err(sql)?).as_deref() == Some("table") {
            tables.push(text_of(row.get_ref(1).map_err(sql)?).unwrap_or_default());
            if tables.len() > 1_000 {
                return Err(damaged("it has too many tables"));
            }
        }
    }
    drop(rows);
    let mut list = conn
        .prepare("SELECT type FROM pragma_table_list WHERE schema = 'main'")
        .map_err(sql)?;
    let kinds = list
        .query_map([], |r| Ok(text_of(r.get_ref(0)?)))
        .map_err(sql)?;
    for kind in kinds {
        if matches!(kind.map_err(sql)?.as_deref(), Some("virtual" | "shadow")) {
            return Err(damaged("it has a virtual table"));
        }
    }
    for table in tables {
        // `hidden` 2 and 3: a generated column (virtual or stored).
        let generated: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM pragma_table_xinfo(?1) WHERE hidden IN (2, 3))",
                [&table],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if generated {
            return Err(damaged("it has a generated column"));
        }
    }
    Ok(())
}

/// A row of [`Collection::each_card`]'s query as a card. A card whose id
/// is not a number has no log and no note.
fn card_of(row: &rusqlite::Row<'_>) -> Result<CardRow, AnkiProblem> {
    let get = |i| row.get_ref(i).map(int_of).map_err(sql);
    let Some(id) = get(0)? else {
        return Ok(CardRow {
            id: i64::MIN,
            did: 0,
            odid: 0,
            ord: -1,
            queue: 0,
            kind: 0,
            note: None,
        });
    };
    let note = match get(6)? {
        None => None,
        Some(_) => Some(NoteRow {
            mid: get(7)?.unwrap_or(0),
            modified: get(8)?.unwrap_or(0),
            fields: column(row.get_ref(9).map_err(sql)?, get(11)? == Some(1)),
            tags: column(row.get_ref(10).map_err(sql)?, get(12)? == Some(1)),
        }),
    };
    Ok(CardRow {
        id,
        did: get(1)?.unwrap_or(0),
        odid: get(2)?.unwrap_or(0),
        ord: get(3)?.unwrap_or(-1),
        queue: get(4)?.unwrap_or(0),
        kind: get(5)?.unwrap_or(0),
        note,
    })
}

/// The fields or templates of each note type, with their ords as read.
type Parts<T> = HashMap<i64, Vec<(Option<i64>, T)>>;

/// Why [`Collection::each_card`] stopped.
#[derive(Debug)]
pub(super) enum CardWalkError<E> {
    /// The collection cannot be read.
    Problem(AnkiProblem),
    /// The caller stopped it.
    Stopped(E),
}

/// Items with their ords as a list by ord: `None` if an ord is missing,
/// negative, repeated or too large (each ord 0 to n-1 exactly once, as
/// Anki keeps them), else the items in ord order.
fn by_ord<T>(items: impl Iterator<Item = (Option<i64>, T)>) -> Option<Vec<T>> {
    let mut slots: Vec<Option<T>> = Vec::new();
    let mut count = 0;
    let items: Vec<(Option<i64>, T)> = items.collect();
    slots.resize_with(items.len(), || None);
    for (ord, item) in items {
        let ord = usize::try_from(ord?).ok()?;
        let slot = slots.get_mut(ord)?;
        if slot.is_some() {
            return None;
        }
        *slot = Some(item);
        count += 1;
    }
    (count == slots.len()).then(|| slots.into_iter().flatten().collect())
}
