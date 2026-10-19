//! Writes on many cards at once, for the All Cards page (SPEC §4.6, ADR
//! 0035).
//!
//! Each is one transaction and all or nothing: if any id is not a live card,
//! nothing is written. Each returns exactly what it changed, so the Undo of
//! the caller reverses only that and leaves alone what was already as asked.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::Store;
use super::decks::ensure_live_deck;
use super::review::{reset_in, undo_in};
use super::settings::scheduler_in;
use super::tags::find_or_create_tag;
use crate::domain::{ReviewEvent, Tag, check_tag_name, name_key};
use crate::error::{Error, Result};
use crate::time::now_ms;

/// A card [`Store::move_cards`] moved, and the deck it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The card.
    pub card_id: String,
    /// The deck it was in before.
    pub from: String,
    /// The deck it was moved to; an Undo only moves it back from here.
    pub to: String,
}

/// What [`Store::add_tags_to_cards`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TagsAdded {
    /// The tags named, found or created, one per name key, in the order
    /// named. Empty when nothing was written.
    pub tags: Vec<Tag>,
    /// Each new link, as (card id, tag id), for the Undo
    /// ([`Store::release_tags`]).
    pub links: Vec<(String, String)>,
    /// The cards that got at least one tag, in the order given.
    pub cards: Vec<String>,
    /// The cards left out because the tags they lack would take them past
    /// the limit, in the order given.
    pub full: Vec<String>,
}

impl Store {
    /// Suspends (or, with `false`, unsuspends) the cards `ids`. Returns the
    /// ids whose flag changed. Content, `updated_at` and the log are left
    /// alone (ADR 0035).
    ///
    /// # Errors
    /// [`Error::CardNotFound`] if an id is not a live card; SQLite errors.
    pub fn set_suspended(&mut self, ids: &[String], suspended: bool) -> Result<Vec<String>> {
        let ids = unique(ids);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_live_cards(&tx, &ids)?;
        let mut changed = Vec::new();
        let mut update =
            tx.prepare("UPDATE cards SET suspended = ?2 WHERE id = ?1 AND suspended != ?2")?;
        for id in &ids {
            if update.execute(params![id, suspended])? == 1 {
                changed.push(id.clone());
            }
        }
        drop(update);
        tx.commit()?;
        Ok(changed)
    }

