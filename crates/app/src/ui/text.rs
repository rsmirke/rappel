//! Texts with numbers (grouped digits, counted nouns, times) and names on
//! one line. Pure, so they are unit-tested. English only until translations exist (SPEC §5).

use std::time::Duration;

use super::time::{DAY_MS, HOUR_MS, MINUTE_MS};

/// A name (deck or tag) on one line: every run of whitespace, such as a
/// line break in a pasted name, as one space. Plain text, never markup.
pub fn one_line(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `name` in a bidi isolate (U+2068 FIRST STRONG ISOLATE … U+2069 POP
/// DIRECTIONAL ISOLATE), for a name set among other text: its own
/// direction is kept, and a right-to-left name, or one holding a bidi
/// override (U+202E) left open, cannot reorder the text after it (Unicode
/// Standard Annex #9, "Explicit Directional Isolates"). Pango draws the two
/// marks as nothing. For visible text; a screen reader gets the names as
/// they are.
pub fn isolate(name: &str) -> String {
    format!("\u{2068}{name}\u{2069}")
}

/// The most names a list of names spells out (a tooltip, an accessible
/// label) before it says how many more there are: a card can have
/// thousands of tags (ADR 0050, row 31).
pub const MAX_NAMED: usize = 20;

/// `names` joined by ", ", at most [`MAX_NAMED`] of them, then "and 4,980
/// more". Each name goes through `each` first ([`isolate`] for visible
/// text).
pub fn name_list(names: &[String], each: impl Fn(&str) -> String) -> String {
    let shown: Vec<String> = names.iter().take(MAX_NAMED).map(|n| each(n)).collect();
    let mut list = shown.join(", ");
    let more = names.len().saturating_sub(MAX_NAMED);
    if more > 0 {
        list.push_str(" and ");
        list.push_str(&group_digits(more));
        list.push_str(" more");
    }
    list
}

/// `n` with its digits in groups of three: 5210 → "5,210".
pub fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `n` and the noun, singular for exactly one: "1 deck", "1,200 cards".
pub fn count(n: usize, singular: &str, plural: &str) -> String {
    let noun = if n == 1 { singular } else { plural };
    format!("{} {noun}", group_digits(n))
}

/// [`count`] of a number of time units ("3 days", "1 hour"); a negative
/// number, which no caller passes, counts as zero.
pub fn unit_count(n: i64, singular: &str, plural: &str) -> String {
    count(usize::try_from(n).unwrap_or(0), singular, plural)
}

/// "12 due", "1,234 due": cards to review now, steps included. The deck
/// list's rows and the deck page's subtitle both say it this way.
pub fn due_count(n: usize) -> String {
    format!("{} due", group_digits(n))
}

/// "5 new", "1,234 new": new cards still allowed today.
pub fn new_count(n: usize) -> String {
    format!("{} new", group_digits(n))
}

/// What there is to study, shown on one line: "12 due · 5 new", "12 due",
/// "5 new", or `None` when both are zero (zero counts are left out).
pub fn due_new_line(due: usize, new: usize) -> Option<String> {
    match (due, new) {
        (0, 0) => None,
        (due, 0) => Some(due_count(due)),
        (0, new) => Some(new_count(new)),
        (due, new) => Some(format!("{} · {}", due_count(due), new_count(new))),
    }
}

/// The non-zero counts of decks, cards and reviews as one phrase:
/// "12 decks, 840 cards and 5,210 reviews", "1 card", or `None` if all
/// are zero.
pub fn decks_cards_reviews(decks: usize, cards: usize, reviews: usize) -> Option<String> {
    let parts: Vec<String> = [
        (decks, "deck", "decks"),
        (cards, "card", "cards"),
        (reviews, "review", "reviews"),
    ]
    .into_iter()
    .filter(|&(n, _, _)| n > 0)
    .map(|(n, singular, plural)| count(n, singular, plural))
    .collect();
    match parts.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        [init @ .., last] => Some(format!("{} and {last}", init.join(", "))),
    }
}

/// "45 s", "4 min 10 s", "1 h 2 min" (seconds rounded down).
pub fn duration_text(time: Duration) -> String {
    let secs = time.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s} s"),
        (0, m) => format!("{m} min {s} s"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A time studied, to the minute: "0 min", "45 s" below a minute,
/// "12 min", "3 h 12 min" (rounded down; a negative time is zero).
pub fn studied_text(ms: i64) -> String {
    let secs = u64::try_from(ms.max(0) / 1000).unwrap_or(0);
    match secs {
        0 => "0 min".to_owned(),
        1..=59 => format!("{secs} s"),
        _ => {
            let (h, m) = (secs / 3600, secs / 60 % 60);
            if h == 0 {
                format!("{m} min")
            } else {
                format!("{h} h {m} min")
            }
        }
    }
}

/// A fraction as a whole percent: 0.912 → "91%". Out-of-range values are
/// clamped, so a rounding error never shows "101%".
pub fn percent(fraction: f64) -> String {
    if fraction.is_nan() {
        // No number at all: not "NaN%".
        return "—".to_owned();
    }
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round())
}

/// A span of time, one unit and rounded: "10 min", "5 h", "3 days",
/// "2 months", "1.5 years". Under a minute is "1 min".
pub fn interval_text(ms: i64) -> String {
    let ms = ms.max(0);
    // Rounding adds half a unit: saturating, since a time from the
    // database can be as large as `i64::MAX` (ADR 0050, row 34).
    let round = |unit: i64| ms.saturating_add(unit / 2) / unit;
    // The unit is chosen from the rounded number, so 59 min 40 s is "1 h",
    // not "60 min".
    let minutes = round(MINUTE_MS).max(1);
    let hours = round(HOUR_MS);
    let days = round(DAY_MS);
    if minutes < 60 {
        unit_count(minutes, "min", "min")
    } else if hours < 24 {
        unit_count(hours, "h", "h")
    } else if days < 60 {
        unit_count(days, "day", "days")
    } else if ms < 730 * DAY_MS {
        let month = DAY_MS * 3044 / 100;
        unit_count(round(month), "month", "months")
    } else {
        let year = DAY_MS * 36525 / 100;
        let tenths = (ms.min(1_000 * year) * 10 + year / 2) / year;
        format!("{}.{} years", tenths / 10, tenths % 10).replace(".0 ", " ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_units_are_counted_and_a_negative_number_is_zero() {
        assert_eq!(unit_count(1, "day", "days"), "1 day");
        assert_eq!(unit_count(0, "day", "days"), "0 days");
        assert_eq!(unit_count(25, "minute", "minutes"), "25 minutes");
        assert_eq!(unit_count(1_500, "day", "days"), "1,500 days");
        assert_eq!(unit_count(-3, "hour", "hours"), "0 hours");
    }

    /// Moved here from `tag_pills::text` (`a_pill_is_one_plain_line`) and
    /// `browser_page::text`, which had a copy each.
    #[test]
    fn a_name_is_one_plain_line() {
        assert_eq!(one_line("maths"), "maths");
        assert_eq!(one_line(" a\n  b\tc "), "a b c");
        assert_eq!(one_line("a\n  b"), "a b");
        // Untrusted names stay as typed: labels never parse markup.
        assert_eq!(one_line("<b>x</b> &amp;"), "<b>x</b> &amp;");
    }

    #[test]
    fn a_name_among_others_is_isolated() {
        assert_eq!(isolate("maths"), "\u{2068}maths\u{2069}");
        // An override left open stays inside its isolate.
        assert_eq!(isolate("\u{202e}evil"), "\u{2068}\u{202e}evil\u{2069}");
    }

    #[test]
    fn a_long_list_of_names_names_the_first_and_counts_the_rest() {
        let names = |n: usize| (0..n).map(|i| format!("t{i}")).collect::<Vec<_>>();
        assert_eq!(name_list(&[], str::to_owned), "");
        assert_eq!(name_list(&names(2), str::to_owned), "t0, t1");
        let twenty = name_list(&names(MAX_NAMED), str::to_owned);
        assert!(twenty.ends_with("t18, t19"));
        assert!(!twenty.contains("more"));
        let many = name_list(&names(5_000), str::to_owned);
        assert!(many.starts_with("t0, t1, "));
        assert!(many.ends_with("t19 and 4,980 more"));
        assert_eq!(name_list(&names(1), isolate), "\u{2068}t0\u{2069}");
    }

    #[test]
    fn digits_are_grouped_by_three() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(5_210), "5,210");
        assert_eq!(group_digits(1_048_576), "1,048,576");
    }

    #[test]
    fn one_is_singular() {
        assert_eq!(count(1, "deck", "decks"), "1 deck");
        assert_eq!(count(0, "deck", "decks"), "0 decks");
        assert_eq!(count(1_200, "card", "cards"), "1,200 cards");
    }

    #[test]
    fn what_to_study_is_grouped_and_leaves_out_zeros() {
        assert_eq!(due_count(1_234), "1,234 due");
        assert_eq!(new_count(5), "5 new");
        assert_eq!(
            due_new_line(1_234, 20).as_deref(),
            Some("1,234 due · 20 new")
        );
        assert_eq!(due_new_line(3, 0).as_deref(), Some("3 due"));
        assert_eq!(due_new_line(0, 5_000).as_deref(), Some("5,000 new"));
        assert_eq!(due_new_line(0, 0), None);
    }

    #[test]
    fn zero_counts_are_left_out() {
        assert_eq!(
            decks_cards_reviews(12, 840, 5_210).as_deref(),
            Some("12 decks, 840 cards and 5,210 reviews")
        );
        assert_eq!(
            decks_cards_reviews(1, 0, 1).as_deref(),
            Some("1 deck and 1 review")
        );
        assert_eq!(decks_cards_reviews(0, 1, 0).as_deref(), Some("1 card"));
        assert_eq!(decks_cards_reviews(0, 0, 0), None);
    }

    #[test]
    fn durations_are_split_into_units() {
        assert_eq!(duration_text(Duration::ZERO), "0 s");
        assert_eq!(duration_text(Duration::from_millis(45_900)), "45 s");
        assert_eq!(duration_text(Duration::from_secs(250)), "4 min 10 s");
        assert_eq!(duration_text(Duration::from_secs(3_600 + 125)), "1 h 2 min");
    }

    #[test]
    fn time_studied_is_rounded_down_to_the_minute() {
        assert_eq!(studied_text(-5), "0 min");
        assert_eq!(studied_text(0), "0 min");
        assert_eq!(studied_text(45_900), "45 s");
        assert_eq!(studied_text(60_000), "1 min");
        assert_eq!(studied_text(12 * 60_000 + 59_000), "12 min");
        assert_eq!(studied_text(3_600_000), "1 h 0 min");
        assert_eq!(studied_text((3 * 60 + 12) * 60_000), "3 h 12 min");
    }

    #[test]
    fn percent_rounds_and_clamps() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.912), "91%");
        assert_eq!(percent(0.995), "100%");
        assert_eq!(percent(1.2), "100%");
        assert_eq!(percent(-0.1), "0%");
        assert_eq!(percent(f64::NAN), "—");
        assert_eq!(percent(f64::INFINITY), "100%");
        assert_eq!(percent(f64::NEG_INFINITY), "0%");
    }

    /// Times as large as the database holds, read back from a hostile
    /// history (ADR 0050, row 34): no overflow, a sane text.
    #[test]
    fn extreme_times_never_overflow() {
        assert_eq!(interval_text(i64::MAX), "1000 years");
        assert_eq!(interval_text(i64::MAX - 1), "1000 years");
        assert_eq!(interval_text(i64::MIN), "1 min");
        assert_eq!(studied_text(i64::MAX), "2562047788015 h 12 min");
        assert_eq!(studied_text(i64::MIN), "0 min");
        assert_eq!(duration_text(Duration::MAX), "5124095576030431 h 0 min");
        assert_eq!(unit_count(i64::MIN, "day", "days"), "0 days");
    }

    #[test]
    fn intervals_use_one_rounded_unit() {
        const MIN: i64 = MINUTE_MS;
        const DAY: i64 = DAY_MS;
        assert_eq!(interval_text(0), "1 min");
        assert_eq!(interval_text(10 * MIN), "10 min");
        assert_eq!(interval_text(59 * MIN), "59 min");
        assert_eq!(interval_text(5 * 60 * MIN), "5 h");
        assert_eq!(interval_text(DAY), "1 day");
        assert_eq!(interval_text(3 * DAY), "3 days");
        assert_eq!(interval_text(59 * DAY), "59 days");
        // Rounding up to the next unit names that unit.
        assert_eq!(interval_text(59 * MIN + 40_000), "1 h");
        assert_eq!(interval_text(23 * 60 * MIN + 40 * MIN), "1 day");
        assert_eq!(interval_text(59 * DAY + DAY / 2), "2 months");
        assert_eq!(interval_text(61 * DAY), "2 months");
        assert_eq!(interval_text(365 * DAY), "12 months");
        assert_eq!(interval_text(730 * DAY), "2 years");
        assert_eq!(interval_text(1_095 * DAY), "3 years");
        assert_eq!(interval_text(1_000 * DAY), "2.7 years");
    }
}
