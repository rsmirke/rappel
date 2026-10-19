//! What a deck row says under the deck's name, and how often the deck list
//! re-reads it. Pure, so it is unit-tested (ADR 0041).
//!
//! | The deck                                  | Shown                   | Read out                   |
//! |-------------------------------------------|-------------------------|----------------------------|
//! | due and new cards                         | 12 due · 5 new          | 12 due, 5 new              |
//! | only due cards                            | 12 due                  | 12 due                     |
//! | only new cards                            | 5 new                   | 5 new                      |
//! | no cards                                  | No cards yet            | no cards yet               |
//! | every card suspended                      | All cards suspended     | all cards suspended        |
//! | nothing now, next card in under an hour   | Next review in 25 min   | next review in 25 minutes  |
//! | … later this study day, or within 12 h    | Next review in 3 h      | next review in 3 hours     |
//! | … during the next study day               | Next review tomorrow    | next review tomorrow       |
//! | … later                                   | Next review in 4 days   | next review in 4 days      |
//! | … 60 days or more                         | Next review in 3 months | next review in 3 months    |
//! | daily limits reached, nothing before the next study day | Done for today | done for today     |
//! | nothing at all to come (cannot happen)    | Nothing to study        | nothing to study           |
//!
//! "Due" counts both reviews and cards in a learning step, as on the deck
//! page. The due count is the part shown in the accent colour.

use rappel_core::domain::DeckSummary;

use crate::i18n::{fill, gettext, ngettext};
use crate::ui::text::{counted_units, due_count, interval_text, list_separator, new_count};
use crate::ui::time::{DAY_MS, HOUR_MS, MINUTE_MS};

/// The line under a deck's name, in parts: the due count (accent colour),
/// the rest (dimmed), and the whole line as a screen reader says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowText {
    /// "12 due", when cards are due.
    pub due: Option<String>,
    /// "5 new", "Next review in 3 h", "No cards yet", …
    pub other: Option<String>,
    /// The line in words, for the row's accessible label: "12 due, 5 new".
    pub spoken: String,
}

impl RowText {
    /// Whether the "·" between the two parts shows.
    pub fn has_separator(&self) -> bool {
        self.due.is_some() && self.other.is_some()
    }

    /// The row's accessible label: "Scratch, 12 due, 5 new".
    pub fn accessible_label(&self, name: &str) -> String {
        // Translators: a deck row for screen readers: {name} is the deck's
        // name, {line} "12 due, 5 new".
        fill(
            &gettext("{name}, {line}"),
            &[("name", name), ("line", &self.spoken)],
        )
    }

    fn status(shown: String, spoken: String) -> Self {
        Self {
            due: None,
            other: Some(shown),
            spoken,
        }
    }
}