    /// Undoes [`Self::set_suspended`]: sets the flag of the cards `ids` that
    /// are still live; the others are left out, not an error. Returns how
    /// many changed.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn restore_suspended(&mut self, ids: &[String], suspended: bool) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0;
        for id in ids {
            changed += tx.execute(
                "UPDATE cards SET suspended = ?2
                 WHERE id = ?1 AND deleted_at IS NULL AND suspended != ?2",
                params![id, suspended],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Moves the cards `ids` to live deck `deck_id`. Returns the cards that
    /// moved with the deck each came from; a card already there stays out of
    /// the list. `updated_at` is not touched, as a move does not change
    /// what a card says.
    ///
    /// # Errors
    /// [`Error::DeckNotFound`], [`Error::CardNotFound`] if an id is not a
    /// live card; SQLite errors.
    pub fn move_cards(&mut self, ids: &[String], deck_id: &str) -> Result<Vec<Moved>> {
        let ids = unique(ids);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_live_deck(&tx, deck_id)?;
        ensure_live_cards(&tx, &ids)?;
        let mut moved = Vec::new();
        for id in &ids {
            let from: String =
                tx.query_row("SELECT deck_id FROM cards WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            if from != deck_id {
                tx.execute(
                    "UPDATE cards SET deck_id = ?2 WHERE id = ?1",
                    params![id, deck_id],
                )?;
                moved.push(Moved {
                    card_id: id.clone(),
                    from,
                    to: deck_id.to_owned(),
                });
            }
        }
        tx.commit()?;
        Ok(moved)
    }

    /// Undoes [`Self::move_cards`]: each card goes back to the deck it came
    /// from. A card deleted since, moved again since, or whose old deck was
    /// deleted, stays where it is. Returns how many went back.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn unmove_cards(&mut self, moved: &[Moved]) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut back = 0;
        for m in moved {
            back += tx.execute(
                "UPDATE cards SET deck_id = ?2
                 WHERE id = ?1 AND deleted_at IS NULL AND deck_id = ?3
                   AND EXISTS (SELECT 1 FROM decks WHERE id = ?2 AND deleted_at IS NULL)",
                params![m.card_id, m.from, m.to],
            )?;
        }
        tx.commit()?;
        Ok(back)
    }

    /// Gives the cards `ids` the tags named `names` (each found by name key
    /// or created; names with the same key are one tag), in one
    /// transaction (ADR 0051). A card gets the tags it lacks only if that
    /// leaves it with at most `max_per_card` tags; else it gets none of
    /// them and is listed in [`TagsAdded::full`]. A card already over the
    /// limit (from an import) thus gets no more, as in the card editor's
    /// tags field (ADR 0050, row 30). When no card gets a tag, nothing is
    /// written, not even a new tag. `updated_at` stays (ADR 0029).
    ///
    /// # Errors
    /// [`Error::CardNotFound`] if an id is not a live card; the errors of
    /// [`check_tag_name`] for any name; SQLite errors. On error nothing is
    /// written.
    pub fn add_tags_to_cards(
        &mut self,
        ids: &[String],
        names: &[String],
        max_per_card: usize,
    ) -> Result<TagsAdded> {
        self.add_tags_to_cards_at(ids, names, max_per_card, now_ms())
    }

    /// [`Self::add_tags_to_cards`] at time `now` (UTC milliseconds).
    pub(super) fn add_tags_to_cards_at(
        &mut self,
        ids: &[String],
        names: &[String],
        max_per_card: usize,
        now: i64,
    ) -> Result<TagsAdded> {
        // Every name is checked, and repeats merged, before any write.
        let mut keys = HashSet::new();
        let mut checked = Vec::new();
        for name in names {
            let name = check_tag_name(name)?;
            if keys.insert(name_key(name)) {
                checked.push(name);
            }
        }
        let ids = unique(ids);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_live_cards(&tx, &ids)?;
        let mut tags = Vec::with_capacity(checked.len());
        for name in checked {
            tags.push(find_or_create_tag(&tx, &mut self.ids, name, now)?);
        }
        let mut added = TagsAdded::default();
        let mut has = tx.prepare("SELECT tag_id FROM card_tags WHERE card_id = ?1")?;
        let mut link = tx.prepare("INSERT INTO card_tags (card_id, tag_id) VALUES (?1, ?2)")?;
        for id in &ids {
            let own: HashSet<String> = has
                .query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let lacking: Vec<&Tag> = tags.iter().filter(|t| !own.contains(&t.id)).collect();
            if lacking.is_empty() {
                continue;
            }
            if own.len().saturating_add(lacking.len()) > max_per_card {
                added.full.push(id.clone());
                continue;
            }
            for tag in lacking {
                link.execute(params![id, tag.id])?;
                added.links.push((id.clone(), tag.id.clone()));
            }
            added.cards.push(id.clone());
        }
        drop((has, link));
        if added.links.is_empty() {
            // Dropping the transaction rolls it back: no new tag is left
            // that no card has.
            return Ok(added);
        }
        tx.commit()?;
        added.tags = tags;
        Ok(added)
    }

    /// Takes tag `tag_id` off the cards `ids`. Returns the ids that had it.
    /// The tag stays (ADR 0029).
    ///
    /// # Errors
    /// [`Error::TagNotFound`], [`Error::CardNotFound`] if an id is not a
    /// live card; SQLite errors.
    pub fn remove_tag_from_cards(&mut self, ids: &[String], tag_id: &str) -> Result<Vec<String>> {
        let ids = unique(ids);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_live_cards(&tx, &ids)?;
        if tx
            .query_row("SELECT 1 FROM tags WHERE id = ?1", [tag_id], |_| Ok(()))
            .optional()?
            .is_none()
        {
            return Err(Error::TagNotFound(tag_id.to_owned()));
        }
        let mut changed = Vec::new();
        let mut unlink = tx.prepare("DELETE FROM card_tags WHERE card_id = ?1 AND tag_id = ?2")?;
        for id in &ids {
            if unlink.execute(params![id, tag_id])? == 1 {
                changed.push(id.clone());
            }
        }
        drop(unlink);
        tx.commit()?;
        Ok(changed)
    }

    /// Undoes [`Self::add_tags_to_cards`]: removes the `links` (card id,
    /// tag id) whose card is still live; the others are left out, not an
    /// error. Returns how many cards lost at least one tag. The tags stay
    /// (ADR 0029).
    ///
    /// # Errors
    /// SQLite errors.
    pub fn release_tags(&mut self, links: &[(String, String)]) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut cards = HashSet::new();
        let mut unlink = tx.prepare(
            "DELETE FROM card_tags WHERE card_id = ?1 AND tag_id = ?2
               AND EXISTS (SELECT 1 FROM cards WHERE id = ?1 AND deleted_at IS NULL)",
        )?;
        for (card_id, tag_id) in links {
            if unlink.execute(params![card_id, tag_id])? == 1 {
                cards.insert(card_id.as_str());
            }
        }
        drop(unlink);
        tx.commit()?;
        Ok(cards.len())
    }

