//! Texts with numbers (grouped digits, counted nouns, times) and names on
//! one line. Pure, so they are unit-tested, in English and in French
//! (ADR 0059). Numbers follow the language through translated
//! separators: Rust has no safe access to the C locale's (`localeconv`),
//! so the thousands separator, the decimal separator and the percent
//! sign's place are messages a translator sets (French: "5 210", "1,5",
//! "91 %", with U+202F NARROW NO-BREAK SPACE).

use std::time::Duration;

use super::time::{DAY_MS, HOUR_MS, MINUTE_MS};
use crate::i18n::{fill, gettext, ngettext, pgettext};

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

/// What separates the items of a list: ", ".
pub fn list_separator() -> String {
    // Translators: what separates the items of a list of names ("maths,
    // physics, history"); keep the space if your language has one.
    pgettext("list separator", ", ")
}

/// `names` joined by ", ", at most [`MAX_NAMED`] of them, then "and 4,980
/// more". Each name goes through `each` first ([`isolate`] for visible
/// text).
pub fn name_list(names: &[String], each: impl Fn(&str) -> String) -> String {
    let shown: Vec<String> = names.iter().take(MAX_NAMED).map(|n| each(n)).collect();
    let list = shown.join(&list_separator());
    let more = names.len().saturating_sub(MAX_NAMED);
    if more == 0 {
        return list;
    }
    fill(
        // Translators: {list} is a list of names, {count} how many more
        // names there are after them ("maths, physics and 4,980 more").
        &ngettext("{list} and {count} more", "{list} and {count} more", more),
        &[("list", &list), ("count", &group_digits(more))],
    )
}

/// `parts` as one phrase: "a", "a and b", "a, b and c".
pub fn and_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => fill(
            // Translators: the end of a list: {list} is its first items
            // ("12 decks, 840 cards"), {last} its last one ("5,210 reviews").
            &gettext("{list} and {last}"),
            &[("list", &init.join(&list_separator())), ("last", last)],
        ),
    }
}

/// `template`, a translated message with `{error}`, with the error's own
/// text, which is in English: `rappel-core` and the libraries do not
/// translate theirs (ADR 0059).
pub fn with_error(template: &str, err: &impl std::fmt::Display) -> String {
    fill(template, &[("error", &err.to_string())])
}

/// `n` with its digits in groups of three: 5210 → "5,210" (French
/// "5 210").
pub fn group_digits(n: usize) -> String {
    group_digit_string(&n.to_string())
}

/// A string of digits in groups of three, as [`group_digits`].
pub fn group_digit_string(digits: &str) -> String {
    // Translators: the thousands separator of numbers ("5,210"); French
    // uses U+202F NARROW NO-BREAK SPACE. Leave it as it is if your
    // language does not group digits.
    let separator = pgettext("thousands separator", ",");
    let count = digits.chars().count();
    let mut out = String::with_capacity(digits.len() + count / 3 * separator.len());
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (count - i).is_multiple_of(3) {
            out.push_str(&separator);
        }
        out.push(digit);
    }
    out
}

/// The decimal separator: "." in English ("1.5"), "," in French.
pub fn decimal_separator() -> String {
    // Translators: the decimal separator of numbers ("1.5 years"); French
    // uses a comma.
    pgettext("decimal separator", ".")
}

/// A decimal number of tenths: 15 → "1.5" (French "1,5"), 20 → "2".
pub fn tenths(n: usize) -> String {
    let whole = group_digits(n / 10);
    match n % 10 {
        0 => whole,
        tenth => format!("{whole}{}{tenth}", decimal_separator()),
    }
}

/// `template`, a counted message from `ngettext`, with `{count}` as `n` in
/// grouped digits: `counted(&ngettext("{count} deck", "{count} decks", n), n)`
/// gives "1 deck", "1,200 decks".
pub fn counted(template: &str, n: usize) -> String {
    fill(template, &[("count", &group_digits(n))])
}

/// [`counted`] for a number of time units ("3 days", "1 hour"); a negative
/// number, which no caller passes, counts as zero.
pub fn counted_units(template: impl Fn(usize) -> String, n: i64) -> String {
    let n = usize::try_from(n).unwrap_or(0);
    counted(&template(n), n)
}

