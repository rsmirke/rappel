//! Domain types shared by storage and scheduling.

use std::fmt;
use std::str::FromStr;

use unicode_normalization::UnicodeNormalization;

mod settings;
mod tag;

pub use tag::{
    MAX_TAG_NAME_BYTES, Tag, TagFieldError, TaggedCard, check_tag_name, parse_tag_field, search_key,
};

pub use settings::{
    DEFAULT_DESIRED_RETENTION, DESIRED_RETENTION_RANGE, MAX_DAILY_LIMIT, MAX_DAY_START_HOUR,
    Settings,
};

/// The answer to a review. Rappel has exactly two grades (SPEC §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grade {
    /// The answer was not recalled.
    Fail,
    /// The answer was recalled.
    Pass,
}

impl Grade {
    /// The value stored in `review_events.grade`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fail => "fail",
            Self::Pass => "pass",
        }
    }
}

impl fmt::Display for Grade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A string that is not a [`Grade`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a grade: {0:?}")]
pub struct ParseGradeError(pub String);

impl FromStr for Grade {
    type Err = ParseGradeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fail" => Ok(Self::Fail),
            "pass" => Ok(Self::Pass),
            other => Err(ParseGradeError(other.to_owned())),
        }
    }
}

/// One entry of a card's log, a row of `review_events` (ADR 0028).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEvent {
    /// Entry id (`UUIDv7` string).
    pub id: String,
    /// The card's id.
    pub card_id: String,
    /// When the entry was made (for a review, when the answer was given),
    /// UTC milliseconds.
    pub reviewed_at: i64,
    /// A review, a reset or an undo.
    pub kind: EventKind,
    /// Time spent on the card, milliseconds; 0 for a reset or an undo.
    pub duration_ms: i64,
}

impl ReviewEvent {
    /// The answer, if this entry is a review.
    #[must_use]
    pub fn grade(&self) -> Option<Grade> {
        match self.kind {
            EventKind::Review(grade) => Some(grade),
            EventKind::Reset | EventKind::Undo { .. } => None,
        }
    }
}

/// What a log entry records (ADR 0028).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// The card was answered.
    Review(Grade),
    /// The card's progress was reset: it is new from here on.
    Reset,
    /// The entry with this id, a review or a reset of the same card, is
    /// cancelled. The log stays append-only; replaying it skips both.
    Undo {
        /// The id of the cancelled entry.
        undoes: String,
    },
}

impl EventKind {
    /// The value stored in `review_events.kind`.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Review(_) => "review",
            Self::Reset => "reset",
            Self::Undo { .. } => "undo",
        }
    }
}

/// Where a card that has been reviewed at least once stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Reviewed but not yet passed after the first showing: in the
    /// 10-minute step.
    Learning,
    /// Passed: scheduled in whole days by FSRS.
    Review,
    /// Failed while in review: in the 10-minute step again.
    Relearning,
}

impl Phase {
    /// The value stored in `card_schedule.phase`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Learning => "learning",
            Self::Review => "review",
            Self::Relearning => "relearning",
        }
    }
}

/// A string that is not a [`Phase`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a phase: {0:?}")]
pub struct ParsePhaseError(pub String);

impl FromStr for Phase {
    type Err = ParsePhaseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "learning" => Ok(Self::Learning),
            "review" => Ok(Self::Review),
            "relearning" => Ok(Self::Relearning),
            other => Err(ParsePhaseError(other.to_owned())),
        }
    }
}

/// The scheduling state of one card, as cached in `card_schedule`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CardState {
    /// Never reviewed (no `card_schedule` row).
    New,
    /// Reviewed at least once.
    Scheduled(Schedule),
}

/// Scheduling state of a card that has been reviewed at least once.
///
/// `stability` and `difficulty` are FSRS's `f32` values. SQLite stores them
/// as `REAL` (`f64`), which holds every `f32` exactly, so the cache and a
/// rebuild compare equal bit for bit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Schedule {
    /// Learning, review or relearning.
    pub phase: Phase,
    /// FSRS stability in days: the interval after which recall probability
    /// drops to 90 %.
    pub stability: f32,
    /// FSRS difficulty, 1 (easy) to 10 (hard).
    pub difficulty: f32,
    /// When the card is next due, UTC milliseconds.
    pub due_at: i64,
    /// When the card was last reviewed, UTC milliseconds.
    pub last_review_at: i64,
    /// Number of reviews so far.
    pub reps: u32,
    /// Number of times the card was failed while in review.
    pub lapses: u32,
}

/// A deck of cards. Deleted decks are never returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deck {
    /// Deck id (`UUIDv7` string).
    pub id: String,
    /// The name, trimmed, unique among decks that are not deleted by its
    /// [`deck_name_key`].
    pub name: String,
    /// When the deck was created, UTC milliseconds.
    pub created_at: i64,
    /// When the deck was last renamed (or created), UTC milliseconds.
    pub updated_at: i64,
}

