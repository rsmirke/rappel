//! The full-text search index and the All Cards query (ADR 0035).
//!
//! `cards_fts` is a cache: one row per card (deleted ones included, so
//! undeleting needs no write), keyed by `cards.seq`, holding the folded
//! plain text of each side ([`search_text`]). Every write of a card's sides
//! calls [`index_card_in`] inside its own transaction, so the index never
//! disagrees with the cards a reader can see.

use std::collections::HashMap;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params_from_iter};

use super::Store;
use super::cards::card_from_row;
use crate::content::search_text;
use crate::domain::{Card, Phase, Tag, search_key};
use crate::error::Result;

/// Sets the index row of card `id` to its current sides, inside the caller's
/// transaction. A card that does not exist is ignored.
pub(crate) fn index_card_in(
    conn: &Connection,
    id: &str,
    front_md: &str,
    back_md: &str,
) -> Result<()> {
    let Some(seq) = conn
        .query_row("SELECT seq FROM cards WHERE id = ?1", [id], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
    else {
        return Ok(());
    };
    conn.execute("DELETE FROM cards_fts WHERE rowid = ?1", [seq])?;
    conn.execute(
        "INSERT INTO cards_fts (rowid, front, back) VALUES (?1, ?2, ?3)",
        rusqlite::params![seq, search_text(front_md), search_text(back_md)],
    )?;
    Ok(())
}

/// Rebuilds the whole index from the cards, inside the caller's transaction
/// (migration 0010).
pub(crate) fn rebuild_search_index(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM cards_fts", [])?;
    let mut read = conn.prepare("SELECT seq, front_md, back_md FROM cards")?;
    let mut write =
        conn.prepare("INSERT INTO cards_fts (rowid, front, back) VALUES (?1, ?2, ?3)")?;
    let mut rows = read.query([])?;
    while let Some(r) = rows.next()? {
        let (seq, front, back): (i64, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
        write.execute(rusqlite::params![
            seq,
            search_text(&front),
            search_text(&back)
        ])?;
    }
    Ok(())
}

/// The FTS5 `MATCH` expression for what the user typed: every word, folded
/// like the index, as a quoted prefix (`"word"*`), all of them required.
/// Quotes are doubled, so nothing typed is ever FTS5 syntax. `None` if
/// no word has a letter or digit to search for.
///
/// A word that is the start of another word of the query (`ca` next to
/// `card`, or `card` twice) is left out: every card the longer one finds,
/// the shorter finds too, so the result is the same. FTS5 reads each word's
/// matches separately, so 5,000 copies of `a` took 10 s of CPU on 5,000
/// cards in a debug build (ADR 0050). The work left is bounded by
/// [`MAX_QUERY_BYTES`].
///
/// Only the first [`MAX_QUERY_BYTES`] of the words searched are read,
/// counted once folded; what separates them (spaces, and words without a
/// letter or digit) does not count.
#[must_use]
pub(crate) fn fts_query(text: &str) -> Option<String> {
    let mut budget = MAX_QUERY_BYTES;
    let mut words: Vec<String> = Vec::new();
    // NUL cannot be quoted in an FTS5 string; it separates words here.
    for raw in text.split(|c: char| c.is_whitespace() || c == '\0') {
        if budget == 0 {
            break;
        }
        // What comes before a word's first letter or digit (`---abc`,
        // accents on their own) holds no token, so it is left out before
        // anything is cut: padding never uses up the budget. Characters
        // Rust calls neither letter nor digit that FTS5 reads as one
        // (private use) only make the word require less.
        let raw = raw.trim_start_matches(|c: char| !c.is_alphanumeric());
        if raw.is_empty() {
            continue;
        }
        // Never folds much more than the budget can keep, even of a huge
        // word: four times the budget of its start. Folding leaves a
        // character at least a quarter of its bytes, except an accent on
        // its own, which folds to nothing; a word cut short by that only
        // requires less, so it still finds at least what the whole would.
        let folded = search_key(cut_to(raw, budget.saturating_mul(4)));
        // Folding can remove a leading mark and uncover punctuation.
        let word = folded.trim_start_matches(|c: char| !c.is_alphanumeric());
        // A word without a letter or digit (`**`, `-`) has no token to match.
        // FTS5 may read a lone symbol Rust calls alphanumeric (ⓐ) as no
        // token at all and ignore that word; harmless.
        if word.is_empty() {
            continue;
        }
        let word = cut_to(word, budget);
        if word.is_empty() {
            // Not even its first character fits: the budget is spent.
            break;
        }
        budget -= word.len();
        words.push(word.to_owned());
    }
    let words = without_implied(&mut words);
    if words.is_empty() {
        None
    } else {
        Some(
            words
                .iter()
                .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

/// How much of the query is searched, in UTF-8 bytes of its folded words
/// from their first letter or digit on: far more than any search needs
/// (about 150 words). Separators and padding do not count, so this is no
/// limit on what may be typed; cutting the entry at this many bytes would
/// hide words behind padding. FTS5 reads the matches of every token of a
/// phrase, so its work grows with the number of tokens in the query times
/// the cards that have them: one pasted word of 20,000 tokens joined by
/// hyphens (`card-card-…`) took 10 s of CPU on 5,000 cards in a debug
/// build (ADR 0050).
pub const MAX_QUERY_BYTES: usize = 1024;

/// The start of `text`, at most `max` bytes, cut between two characters.
/// A word cut in two is searched as the start of a word, and a word left
/// out requires nothing, so a cut query finds at least what the whole one
/// would.
fn cut_to(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `words` without each word that starts another one (or equals it), in
/// sorted order. Sorted, every word that starts with `w` comes right after
/// `w`, so comparing each word with the next one is enough.
///
/// Leaving such a word out never changes what is found. FTS5's tokenizer
/// reads a quoted string one character at a time, so the tokens of a
/// string's start are the string's own tokens, except that the last one
/// may be cut short: the phrase of a longer word holds the shorter one's
/// tokens in order, its last one at least as long, and a prefix query of
/// the shorter phrase matches wherever the longer one does.
fn without_implied(words: &mut [String]) -> Vec<&String> {
    words.sort_unstable();
    let mut kept = Vec::with_capacity(words.len());
    for (i, w) in words.iter().enumerate() {
        if words
            .get(i + 1)
            .is_none_or(|next| !next.starts_with(w.as_str()))
        {
            kept.push(w);
        }
    }
    kept
}

/// Which cards of the All Cards page to keep (SPEC §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    /// Never reviewed (or reset), not suspended.
    New,
    /// In the 10-minute step, not suspended.
    Learning,
    /// In review and due now, not suspended.
    Due,
    /// Suspended.
    Suspended,
}

/// What [`Store::browse`] looks for. An empty query lists every card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowseQuery {
    /// Words typed by the user; every word must start a word of the card.
    pub text: String,
    /// Only this deck.
    pub deck_id: Option<String>,
    /// Only cards with this tag.
    pub tag_id: Option<String>,
    /// Only cards in this state.
    pub state: Option<StateFilter>,
}

/// Where a card stands, for its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardStatus {
    /// Never reviewed (or reset).
    New,
    /// In the 10-minute step, due at this time (UTC milliseconds).
    Learning(i64),
    /// In review and due now since this time.
    Due(i64),
    /// In review, next due at this time.
    Scheduled(i64),
    /// Suspended.
    Suspended,
}

/// One row of the All Cards page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseRow {
    /// The card.
    pub card: Card,
    /// The name of its deck.
    pub deck_name: String,
    /// Its tags, by name key.
    pub tags: Vec<Tag>,
    /// Where it stands at the time of the query.
    pub status: CardStatus,
    /// No surviving review since it was created or last reset, so a reset
    /// has nothing to do. Also for a suspended card, whose `status` hides it.
    pub is_new: bool,
}

const ROW_SELECT: &str = "c.id, c.deck_id, c.front_md, c.back_md, c.created_at, c.updated_at, \
     c.suspended, d.name, s.phase, s.due_at";

impl Store {
    /// The live cards in live decks that match `query`, newest first, with
    /// their deck names, tags and status at `now` (UTC milliseconds), read
    /// in one transaction.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn browse(&self, query: &BrowseQuery, now: i64) -> Result<Vec<BrowseRow>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut sql = format!(
            "SELECT {ROW_SELECT} FROM cards c
             JOIN decks d ON d.id = c.deck_id AND d.deleted_at IS NULL
             LEFT JOIN card_schedule s ON s.card_id = c.id
             WHERE c.deleted_at IS NULL"
        );
        let mut args: Vec<Value> = Vec::new();
        if !query.text.trim().is_empty() {
            let Some(m) = fts_query(&query.text) else {
                // Typed, but nothing in it can be found: no card matches.
                tx.finish()?;
                return Ok(Vec::new());
            };
            sql.push_str(" AND c.seq IN (SELECT rowid FROM cards_fts WHERE cards_fts MATCH ?)");
            args.push(Value::Text(m));
        }
        if let Some(deck) = &query.deck_id {
            sql.push_str(" AND c.deck_id = ?");
            args.push(Value::Text(deck.clone()));
        }
        if let Some(tag) = &query.tag_id {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM card_tags ct WHERE ct.card_id = c.id AND ct.tag_id = ?)",
            );
            args.push(Value::Text(tag.clone()));
        }
        // Phases are our own constant strings, never user input.
        match query.state {
            None => {}
            Some(StateFilter::New) => {
                sql.push_str(" AND c.suspended = 0 AND s.card_id IS NULL");
            }
            Some(StateFilter::Learning) => {
                let clause = format!(
                    " AND c.suspended = 0 AND s.phase IN ('{}', '{}')",
                    Phase::Learning.as_str(),
                    Phase::Relearning.as_str()
                );
                sql.push_str(&clause);
            }
            Some(StateFilter::Due) => {
                let clause = format!(
                    " AND c.suspended = 0 AND s.phase = '{}' AND s.due_at <= ?",
                    Phase::Review.as_str()
                );
                sql.push_str(&clause);
                args.push(Value::Integer(now));
            }
            Some(StateFilter::Suspended) => sql.push_str(" AND c.suspended = 1"),
        }
        sql.push_str(" ORDER BY c.seq DESC");
        let mut rows = {
            let mut stmt = tx.prepare(&sql)?;
            let found = stmt.query_map(params_from_iter(args), |r| row_from(r, now))?;
            found.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut tags = tags_of_live_cards(&tx)?;
        for row in &mut rows {
            row.tags = tags.remove(&row.card.id).unwrap_or_default();
        }
        tx.finish()?;
        Ok(rows)
    }

    /// The row of card `card_id` as [`Self::browse`] shows it, or `None` if
    /// the card or its deck is deleted.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn browse_row(&self, card_id: &str, now: i64) -> Result<Option<BrowseRow>> {
        let tx = self.conn.unchecked_transaction()?;
        let row = tx
            .query_row(
                &format!(
                    "SELECT {ROW_SELECT} FROM cards c
                     JOIN decks d ON d.id = c.deck_id AND d.deleted_at IS NULL
                     LEFT JOIN card_schedule s ON s.card_id = c.id
                     WHERE c.id = ?1 AND c.deleted_at IS NULL"
                ),
                [card_id],
                |r| row_from(r, now),
            )
            .optional()?;
        let row = match row {
            Some(mut row) => {
                row.tags = super::tags::card_tags_in(&tx, card_id)?;
                Some(row)
            }
            None => None,
        };
        tx.finish()?;
        Ok(row)
    }
}

