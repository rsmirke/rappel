//! Reading review events and the `card_schedule` cache, and rebuilding the
//! cache from the review log.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::Store;
use crate::domain::{CardState, EventKind, Grade, ReviewEvent, Schedule};
use crate::error::{Error, Result};
use crate::scheduling::Scheduler;

impl Store {
    /// The log of `card_id`, every kind of entry, oldest first
    /// (`reviewed_at`, then id).
    ///
    /// # Errors
    /// SQLite errors, or [`Error::InvalidColumn`] for a value the schema
    /// should have rejected.
    pub fn review_events(&self, card_id: &str) -> Result<Vec<ReviewEvent>> {
        read_log(&self.conn, card_id)
    }

    /// The cached scheduling state of `card_id`; [`CardState::New`] if it
    /// has no `card_schedule` row.
    ///
    /// # Errors
    /// SQLite errors, or [`Error::InvalidColumn`] for an invalid phase or
    /// count.
    pub fn card_state(&self, card_id: &str) -> Result<CardState> {
        read_card_state(&self.conn, card_id)
    }

    /// Rebuilds the whole `card_schedule` cache from `review_events` with
    /// `scheduler`, in one transaction. Returns the number of cards with a
    /// schedule.
    ///
    /// # Errors
    /// SQLite and scheduler errors. On error the cache is left unchanged.
    #[cfg(test)]
    pub(crate) fn rebuild_schedule_cache(&mut self, scheduler: &Scheduler) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let cards = write_schedule_cache(&tx, scheduler)?;
        tx.commit()?;
        Ok(cards)
    }
}

/// Replaces every `card_schedule` row with one rebuilt from
/// `review_events`. Returns the number of rows written. The caller provides
/// the transaction (a migration's, or `Store::rebuild_schedule_cache`'s).
/// It also reads a log from before migration 0007, for the migrations
/// that rebuild before it.
pub(super) fn write_schedule_cache(tx: &Transaction<'_>, scheduler: &Scheduler) -> Result<usize> {
    tx.execute("DELETE FROM card_schedule", [])?;
    let mut cards = 0;
    {
        let mut write = |card_id: &str, events: &[ReviewEvent]| -> Result<()> {
            if let CardState::Scheduled(s) = scheduler.rebuild_state(card_id, events)? {
                upsert_schedule(tx, card_id, &s)?;
                cards += 1;
            }
            Ok(())
        };

        // Streams the log card by card, so memory use is one card's
        // history, not the whole log.
        let columns = event_columns(tx)?;
        let mut select = tx.prepare(&format!(
            "SELECT {columns} FROM review_events
             ORDER BY card_id, reviewed_at, id"
        ))?;
        let mut rows = select.query([])?;
        let mut batch: Vec<ReviewEvent> = Vec::new();
        while let Some(row) = rows.next()? {
            let event = raw_event(row)?.into_event()?;
            if batch.first().is_some_and(|e| e.card_id != event.card_id) {
                write(&batch[0].card_id.clone(), &batch)?;
                batch.clear();
            }
            batch.push(event);
        }
        if let Some(first) = batch.first() {
            write(&first.card_id.clone(), &batch)?;
        }
    }
    Ok(cards)
}

/// Rebuilds the `card_schedule` row of each card in `card_ids` from its
/// review log, inside the caller's transaction (an import's).
pub(crate) fn rebuild_cards<'a>(
    conn: &Connection,
    scheduler: &Scheduler,
    card_ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let columns = event_columns(conn)?;
    for card_id in card_ids {
        let state = scheduler.rebuild_state(card_id, &log_with(conn, columns, card_id)?)?;
        write_card_state(conn, card_id, &state)?;
    }
    Ok(())
}

/// The log of `card_id`, oldest first (see [`Store::review_events`]), in
/// any schema version (the migration tests read old logs).
pub(super) fn read_log(conn: &Connection, card_id: &str) -> Result<Vec<ReviewEvent>> {
    log_with(conn, event_columns(conn)?, card_id)
}

/// [`read_log`] with the columns already chosen by [`event_columns`].
fn log_with(conn: &Connection, columns: &str, card_id: &str) -> Result<Vec<ReviewEvent>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {columns} FROM review_events
         WHERE card_id = ?1 ORDER BY reviewed_at, id"
    ))?;
    let rows = stmt.query_map([card_id], raw_event)?;
    rows.map(|row| row?.into_event()).collect()
}

/// The cached state of `card_id` (see [`Store::card_state`]).
pub(super) fn read_card_state(conn: &Connection, card_id: &str) -> Result<CardState> {
    let row = conn
        .query_row(
            "SELECT phase, due_at, stability, difficulty, last_review_at, reps, lapses
             FROM card_schedule WHERE card_id = ?1",
            [card_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((phase, due_at, stability, difficulty, last_review_at, reps, lapses)) = row else {
        return Ok(CardState::New);
    };
    Ok(CardState::Scheduled(Schedule {
        phase: phase
            .parse()
            .map_err(|_| invalid("card_schedule.phase", &phase))?,
        stability: to_f32(stability),
        difficulty: to_f32(difficulty),
        due_at,
        last_review_at,
        reps: u32::try_from(reps).map_err(|_| invalid("card_schedule.reps", &reps))?,
        lapses: u32::try_from(lapses).map_err(|_| invalid("card_schedule.lapses", &lapses))?,
    }))
}

/// Writes `schedule` as the `card_schedule` row of `card_id`, replacing
/// any row it had. Used by the rebuild and by recording a review, so both
/// write the cache the same way.
pub(super) fn upsert_schedule(conn: &Connection, card_id: &str, schedule: &Schedule) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO card_schedule
             (card_id, phase, due_at, stability, difficulty, last_review_at, reps, lapses)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (card_id) DO UPDATE SET
             phase = excluded.phase, due_at = excluded.due_at,
             stability = excluded.stability, difficulty = excluded.difficulty,
             last_review_at = excluded.last_review_at,
             reps = excluded.reps, lapses = excluded.lapses",
    )?;
    stmt.execute(params![
        card_id,
        schedule.phase.as_str(),
        schedule.due_at,
        f64::from(schedule.stability),
        f64::from(schedule.difficulty),
        schedule.last_review_at,
        schedule.reps,
        schedule.lapses,
    ])?;
    Ok(())
}