    /// Re-links `tag_id` to `card_ids` that are still live, for the Undo of
    /// [`Self::remove_tag_from_cards`]. Returns how many were linked.
    ///
    /// # Errors
    /// [`Error::TagNotFound`], SQLite errors.
    pub fn restore_tag_on_cards(&mut self, card_ids: &[String], tag_id: &str) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx
            .query_row("SELECT 1 FROM tags WHERE id = ?1", [tag_id], |_| Ok(()))
            .optional()?
            .is_none()
        {
            return Err(Error::TagNotFound(tag_id.to_owned()));
        }
        let mut linked = 0;
        for id in card_ids {
            linked += tx.execute(
                "INSERT INTO card_tags (card_id, tag_id)
                 SELECT id, ?2 FROM cards WHERE id = ?1 AND deleted_at IS NULL
                 ON CONFLICT DO NOTHING",
                params![id, tag_id],
            )?;
        }
        tx.commit()?;
        Ok(linked)
    }

    /// Resets the cards `ids` at `now` (see [`Self::reset_card`]). A card
    /// that is already new is left alone, with nothing written for it.
    /// Returns the entries written, one per card that was reset (empty if
    /// every card was new), for [`Self::undo_entries`].
    ///
    /// # Errors
    /// [`Error::CardNotFound`] if an id is not a live card; id and SQLite
    /// errors.
    pub fn reset_cards(&mut self, ids: &[String], now: i64) -> Result<Vec<ReviewEvent>> {
        let ids = unique(ids);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_live_cards(&tx, &ids)?;
        let mut events = Vec::with_capacity(ids.len());
        for id in &ids {
            match reset_in(&tx, &mut self.ids, id, now) {
                Ok(event) => events.push(event),
                Err(Error::CardAlreadyNew(_)) => {}
                Err(err) => return Err(err),
            }
        }
        tx.commit()?;
        Ok(events)
    }

    /// Cancels `entries` (see [`Self::undo`]) at `now`, each on its own
    /// card. An entry that is no longer its card's latest surviving one (the
    /// card was studied since) or whose card was deleted is skipped and its
    /// card id returned, the rest are cancelled (ADR 0030).
    ///
    /// # Errors
    /// Id, scheduler and SQLite errors.
    pub fn undo_entries(&mut self, entries: &[ReviewEvent], now: i64) -> Result<Vec<String>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scheduler = scheduler_in(&tx)?;
        let mut skipped = Vec::new();
        for e in entries {
            match undo_in(&tx, &mut self.ids, &scheduler, &e.card_id, &e.id, now) {
                Ok(_) => {}
                Err(
                    Error::NotLatestEntry(_) | Error::CardNotFound(_) | Error::EventNotFound(_),
                ) => {
                    skipped.push(e.card_id.clone());
                }
                Err(err) => return Err(err),
            }
        }
        tx.commit()?;
        Ok(skipped)
    }

    /// Soft-deletes the cards `ids` with one shared `deleted_at`. Their
    /// history is kept.
    ///
    /// # Errors
    /// [`Error::CardNotFound`] if an id is not a live card; SQLite errors.
    pub fn delete_cards(&mut self, ids: &[String]) -> Result<()> {
        self.delete_cards_at(ids, now_ms())
    }

    /// [`Self::delete_cards`] at time `now` (UTC milliseconds).
    pub(super) fn delete_cards_at(&mut self, ids: &[String], now: i64) -> Result<()> {
        let ids = unique(ids);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_live_cards(&tx, &ids)?;
        let mut delete = tx.prepare("UPDATE cards SET deleted_at = ?2 WHERE id = ?1")?;
        for id in &ids {
            delete.execute(params![id, now])?;
        }
        drop(delete);
        tx.commit()?;
        Ok(())
    }

    /// Undoes [`Self::delete_cards`]: the cards `ids` that are deleted come
    /// back, except those whose deck is deleted (ADR 0013). Returns how many
    /// came back.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn undelete_cards(&mut self, ids: &[String]) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut back = 0;
        for id in ids {
            back += tx.execute(
                "UPDATE cards SET deleted_at = NULL
                 WHERE id = ?1 AND deleted_at IS NOT NULL
                   AND EXISTS (SELECT 1 FROM decks d WHERE d.id = cards.deck_id
                               AND d.deleted_at IS NULL)",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(back)
    }
}

/// `ids` without repeats, in order.
fn unique(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect()
}

/// Fails with [`Error::CardNotFound`] for the first id that is not a live
/// card.
fn ensure_live_cards(conn: &Connection, ids: &[String]) -> Result<()> {
    let mut stmt = conn.prepare("SELECT 1 FROM cards WHERE id = ?1 AND deleted_at IS NULL")?;
    for id in ids {
        if !stmt.exists([id])? {
            return Err(Error::CardNotFound(id.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