/// The key under which deck names must be unique (ADR 0010): the trimmed
/// name in Unicode NFC, lower-cased, and in NFC again. NFC makes a composed "É" (U+00C9) and
/// "E" + combining acute (U+0301) equal; lower-casing makes "Maths" and
/// "maths" equal. Accents still count ("Économie" ≠ "Economie"), and this
/// is lower-casing, not full case folding ("STRASSE" ≠ "straße").
///
/// Migration 0004 stored keys computed with this function, so changing
/// its result for any name needs a new migration that recomputes them.
/// Tags use the same key ([`name_key`], ADR 0029).
#[must_use]
pub fn deck_name_key(name: &str) -> String {
    name_key(name)
}

/// The key under which deck and tag names must be unique; see
/// [`deck_name_key`]. Stored in `decks.name_key` and `tags.name_key`, so
/// changing its result needs a migration that recomputes both.
#[must_use]
pub fn name_key(name: &str) -> String {
    // NFC again after lower-casing: case mapping does not promise NFC
    // output (Unicode's recommended caseless matching normalises on both
    // sides).
    name.trim()
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect()
}

/// The longest deck name, in UTF-8 bytes once trimmed. Creating or
/// renaming a deck refuses a longer name, and an import refuses a file
/// with one, so every export can be imported again.
pub const MAX_DECK_NAME_BYTES: usize = 1 << 10;

/// The longest card front or back, in UTF-8 bytes (as stored, not
/// trimmed). Adding or editing a card refuses a longer side, and an import
/// refuses a file with one.
pub const MAX_CARD_SIDE_BYTES: usize = 1 << 20;

/// Whether `name`, trimmed, is within [`MAX_DECK_NAME_BYTES`].
#[must_use]
pub fn deck_name_fits(name: &str) -> bool {
    name.trim().len() <= MAX_DECK_NAME_BYTES
}

/// Whether `text` has a NUL (U+0000). GTK takes C strings, which end at a
/// NUL (glib-rs asserts there is none in debug builds), so a name or card
/// side with one is refused when written and when imported.
#[must_use]
pub(crate) fn has_nul(text: &str) -> bool {
    text.contains('\0')
}

/// Whether one card side is within [`MAX_CARD_SIDE_BYTES`].
#[must_use]
pub fn card_side_fits(text: &str) -> bool {
    text.len() <= MAX_CARD_SIDE_BYTES
}

/// One side of a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardSide {
    /// The front (question).
    Front,
    /// The back (answer).
    Back,
}

impl fmt::Display for CardSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Front => "front",
            Self::Back => "back",
        })
    }
}

/// A deck with what it has to study, counted like the deck page's
/// study session (`Store::study_input`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckSummary {
    /// The deck.
    pub deck: Deck,
    /// The new cards still allowed today (at most 20 a day per deck).
    pub new_count: u32,
    /// Due reviews and step cards, those due within 20 minutes included.
    pub due_count: u32,
    /// Live cards in the deck, suspended ones included.
    pub card_count: u32,
    /// Live cards that are suspended.
    pub suspended_count: u32,
    /// Cards that would be studied now but for the daily limits: new cards
    /// past today's allowance and due reviews past today's allowance. They
    /// come back when the next study day begins.
    pub held_back_count: u32,
    /// When the deck next offers one more reviewed card, after `now`
    /// (`Store::next_study_change`); `None` if no card will become due.
    /// The next study day, which renews the limits, is the caller's to add.
    pub next_change_at: Option<i64>,
}

/// A card. Deleted cards are never returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// Card id (`UUIDv7` string).
    pub id: String,
    /// The deck it belongs to.
    pub deck_id: String,
    /// Front (question), Markdown. Untrusted content.
    pub front_md: String,
    /// Back (answer), Markdown. Untrusted content.
    pub back_md: String,
    /// When the card was created, UTC milliseconds.
    pub created_at: i64,
    /// When the card was last edited (or created), UTC milliseconds.
    pub updated_at: i64,
    /// Excluded from study, kept otherwise (ADR 0035).
    pub suspended: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grade_round_trips_through_its_db_string() {
        for g in [Grade::Fail, Grade::Pass] {
            assert_eq!(g.as_str().parse::<Grade>(), Ok(g));
            assert_eq!(g.to_string(), g.as_str());
        }
        assert!("Pass".parse::<Grade>().is_err());
        assert!("again".parse::<Grade>().is_err());
    }

    #[test]
    fn deck_name_key_ignores_case_and_unicode_composition() {
        let composed = "\u{C9}conomie";
        let decomposed = "E\u{301}conomie";
        assert_ne!(composed, decomposed);
        assert_eq!(deck_name_key(composed), "\u{E9}conomie");
        assert_eq!(deck_name_key(decomposed), "\u{E9}conomie");
        assert_eq!(deck_name_key("  \u{E9}conomie\n"), "\u{E9}conomie");
        assert_eq!(deck_name_key("Maths"), deck_name_key("maths"));
        assert_eq!(deck_name_key("MATHS"), "maths");
        // Accents count; this is lower-casing, not case folding.
        assert_ne!(deck_name_key(composed), deck_name_key("Economie"));
        assert_ne!(deck_name_key("STRASSE"), deck_name_key("stra\u{DF}e"));
    }

    #[test]
    fn phase_round_trips_through_its_db_string() {
        for p in [Phase::Learning, Phase::Review, Phase::Relearning] {
            assert_eq!(p.as_str().parse::<Phase>(), Ok(p));
        }
        assert!("new".parse::<Phase>().is_err());
    }
}
