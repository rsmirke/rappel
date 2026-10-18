//! Cards: add, edit, list, soft delete (ADR 0008) and its undo (ADR 0013).

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use super::decks::ensure_live_deck;
use super::search::index_card_in;
use super::tags::set_card_tags_in;
use super::{Store, observe_max_id};
use crate::domain::{Card, CardSide, MAX_CARD_SIDE_BYTES, TaggedCard, card_side_fits, has_nul};
use crate::error::{Error, Result};
use crate::id::IdGen;
use crate::time::now_ms;

/// The columns [`card_from_row`] reads, for a query with `cards` as `c`.
pub(super) const CARD_COLUMNS: &str =
    "c.id, c.deck_id, c.front_md, c.back_md, c.created_at, c.updated_at, c.suspended";

/// A [`Card`] from a row selected with [`CARD_COLUMNS`].
pub(super) fn card_from_row(r: &Row<'_>) -> rusqlite::Result<Card> {
    Ok(Card {
        id: r.get(0)?,
        deck_id: r.get(1)?,
        front_md: r.get(2)?,
        back_md: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        suspended: r.get(6)?,
    })
}

impl Store {
    /// Adds a card to deck `deck_id`. The Markdown is stored as given.
    ///
    /// # Errors
    /// [`Error::EmptyCardFront`] if the front is blank,
    /// [`Error::CardSideTooLong`],
    /// [`Error::DeckNotFound`], id and SQLite errors.
    pub fn add_card(&mut self, deck_id: &str, front_md: &str, back_md: &str) -> Result<Card> {
        self.add_card_at(deck_id, front_md, back_md, now_ms())
    }

    /// [`Self::add_card`] at time `now` (UTC milliseconds).
    pub(super) fn add_card_at(
        &mut self,
        deck_id: &str,
        front_md: &str,
        back_md: &str,
        now: i64,
    ) -> Result<Card> {
        check_sides(front_md, back_md)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = insert_card(&tx, &mut self.ids, deck_id, front_md, back_md, now)?;
        recover_named_media(&tx, self.media_dir.as_deref(), front_md, back_md, now);
        tx.commit()?;
        Ok(card)
    }

    /// [`Self::add_card`] with its tags, written in one transaction: the
    /// card has exactly `tag_names` (see [`Self::edit_card_with_tags`]).
    ///
    /// # Errors
    /// Those of [`Self::add_card`] and of
    /// [`check_tag_name`](crate::domain::check_tag_name). On error nothing
    /// is written.
    pub fn add_card_with_tags(
        &mut self,
        deck_id: &str,
        front_md: &str,
        back_md: &str,
        tag_names: &[String],
    ) -> Result<TaggedCard> {
        self.add_card_with_tags_at(deck_id, front_md, back_md, tag_names, now_ms())
    }

    /// [`Self::add_card_with_tags`] at time `now` (UTC milliseconds).
    pub(super) fn add_card_with_tags_at(
        &mut self,
        deck_id: &str,
        front_md: &str,
        back_md: &str,
        tag_names: &[String],
        now: i64,
    ) -> Result<TaggedCard> {
        check_sides(front_md, back_md)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = insert_card(&tx, &mut self.ids, deck_id, front_md, back_md, now)?;
        let tags = set_card_tags_in(&tx, &mut self.ids, &card.id, tag_names, now)?;
        recover_named_media(&tx, self.media_dir.as_deref(), front_md, back_md, now);
        tx.commit()?;
        Ok(TaggedCard { card, tags })
    }

    /// [`Self::edit_card`] with its tags, in one transaction. With
    /// `tag_names`, the card ends up with exactly those tags: a tag it has
    /// that is not named is unlinked (the tag stays, ADR 0029), and one that
    /// does not exist is created. `None` leaves the tags alone. The card row
    /// (and its `updated_at`) is written only if a side changed, since tags
    /// are not part of the card's content (ADR 0029).
    ///
    /// # Errors
    /// Those of [`Self::edit_card`] and of
    /// [`check_tag_name`](crate::domain::check_tag_name). On error nothing
    /// is written.
    pub fn edit_card_with_tags(
        &mut self,
        id: &str,
        front_md: &str,
        back_md: &str,
        tag_names: Option<&[String]>,
    ) -> Result<TaggedCard> {
        self.edit_card_with_tags_at(id, front_md, back_md, tag_names, now_ms())
    }

