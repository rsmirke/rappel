//! Dates and times for display, in the user's local time zone and locale
//! (`GLib` formats them), and the pure "in 3 days" / "overdue by 2 days"
//! phrase. English only until translations exist (SPEC §5).

use gtk::glib;

use super::text::{count, interval_text};

/// Lengths of time in milliseconds, the unit of every timestamp.
pub const MINUTE_MS: i64 = 60_000;
pub const HOUR_MS: i64 = 60 * MINUTE_MS;
pub const DAY_MS: i64 = 24 * HOUR_MS;

/// `ms` (UTC milliseconds) formatted by `GLib` in the local zone, or
/// `None` when `GLib` cannot represent it.
fn local_format(ms: i64, format: &str) -> Option<String> {
    glib::DateTime::from_unix_local(ms.div_euclid(1000))
        .and_then(|time| time.format(format))
        .ok()
        .map(String::from)
}

/// Date and time in the locale's format ("%x %R").
pub fn date_time(ms: i64) -> String {
    local_format(ms, "%x %R").unwrap_or_else(|| "Unknown date".to_owned())
}

/// Date and time with seconds ("%x %X"): backups made within a minute of
/// each other must differ on the recovery page.
pub fn date_time_seconds(ms: i64) -> String {
    local_format(ms, "%x %X").unwrap_or_else(|| "Unknown date".to_owned())
}

/// Day and short month: "17 Sep" (chart axes).
pub fn day_short(ms: i64) -> String {
    local_format(ms, "%-d %b").unwrap_or_default()
}

/// Day and month: "14 October" (chart summaries).
pub fn day_long(ms: i64) -> String {
    local_format(ms, "%-d %B").unwrap_or_default()
}

/// Weekday, day and month: "Tue 14 Oct" (a selected day).
pub fn day_with_weekday(ms: i64) -> String {
    local_format(ms, "%a %-d %b").unwrap_or_default()
}

/// How far `due` is from `now`: "due now", "in 3 days", "in 10 min",
/// "overdue by 2 days". A card that is less than a day late is "due now".
pub fn relative_due(due: i64, now: i64) -> String {
    let delta = due.saturating_sub(now);
    if delta > 0 {
        format!("in {}", interval_text(delta))
    } else {
        let days = usize::try_from(delta.saturating_neg() / DAY_MS).unwrap_or(0);
        if days == 0 {
            "due now".to_owned()
        } else {
            format!("overdue by {}", count(days, "day", "days"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_due_time_is_described_from_now() {
        const NOW: i64 = 1_000_000_000_000;
        assert_eq!(relative_due(NOW + 600_000, NOW), "in 10 min");
        assert_eq!(relative_due(NOW + 3 * DAY_MS, NOW), "in 3 days");
        assert_eq!(relative_due(NOW, NOW), "due now");
        assert_eq!(relative_due(NOW - 3_600_000, NOW), "due now");
        assert_eq!(relative_due(NOW - DAY_MS, NOW), "overdue by 1 day");
        assert_eq!(relative_due(NOW - 2 * DAY_MS - 5, NOW), "overdue by 2 days");
    }

    #[test]
    fn formats_never_panic_even_far_outside_real_dates() {
        for ms in [0, -1, i64::MIN, i64::MAX, 1_700_000_000_000] {
            let _ = (
                date_time(ms),
                date_time_seconds(ms),
                day_short(ms),
                day_long(ms),
            );
            let _ = day_with_weekday(ms);
        }
        assert_ne!(day_short(1_700_000_000_000), "");
    }
}
