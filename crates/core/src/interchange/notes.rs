//! What an import keeps of the rows it has read (ADR 0053): ids as their
//! 122-bit values, names as 64-bit digests, a few numbers per log entry.
//! The rows themselves are dropped as they are read. The lists of ids are
//! sorted once their table has been read, then looked up by binary search;
//! a value that appears twice there is a candidate duplicate, which a
//! second read of the table ([`Locate`]) confirms exactly, in row order.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use sha2::{Digest, Sha256};

use super::stream::Table;
use super::validate::{Kind, reference};

/// The bits of an id's value; the top bit of a deck note marks a deleted
/// deck, and the top bit of a target key one that is not an id at all.
const ID_BITS: u128 = (1 << 122) - 1;
const FLAG: u128 = 1 << 127;

/// A 64-bit digest of a name key, for finding names used twice. Equal keys
/// give equal digests; equal digests are only candidates, confirmed on the
/// names themselves. `DefaultHasher::new` gives the same digests every
/// time within a process, which the plan and the apply share.
pub(super) fn name_digest(key: &str) -> u64 {
    #[cfg(test)]
    if COLLIDING_NAMES.get() {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
thread_local! {
    /// Makes every name digest the same on this thread, so tests can
    /// reach what a collision of two names' digests does.
    pub(super) static COLLIDING_NAMES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The key of the entry an undo names: the id's value, or, for a string
/// that is not an id (which no entry can have, so the undo is refused
/// later anyway), a digest of the string with the top bit set, so two
/// undos naming the same string still collide.
pub(super) fn target_key(target: &str) -> u128 {
    reference(target).unwrap_or_else(|| {
        let digest: [u8; 32] = Sha256::digest(target.as_bytes()).into();
        let mut first = [0; 16];
        first.copy_from_slice(&digest[..16]);
        u128::from_le_bytes(first) | FLAG
    })
}

/// A log entry, in 32 bytes: its id, its time and kind, its card (as the
/// card's position in the sorted card ids) and its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EventNote {
    pub(super) id: u128,
    /// `reviewed_at << 3 | new << 2 | kind`; a timestamp is below 2^48.
    packed: u64,
    pub(super) card: u32,
    pub(super) index: u32,
}

impl EventNote {
    pub(super) fn new(id: u128, at: i64, kind: Kind, card: u32, index: u32) -> Self {
        let kind = match kind {
            Kind::Review => 0,
            Kind::Reset => 1,
            Kind::Undo => 2,
        };
        let at = u64::try_from(at).unwrap_or(0);
        Self {
            id,
            packed: (at << 3) | kind,
            card,
            index,
        }
    }

    pub(super) fn at(self) -> u64 {
        self.packed >> 3
    }

    pub(super) fn is_undo(self) -> bool {
        self.packed & 3 == 2
    }

    /// Whether this entry is new to the database (set on undos, which are
    /// inserted after the whole list).
    pub(super) fn is_new(self) -> bool {
        self.packed & 4 != 0
    }

    pub(super) fn mark_new(&mut self) {
        self.packed |= 4;
    }
}

/// An undo and the key of the entry it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UndoNote {
    pub(super) id: u128,
    pub(super) target: u128,
}

/// The notes of every table read so far.
#[derive(Debug, Default)]
pub(super) struct Notes {
    /// Deck ids, with [`FLAG`] on deleted decks.
    decks: Vec<u128>,
    /// Digests of the live decks' name keys.
    pub(super) deck_names: Vec<u64>,
    pub(super) cards: Vec<u128>,
    pub(super) tags: Vec<u128>,
    pub(super) tag_names: Vec<u64>,
    pub(super) events: Vec<EventNote>,
    pub(super) undos: Vec<UndoNote>,
}

impl Notes {
    pub(super) fn push_deck(&mut self, id: u128, deleted: bool) {
        self.decks.push(if deleted { id | FLAG } else { id });
    }

    /// Whether the deck is deleted, if the file has it.
    pub(super) fn deck(&self, id: u128) -> Option<bool> {
        self.decks
            .binary_search_by_key(&id, |d| d & ID_BITS)
            .ok()
            .map(|i| self.decks[i] & FLAG != 0)
    }

    /// The card's position among the file's sorted card ids.
    pub(super) fn card_position(&self, id: u128) -> Option<u32> {
        self.cards
            .binary_search(&id)
            .ok()
            .and_then(|i| u32::try_from(i).ok())
    }

    pub(super) fn has_tag(&self, id: u128) -> bool {
        self.tags.binary_search(&id).is_ok()
    }

    pub(super) fn event(&self, id: u128) -> Option<EventNote> {
        self.events
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| self.events[i])
    }

    /// Sorts the notes of `table` for lookups and returns the values that
    /// appear more than once in them.
    pub(super) fn seal(&mut self, table: Table) -> Candidates {
        let mut found = Candidates::default();
        match table {
            Table::Decks => {
                self.decks.sort_unstable_by_key(|d| d & ID_BITS);
                found.ids = repeated(self.decks.iter().map(|d| d & ID_BITS));
                self.deck_names.sort_unstable();
                found.names = repeated(self.deck_names.iter().copied());
                // Not needed past the decks.
                self.deck_names = Vec::new();
            }
            Table::Cards => {
                self.cards.sort_unstable();
                found.ids = repeated(self.cards.iter().copied());
            }
            Table::Tags => {
                self.tags.sort_unstable();
                found.ids = repeated(self.tags.iter().copied());
                self.tag_names.sort_unstable();
                found.names = repeated(self.tag_names.iter().copied());
                self.tag_names = Vec::new();
            }
            Table::CardTags => {}
            Table::Events => {
                self.events.sort_unstable_by_key(|e| e.id);
                found.ids = repeated(self.events.iter().map(|e| e.id));
                self.undos.sort_unstable_by_key(|u| u.target);
                found.targets = repeated(self.undos.iter().map(|u| u.target));
            }
        }
        found
    }
}