    /// [`Self::edit_card_with_tags`] at time `now` (UTC milliseconds).
    pub(super) fn edit_card_with_tags_at(
        &mut self,
        id: &str,
        front_md: &str,
        back_md: &str,
        tag_names: Option<&[String]>,
        now: i64,
    ) -> Result<TaggedCard> {
        check_sides(front_md, back_md)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = live_card(&tx, id)?;
        if card.front_md != front_md || card.back_md != back_md {
            card = tx.query_row(
                "UPDATE cards SET front_md = ?2, back_md = ?3, updated_at = ?4
                 WHERE id = ?1
                 RETURNING id, deck_id, front_md, back_md, created_at, updated_at, suspended",
                params![id, front_md, back_md, now],
                card_from_row,
            )?;
            index_card_in(&tx, id, front_md, back_md)?;
            recover_named_media(&tx, self.media_dir.as_deref(), front_md, back_md, now);
        }
        let tags = match tag_names {
            Some(names) => set_card_tags_in(&tx, &mut self.ids, id, names, now)?,
            None => super::tags::card_tags_in(&tx, id)?,
        };
        tx.commit()?;
        Ok(TaggedCard { card, tags })
    }

    /// Replaces the front and back of card `id` and returns the card as
    /// stored.
    ///
    /// # Errors
    /// [`Error::EmptyCardFront`], [`Error::CardSideTooLong`],
    /// [`Error::CardNotFound`], SQLite errors.
    pub fn edit_card(&mut self, id: &str, front_md: &str, back_md: &str) -> Result<Card> {
        self.edit_card_at(id, front_md, back_md, now_ms())
    }

    /// [`Self::edit_card`] at time `now` (UTC milliseconds).
    pub(super) fn edit_card_at(
        &mut self,
        id: &str,
        front_md: &str,
        back_md: &str,
        now: i64,
    ) -> Result<Card> {
        check_sides(front_md, back_md)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // One statement, so the returned card is exactly what was written.
        let card = tx
            .query_row(
                // RETURNING takes no table-qualified names; same order as
                // CARD_COLUMNS.
                "UPDATE cards SET front_md = ?2, back_md = ?3, updated_at = ?4
                 WHERE id = ?1 AND deleted_at IS NULL
                 RETURNING id, deck_id, front_md, back_md, created_at, updated_at, suspended",
                params![id, front_md, back_md, now],
                card_from_row,
            )
            .optional()?
            .ok_or_else(|| Error::CardNotFound(id.to_owned()))?;
        index_card_in(&tx, id, front_md, back_md)?;
        recover_named_media(&tx, self.media_dir.as_deref(), front_md, back_md, now);
        tx.commit()?;
        Ok(card)
    }

    /// Card `id`.
    ///
    /// # Errors
    /// [`Error::CardNotFound`], SQLite errors.
    pub fn card(&self, id: &str) -> Result<Card> {
        live_card(&self.conn, id)
    }