/// A [`BrowseRow`] (without tags) from a row selected with `ROW_SELECT`.
fn row_from(r: &rusqlite::Row<'_>, now: i64) -> rusqlite::Result<BrowseRow> {
    let card = card_from_row(r)?;
    let deck_name: String = r.get(7)?;
    let phase: Option<String> = r.get(8)?;
    let due_at: Option<i64> = r.get(9)?;
    let is_new = phase.is_none();
    let status = if card.suspended {
        CardStatus::Suspended
    } else {
        match (phase.as_deref(), due_at) {
            (Some(p), Some(due)) if p == Phase::Review.as_str() => {
                if due <= now {
                    CardStatus::Due(due)
                } else {
                    CardStatus::Scheduled(due)
                }
            }
            (Some(_), Some(due)) => CardStatus::Learning(due),
            _ => CardStatus::New,
        }
    };
    Ok(BrowseRow {
        card,
        deck_name,
        tags: Vec::new(),
        status,
        is_new,
    })
}

/// Every tag link of live cards, by card id, each list ordered by name key.
fn tags_of_live_cards(conn: &Connection) -> Result<HashMap<String, Vec<Tag>>> {
    let mut stmt = conn.prepare(
        "SELECT ct.card_id, t.id, t.name
         FROM card_tags ct
         JOIN tags t ON t.id = ct.tag_id
         JOIN cards c ON c.id = ct.card_id
         WHERE c.deleted_at IS NULL
         ORDER BY t.name_key, t.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Tag {
                id: r.get(1)?,
                name: r.get(2)?,
            },
        ))
    })?;
    let mut tags: HashMap<String, Vec<Tag>> = HashMap::new();
    for row in rows {
        let (card_id, tag) = row?;
        tags.entry(card_id).or_default().push(tag);
    }
    Ok(tags)
}

#[cfg(test)]
mod tests;