/// The values a sorted sequence has more than once, each once.
fn repeated<T: PartialEq + Copy>(sorted: impl Iterator<Item = T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let mut previous = None;
    for value in sorted {
        if previous == Some(value) && out.last() != Some(&value) {
            out.push(value);
        }
        previous = Some(value);
    }
    out
}

/// Values of one table that its notes have more than once: ids, name
/// digests, undo targets. Sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Candidates {
    pub(super) ids: Vec<u128>,
    pub(super) names: Vec<u64>,
    pub(super) targets: Vec<u128>,
}

impl Candidates {
    pub(super) fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.names.is_empty() && self.targets.is_empty()
    }
}

/// A second read of a table that has candidates: each row is checked
/// again in order, and a candidate seen a second time is a duplicate at
/// that row, exactly as a set of every value would find it. Only the
/// candidates are tracked, in memory fixed by their number: a flag per
/// candidate id or target, and per candidate name digest the 128-bit hash
/// of the first name seen with it ([`name_hash`]), so two names that only
/// share a 64-bit digest are told apart.
#[derive(Debug)]
pub(super) struct Locate {
    found: Candidates,
    ids_seen: Vec<bool>,
    /// By candidate digest, the hash of the first name with it (0: none).
    names_first: Vec<u128>,
    /// Hashes of other names whose digest a candidate shares: only when
    /// different names collide in 64 bits.
    names_more: HashSet<u128>,
    targets_seen: Vec<bool>,
}

/// A 128-bit hash of a name key (the first half of its SHA-256, top bit
/// set so it is never 0): equal for equal keys, and for different keys
/// equal with a chance of 2^-127.
fn name_hash(key: &str) -> u128 {
    let digest: [u8; 32] = Sha256::digest(key.as_bytes()).into();
    let mut first = [0; 16];
    first.copy_from_slice(&digest[..16]);
    u128::from_le_bytes(first) | FLAG
}