/// What deck `summary` has, at `now`, when the next study day begins at
/// `next_day` (UTC milliseconds).
pub fn row_text(summary: &DeckSummary, now: i64, next_day: i64) -> RowText {
    let due = summary.due_count;
    let new = summary.new_count;
    if due > 0 || new > 0 {
        let due_text = (due > 0).then(|| due_count(widen(due)));
        let new_text = (new > 0).then(|| new_count(widen(new)));
        let spoken = [due_text.as_deref(), new_text.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(&list_separator());
        return RowText {
            due: due_text,
            other: new_text,
            spoken,
        };
    }
    if summary.card_count == 0 {
        return RowText::status(gettext("No cards yet"), gettext("no cards yet"));
    }
    if summary.suspended_count >= summary.card_count {
        return RowText::status(
            gettext("All cards suspended"),
            gettext("all cards suspended"),
        );
    }
    let held_back = summary.held_back_count > 0;
    match summary.next_change_at {
        // The limits hold cards back until the next study day, and nothing
        // comes before it: say so rather than name a later review.
        Some(at) if held_back && at >= next_day => done_for_today(),
        Some(at) => {
            let (shown, spoken) = when(at, now, next_day);
            RowText {
                due: None,
                // Translators: under a deck's name; {when} is "in 25 min",
                // "tomorrow" or "in 4 days".
                other: Some(fill(&gettext("Next review {when}"), &[("when", &shown)])),
                // Translators: the same for screen readers; {when} is "in 25
                // minutes", "tomorrow" or "in 4 days".
                spoken: fill(&gettext("next review {when}"), &[("when", &spoken)]),
            }
        }
        None if held_back => done_for_today(),
        // Every card that is not suspended is new (counted or held back),
        // due now (counted or held back) or due later; kept for safety.
        None => RowText::status(gettext("Nothing to study"), gettext("nothing to study")),
    }
}

fn done_for_today() -> RowText {
    RowText::status(gettext("Done for today"), gettext("done for today"))
}

fn widen(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// When `at` is, from `now`: shown ("in 25 min") and spoken ("in 25
/// minutes"). Minutes under an hour; hours until the next study day begins
/// or within 12 hours; "tomorrow" during the next study day; then days,
/// counted in study days so "tomorrow" is never followed by "in 1 day".
fn when(at: i64, now: i64, next_day: i64) -> (String, String) {
    let delta = at.saturating_sub(now).max(0);
    let minutes = ((delta + MINUTE_MS / 2) / MINUTE_MS).max(1);
    // Translators: when something comes; {interval} is "25 min", "3 days".
    let within = |interval: String| fill(&gettext("in {interval}"), &[("interval", &interval)]);
    if minutes < 60 {
        return (
            // Translators: an interval in minutes ("10 min").
            within(counted_units(
                |n| ngettext("{count} min", "{count} min", n),
                minutes,
            )),
            within(counted_units(
                // Translators: an interval in minutes, in words for screen
                // readers ("25 minutes").
                |n| ngettext("{count} minute", "{count} minutes", n),
                minutes,
            )),
        );
    }
    if at < next_day || delta < 12 * HOUR_MS {
        let hours = ((delta + HOUR_MS / 2) / HOUR_MS).clamp(1, 23);
        return (
            // Translators: an interval in hours ("5 h").
            within(counted_units(
                |n| ngettext("{count} h", "{count} h", n),
                hours,
            )),
            within(counted_units(
                // Translators: an interval in hours, in words for screen
                // readers ("3 hours").
                |n| ngettext("{count} hour", "{count} hours", n),
                hours,
            )),
        );
    }
    let days = at.saturating_sub(next_day) / DAY_MS + 1;
    if days <= 1 {
        (gettext("tomorrow"), gettext("tomorrow"))
    } else if days < 60 {
        // Translators: an interval in days ("3 days").
        let text = within(counted_units(
            |n| ngettext("{count} day", "{count} days", n),
            days,
        ));
        (text.clone(), text)
    } else {
        // "in 3 months", "in 1.5 years": the same words either way.
        let text = within(interval_text(delta));
        (text.clone(), text)
    }
}

/// The longest wait between two re-reads of the deck list, by how soon the
/// next change is: a minute while some row counts minutes, ten minutes
/// while one counts hours, an hour otherwise (as the deck page, so a
/// changed clock or time zone is noticed within an hour).
const STEPS_MS: [(i64, i64); 2] = [(HOUR_MS, MINUTE_MS), (DAY_MS, 10 * MINUTE_MS)];
const MAX_WAIT_MS: i64 = HOUR_MS;
/// Never sooner than this, so a time at the edge cannot spin the timer.
const MIN_WAIT_MS: i64 = 1_000;

/// How long the deck list waits from `now` before reading the decks again:
/// until the earliest `next_change_at` of `summaries` or the next study day
/// (`next_day`), whichever is first, and no longer than its text stays
/// right ([`STEPS_MS`]). All times are UTC milliseconds.
pub fn refresh_delay_ms(summaries: &[DeckSummary], now: i64, next_day: i64) -> u64 {
    let soonest = summaries.iter().filter_map(|s| s.next_change_at).min();
    let step = soonest.map_or(MAX_WAIT_MS, |at| {
        let delta = at.saturating_sub(now);
        STEPS_MS
            .iter()
            .find(|(within, _)| delta < *within)
            .map_or(MAX_WAIT_MS, |(_, step)| *step)
    });
    let until = soonest.map_or(next_day, |at| at.min(next_day));
    let wait = until.saturating_sub(now).min(step).max(MIN_WAIT_MS);
    u64::try_from(wait).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use rappel_core::domain::Deck;

    use super::*;

    /// 2026-10-17T10:00:00Z.
    const NOW: i64 = 1_792_231_200_000;
    /// The next study day begins 18 hours later.
    const NEXT_DAY: i64 = NOW + 18 * HOUR_MS;

    fn summary(due: u32, new: u32) -> DeckSummary {
        DeckSummary {
            deck: Deck {
                id: "d".to_owned(),
                name: "Scratch".to_owned(),
                created_at: 0,
                updated_at: 0,
            },
            new_count: new,
            due_count: due,
            card_count: 50,
            suspended_count: 0,
            held_back_count: 0,
            next_change_at: None,
        }
    }

    fn next_at(at: i64) -> DeckSummary {
        DeckSummary {
            next_change_at: Some(at),
            ..summary(0, 0)
        }
    }

    /// The shown line, with "·" between the parts as the row draws it.
    fn shown(text: &RowText) -> String {
        [text.due.as_deref(), text.other.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }

    fn line(s: &DeckSummary) -> (String, String) {
        let text = row_text(s, NOW, NEXT_DAY);
        (shown(&text), text.spoken)
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_owned(), b.to_owned())
    }

    #[test]
    fn due_and_new_counts_are_shown_when_there_is_something_to_study() {
        assert_eq!(
            line(&summary(12, 5)),
            pair("12 due · 5 new", "12 due, 5 new")
        );
        assert_eq!(line(&summary(12, 0)), pair("12 due", "12 due"));
        assert_eq!(line(&summary(0, 5)), pair("5 new", "5 new"));
        assert_eq!(
            line(&summary(1_234, 1)),
            pair("1,234 due · 1 new", "1,234 due, 1 new")
        );
    }

    #[test]
    fn only_the_due_count_is_its_own_part_and_the_dot_shows_between_two() {
        let both = row_text(&summary(12, 5), NOW, NEXT_DAY);
        assert_eq!(both.due.as_deref(), Some("12 due"));
        assert_eq!(both.other.as_deref(), Some("5 new"));
        assert!(both.has_separator());
        let due = row_text(&summary(3, 0), NOW, NEXT_DAY);
        assert_eq!(
            (due.due.as_deref(), due.other.as_deref()),
            (Some("3 due"), None)
        );
        assert!(!due.has_separator());
        let new = row_text(&summary(0, 3), NOW, NEXT_DAY);
        assert_eq!(
            (new.due.as_deref(), new.other.as_deref()),
            (None, Some("3 new"))
        );
        assert!(!new.has_separator());
        // No status line has a due part, so none is in the accent colour.
        for s in [next_at(NOW + HOUR_MS), summary(0, 0)] {
            assert_eq!(row_text(&s, NOW, NEXT_DAY).due, None);
        }
    }

    #[test]
    fn an_empty_deck_and_a_suspended_one_say_so() {
        let empty = DeckSummary {
            card_count: 0,
            ..summary(0, 0)
        };
        assert_eq!(line(&empty), pair("No cards yet", "no cards yet"));
        let suspended = DeckSummary {
            suspended_count: 50,
            next_change_at: None,
            ..summary(0, 0)
        };
        assert_eq!(
            line(&suspended),
            pair("All cards suspended", "all cards suspended")
        );
        // Some suspended, the others learned: the next review counts.
        let some = DeckSummary {
            suspended_count: 49,
            ..next_at(NOW + 2 * HOUR_MS)
        };
        assert_eq!(line(&some).0, "Next review in 2 h");
    }

    #[test]
    fn the_next_review_is_in_minutes_within_the_hour() {
        assert_eq!(
            line(&next_at(NOW + 25 * MINUTE_MS)),
            pair("Next review in 25 min", "next review in 25 minutes")
        );
        assert_eq!(
            line(&next_at(NOW + 1)),
            pair("Next review in 1 min", "next review in 1 minute")
        );
        assert_eq!(
            line(&next_at(NOW + 59 * MINUTE_MS)).0,
            "Next review in 59 min"
        );
        // 59 min 40 s rounds to an hour.
        assert_eq!(
            line(&next_at(NOW + HOUR_MS - 20_000)),
            pair("Next review in 1 h", "next review in 1 hour")
        );
    }

    #[test]
    fn the_next_review_is_in_hours_until_the_next_study_day() {
        assert_eq!(
            line(&next_at(NOW + 3 * HOUR_MS)),
            pair("Next review in 3 h", "next review in 3 hours")
        );
        // 17 h 59 min: still today's study day.
        assert_eq!(
            line(&next_at(NEXT_DAY - MINUTE_MS)).0,
            "Next review in 18 h"
        );
        // Late in the evening a morning review after the day start is still
        // "in 9 h", not "tomorrow".
        let late = NEXT_DAY - 2 * HOUR_MS;
        let text = row_text(&next_at(late + 9 * HOUR_MS), late, NEXT_DAY);
        assert_eq!(shown(&text), "Next review in 9 h");
    }

    #[test]
    fn the_next_review_is_tomorrow_then_in_study_days() {
        assert_eq!(
            line(&next_at(NEXT_DAY)),
            pair("Next review tomorrow", "next review tomorrow")
        );
        assert_eq!(
            line(&next_at(NEXT_DAY + DAY_MS - 1)).0,
            "Next review tomorrow"
        );
        assert_eq!(
            line(&next_at(NEXT_DAY + DAY_MS)),
            pair("Next review in 2 days", "next review in 2 days")
        );
        assert_eq!(
            line(&next_at(NEXT_DAY + 3 * DAY_MS + 5)).0,
            "Next review in 4 days"
        );
        assert_eq!(
            line(&next_at(NEXT_DAY + 58 * DAY_MS)).0,
            "Next review in 59 days"
        );
        assert_eq!(
            line(&next_at(NOW + 90 * DAY_MS)).0,
            "Next review in 3 months"
        );
        assert_eq!(
            line(&next_at(NOW + 1_000 * DAY_MS)).0,
            "Next review in 2.7 years"
        );
    }

    #[test]
    fn a_stale_or_far_time_never_panics() {
        for at in [i64::MIN, NOW - DAY_MS, NOW, i64::MAX] {
            let _ = row_text(&next_at(at), NOW, NEXT_DAY);
        }
        assert_eq!(line(&next_at(NOW - 5)).0, "Next review in 1 min");
    }

    #[test]
    fn the_daily_limits_make_the_deck_done_for_today() {
        let held = |next| DeckSummary {
            held_back_count: 10,
            next_change_at: next,
            ..summary(0, 0)
        };
        assert_eq!(line(&held(None)), pair("Done for today", "done for today"));
        assert_eq!(line(&held(Some(NEXT_DAY))).0, "Done for today");
        assert_eq!(line(&held(Some(NEXT_DAY + 5 * DAY_MS))).0, "Done for today");
        // A card comes back before the new study day: that is said.
        assert_eq!(
            line(&held(Some(NOW + 10 * MINUTE_MS))).0,
            "Next review in 10 min"
        );
        // Cards to study now win over the held-back ones.
        let study = DeckSummary {
            held_back_count: 10,
            ..summary(4, 0)
        };
        assert_eq!(line(&study).0, "4 due");
    }

    #[test]
    fn nothing_to_come_says_nothing_to_study() {
        assert_eq!(
            line(&summary(0, 0)),
            pair("Nothing to study", "nothing to study")
        );
    }

    #[test]
    fn the_accessible_label_is_the_name_and_the_line_in_words() {
        let text = row_text(&summary(12, 5), NOW, NEXT_DAY);
        assert_eq!(text.accessible_label("Scratch"), "Scratch, 12 due, 5 new");
        let text = row_text(&next_at(NOW + 3 * HOUR_MS), NOW, NEXT_DAY);
        assert_eq!(
            text.accessible_label("Maths"),
            "Maths, next review in 3 hours"
        );
    }

    #[test]
    fn the_list_is_read_again_when_its_texts_would_change() {
        let at = |t| vec![summary(1, 0), next_at(t)];
        // Minutes: every minute, or at the change if sooner.
        assert_eq!(
            refresh_delay_ms(&at(NOW + 25 * MINUTE_MS), NOW, NEXT_DAY),
            60_000
        );
        assert_eq!(refresh_delay_ms(&at(NOW + 30_000), NOW, NEXT_DAY), 30_000);
        // Hours: every ten minutes.
        assert_eq!(
            refresh_delay_ms(&at(NOW + 3 * HOUR_MS), NOW, NEXT_DAY),
            600_000
        );
        // Days, or nothing coming: hourly, or at the next study day.
        assert_eq!(
            refresh_delay_ms(&at(NOW + 5 * DAY_MS), NOW, NEXT_DAY),
            3_600_000
        );
        assert_eq!(refresh_delay_ms(&[summary(0, 0)], NOW, NEXT_DAY), 3_600_000);
        assert_eq!(
            refresh_delay_ms(&[summary(0, 0)], NOW, NOW + 20 * MINUTE_MS),
            1_200_000
        );
        // The soonest deck decides; no deck at all waits for the day.
        assert_eq!(
            refresh_delay_ms(
                &[next_at(NOW + 5 * DAY_MS), next_at(NOW + 90_000)],
                NOW,
                NEXT_DAY
            ),
            60_000
        );
        assert_eq!(refresh_delay_ms(&[], NOW, NOW + 5_000), 5_000);
        // A time already past waits a second, never zero.
        assert_eq!(refresh_delay_ms(&at(NOW - 1), NOW, NEXT_DAY), 1_000);
        assert_eq!(refresh_delay_ms(&[], NOW, NOW - DAY_MS), 1_000);
        assert_eq!(refresh_delay_ms(&at(i64::MIN), i64::MAX, i64::MIN), 1_000);
    }
}
