//! What the Card Information dialog says, as pure functions of a
//! [`CardInfo`] (unit-tested). Dates come in as a closure, so the tests
//! need no time zone.

use std::time::Duration;

use rappel_core::domain::Grade;
use rappel_core::stats::{CardInfo, HistoryEntry};

use crate::ui::text::{count, duration_text, group_digits, interval_text, percent};
use crate::ui::time::relative_due;

/// The most history entries the dialog lists, newest first: a card with
/// thousands of reviews would make the dialog slow to open for rows nobody
/// scrolls to.
pub const MAX_HISTORY: usize = 500;

/// A date and time, as the dialog shows one.
pub type When<'a> = &'a dyn Fn(i64) -> String;

/// The values of the memory rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    /// "New card" for a card without a schedule (never studied, or reset).
    pub status: Option<&'static str>,
    pub reviews: String,
    pub pass_rate: String,
    pub lapses: String,
    /// `None` for a new card: these rows are hidden.
    pub scheduled: Option<Scheduled>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    pub stability: String,
    pub difficulty: String,
    pub recall: String,
    pub next_due: String,
}

/// "12.4 days", "1 day", "130 days" (stability is in days). Not a number,
/// or an infinite one, is "—" (ADR 0050, row 34).
pub fn stability_text(days: f32) -> String {
    if !days.is_finite() {
        return "—".to_owned();
    }
    // A negative stability (never valid) as zero, and -0 without its sign.
    let days = if days > 0.0 { days } else { 0.0 };
    if (days - 1.0).abs() < 0.05 {
        "1 day".to_owned()
    } else if days >= 100.0 {
        format!("{days:.0} days")
    } else {
        format!("{days:.1} days")
    }
}

/// "5.3 of 10"; "—" for not a number or an infinite one.
pub fn difficulty_text(difficulty: f32) -> String {
    if difficulty.is_finite() {
        format!("{difficulty:.1} of 10")
    } else {
        "—".to_owned()
    }
}

pub fn memory(info: &CardInfo, now: i64, when: When<'_>) -> Memory {
    let scheduled = match (info.stability, info.difficulty, info.next_due) {
        (Some(stability), Some(difficulty), Some(due)) => Some(Scheduled {
            stability: stability_text(stability),
            difficulty: difficulty_text(difficulty),
            recall: info
                .retrievability
                .map_or_else(|| "—".to_owned(), |r| percent(f64::from(r))),
            next_due: format!("{} · {}", when(due), relative_due(due, now)),
        }),
        _ => None,
    };
    Memory {
        status: scheduled.is_none().then_some("New card"),
        reviews: group_digits(info.reviews as usize),
        pass_rate: info.pass_rate().map_or_else(|| "—".to_owned(), percent),
        lapses: group_digits(info.lapses as usize),
        scheduled,
    }
}

/// An icon for the result, so it never depends on colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Passed,
    Failed,
    Reset,
}

impl Mark {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Passed => "object-select-symbolic",
            Self::Failed => "window-close-symbolic",
            Self::Reset => "view-refresh-symbolic",
        }
    }

    /// A libadwaita style class, or "".
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Passed => "success",
            Self::Failed => "error",
            Self::Reset => "",
        }
    }
}

/// One row of the history list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub mark: Mark,
    pub title: &'static str,
    pub subtitle: String,
}

/// The history rows, newest first, and a note when older entries are left
/// out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    pub lines: Vec<Line>,
    pub note: Option<String>,
    /// Said instead of the list when there is no entry.
    pub empty: Option<&'static str>,
}