impl Locate {
    pub(super) fn new(found: Candidates) -> Self {
        Self {
            ids_seen: vec![false; found.ids.len()],
            targets_seen: vec![false; found.targets.len()],
            names_first: vec![0; found.names.len()],
            names_more: HashSet::new(),
            found,
        }
    }

    /// Whether `id` was seen in an earlier row.
    pub(super) fn repeats_id(&mut self, id: u128) -> bool {
        seen_again(&self.found.ids, &mut self.ids_seen, id)
    }

    /// Whether the name key `key` was seen in an earlier row.
    pub(super) fn repeats_name(&mut self, key: &str) -> bool {
        let Ok(i) = self.found.names.binary_search(&name_digest(key)) else {
            return false;
        };
        let hash = name_hash(key);
        match self.names_first[i] {
            0 => {
                self.names_first[i] = hash;
                false
            }
            first if first == hash => true,
            _ => !self.names_more.insert(hash),
        }
    }

    /// Whether an earlier undo named the same entry.
    pub(super) fn repeats_target(&mut self, target: u128) -> bool {
        seen_again(&self.found.targets, &mut self.targets_seen, target)
    }
}

fn seen_again(candidates: &[u128], seen: &mut [bool], value: u128) -> bool {
    match candidates.binary_search(&value) {
        Ok(i) => std::mem::replace(&mut seen[i], true),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_values_are_listed_once() {
        assert_eq!(repeated([1, 1, 1, 2, 3, 3].into_iter()), [1, 3]);
        assert_eq!(repeated([1, 2, 3].into_iter()), Vec::<i32>::new());
        assert_eq!(repeated(std::iter::empty::<u8>()), Vec::<u8>::new());
    }

    #[test]
    fn notes_are_small() {
        assert_eq!(size_of::<EventNote>(), 32);
        assert_eq!(size_of::<UndoNote>(), 32);
    }

    #[test]
    fn an_event_note_keeps_its_fields() {
        let mut note = EventNote::new(7, 253_402_300_799_999, Kind::Undo, 3, 9);
        assert_eq!(note.at(), 253_402_300_799_999);
        assert!(note.is_undo());
        assert!(!note.is_new());
        note.mark_new();
        assert!(note.is_new() && note.is_undo());
        assert!(!EventNote::new(7, 0, Kind::Reset, 0, 0).is_undo());
    }

    #[test]
    fn deck_notes_keep_the_deleted_flag_out_of_the_order() {
        let mut notes = Notes::default();
        notes.push_deck(5, true);
        notes.push_deck(3, false);
        notes.push_deck(5, false);
        let found = notes.seal(Table::Decks);
        assert_eq!(found.ids, [5]);
        assert_eq!(notes.deck(3), Some(false));
        assert!(notes.deck(5).is_some());
        assert_eq!(notes.deck(4), None);
    }

    #[test]
    fn targets_that_are_not_ids_still_compare_by_string() {
        let id = "01920000-0000-7000-8000-000000000001";
        assert_eq!(target_key(id), reference(id).unwrap());
        assert_eq!(target_key("x"), target_key("x"));
        assert_ne!(target_key("x"), target_key("y"));
        assert_ne!(target_key("X"), target_key("x"));
        assert!(target_key("x") > ID_BITS);
    }

    #[test]
    fn locate_confirms_each_candidate_once_seen_twice() {
        let mut locate = Locate::new(Candidates {
            ids: vec![2, 9],
            names: vec![name_digest("a")],
            targets: vec![],
        });
        assert!(!locate.repeats_id(1));
        assert!(!locate.repeats_id(2));
        assert!(!locate.repeats_id(1));
        assert!(locate.repeats_id(2));
        assert!(!locate.repeats_name("a"));
        assert!(!locate.repeats_name("b"));
        assert!(locate.repeats_name("a"));
        assert!(!locate.repeats_target(4));
    }
}
