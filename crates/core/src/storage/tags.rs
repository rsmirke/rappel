//! Tags on cards: flat, unique by name key, never deleted (ADR 0029).

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use std::collections::{HashMap, HashSet};

use super::cards::{CARD_COLUMNS, card_from_row, live_card};
use super::decks::ensure_live_deck;
use super::{Store, observe_max_id};
use crate::domain::{Tag, TaggedCard, check_tag_name, name_key};
use crate::error::{Error, Result};
use crate::id::IdGen;
use crate::time::now_ms;

impl Store {
    /// Every tag, unused ones included, ordered by name key.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM tags ORDER BY name_key, id")?;
        let rows = stmt.query_map([], tag_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The tags at least one live card has (not deleted, in a deck that is
    /// not deleted; suspended cards count), ordered by name key: what the
    /// tag suggestions and the tag filter offer. Tags are never deleted
    /// (ADR 0029), so one no card uses any more, such as a typo taken off
    /// again, stays in [`Self::tags`] but not here (ADR 0052).
    ///
    /// # Errors
    /// SQLite errors.
    pub fn used_tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.name FROM tags t
             WHERE EXISTS (
                 SELECT 1 FROM card_tags ct
                 JOIN cards c ON c.id = ct.card_id
                 JOIN decks d ON d.id = c.deck_id
                 WHERE ct.tag_id = t.id AND c.deleted_at IS NULL AND d.deleted_at IS NULL
             )
             ORDER BY t.name_key, t.id",
        )?;
        let rows = stmt.query_map([], tag_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The tags of live card `card_id`, ordered by name key.
    ///
    /// # Errors
    /// [`Error::CardNotFound`] for a missing or deleted card, SQLite errors.
    pub fn card_tags(&self, card_id: &str) -> Result<Vec<Tag>> {
        live_card(&self.conn, card_id)?;
        card_tags_in(&self.conn, card_id)
    }

    /// Puts the tag named `name` (trimmed) on live card `card_id`, and
    /// returns it. The tag whose name key is `name`'s is used, with its own
    /// spelling, if there is one; else a new tag is created. A tag the
    /// card already has is left as it is. The card itself (and its
    /// `updated_at`) is not changed.
    ///
    /// # Errors
    /// The errors of [`check_tag_name`](crate::domain::check_tag_name),
    /// [`Error::CardNotFound`] for a missing or deleted card, id and SQLite
    /// errors. On error nothing is written.
    pub fn add_tag(&mut self, card_id: &str, name: &str) -> Result<Tag> {
        self.add_tag_at(card_id, name, now_ms())
    }

    /// [`Self::add_tag`] at time `now` (UTC milliseconds, for a new id).
    pub(super) fn add_tag_at(&mut self, card_id: &str, name: &str, now: i64) -> Result<Tag> {
        let name = check_tag_name(name)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        live_card(&tx, card_id)?;
        let tag = find_or_create_tag(&tx, &mut self.ids, name, now)?;
        tx.execute(
            "INSERT INTO card_tags (card_id, tag_id) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
            params![card_id, tag.id],
        )?;
        tx.commit()?;
        Ok(tag)
    }

    /// Every live card of deck `deck_id` in creation order, each with its
    /// tags, read together (two queries in one read transaction, however
    /// many cards there are).
    ///
    /// # Errors
    /// [`Error::DeckNotFound`], SQLite errors.
    pub fn list_tagged_cards(&self, deck_id: &str) -> Result<Vec<TaggedCard>> {
        let tx = self.conn.unchecked_transaction()?;
        ensure_live_deck(&tx, deck_id)?;
        let cards = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {CARD_COLUMNS} FROM cards c
                 WHERE c.deck_id = ?1 AND c.deleted_at IS NULL ORDER BY c.seq"
            ))?;
            let rows = stmt.query_map([deck_id], card_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut tags: HashMap<String, Vec<Tag>> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT ct.card_id, t.id, t.name
                 FROM card_tags ct
                 JOIN tags t ON t.id = ct.tag_id
                 JOIN cards c ON c.id = ct.card_id
                 WHERE c.deck_id = ?1 AND c.deleted_at IS NULL
                 ORDER BY t.name_key, t.id",
            )?;
            let rows = stmt.query_map([deck_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Tag {
                        id: r.get(1)?,
                        name: r.get(2)?,
                    },
                ))
            })?;
            for row in rows {
                let (card_id, tag) = row?;
                tags.entry(card_id).or_default().push(tag);
            }
        }
        Ok(cards
            .into_iter()
            .map(|card| TaggedCard {
                tags: tags.remove(&card.id).unwrap_or_default(),
                card,
            })
            .collect())
    }

    /// Takes tag `tag_id` off live card `card_id`. Returns whether the card
    /// had it. The tag itself stays, even if no card has it any more.
    ///
    /// # Errors
    /// [`Error::CardNotFound`] for a missing or deleted card,
    /// [`Error::TagNotFound`], SQLite errors.
    pub fn remove_tag(&mut self, card_id: &str, tag_id: &str) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        live_card(&tx, card_id)?;
        let exists = tx
            .query_row("SELECT 1 FROM tags WHERE id = ?1", [tag_id], |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            return Err(Error::TagNotFound(tag_id.to_owned()));
        }
        let removed = tx.execute(
            "DELETE FROM card_tags WHERE card_id = ?1 AND tag_id = ?2",
            params![card_id, tag_id],
        )?;
        tx.commit()?;
        Ok(removed > 0)
    }
}