    /// The live cards of deck `deck_id`, oldest first.
    ///
    /// # Errors
    /// [`Error::DeckNotFound`], SQLite errors.
    pub fn list_cards(&self, deck_id: &str) -> Result<Vec<Card>> {
        ensure_live_deck(&self.conn, deck_id)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {CARD_COLUMNS} FROM cards c
             WHERE c.deck_id = ?1 AND c.deleted_at IS NULL ORDER BY c.seq"
        ))?;
        let rows = stmt.query_map([deck_id], card_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Soft-deletes card `id`. Its review history is kept.
    ///
    /// # Errors
    /// [`Error::CardNotFound`], SQLite errors.
    pub fn delete_card(&mut self, id: &str) -> Result<()> {
        self.delete_card_at(id, now_ms())
    }

    /// [`Self::delete_card`] at time `now` (UTC milliseconds).
    pub(super) fn delete_card_at(&mut self, id: &str, now: i64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE cards SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            params![id, now],
        )?;
        if changed == 0 {
            return Err(Error::CardNotFound(id.to_owned()));
        }
        Ok(())
    }

    /// Undoes [`Self::delete_card`]: card `id` is live again, with its
    /// content, review history and place in creation order. A live card is
    /// returned unchanged, so a second Undo is harmless. `updated_at` is
    /// not touched, as delete did not touch it.
    ///
    /// A card never comes back into a deleted deck: that fails with
    /// [`Error::DeckNotFound`] and changes nothing (ADR 0013).
    ///
    /// # Errors
    /// [`Error::CardNotFound`] for an unknown id, [`Error::DeckNotFound`],
    /// SQLite errors.
    pub fn undelete_card(&mut self, id: &str) -> Result<Card> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (card, deleted_at): (Card, Option<i64>) = tx
            .query_row(
                &format!("SELECT {CARD_COLUMNS}, c.deleted_at FROM cards c WHERE c.id = ?1"),
                [id],
                |r| Ok((card_from_row(r)?, r.get(7)?)),
            )
            .optional()?
            .ok_or_else(|| Error::CardNotFound(id.to_owned()))?;
        if deleted_at.is_none() {
            return Ok(card);
        }
        ensure_live_deck(&tx, &card.deck_id)?;
        tx.execute("UPDATE cards SET deleted_at = NULL WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(card)
    }
}

/// Brings back from the media trash the files a card being written names
/// and the media folder lacks (ADR 0057): an editor may have held a
/// reference to a file that a clean-up took while it was open. Inside the
/// write's transaction. A failure is only logged, never refuses the write:
/// the file stays in the trash, and the next start brings it back, since
/// the card now uses it.
fn recover_named_media(
    tx: &Connection,
    media_dir: Option<&std::path::Path>,
    front_md: &str,
    back_md: &str,
    now: i64,
) {
    let Some(media_dir) = media_dir else {
        return;
    };
    match crate::media::recover_refs(tx, media_dir, &[front_md, back_md], now) {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            files = n,
            "media files a card names brought back from the trash"
        ),
        Err(err) => tracing::warn!("cannot bring a card's media back from the trash: {err}"),
    }
}

/// Inserts a card into live deck `deck_id`, inside the caller's transaction.
fn insert_card(
    conn: &Connection,
    ids: &mut IdGen,
    deck_id: &str,
    front_md: &str,
    back_md: &str,
    now: i64,
) -> Result<Card> {
    ensure_live_deck(conn, deck_id)?;
    observe_max_id(conn, ids, "cards.id")?;
    let card = Card {
        id: ids.next(now)?,
        deck_id: deck_id.to_owned(),
        front_md: front_md.to_owned(),
        back_md: back_md.to_owned(),
        created_at: now,
        updated_at: now,
        suspended: false,
    };
    conn.execute(
        "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            card.id,
            card.deck_id,
            card.front_md,
            card.back_md,
            card.created_at,
            card.updated_at
        ],
    )?;
    index_card_in(conn, &card.id, front_md, back_md)?;
    Ok(card)
}

/// Card `id` if it exists and is live, else [`Error::CardNotFound`].
pub(super) fn live_card(conn: &Connection, id: &str) -> Result<Card> {
    conn.query_row(
        &format!("SELECT {CARD_COLUMNS} FROM cards c WHERE c.id = ?1 AND c.deleted_at IS NULL"),
        [id],
        card_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::CardNotFound(id.to_owned()))
}

/// A blank front, a side too long to import again, or a NUL is refused.
fn check_sides(front_md: &str, back_md: &str) -> Result<()> {
    if front_md.trim().is_empty() {
        return Err(Error::EmptyCardFront);
    }
    for (side, text) in [(CardSide::Front, front_md), (CardSide::Back, back_md)] {
        if has_nul(text) {
            return Err(Error::ContainsNul);
        }
        if !card_side_fits(text) {
            return Err(Error::CardSideTooLong {
                side,
                limit: MAX_CARD_SIDE_BYTES,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
