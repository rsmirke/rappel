//! What an import does with each record as it streams by (ADR 0053):
//! check it, compare it with the database, and, when applying, write it.
//! Only compact notes are kept ([`super::notes`]); the record is dropped.
//!
//! The same code runs for the plan and for the apply, so the apply makes
//! exactly the decisions the plan reported, against the database as it is
//! inside the apply's transaction. A digest of every decision, in order,
//! lets the apply tell whether they are still the plan's.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Statement, params};
use sha2::{Digest, Sha256};

use super::format::{
    CardRecord, CardTagRecord, DeckRecord, EventRecord, FORMAT_VERSION, SettingsRecord, TagRecord,
};
use super::import::{Outcome, compare};
use super::notes::{Candidates, EventNote, Locate, Notes, UndoNote, name_digest, target_key};
use super::stream::{Sink, Stopped, Table};
use super::validate::{
    Kind, card_fields, check_id, check_settings, check_top_level, deck_fields, event_fields,
    invalid, reference, tag_fields,
};
use super::{Conflict, FieldProblem, ImportProblem, ImportSummary, Limits, SettingsImport};
use crate::domain::{deck_name_key, name_key};
use crate::error::Error;
use crate::id::format_id;
use crate::storage::{StoredSettings, index_card_in, stored_settings};

/// Why the engine stopped a read.
#[derive(Debug)]
pub(super) enum Stop {
    /// Reading everything in one pass did not work out (a problem, or the
    /// lists out of order): read the file again, one list at a time.
    Fallback,
    /// The file breaks a rule (when reading one list at a time).
    Problem(ImportProblem),
    /// The table just read has candidate duplicates to confirm.
    Locate,
    /// Applying: the database no longer gives the plan's decisions.
    Stale,
    /// The database failed.
    Db(Error),
}

impl From<rusqlite::Error> for Stop {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err.into())
    }
}

impl From<Error> for Stop {
    fn from(err: Error) -> Self {
        Self::Db(err)
    }
}

/// What a whole read of the file found.
#[derive(Debug)]
pub(super) struct Found {
    pub(super) summary: ImportSummary,
    /// The digest of every decision, in order.
    pub(super) digest: [u8; 32],
    pub(super) settings: SettingsRecord,
    /// Applying: the cards that got log entries, to rebuild.
    pub(super) reviewed: Vec<String>,
}

/// The queries that compare a row with the database.
struct Reads<'c> {
    deck: Statement<'c>,
    deck_name: Statement<'c>,
    card: Statement<'c>,
    batch: Statement<'c>,
    tag: Statement<'c>,
    tag_by_key: Statement<'c>,
    card_tag: Statement<'c>,
    event: Statement<'c>,
    undone_by: Statement<'c>,
}

/// The writes of an apply.
struct Writes<'c> {
    deck: Statement<'c>,
    revive_deck: Statement<'c>,
    card: Statement<'c>,
    revive_card: Statement<'c>,
    tag: Statement<'c>,
    card_tag: Statement<'c>,
    event: Statement<'c>,
}

/// Where each decision goes in the digest.
mod code {
    pub(super) const DECK: u8 = 0;
    pub(super) const CARD: u8 = 1;
    pub(super) const BATCH: u8 = 2;
    pub(super) const TAG: u8 = 3;
    pub(super) const CARD_TAG: u8 = 4;
    pub(super) const EVENT: u8 = 5;
    pub(super) const SETTINGS: u8 = 6;
    pub(super) const NEW: u8 = 0;
    pub(super) const PRESENT: u8 = 1;
    pub(super) const REVIVED: u8 = 2;
    pub(super) const CONFLICT: u8 = 3;
    pub(super) const MAPPED: u8 = 4;
}

/// How an engine reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// The whole file in one pass: any problem is a reason to read again,
    /// step by step.
    OnePass,
    /// One list at a time, where a problem is reported; rows compared with
    /// the database (and written, when applying).
    StepByStep,
    /// One list at a time, only checked: nothing compared.
    ChecksOnly,
}