/// "12 due", "1,234 due": cards to review now, steps included. The deck
/// list's rows and the deck page's subtitle both say it this way.
pub fn due_count(n: usize) -> String {
    // Translators: {count} is a number of cards to review now ("12 due"),
    // shown under a deck's name.
    counted(&ngettext("{count} due", "{count} due", n), n)
}

/// "5 new", "1,234 new": new cards still allowed today.
pub fn new_count(n: usize) -> String {
    // Translators: {count} is a number of new cards to learn today ("5
    // new"), shown under a deck's name.
    counted(&ngettext("{count} new", "{count} new", n), n)
}

/// What there is to study, shown on one line: "12 due · 5 new", "12 due",
/// "5 new", or `None` when both are zero (zero counts are left out).
pub fn due_new_line(due: usize, new: usize) -> Option<String> {
    match (due, new) {
        (0, 0) => None,
        (due, 0) => Some(due_count(due)),
        (0, new) => Some(new_count(new)),
        (due, new) => Some(fill(
            // Translators: {due} is "12 due", {new} is "5 new", on one line.
            &gettext("{due} · {new}"),
            &[("due", &due_count(due)), ("new", &new_count(new))],
        )),
    }
}

/// The non-zero counts of decks, cards and reviews as one phrase:
/// "12 decks, 840 cards and 5,210 reviews", "1 card", or `None` if all
/// are zero.
pub fn decks_cards_reviews(decks: usize, cards: usize, reviews: usize) -> Option<String> {
    let mut parts = Vec::new();
    if decks > 0 {
        // Translators: {count} is a number of decks.
        parts.push(counted(
            &ngettext("{count} deck", "{count} decks", decks),
            decks,
        ));
    }
    if cards > 0 {
        // Translators: {count} is a number of cards.
        parts.push(counted(
            &ngettext("{count} card", "{count} cards", cards),
            cards,
        ));
    }
    if reviews > 0 {
        // Translators: {count} is a number of reviews (answers given to
        // cards).
        parts.push(counted(
            &ngettext("{count} review", "{count} reviews", reviews),
            reviews,
        ));
    }
    (!parts.is_empty()).then(|| and_list(&parts))
}

/// A whole number of seconds, minutes or hours, grouped.
fn number(n: u64) -> String {
    group_digits(usize::try_from(n).unwrap_or(usize::MAX))
}

/// "45 s", "4 min 10 s", "1 h 2 min" (seconds rounded down).
pub fn duration_text(time: Duration) -> String {
    let secs = time.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (h, m) {
        // Translators: a time in seconds ("45 s").
        (0, 0) => fill(&gettext("{seconds} s"), &[("seconds", &number(s))]),
        (0, m) => fill(
            // Translators: a time in minutes and seconds ("4 min 10 s").
            &gettext("{minutes} min {seconds} s"),
            &[("minutes", &number(m)), ("seconds", &number(s))],
        ),
        (h, m) => hours_minutes(h, m),
    }
}

/// "3 h 12 min".
fn hours_minutes(h: u64, m: u64) -> String {
    fill(
        // Translators: a time in hours and minutes ("3 h 12 min").
        &gettext("{hours} h {minutes} min"),
        &[("hours", &number(h)), ("minutes", &number(m))],
    )
}

/// "12 min".
fn minutes(m: u64) -> String {
    // Translators: a time in minutes ("12 min").
    fill(&gettext("{minutes} min"), &[("minutes", &number(m))])
}

/// A time studied, to the minute: "0 min", "45 s" below a minute,
/// "12 min", "3 h 12 min" (rounded down; a negative time is zero).
pub fn studied_text(ms: i64) -> String {
    let secs = u64::try_from(ms.max(0) / 1000).unwrap_or(0);
    match secs {
        0 => minutes(0),
        1..=59 => fill(&gettext("{seconds} s"), &[("seconds", &number(secs))]),
        _ => {
            let (h, m) = (secs / 3600, secs / 60 % 60);
            if h == 0 {
                minutes(m)
            } else {
                hours_minutes(h, m)
            }
        }
    }
}

/// A fraction as a whole percent: 0.912 → "91%" (French "91 %").
/// Out-of-range values are clamped, so a rounding error never shows
/// "101%".
pub fn percent(fraction: f64) -> String {
    if fraction.is_nan() {
        // No number at all: not "NaN%".
        return "—".to_owned();
    }
    whole_percent((fraction.clamp(0.0, 1.0) * 100.0).round())
}

