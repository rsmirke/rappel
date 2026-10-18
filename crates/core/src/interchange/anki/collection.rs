//! Reading an Anki collection: an untrusted SQLite file, the temporary
//! copy [`super::package`] wrote (ADR 0061).
//!
//! It is opened read-only, with SQLite's defensive mode, no trusted
//! schema, no triggers and no views (so nothing in the file's schema runs),
//! `query_only`, no memory map and cell size checks, and a quick integrity
//! check first. Each table read must be a real table. Every value is read
//! by its type as SQLite stores it, whatever the schema says: an id that
//! is not an integer, text that is not UTF-8 or a field too large is the
//! row's problem, never a panic, and large values are measured in SQL
//! before they are fetched.
//!
//! Two layouts: schema 11 (Anki 2.0 and 2.1 packages) keeps the note types
//! and decks as JSON in `col`; schema 15 to 18 (the current packages)
//! keeps them in tables, with protobuf configs.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, config::DbConfig};
use serde::Deserialize;

use super::AnkiProblem;
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

/// A card's note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NoteRow {
    pub(super) mid: i64,
    /// When it was last changed, in seconds.
    pub(super) modified: i64,
    /// The fields joined by U+001F; `None` if larger than the limit.
    pub(super) fields: Option<String>,
    /// The tags, separated by spaces; `None` if larger than the limit.
    pub(super) tags: Option<String>,
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
    pub(super) fn open(path: &Path) -> Result<Self, AnkiProblem> {
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
        // The schema of a current collection sorts names with Anki's own
        // collation; without it, its tables cannot be read. Case-insensitive
        // here too: only the order of a few indexes depends on it.
        conn.create_collation("unicase", |a: &str, b: &str| {
            a.to_lowercase().cmp(&b.to_lowercase())
        })
        .map_err(sql)?;

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

    /// Each of `names` must be a table (not a view, which is disabled, nor
    /// missing).
    fn require_tables(&self, names: &[&str]) -> Result<(), AnkiProblem> {
        for name in names {
            let kind: Option<String> = self
                .conn
                .query_row(
                    "SELECT type FROM sqlite_schema WHERE name = ?1 LIMIT 1",
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
                #[derive(Deserialize)]
                struct Deck {
                    name: serde_json::Value,
                }
                let json = self.col_json("decks", limits)?;
                let decks: HashMap<String, Deck> =
                    serde_json::from_str(&json).map_err(|_| damaged("its decks"))?;
                if decks.len() > limits.decks {
                    return Err(damaged("it has too many decks"));
                }
                Ok(decks
                    .into_iter()
                    .filter_map(|(id, deck)| {
                        let serde_json::Value::String(name) = deck.name else {
                            return None;
                        };
                        Some((id.parse().ok()?, name))
                    })
                    .collect())
            }
            Schema::Tables => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT id, substr(CAST(name AS BLOB), 1, 4096) FROM decks LIMIT ?1")
                    .map_err(sql)?;
                let mut rows = stmt.query([sql_n(limits.decks + 1)]).map_err(sql)?;
                let mut decks = HashMap::new();
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
                    decks.insert(id, name);
                }
                Ok(decks)
            }
        }
    }

    /// `col.<column>` as text, at most the legacy JSON limit.
    fn col_json(&self, column: &str, limits: &AnkiLimits) -> Result<String, AnkiProblem> {
        let len: i64 = self
            .conn
            .query_row(
                &format!("SELECT length(CAST({column} AS BLOB)) FROM col LIMIT 1"),
                [],
                |r| Ok(int_of(r.get_ref(0)?).unwrap_or(0)),
            )
            .map_err(sql)?;
        if usize::try_from(len).map_or(true, |len| len > limits.legacy_json) {
            return Err(damaged(format!("its {column} are too large")));
        }
        self.conn
            .query_row(&format!("SELECT {column} FROM col LIMIT 1"), [], |r| {
                Ok(text_of(r.get_ref(0)?).unwrap_or_default())
            })
            .map_err(sql)
    }

    fn json_notetypes(&self, limits: &AnkiLimits) -> Result<HashMap<i64, Notetype>, AnkiProblem> {
        #[derive(Deserialize)]
        struct Model {
            #[serde(default)]
            name: serde_json::Value,
            #[serde(default, rename = "type")]
            kind: serde_json::Value,
            #[serde(default)]
            flds: Vec<Field>,
            #[serde(default)]
            tmpls: Vec<Tmpl>,
        }
        #[derive(Deserialize)]
        struct Field {
            #[serde(default)]
            name: serde_json::Value,
            #[serde(default)]
            ord: serde_json::Value,
        }
        #[derive(Deserialize)]
        struct Tmpl {
            #[serde(default)]
            name: serde_json::Value,
            #[serde(default)]
            ord: serde_json::Value,
            #[serde(default)]
            qfmt: serde_json::Value,
            #[serde(default)]
            afmt: serde_json::Value,
        }
        let string = |v: serde_json::Value| match v {
            serde_json::Value::String(s) => Some(s),
            _ => None,
        };
        let json = self.col_json("models", limits)?;
        let models: HashMap<String, Model> =
            serde_json::from_str(&json).map_err(|_| damaged("its note types"))?;
        drop(json);
        if models.len() > limits.notetypes {
            return Err(damaged("it has too many note types"));
        }
        let mut out = HashMap::new();
        for (id, model) in models {
            let Ok(id) = id.parse::<i64>() else { continue };
            if model.flds.len() > limits.notetype_parts || model.tmpls.len() > limits.notetype_parts
            {
                continue;
            }
            let fields = by_ord(
                model
                    .flds
                    .into_iter()
                    .map(|f| (f.ord.as_i64(), string(f.name).unwrap_or_default())),
            );
            let templates = by_ord(model.tmpls.into_iter().map(|t| {
                let formats = match (string(t.qfmt), string(t.afmt)) {
                    (Some(q), Some(a))
                        if q.len() <= limits.template && a.len() <= limits.template =>
                    {
                        Some((q, a))
                    }
                    _ => None,
                };
                (
                    t.ord.as_i64(),
                    Template {
                        name: string(t.name).unwrap_or_default(),
                        formats,
                    },
                )
            }));
            // Fields and templates must be exactly ords 0 to n-1, each once.
            let (Some(fields), Some(templates)) = (fields, templates) else {
                continue;
            };
            out.insert(
                id,
                Notetype {
                    name: string(model.name).unwrap_or_default(),
                    cloze: model.kind.as_i64() == Some(1),
                    fields,
                    templates,
                },
            );
        }
        Ok(out)
    }

    fn table_notetypes(&self, limits: &AnkiLimits) -> Result<HashMap<i64, Notetype>, AnkiProblem> {
        let mut out = self.notetype_rows(limits)?;
        let mut fields = self.field_rows(limits, &out)?;
        let mut templates = self.template_rows(limits, &out)?;
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
    fn notetype_rows(&self, limits: &AnkiLimits) -> Result<HashMap<i64, Notetype>, AnkiProblem> {
        let mut out = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT id, substr(CAST(name AS BLOB), 1, 4096),
                            CASE WHEN length(CAST(config AS BLOB)) <= ?2 THEN config END
                     FROM notetypes LIMIT ?1",
                )
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
    ) -> Result<Parts<String>, AnkiProblem> {
        let mut fields: Parts<String> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT ntid, ord, substr(CAST(name AS BLOB), 1, 4096) FROM fields")
                .map_err(sql)?;
            let mut rows = stmt.query([]).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                let Some(ntid) = int_of(row.get_ref(0).map_err(sql)?) else {
                    continue;
                };
                let ord = int_of(row.get_ref(1).map_err(sql)?);
                let name = text_of(row.get_ref(2).map_err(sql)?).unwrap_or_default();
                if out.contains_key(&ntid) {
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
    ) -> Result<Parts<Template>, AnkiProblem> {
        let mut templates: Parts<Template> = HashMap::new();
        let mut total = 0_usize;
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT ntid, ord, substr(CAST(name AS BLOB), 1, 4096),
                            CASE WHEN length(CAST(config AS BLOB)) <= ?1 THEN config END
                     FROM templates",
                )
                .map_err(sql)?;
            let mut rows = stmt.query([sql_n(limits.template * 2 + 64)]).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                let Some(ntid) = int_of(row.get_ref(0).map_err(sql)?) else {
                    continue;
                };
                if !out.contains_key(&ntid) {
                    continue;
                }
                let ord = int_of(row.get_ref(1).map_err(sql)?);
                let name = text_of(row.get_ref(2).map_err(sql)?).unwrap_or_default();
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
                        CASE WHEN length(CAST(n.flds AS BLOB)) <= ?1 THEN n.flds END,
                        CASE WHEN length(CAST(n.tags AS BLOB)) <= ?2 THEN n.tags END
                 FROM cards c LEFT JOIN notes n ON n.id = c.nid
                 ORDER BY c.id",
            )
            .map_err(|e| CardWalkError::Problem(sql(e)))?;
        let mut revlog = self
            .conn
            .prepare("SELECT id, cid, ease, time, type FROM revlog ORDER BY cid, id")
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
            let get = |i| {
                row.get_ref(i)
                    .map(int_of)
                    .map_err(|e| CardWalkError::Problem(sql(e)))
            };
            // A card whose id is not a number has no log and no note.
            let Some(id) = get(0)? else {
                let card = CardRow {
                    id: i64::MIN,
                    did: 0,
                    odid: 0,
                    ord: -1,
                    queue: 0,
                    kind: 0,
                    note: None,
                };
                each(card, &mut take).map_err(CardWalkError::Stopped)?;
                continue;
            };
            let note = match get(6)? {
                None => None,
                Some(_) => Some(NoteRow {
                    mid: get(7)?.unwrap_or(0),
                    modified: get(8)?.unwrap_or(0),
                    fields: text_of(row.get_ref(9).map_err(|e| CardWalkError::Problem(sql(e)))?),
                    tags: text_of(
                        row.get_ref(10)
                            .map_err(|e| CardWalkError::Problem(sql(e)))?,
                    ),
                }),
            };
            let card = CardRow {
                id,
                did: get(1)?.unwrap_or(0),
                odid: get(2)?.unwrap_or(0),
                ord: get(3)?.unwrap_or(-1),
                queue: get(4)?.unwrap_or(0),
                kind: get(5)?.unwrap_or(0),
                note,
            };
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
            .prepare("SELECT CASE WHEN length(CAST(flds AS BLOB)) <= ?1 THEN flds END FROM notes")
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