pub(super) struct Engine<'c> {
    conn: &'c Connection,
    limits: Limits,
    /// How this engine reads.
    reading: Reading,
    reads: Reads<'c>,
    writes: Option<Writes<'c>>,
    notes: Notes,
    /// Set while a table is read again to confirm its candidates.
    locate: Option<Locate>,
    /// Candidates found when a table ended.
    candidates: Option<Candidates>,
    stop: Option<Stop>,
    previous_pair: Option<(u128, u128)>,
    summary: ImportSummary,
    name_conflicts: Vec<Conflict>,
    digest: Sha256,
    /// File tag → the database's tag with the same name key.
    mapped_tags: HashMap<u128, String>,
    /// Each revived deck and the stamp of its delete batch.
    batches: Vec<(String, i64)>,
    /// Applying: by card position, whether it got a log entry.
    reviewed: Vec<bool>,
    exported_at: Option<i64>,
    settings: Option<SettingsRecord>,
    has_media: bool,
    /// Taken before anything is written.
    has_decks: bool,
    stored: StoredSettings,
}

impl std::fmt::Debug for Engine<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

impl<'c> Engine<'c> {
    /// An engine on `conn`; `write` to apply. `exact` to read one list at
    /// a time and report problems.
    pub(super) fn new(
        conn: &'c Connection,
        limits: Limits,
        write: bool,
        exact: bool,
    ) -> crate::Result<Self> {
        let reads = Reads {
            deck: conn.prepare(
                "SELECT id, name, created_at, updated_at, deleted_at FROM decks WHERE id = ?1",
            )?,
            deck_name: conn.prepare(
                "SELECT id FROM decks WHERE name_key = ?1 AND deleted_at IS NULL AND id <> ?2",
            )?,
            card: conn.prepare(
                "SELECT id, deck_id, front_md, back_md, created_at, updated_at, suspended, deleted_at
                 FROM cards WHERE id = ?1",
            )?,
            batch: conn.prepare(
                "SELECT id FROM cards WHERE deck_id = ?1 AND deleted_at = ?2 ORDER BY id",
            )?,
            tag: conn.prepare("SELECT name FROM tags WHERE id = ?1")?,
            tag_by_key: conn.prepare("SELECT id FROM tags WHERE name_key = ?1")?,
            card_tag: conn
                .prepare("SELECT 1 FROM card_tags WHERE card_id = ?1 AND tag_id = ?2")?,
            event: conn.prepare(
                "SELECT id, card_id, kind, reviewed_at, grade, duration_ms, undoes
                 FROM review_events WHERE id = ?1",
            )?,
            undone_by: conn.prepare("SELECT id FROM review_events WHERE undoes = ?1")?,
        };
        let writes = if write {
            Some(Writes {
                deck: conn.prepare(
                    "INSERT INTO decks (id, name, name_key, created_at, updated_at, deleted_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?,
                // `updated_at` is not touched, as deleting did not touch it.
                revive_deck: conn.prepare(
                    "UPDATE decks SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL",
                )?,
                card: conn.prepare(
                    "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at, suspended, deleted_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?,
                revive_card: conn.prepare(
                    "UPDATE cards SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL",
                )?,
                tag: conn.prepare("INSERT INTO tags (id, name, name_key) VALUES (?1, ?2, ?3)")?,
                card_tag: conn.prepare("INSERT INTO card_tags (card_id, tag_id) VALUES (?1, ?2)")?,
                event: conn.prepare(
                    "INSERT INTO review_events (id, card_id, kind, reviewed_at, grade, duration_ms, undoes)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?,
            })
        } else {
            None
        };
        let has_decks = conn.query_row("SELECT EXISTS (SELECT 1 FROM decks)", [], |r| r.get(0))?;
        Ok(Self {
            conn,
            limits,
            reading: if exact {
                Reading::StepByStep
            } else {
                Reading::OnePass
            },
            reads,
            writes,
            notes: Notes::default(),
            locate: None,
            candidates: None,
            stop: None,
            previous_pair: None,
            summary: ImportSummary::default(),
            name_conflicts: Vec::new(),
            digest: Sha256::new(),
            mapped_tags: HashMap::new(),
            batches: Vec::new(),
            reviewed: Vec::new(),
            exported_at: None,
            settings: None,
            has_media: false,
            has_decks,
            stored: stored_settings(conn)?,
        })
    }

    /// Whether a problem is reported (rather than read again).
    fn exact(&self) -> bool {
        self.reading != Reading::OnePass
    }

    /// Whether rows are compared with the database (and written).
    fn classifies(&self) -> bool {
        self.reading != Reading::ChecksOnly
    }

    /// An engine that only checks the file: nothing is compared with the
    /// database, so a file that turns out to be refused costs no query.
    pub(super) fn checks_only(mut self) -> Self {
        self.reading = Reading::ChecksOnly;
        self
    }

    /// Takes `exported_at`, `settings` and `media` from an engine that read
    /// them, for one that reads only the lists.
    pub(super) fn adopt_top_level(&mut self, other: &Self) {
        self.exported_at = other.exported_at;
        self.settings = other.settings;
        self.has_media = other.has_media;
    }

    /// Why the last read stopped, if the engine stopped it.
    pub(super) fn take_stop(&mut self) -> Option<Stop> {
        self.stop.take()
    }

    pub(super) fn stopped(&self) -> bool {
        self.stop.is_some()
    }

    /// The candidate duplicates of `table` after a read of it stopped at a
    /// problem: its notes so far, sorted.
    pub(super) fn candidates_after_problem(&mut self, table: Table) -> Candidates {
        self.notes.seal(table)
    }

    /// The candidates [`Stop::Locate`] was about.
    pub(super) fn take_candidates(&mut self) -> Candidates {
        self.candidates.take().unwrap_or_default()
    }

    /// The next read of a table confirms `candidates`; then
    /// [`Self::end_locate`].
    pub(super) fn start_locate(&mut self, candidates: Candidates) {
        self.locate = Some(Locate::new(candidates));
    }

    pub(super) fn end_locate(&mut self) {
        self.locate = None;
    }

    /// The problem to report: with [`Self::exact`], this one; else read
    /// again, one list at a time, which finds it.
    fn problem(&self, problem: ImportProblem) -> Stop {
        if self.exact() {
            Stop::Problem(problem)
        } else {
            Stop::Fallback
        }
    }

    /// `exported_at` and `media`, which come before every list.
    pub(super) fn check_top_level(&self) -> Result<(), ImportProblem> {
        check_top_level(self.exported_at.unwrap_or(-1), self.has_media)
    }

    /// The settings, checked after every list.
    pub(super) fn check_settings(&self) -> Result<(), ImportProblem> {
        check_settings(&self.settings.unwrap_or_default())
    }

    /// A conflict. Applying, the plan had none, so the database changed.
    fn conflict(&mut self, conflict: Conflict) -> Result<(), Stop> {
        if self.writes.is_some() {
            return Err(Stop::Stale);
        }
        self.summary.conflicts.push(conflict);
        Ok(())
    }

    fn deck_row(&mut self, index: usize, d: &DeckRecord) -> Result<(), Stop> {
        let at = |field, reason| invalid("decks", index, field, reason);
        let id = check_id(&d.id).map_err(|r| self.problem(at("id", r)))?;
        match self.locate.as_mut().map(|l| l.repeats_id(id)) {
            Some(true) => return Err(self.problem(at("id", FieldProblem::DuplicateId))),
            Some(false) => {}
            None => self.notes.push_deck(id, d.deleted_at.is_some()),
        }
        deck_fields(d, &self.limits).map_err(|(f, r)| self.problem(at(f, r)))?;
        let key = d.deleted_at.is_none().then(|| deck_name_key(&d.name));
        if let Some(key) = &key {
            match self.locate.as_mut().map(|l| l.repeats_name(key)) {
                Some(true) => {
                    return Err(self.problem(at("name", FieldProblem::DuplicateDeckName)));
                }
                Some(false) => {}
                None => self.notes.deck_names.push(name_digest(key)),
            }
        }
        if self.locate.is_some() || !self.classifies() {
            return Ok(());
        }
        self.classify_deck(index, d, key.as_deref())
    }

    /// Unknown id: new; otherwise as [`compare`] says. A live deck (new,
    /// present or revived) is also checked against the database's other
    /// live decks' names, first, so an apply never inserts a name in use.
    fn classify_deck(
        &mut self,
        index: usize,
        d: &DeckRecord,
        key: Option<&str>,
    ) -> Result<(), Stop> {
        if let Some(key) = key {
            let existing: Option<String> = self
                .reads
                .deck_name
                .query_row(params![key, d.id], |r| r.get(0))
                .optional()?;
            if let Some(existing_id) = existing {
                if self.writes.is_some() {
                    return Err(Stop::Stale);
                }
                self.name_conflicts.push(Conflict::DeckNameTaken {
                    deck_id: d.id.clone(),
                    existing_id,
                });
            }
        }
        let stored = self
            .reads
            .deck
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
            self.summary.decks.new += 1;
            record(&mut self.digest, code::DECK, index, code::NEW, &[]);
            if let Some(w) = &mut self.writes {
                w.deck.execute(params![
                    d.id,
                    d.name,
                    deck_name_key(&d.name),
                    d.created_at,
                    d.updated_at,
                    d.deleted_at
                ])?;
            }
            return Ok(());
        };
        let stored_deleted = s.deleted_at;
        let rest_equal = DeckRecord {
            deleted_at: d.deleted_at,
            ..s
        } == *d;
        match compare(rest_equal, stored_deleted, d.deleted_at) {
            Outcome::Present => {
                self.summary.decks.present += 1;
                record(&mut self.digest, code::DECK, index, code::PRESENT, &[]);
            }
            Outcome::Revive(stamp) => {
                self.summary.decks.revived += 1;
                record(
                    &mut self.digest,
                    code::DECK,
                    index,
                    code::REVIVED,
                    &stamp.to_le_bytes(),
                );
                self.batches.push((d.id.clone(), stamp));
                if let Some(w) = &mut self.writes {
                    w.revive_deck.execute([&d.id])?;
                }
            }
            Outcome::Conflict => {
                record(&mut self.digest, code::DECK, index, code::CONFLICT, &[]);
                self.conflict(Conflict::DeckDiffers(d.id.clone()))?;
            }
        }
        Ok(())
    }

    fn card_row(&mut self, index: usize, c: &CardRecord) -> Result<(), Stop> {
        let at = |field, reason| invalid("cards", index, field, reason);
        let id = check_id(&c.id).map_err(|r| self.problem(at("id", r)))?;
        match self.locate.as_mut().map(|l| l.repeats_id(id)) {
            Some(true) => return Err(self.problem(at("id", FieldProblem::DuplicateId))),
            Some(false) => {}
            None => self.notes.cards.push(id),
        }
        let Some(deck_deleted) = reference(&c.deck_id).and_then(|d| self.notes.deck(d)) else {
            return Err(self.problem(at("deck_id", FieldProblem::UnknownReference)));
        };
        if deck_deleted && c.deleted_at.is_none() {
            return Err(self.problem(at("deleted_at", FieldProblem::LiveCardInDeletedDeck)));
        }
        card_fields(c, &self.limits).map_err(|(f, r)| self.problem(at(f, r)))?;
        if self.locate.is_some() || !self.classifies() {
            return Ok(());
        }
        self.classify_card(index, c)
    }

    fn classify_card(&mut self, index: usize, c: &CardRecord) -> Result<(), Stop> {
        let stored = self
            .reads
            .card
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
            self.summary.cards.new += 1;
            record(&mut self.digest, code::CARD, index, code::NEW, &[]);
            if let Some(w) = &mut self.writes {
                w.card.execute(params![
                    c.id,
                    c.deck_id,
                    c.front_md,
                    c.back_md,
                    c.created_at,
                    c.updated_at,
                    c.suspended,
                    c.deleted_at
                ])?;
                index_card_in(self.conn, &c.id, &c.front_md, &c.back_md)?;
            }
            return Ok(());
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
            Outcome::Present => {
                self.summary.cards.present += 1;
                record(&mut self.digest, code::CARD, index, code::PRESENT, &[]);
            }
            // The stamp matters only for a deck, where it marks the batch.
            Outcome::Revive(_) => {
                self.summary.cards.revived += 1;
                record(&mut self.digest, code::CARD, index, code::REVIVED, &[]);
                if let Some(w) = &mut self.writes {
                    w.revive_card.execute([&c.id])?;
                }
            }
            Outcome::Conflict => {
                record(&mut self.digest, code::CARD, index, code::CONFLICT, &[]);
                self.conflict(Conflict::CardDiffers(c.id.clone()))?;
            }
        }
        Ok(())
    }

    /// The cards deleted with each revived deck (same deck, same stamp, as
    /// `undelete_deck`) that the file does not have come back too.
    fn add_batch_cards(&mut self) -> Result<(), Stop> {
        let batches = std::mem::take(&mut self.batches);
        for (deck_id, stamp) in &batches {
            let ids: Vec<String> = self
                .reads
                .batch
                .query_map(params![deck_id, stamp], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in ids {
                let in_file = reference(&id).is_some_and(|v| self.notes.card_position(v).is_some());
                if in_file {
                    continue;
                }
                self.summary.cards.revived += 1;
                record(
                    &mut self.digest,
                    code::BATCH,
                    0,
                    code::REVIVED,
                    id.as_bytes(),
                );
                if let Some(w) = &mut self.writes {
                    w.revive_card.execute([&id])?;
                }
            }
        }
        Ok(())
    }

    fn tag_row(&mut self, index: usize, t: &TagRecord) -> Result<(), Stop> {
        let at = |field, reason| invalid("tags", index, field, reason);
        let id = check_id(&t.id).map_err(|r| self.problem(at("id", r)))?;
        match self.locate.as_mut().map(|l| l.repeats_id(id)) {
            Some(true) => return Err(self.problem(at("id", FieldProblem::DuplicateId))),
            Some(false) => {}
            None => self.notes.tags.push(id),
        }
        tag_fields(t, &self.limits).map_err(|(f, r)| self.problem(at(f, r)))?;
        let key = name_key(&t.name);
        match self.locate.as_mut().map(|l| l.repeats_name(&key)) {
            Some(true) => return Err(self.problem(at("name", FieldProblem::DuplicateTagName))),
            Some(false) => return Ok(()),
            None => self.notes.tag_names.push(name_digest(&key)),
        }
        if !self.classifies() {
            return Ok(());
        }
        self.classify_tag(index, t, id, &key)
    }

    /// A tag with the same id is present if it has the same name, else a
    /// conflict. A tag whose id is not here but whose name key a database
    /// tag has is that tag (two machines that each created "maths"): it is
    /// mapped onto the database's id, counted as present, and the file's
    /// card tags that name it attach to the database's tag. The database
    /// keeps its own spelling. Else the tag is new.
    fn classify_tag(
        &mut self,
        index: usize,
        t: &TagRecord,
        id: u128,
        key: &str,
    ) -> Result<(), Stop> {
        let stored: Option<String> = self.reads.tag.query_row([&t.id], |r| r.get(0)).optional()?;
        match stored {
            Some(name) if name == t.name => {
                self.summary.tags.present += 1;
                record(&mut self.digest, code::TAG, index, code::PRESENT, &[]);
            }
            Some(_) => {
                record(&mut self.digest, code::TAG, index, code::CONFLICT, &[]);
                self.conflict(Conflict::TagDiffers(t.id.clone()))?;
            }
            None => {
                let existing: Option<String> = self
                    .reads
                    .tag_by_key
                    .query_row([key], |r| r.get(0))
                    .optional()?;
                if let Some(existing_id) = existing {
                    self.summary.tags.present += 1;
                    record(
                        &mut self.digest,
                        code::TAG,
                        index,
                        code::MAPPED,
                        existing_id.as_bytes(),
                    );
                    self.mapped_tags.insert(id, existing_id);
                } else {
                    self.summary.tags.new += 1;
                    record(&mut self.digest, code::TAG, index, code::NEW, &[]);
                    if let Some(w) = &mut self.writes {
                        w.tag.execute(params![t.id, t.name, key])?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The rows must come in the order an export writes them, by card id
    /// then tag id, so a repeated row is the one just before: no set of
    /// pairs is held (ADR 0025, row 9).
    fn card_tag_row(&mut self, index: usize, ct: &CardTagRecord) -> Result<(), Stop> {
        let at = |field, reason| invalid("card_tags", index, field, reason);
        let Some(card) = reference(&ct.card_id).filter(|c| self.notes.card_position(*c).is_some())
        else {
            return Err(self.problem(at("card_id", FieldProblem::UnknownReference)));
        };
        let Some(tag) = reference(&ct.tag_id).filter(|t| self.notes.has_tag(*t)) else {
            return Err(self.problem(at("tag_id", FieldProblem::UnknownReference)));
        };
        let pair = (card, tag);
        match self.previous_pair {
            Some(p) if p == pair => {
                return Err(self.problem(at("tag_id", FieldProblem::DuplicateRow)));
            }
            Some(p) if p > pair => {
                return Err(self.problem(at("card_id", FieldProblem::OutOfOrder)));
            }
            _ => {}
        }
        self.previous_pair = Some(pair);
        if self.locate.is_some() || !self.classifies() {
            return Ok(());
        }
        // A card tag is present or new, once its tag is mapped; it has no
        // content to differ.
        let tag_id = self
            .mapped_tags
            .get(&tag)
            .map_or(ct.tag_id.as_str(), String::as_str);
        if self.reads.card_tag.exists(params![ct.card_id, tag_id])? {
            self.summary.card_tags.present += 1;
            record(&mut self.digest, code::CARD_TAG, index, code::PRESENT, &[]);
        } else {
            self.summary.card_tags.new += 1;
            record(&mut self.digest, code::CARD_TAG, index, code::NEW, &[]);
            if let Some(w) = &mut self.writes {
                w.card_tag.execute(params![ct.card_id, tag_id])?;
            }
        }
        Ok(())
    }

    /// Each row on its own first; each undo against the entry it cancels,
    /// which may come anywhere in the list, once the list is read
    /// ([`Self::check_undos`]).
    fn event_row(&mut self, index: usize, e: &EventRecord) -> Result<(), Stop> {
        let at = |field, reason| invalid("review_events", index, field, reason);
        let row = u32::try_from(index).unwrap_or(u32::MAX);
        let id = check_id(&e.id).map_err(|r| self.problem(at("id", r)))?;
        let collecting = match self.locate.as_mut().map(|l| l.repeats_id(id)) {
            Some(true) => return Err(self.problem(at("id", FieldProblem::DuplicateId))),
            Some(false) => false,
            None => true,
        };
        // A row that fails past its id still counts for duplicates of it.
        let partial = |this: &mut Self, stop: Stop| {
            if collecting {
                this.notes
                    .events
                    .push(EventNote::new(id, 0, Kind::Review, 0, row));
            }
            stop
        };
        let Some(card) = reference(&e.card_id).and_then(|c| self.notes.card_position(c)) else {
            let stop = self.problem(at("card_id", FieldProblem::UnknownReference));
            return Err(partial(self, stop));
        };
        let kind = match event_fields(e) {
            Ok(kind) => kind,
            Err((f, r)) => {
                let stop = self.problem(at(f, r));
                return Err(partial(self, stop));
            }
        };
        let target = e.undoes.as_deref().map(target_key);
        if let Some(target) = target {
            match self.locate.as_mut().map(|l| l.repeats_target(target)) {
                Some(true) => return Err(self.problem(at("undoes", FieldProblem::AlreadyUndone))),
                Some(false) => {}
                None => self.notes.undos.push(UndoNote { id, target }),
            }
        }
        if !collecting {
            return Ok(());
        }
        self.notes
            .events
            .push(EventNote::new(id, e.reviewed_at, kind, card, row));
        if !self.classifies() {
            return Ok(());
        }
        self.classify_event(index, e, kind, card)
    }

    /// An entry with the same id is present if it is the same, else a
    /// conflict. A new undo of an entry that the database has already
    /// undone (with another undo) is a conflict too: an entry is undone
    /// once.
    fn classify_event(
        &mut self,
        index: usize,
        e: &EventRecord,
        kind: Kind,
        card: u32,
    ) -> Result<(), Stop> {
        let stored = self
            .reads
            .event
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
                    Some(target) => self
                        .reads
                        .undone_by
                        .query_row([target], |r| r.get(0))
                        .optional()?,
                    None => None,
                };
                if let Some(existing_id) = existing {
                    record(
                        &mut self.digest,
                        code::EVENT,
                        index,
                        code::CONFLICT,
                        existing_id.as_bytes(),
                    );
                    return self.conflict(Conflict::AlreadyUndone {
                        event_id: e.id.clone(),
                        existing_id,
                    });
                }
                self.summary.review_events.new += 1;
                record(&mut self.digest, code::EVENT, index, code::NEW, &[]);
                if let Some(w) = &mut self.writes {
                    if let Some(flag) = self.reviewed.get_mut(card as usize) {
                        *flag = true;
                    }
                    if kind == Kind::Undo {
                        // Inserted once the whole list is read, after
                        // every entry it may name.
                        if let Some(note) = self.notes.events.last_mut() {
                            note.mark_new();
                        }
                    } else {
                        w.event.execute(params![
                            e.id,
                            e.card_id,
                            e.kind,
                            e.reviewed_at,
                            e.grade,
                            e.duration_ms,
                            e.undoes
                        ])?;
                    }
                }
            }
            Some(s) if s == *e => {
                self.summary.review_events.present += 1;
                record(&mut self.digest, code::EVENT, index, code::PRESENT, &[]);
            }
            Some(_) => {
                record(&mut self.digest, code::EVENT, index, code::CONFLICT, &[]);
                self.conflict(Conflict::EventDiffers(e.id.clone()))?;
            }
        }
        Ok(())
    }

    /// Each undo cancels an earlier review or reset of the same card in
    /// the file: dated no later and with a smaller id, so it is replayed
    /// (and inserted) first, as the schema requires. The first undo in
    /// row order that does not is reported.
    fn check_undos(&self) -> Result<(), Stop> {
        let mut first: Option<u32> = None;
        for u in &self.notes.undos {
            let Some(undo) = self.notes.event(u.id) else {
                continue;
            };
            let cancels = self.notes.event(u.target).is_some_and(|t| {
                t.card == undo.card && !t.is_undo() && t.at() <= undo.at() && t.id < undo.id
            });
            if !cancels {
                first = Some(first.map_or(undo.index, |f| f.min(undo.index)));
            }
        }
        match first {
            Some(index) => Err(self.problem(invalid(
                "review_events",
                index as usize,
                "undoes",
                FieldProblem::InvalidUndo,
            ))),
            None => Ok(()),
        }
    }

    /// Applying: the new undos, in id order, after every entry they name.
    fn insert_undos(&mut self) -> Result<(), Stop> {
        let Some(w) = &mut self.writes else {
            return Ok(());
        };
        self.notes.undos.sort_unstable_by_key(|u| u.id);
        for u in &self.notes.undos {
            let Some(note) = self
                .notes
                .events
                .binary_search_by_key(&u.id, |e| e.id)
                .ok()
                .map(|i| self.notes.events[i])
            else {
                continue;
            };
            if !note.is_new() {
                continue;
            }
            let card = self.notes.cards[note.card as usize];
            w.event.execute(params![
                format_id(u.id),
                format_id(card),
                "undo",
                i64::try_from(note.at()).unwrap_or(i64::MAX),
                None::<String>,
                0,
                format_id(u.target)
            ])?;
        }
        Ok(())
    }

    /// What follows a whole table: the batch cards after the cards, the
    /// undo checks and the undos after the log.
    pub(super) fn after_table(&mut self, table: Table) -> Result<(), Stop> {
        match table {
            Table::Cards => {
                if self.classifies() {
                    self.add_batch_cards()?;
                }
                if self.writes.is_some() {
                    self.reviewed = vec![false; self.notes.cards.len()];
                }
            }
            Table::Events => {
                self.check_undos()?;
                self.insert_undos()?;
            }
            Table::Decks | Table::Tags | Table::CardTags => {}
        }
        Ok(())
    }

    /// Runs `f` and keeps its stop, for the stream.
    fn run(&mut self, f: impl FnOnce(&mut Self) -> Result<(), Stop>) -> Result<(), Stopped> {
        f(self).map_err(|stop| {
            self.stop = Some(stop);
            Stopped
        })
    }

    /// The end of a whole read: the settings decision (the file's settings
    /// go in only into a database without decks, deleted ones included,
    /// ADR 0027), the deck name conflicts after every other, and the
    /// digest.
    pub(super) fn finish(mut self) -> Found {
        let settings = self.settings.unwrap_or_default();
        self.summary.settings = if super::import::stored_of(&settings) == self.stored {
            SettingsImport::Unchanged
        } else if self.has_decks {
            SettingsImport::Kept
        } else {
            SettingsImport::Applied
        };
        let decision = match self.summary.settings {
            SettingsImport::Unchanged => 0,
            SettingsImport::Kept => 1,
            SettingsImport::Applied => 2,
        };
        record(&mut self.digest, code::SETTINGS, 0, decision, &[]);
        self.summary.conflicts.append(&mut self.name_conflicts);
        let reviewed = self
            .reviewed
            .iter()
            .zip(&self.notes.cards)
            .filter(|(r, _)| **r)
            .map(|(_, &card)| format_id(card))
            .collect();
        Found {
            summary: self.summary,
            digest: self.digest.finalize().into(),
            settings,
            reviewed,
        }
    }
}

/// Adds a decision to the digest: which table, which row, what was
/// decided, and what it depends on (a stamp, a database id).
fn record(digest: &mut Sha256, table: u8, index: usize, decision: u8, extra: &[u8]) {
    digest.update([table, decision]);
    digest.update((index as u64).to_le_bytes());
    digest.update((extra.len() as u64).to_le_bytes());
    digest.update(extra);
}

impl Sink for Engine<'_> {
    fn version(&mut self, version: u32) -> Result<(), Stopped> {
        self.run(|e| {
            if version == FORMAT_VERSION {
                Ok(())
            } else {
                Err(e.problem(ImportProblem::UnsupportedFormatVersion {
                    found: i64::from(version),
                }))
            }
        })
    }

    fn exported_at(&mut self, at: i64) {
        self.exported_at = Some(at);
    }

    fn settings(&mut self, settings: SettingsRecord) {
        self.settings = Some(settings);
    }

    fn media_item(&mut self) {
        self.has_media = true;
    }

    fn table_starts(&mut self, _table: Table) -> Result<(), Stopped> {
        self.previous_pair = None;
        Ok(())
    }

    fn deck(&mut self, index: usize, deck: DeckRecord) -> Result<(), Stopped> {
        self.run(|e| e.deck_row(index, &deck))
    }

    fn card(&mut self, index: usize, card: CardRecord) -> Result<(), Stopped> {
        self.run(|e| e.card_row(index, &card))
    }

    fn tag(&mut self, index: usize, tag: TagRecord) -> Result<(), Stopped> {
        self.run(|e| e.tag_row(index, &tag))
    }

    fn card_tag(&mut self, index: usize, card_tag: CardTagRecord) -> Result<(), Stopped> {
        self.run(|e| e.card_tag_row(index, &card_tag))
    }

    fn event(&mut self, index: usize, event: EventRecord) -> Result<(), Stopped> {
        self.run(|e| e.event_row(index, &event))
    }

    /// Seals the table's notes. Candidate duplicates stop the read (to
    /// confirm them with another read, or to read again one list at a
    /// time); else what follows the table runs. A read that confirms
    /// candidates does nothing here.
    fn table_ends(&mut self, table: Table) -> Result<(), Stopped> {
        if self.locate.is_some() {
            return Ok(());
        }
        self.run(|e| {
            let candidates = e.notes.seal(table);
            if !candidates.is_empty() {
                if !e.exact() {
                    return Err(Stop::Fallback);
                }
                e.candidates = Some(candidates);
                return Err(Stop::Locate);
            }
            e.after_table(table)
        })
    }

    fn out_of_order(&mut self) -> Stopped {
        self.stop = Some(Stop::Fallback);
        Stopped
    }
}