pub fn history(info: &CardInfo, when: When<'_>) -> History {
    let lines: Vec<Line> = info
        .history
        .iter()
        .rev()
        .take(MAX_HISTORY)
        .map(|entry| match *entry {
            HistoryEntry::Review {
                at,
                grade,
                duration_ms,
                interval_ms,
            } => {
                let passed = grade == Grade::Pass;
                Line {
                    mark: if passed { Mark::Passed } else { Mark::Failed },
                    title: if passed { "Passed" } else { "Failed" },
                    subtitle: format!(
                        "{} · {} · next in {}",
                        when(at),
                        duration_text(Duration::from_millis(duration_ms.max(0).unsigned_abs())),
                        interval_text(interval_ms)
                    ),
                }
            }
            HistoryEntry::Reset { at } => Line {
                mark: Mark::Reset,
                title: "Progress Reset",
                subtitle: format!("{} · counts start again from here", when(at)),
            },
        })
        .collect();
    let hidden = info.history.len().saturating_sub(MAX_HISTORY);
    History {
        empty: lines.is_empty().then_some("Not studied yet"),
        note: (hidden > 0).then(|| {
            format!(
                "{} not shown",
                count(hidden, "older entry", "older entries")
            )
        }),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn when(t: i64) -> String {
        format!("@{t}")
    }

    fn info() -> CardInfo {
        CardInfo {
            created_at: 1,
            updated_at: 2,
            reviews: 0,
            passes: 0,
            lapses: 0,
            stability: None,
            difficulty: None,
            retrievability: None,
            next_due: None,
            suspended: false,
            history: vec![],
        }
    }

    fn review(at: i64, grade: Grade) -> HistoryEntry {
        HistoryEntry::Review {
            at,
            grade,
            duration_ms: 4_500,
            interval_ms: 3 * 86_400_000,
        }
    }

    #[test]
    fn a_new_card_has_a_status_and_no_schedule() {
        let m = memory(&info(), 0, &when);
        assert_eq!(m.status, Some("New card"));
        assert_eq!(m.reviews, "0");
        assert_eq!(m.pass_rate, "—");
        assert_eq!(m.lapses, "0");
        assert_eq!(m.scheduled, None);
    }

    #[test]
    fn a_scheduled_card_shows_its_memory_and_when_it_is_due() {
        let mut i = info();
        i.reviews = 12;
        i.passes = 9;
        i.lapses = 2;
        i.stability = Some(12.44);
        i.difficulty = Some(5.27);
        i.retrievability = Some(0.874);
        i.next_due = Some(3 * 86_400_000);
        let m = memory(&i, 0, &when);
        assert_eq!(m.status, None);
        assert_eq!(m.reviews, "12");
        assert_eq!(m.pass_rate, "75%");
        let s = m.scheduled.unwrap();
        assert_eq!(s.stability, "12.4 days");
        assert_eq!(s.difficulty, "5.3 of 10");
        assert_eq!(s.recall, "87%");
        assert_eq!(s.next_due, "@259200000 · in 3 days");
    }

    #[test]
    fn stability_reads_naturally() {
        assert_eq!(stability_text(0.4), "0.4 days");
        assert_eq!(stability_text(1.0), "1 day");
        assert_eq!(stability_text(99.94), "99.9 days");
        assert_eq!(stability_text(130.4), "130 days");
    }

    #[test]
    fn the_history_is_newest_first_with_resets_marked() {
        let mut i = info();
        i.history = vec![
            review(10, Grade::Fail),
            review(20, Grade::Pass),
            HistoryEntry::Reset { at: 30 },
            review(40, Grade::Pass),
        ];
        let h = history(&i, &when);
        let titles: Vec<_> = h.lines.iter().map(|l| l.title).collect();
        assert_eq!(titles, ["Passed", "Progress Reset", "Passed", "Failed"]);
        assert_eq!(h.lines[0].subtitle, "@40 · 4 s · next in 3 days");
        assert_eq!(h.lines[1].mark, Mark::Reset);
        assert_eq!(h.lines[1].subtitle, "@30 · counts start again from here");
        assert_eq!(h.lines[3].mark, Mark::Failed);
        assert_eq!((h.note, h.empty), (None, None));
    }

    #[test]
    fn marks_differ_in_icon_not_only_in_colour() {
        let icons = [Mark::Passed, Mark::Failed, Mark::Reset].map(Mark::icon);
        assert_eq!(icons.len(), 3);
        assert!(icons[0] != icons[1] && icons[1] != icons[2] && icons[0] != icons[2]);
    }

    #[test]
    fn an_empty_history_is_said() {
        let h = history(&info(), &when);
        assert_eq!(h.lines.len(), 0);
        assert_eq!(h.empty, Some("Not studied yet"));
    }

    #[test]
    fn a_long_history_is_cut_at_the_newest_entries() {
        let mut i = info();
        let max = i64::try_from(MAX_HISTORY).unwrap();
        i.history = (0..=max).map(|t| review(t, Grade::Pass)).collect();
        let h = history(&i, &when);
        assert_eq!(h.lines.len(), MAX_HISTORY);
        assert_eq!(h.note.as_deref(), Some("1 older entry not shown"));
        // The newest is first, the oldest left out.
        assert!(h.lines[0].subtitle.starts_with(&format!("@{MAX_HISTORY} ")));
        i.history.extend((0..1_511).map(|t| review(t, Grade::Fail)));
        let h = history(&i, &when);
        assert_eq!(h.note.as_deref(), Some("1,512 older entries not shown"));
    }

    /// A hostile history (ADR 0050, rows 23 and 34): counts at `u32::MAX`,
    /// times at both ends of `i64`, values that are not numbers. Nothing
    /// panics and every text is sane.
    #[test]
    fn hostile_values_never_panic() {
        let mut card = info();
        card.reviews = u32::MAX;
        card.passes = u32::MAX;
        card.lapses = u32::MAX;
        card.stability = Some(f32::NAN);
        card.difficulty = Some(f32::INFINITY);
        card.retrievability = Some(f32::NAN);
        card.next_due = Some(i64::MAX);
        card.created_at = i64::MIN;
        card.history = vec![
            HistoryEntry::Review {
                at: i64::MIN,
                grade: Grade::Pass,
                duration_ms: i64::MAX,
                interval_ms: i64::MAX,
            },
            HistoryEntry::Review {
                at: i64::MAX,
                grade: Grade::Fail,
                duration_ms: i64::MIN,
                interval_ms: i64::MIN,
            },
            HistoryEntry::Reset { at: 0 },
        ];
        let dates = |t: i64| crate::ui::time::date_time(t);
        let shown = memory(&card, i64::MIN, &dates);
        assert_eq!(shown.reviews, "4,294,967,295");
        assert_eq!(shown.pass_rate, "100%");
        let scheduled = shown.scheduled.expect("scheduled");
        assert_eq!(scheduled.stability, "—");
        assert_eq!(scheduled.difficulty, "—");
        assert_eq!(scheduled.recall, "—");
        assert!(
            scheduled.next_due.ends_with("in 1000 years"),
            "{}",
            scheduled.next_due
        );
        let lines = history(&card, &dates).lines;
        assert_eq!(lines.len(), 3);
        assert!(
            lines[1].subtitle.ends_with("0 s · next in 1 min"),
            "{}",
            lines[1].subtitle
        );
        assert!(
            lines[2].subtitle.ends_with("next in 1000 years"),
            "{}",
            lines[2].subtitle
        );
        assert_eq!(stability_text(f32::MAX), format!("{:.0} days", f32::MAX));
        assert_eq!(stability_text(-0.0), "0.0 days");
    }
}