/// Makes the `card_schedule` row of `card_id` hold `state`: no row for
/// [`CardState::New`], else [`upsert_schedule`].
pub(super) fn write_card_state(conn: &Connection, card_id: &str, state: &CardState) -> Result<()> {
    match state {
        CardState::New => {
            conn.execute("DELETE FROM card_schedule WHERE card_id = ?1", [card_id])?;
            Ok(())
        }
        CardState::Scheduled(s) => upsert_schedule(conn, card_id, s),
    }
}

/// The columns [`raw_event`] reads, in its order.
pub(super) const EVENT_COLUMNS: &str = "id, card_id, kind, reviewed_at, grade, duration_ms, undoes";

/// [`EVENT_COLUMNS`] for a log from before migration 0007, where every
/// entry is a review. Migrations 0002 and 0005 rebuild the cache from such
/// a log when a database (or a restored backup) is upgraded.
const BEFORE_KINDS_COLUMNS: &str = "id, card_id, 'review', reviewed_at, grade, duration_ms, NULL";

/// The columns to select for [`raw_event`]: [`EVENT_COLUMNS`], or
/// [`BEFORE_KINDS_COLUMNS`] if `review_events` does not have migration
/// 0007's `kind` column yet.
fn event_columns(conn: &Connection) -> Result<&'static str> {
    let has_kind = conn
        .prepare("SELECT 1 FROM pragma_table_info('review_events') WHERE name = 'kind'")?
        .exists([])?;
    Ok(if has_kind {
        EVENT_COLUMNS
    } else {
        BEFORE_KINDS_COLUMNS
    })
}

/// A `review_events` row before its kind and grade are parsed.
pub(super) struct RawEvent {
    id: String,
    card_id: String,
    kind: String,
    reviewed_at: i64,
    grade: Option<String>,
    duration_ms: i64,
    undoes: Option<String>,
}

/// Reads a row selected with [`EVENT_COLUMNS`].
pub(super) fn raw_event(r: &Row<'_>) -> rusqlite::Result<RawEvent> {
    Ok(RawEvent {
        id: r.get(0)?,
        card_id: r.get(1)?,
        kind: r.get(2)?,
        reviewed_at: r.get(3)?,
        grade: r.get(4)?,
        duration_ms: r.get(5)?,
        undoes: r.get(6)?,
    })
}

impl RawEvent {
    pub(super) fn into_event(self) -> Result<ReviewEvent> {
        let kind = match (self.kind.as_str(), self.grade, self.undoes) {
            ("review", Some(grade), None) => EventKind::Review(
                grade
                    .parse::<Grade>()
                    .map_err(|_| invalid("review_events.grade", &grade))?,
            ),
            ("reset", None, None) => EventKind::Reset,
            ("undo", None, Some(undoes)) => EventKind::Undo { undoes },
            // The schema's CHECKs refuse every other combination.
            _ => return Err(invalid("review_events.kind", &self.kind)),
        };
        Ok(ReviewEvent {
            id: self.id,
            card_id: self.card_id,
            reviewed_at: self.reviewed_at,
            kind,
            duration_ms: self.duration_ms,
        })
    }
}

/// Appends `event` to `review_events`: how the store records, resets and
/// undoes. An import inserts the file's checked rows as they are.
pub(crate) fn insert_event(conn: &Connection, event: &ReviewEvent) -> Result<()> {
    let undoes = match &event.kind {
        EventKind::Undo { undoes } => Some(undoes.as_str()),
        EventKind::Review(_) | EventKind::Reset => None,
    };
    conn.prepare(
        "INSERT INTO review_events (id, card_id, kind, reviewed_at, grade, duration_ms, undoes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        event.id,
        event.card_id,
        event.kind.as_str(),
        event.reviewed_at,
        event.grade().map(Grade::as_str),
        event.duration_ms,
        undoes,
    ])?;
    Ok(())
}

/// How much of a bad value [`invalid`] keeps, in characters. A settings
/// value is free text in the database, which a damaged or hand-edited file
/// may make as large as SQLite allows; the error is logged and may be
/// shown, so it keeps only the start (ADR 0050).
const SHOWN_VALUE_CHARS: usize = 64;

/// [`Error::InvalidColumn`] for `value` read from `column`, its text cut to
/// [`SHOWN_VALUE_CHARS`] characters, with "…" where it was cut.
pub(super) fn invalid(column: &'static str, value: &dyn std::fmt::Display) -> Error {
    let mut value = value.to_string();
    if let Some((cut, _)) = value.char_indices().nth(SHOWN_VALUE_CHARS) {
        value.truncate(cut);
        value.push('…');
    }
    Error::InvalidColumn { column, value }
}

/// A `REAL` written from an `f32` back to `f32`.
pub(super) fn to_f32(v: f64) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "every value in these columns was written with `f64::from(f32)`, so the \
                  conversion back is exact; anything else is a cache value that a rebuild \
                  overwrites anyway"
    )]
    let v = v as f32;
    v
}