/// The tag named `name` (already checked and trimmed): the one with its name
/// key if there is one, else a new tag.
pub(crate) fn find_or_create_tag(
    conn: &Connection,
    ids: &mut IdGen,
    name: &str,
    now: i64,
) -> Result<Tag> {
    let key = name_key(name);
    if let Some(tag) = tag_by_key(conn, &key)? {
        return Ok(tag);
    }
    observe_max_id(conn, ids, "tags.id")?;
    let tag = Tag {
        id: ids.next(now)?,
        name: name.to_owned(),
    };
    conn.execute(
        "INSERT INTO tags (id, name, name_key) VALUES (?1, ?2, ?3)",
        params![tag.id, tag.name, key],
    )?;
    Ok(tag)
}

/// Makes the tags of live card `card_id` exactly `names`, inside the caller's
/// transaction: tags are found or created by name key, links not wanted any
/// more are removed (the tags themselves stay, ADR 0029). Returns the card's
/// tags, ordered by name key. Every name is checked before anything is
/// written.
pub(super) fn set_card_tags_in(
    conn: &Connection,
    ids: &mut IdGen,
    card_id: &str,
    names: &[String],
    now: i64,
) -> Result<Vec<Tag>> {
    let names = names
        .iter()
        .map(|name| check_tag_name(name))
        .collect::<Result<Vec<_>>>()?;
    // Sets, so a card with thousands of tags costs no more than linear
    // time (5,000 names took 0.6 s with lists; ADR 0050). `wanted` keeps
    // the order typed, so the inserts are the same on every run.
    let mut wanted: Vec<String> = Vec::with_capacity(names.len());
    let mut wanted_set: HashSet<String> = HashSet::with_capacity(names.len());
    for name in names {
        let id = find_or_create_tag(conn, ids, name, now)?.id;
        // The same tag named twice ("x", "X") is one link.
        if wanted_set.insert(id.clone()) {
            wanted.push(id);
        }
    }
    let mut stmt = conn.prepare("SELECT tag_id FROM card_tags WHERE card_id = ?1")?;
    let linked = stmt
        .query_map([card_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    let mut unlink = conn.prepare("DELETE FROM card_tags WHERE card_id = ?1 AND tag_id = ?2")?;
    for tag_id in linked.iter().filter(|id| !wanted_set.contains(*id)) {
        unlink.execute(params![card_id, tag_id])?;
    }
    let mut link = conn.prepare("INSERT INTO card_tags (card_id, tag_id) VALUES (?1, ?2)")?;
    for tag_id in wanted.iter().filter(|id| !linked.contains(*id)) {
        link.execute(params![card_id, tag_id])?;
    }
    card_tags_in(conn, card_id)
}

/// The tags of `card_id`, ordered by name key.
pub(super) fn card_tags_in(conn: &Connection, card_id: &str) -> Result<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name FROM card_tags ct JOIN tags t ON t.id = ct.tag_id
         WHERE ct.card_id = ?1 ORDER BY t.name_key, t.id",
    )?;
    let rows = stmt.query_map([card_id], tag_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The tag whose name key is `key`.
fn tag_by_key(conn: &Connection, key: &str) -> Result<Option<Tag>> {
    Ok(conn
        .query_row(
            "SELECT id, name FROM tags WHERE name_key = ?1",
            [key],
            tag_from_row,
        )
        .optional()?)
}

fn tag_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: r.get(0)?,
        name: r.get(1)?,
    })
}

#[cfg(test)]
mod tests;