/// A whole number of percent: 91 → "91%" (French "91 %").
pub fn whole_percent(n: impl std::fmt::Display) -> String {
    fill(
        // Translators: a percentage; {percent} is a whole number ("91%").
        // French writes "{percent} %" with U+202F NARROW NO-BREAK SPACE.
        &pgettext("percent", "{percent}%"),
        &[("percent", &n.to_string())],
    )
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
        // Translators: an interval in minutes ("10 min").
        counted_units(|n| ngettext("{count} min", "{count} min", n), minutes)
    } else if hours < 24 {
        // Translators: an interval in hours ("5 h").
        counted_units(|n| ngettext("{count} h", "{count} h", n), hours)
    } else if days < 60 {
        // Translators: an interval in days ("3 days").
        counted_units(|n| ngettext("{count} day", "{count} days", n), days)
    } else if ms < 730 * DAY_MS {
        let month = DAY_MS * 3044 / 100;
        // Translators: an interval in months ("2 months").
        counted_units(
            |n| ngettext("{count} month", "{count} months", n),
            round(month),
        )
    } else {
        let year = DAY_MS * 36525 / 100;
        let tenths_of_years = (ms.min(1_000 * year) * 10 + year / 2) / year;
        let n = usize::try_from(tenths_of_years).unwrap_or(0);
        fill(
            // Translators: an interval in years, at least 2, maybe with a
            // decimal ("2.7 years").
            &ngettext("{count} year", "{count} years", n.div_ceil(10)),
            &[("count", &tenths(n))],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(n: usize) -> String {
        crate::i18n::ngettext("{count} day", "{count} days", n)
    }

    #[test]
    fn time_units_are_counted_and_a_negative_number_is_zero() {
        assert_eq!(counted_units(days, 1), "1 day");
        assert_eq!(counted_units(days, 0), "0 days");
        assert_eq!(counted_units(days, 1_500), "1,500 days");
        assert_eq!(counted_units(days, -3), "0 days");
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
        let decks = |n| counted(&ngettext("{count} deck", "{count} decks", n), n);
        assert_eq!(decks(1), "1 deck");
        assert_eq!(decks(0), "0 decks");
        assert_eq!(decks(1_200), "1,200 decks");
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
        assert_eq!(interval_text(i64::MAX), "1,000 years");
        assert_eq!(interval_text(i64::MAX - 1), "1,000 years");
        assert_eq!(interval_text(i64::MIN), "1 min");
        assert_eq!(studied_text(i64::MAX), "2,562,047,788,015 h 12 min");
        assert_eq!(studied_text(i64::MIN), "0 min");
        assert_eq!(
            duration_text(Duration::MAX),
            "5,124,095,576,030,431 h 0 min"
        );
        assert_eq!(counted_units(days, i64::MIN), "0 days");
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

    /// The same texts in French (`po/fr.po`): narrow no-break spaces
    /// (U+202F) to group digits and before "%", a decimal comma, and
    /// French's plural rule, where 0 and 1 are singular.
    #[test]
    fn numbers_and_counts_follow_the_language() {
        crate::i18n::tests::in_french(|| {
            assert_eq!(group_digits(5_210), "5\u{202f}210");
            assert_eq!(group_digits(1_048_576), "1\u{202f}048\u{202f}576");
            assert_eq!(percent(0.912), "91\u{202f}%");
            assert_eq!(interval_text(1_000 * DAY_MS), "2,7\u{a0}ans");
            assert_eq!(interval_text(3 * DAY_MS), "3\u{a0}jours");
            assert_eq!(interval_text(DAY_MS), "1\u{a0}jour");
            assert_eq!(counted_units(days, 0), "0\u{a0}jour");
            assert_eq!(
                due_new_line(1_234, 20).as_deref(),
                Some("1\u{202f}234 à revoir · 20 nouvelles")
            );
            assert_eq!(
                decks_cards_reviews(12, 1, 5_210).as_deref(),
                Some("12 paquets, 1 carte et 5\u{202f}210 révisions")
            );
            assert_eq!(
                duration_text(Duration::from_secs(250)),
                "4\u{a0}min 10\u{a0}s"
            );
            let names: Vec<String> = (0..25).map(|i| format!("t{i}")).collect();
            assert!(name_list(&names, str::to_owned).ends_with("t19 et 5 autres"));
        });
    }
}
