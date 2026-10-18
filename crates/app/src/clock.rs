//! When "today" began for studying: at the settings' day start hour in
//! local time, 4 a.m. by default (ADR 0014, 0027).
//!
//! Core has no time zones (ADR 0006). The app computes the day start with
//! `GLib`, which knows the local zone and its daylight-saving rules, and
//! passes it to core as a UTC timestamp.
//!
//! Daylight saving, per the `GLib` documentation of `g_date_time_new`: a
//! local time that does not exist (02:30 on the day clocks go forward from
//! 02:00 to 03:00) is rounded up to the nearest time that does (03:00), and
//! an ambiguous one (01:30 when clocks go back from 03:00 to 02:00) takes
//! the standard-time occurrence. So every calendar day has exactly one
//! start, and the study day it begins is 23 to 25 hours long.

use gtk::glib;

use crate::ui::time::{DAY_MS, HOUR_MS};

/// The start of the study day containing `now_ms` in time zone `tz`, UTC
/// milliseconds: `hour`:00 local of today if that has passed, else of
/// yesterday.
///
/// # Errors
/// If `GLib` cannot represent the date (far outside any real clock).
pub fn day_start_ms(now_ms: i64, tz: &glib::TimeZone, hour: u8) -> Result<i64, glib::BoolError> {
    let noon = local_noon(now_ms, tz)?;
    let mut start = at_hour(&noon, tz, hour)?;
    if start.to_unix().saturating_mul(1000) > now_ms {
        // The calendar day before, then `hour` of it: right across a
        // daylight-saving change, unlike subtracting 24 hours.
        start = at_hour(&noon.add_days(-1)?, tz, hour)?;
    }
    Ok(start.to_unix().saturating_mul(1000))
}

/// The start of the study day after the one containing `now_ms` in time
/// zone `tz`, UTC milliseconds: `hour`:00 local of the next calendar day.
/// That day may be 23 or 25 hours away across a daylight-saving change.
///
/// # Errors
/// If `GLib` cannot represent the date (far outside any real clock).
pub fn next_day_start_ms(
    now_ms: i64,
    tz: &glib::TimeZone,
    hour: u8,
) -> Result<i64, glib::BoolError> {
    let current = day_start_ms(now_ms, tz, hour)?;
    // The first day start after the current one. A start hour skipped at
    // the end of a day is rounded into the next calendar date, so the date
    // after the current start's can give the same instant or one a day too
    // late; look at the dates around `now` and take the first start that
    // comes after.
    let noon = local_noon(now_ms, tz)?;
    let mut next = i64::MAX;
    for days in -1..=2 {
        let candidate = at_hour(&noon.add_days(days)?, tz, hour)?
            .to_unix()
            .saturating_mul(1000);
        if candidate > current {
            next = next.min(candidate);
        }
    }
    Ok(next)
}

/// 12:00 on the local calendar day of `ms`: a time of day that exists on
/// every day, so `add_days` from it moves exactly one calendar date (from
/// 23:30, say, it would land after midnight on a day where 23:30 is
/// skipped).
fn local_noon(ms: i64, tz: &glib::TimeZone) -> Result<glib::DateTime, glib::BoolError> {
    let (year, month, day) = local_time(ms, tz)?.ymd();
    glib::DateTime::new(tz, year, month, day, 12, 0, 0.0)
}

/// `ms` (UTC milliseconds) as a date and time in `tz`.
fn local_time(ms: i64, tz: &glib::TimeZone) -> Result<glib::DateTime, glib::BoolError> {
    glib::DateTime::from_unix_utc(ms.div_euclid(1000))?.to_timezone(tz)
}

/// `hour`:00 in `tz` on the calendar day of `date`.
fn at_hour(
    date: &glib::DateTime,
    tz: &glib::TimeZone,
    hour: u8,
) -> Result<glib::DateTime, glib::BoolError> {
    let (year, month, day) = date.ymd();
    glib::DateTime::new(tz, year, month, day, i32::from(hour), 0, 0.0)
}

/// The day start for now in the local time zone. If `GLib` fails, which
/// needs a clock far outside any real date, `hour`:00 UTC is used instead.
pub fn local_day_start_ms(now_ms: i64, hour: u8) -> i64 {
    day_start_ms(now_ms, &glib::TimeZone::local(), hour).unwrap_or_else(|err| {
        tracing::warn!("cannot compute the local day start, using UTC: {err}");
        let offset = i64::from(hour) * HOUR_MS;
        now_ms - (now_ms - offset).rem_euclid(DAY_MS)
    })
}

/// The next day start after now in the local time zone. If `GLib` fails,
/// 24 hours after [`local_day_start_ms`] is used instead.
pub fn local_next_day_start_ms(now_ms: i64, hour: u8) -> i64 {
    next_day_start_ms(now_ms, &glib::TimeZone::local(), hour).unwrap_or_else(|err| {
        tracing::warn!("cannot compute the next local day start: {err}");
        local_day_start_ms(now_ms, hour) + DAY_MS
    })
}

/// The starts of `count` consecutive study days in `tz`, UTC milliseconds:
/// the study day containing `now_ms` shifted by `first` calendar days (a
/// negative `first` is in the past), then each following day. These are
/// the bounds of a statistics `DayGrid` (ADR 0032): `count - 1` days. Each
/// start is `hour`:00 of its calendar date, so a day across a
/// daylight-saving change is 23 or 25 hours long and nothing else is
/// special. The result is strictly increasing even where `GLib` rounds a
/// skipped hour into the next date.
///
/// # Errors
/// If `GLib` cannot represent a date (far outside any real clock).
pub fn day_bounds_ms(
    now_ms: i64,
    tz: &glib::TimeZone,
    hour: u8,
    first: i32,
    count: usize,
) -> Result<Vec<i64>, glib::BoolError> {
    // The calendar date the current study day began on, at noon, so that
    // `add_days` moves exactly one date.
    let today = local_noon(day_start_ms(now_ms, tz, hour)?, tz)?;
    let mut bounds: Vec<i64> = Vec::with_capacity(count);
    for i in 0..count {
        let offset = i32::try_from(i)
            .ok()
            .and_then(|i| first.checked_add(i))
            .ok_or_else(|| glib::bool_error!("day offset out of range"))?;
        let start = at_hour(&today.add_days(offset)?, tz, hour)?
            .to_unix()
            .saturating_mul(1000);
        bounds.push(bounds.last().map_or(start, |&prev| start.max(prev + 1)));
    }
    Ok(bounds)
}

/// [`day_bounds_ms`] in the local time zone. If `GLib` fails, which needs a
/// clock far outside any real date, 24-hour steps from [`local_day_start_ms`]
/// are used instead.
pub fn local_day_bounds_ms(now_ms: i64, hour: u8, first: i32, count: usize) -> Vec<i64> {
    day_bounds_ms(now_ms, &glib::TimeZone::local(), hour, first, count).unwrap_or_else(|err| {
        tracing::warn!("cannot compute the local day bounds, using 24-hour days: {err}");
        let today = local_day_start_ms(now_ms, hour);
        (0..count)
            .map(|i| {
                let days = i64::from(first) + i64::try_from(i).unwrap_or(0);
                today.saturating_add(days.saturating_mul(DAY_MS))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use rappel_core::time::{UtcParts, utc_ms};

    use super::*;

    /// The default day start hour.
    const FOUR: u8 = 4;

    fn utc(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> i64 {
        utc_ms(UtcParts {
            year,
            month,
            day,
            hour,
            minute,
            second: 0,
        })
        .unwrap()
    }

    /// Central Europe as a POSIX rule, so the test needs no tzdata: CET
    /// (UTC+1), CEST (UTC+2) from the last Sunday of March 02:00 to the
    /// last Sunday of October 03:00.
    fn paris() -> glib::TimeZone {
        glib::TimeZone::from_identifier(Some("CET-1CEST,M3.5.0,M10.5.0/3"))
            .expect("a valid POSIX rule")
    }

    #[test]
    fn the_day_starts_at_four_local_time() {
        let tz = glib::TimeZone::from_offset(-5 * 3600); // UTC−5
        // 2026-10-15 03:59 local = 08:59 UTC: still the 14th's day.
        assert_eq!(
            day_start_ms(utc(2026, 10, 15, 8, 59), &tz, FOUR).unwrap(),
            (utc(2026, 10, 14, 9, 0))
        );
        // 04:00 local = 09:00 UTC: the 15th's day begins.
        assert_eq!(
            day_start_ms(utc(2026, 10, 15, 9, 0), &tz, FOUR).unwrap(),
            (utc(2026, 10, 15, 9, 0))
        );
        // 23:30 local on the 15th = 04:30 UTC on the 16th.
        assert_eq!(
            day_start_ms(utc(2026, 10, 16, 4, 30), &tz, FOUR).unwrap(),
            (utc(2026, 10, 15, 9, 0))
        );
    }

    #[test]
    fn milliseconds_before_four_belong_to_the_previous_day() {
        let tz = glib::TimeZone::utc();
        let four = utc(2026, 1, 2, 4, 0);
        assert_eq!(
            day_start_ms(four - 1, &tz, FOUR).unwrap(),
            (utc(2026, 1, 1, 4, 0))
        );
        assert_eq!(day_start_ms(four, &tz, FOUR).unwrap(), (four));
    }

    #[test]
    fn daylight_saving_changes_keep_four_local() {
        let tz = paris();
        // 2026-03-29: clocks go from 02:00 CET to 03:00 CEST. 04:00 CEST
        // is 02:00 UTC.
        assert_eq!(
            day_start_ms(utc(2026, 3, 29, 8, 0), &tz, FOUR).unwrap(),
            (utc(2026, 3, 29, 2, 0))
        );
        // The next night, 03:00 CEST = 01:00 UTC: still the 29th's day.
        assert_eq!(
            day_start_ms(utc(2026, 3, 30, 1, 0), &tz, FOUR).unwrap(),
            (utc(2026, 3, 29, 2, 0))
        );
        // 2026-10-25: clocks go from 03:00 CEST back to 02:00 CET. 04:00 CET
        // is 03:00 UTC.
        assert_eq!(
            day_start_ms(utc(2026, 10, 25, 12, 0), &tz, FOUR).unwrap(),
            (utc(2026, 10, 25, 3, 0))
        );
        // The day before the change, 04:00 CEST is 02:00 UTC.
        assert_eq!(
            day_start_ms(utc(2026, 10, 25, 1, 30), &tz, FOUR).unwrap(),
            (utc(2026, 10, 24, 2, 0))
        );
    }

    #[test]
    fn the_local_zone_gives_a_start_within_the_last_day() {
        let now = rappel_core::time::now_ms();
        let start = local_day_start_ms(now, FOUR);
        assert!(start <= now && now - start < 25 * 3_600_000);
    }

    #[test]
    fn the_next_day_starts_at_four_the_next_morning() {
        let tz = glib::TimeZone::from_offset(-5 * 3600); // UTC−5
        // 03:59 local on the 15th belongs to the 14th: next start the 15th.
        assert_eq!(
            next_day_start_ms(utc(2026, 10, 15, 8, 59), &tz, FOUR).unwrap(),
            utc(2026, 10, 15, 9, 0)
        );
        // 04:00 local on the 15th: next start the 16th.
        assert_eq!(
            next_day_start_ms(utc(2026, 10, 15, 9, 0), &tz, FOUR).unwrap(),
            utc(2026, 10, 16, 9, 0)
        );
    }

    #[test]
    fn the_next_day_start_follows_daylight_saving_changes() {
        let tz = paris();
        // The day of 2026-03-28 (CET) ends at 04:00 CEST on the 29th,
        // 02:00 UTC: only 23 hours after it began at 03:00 UTC.
        let start = utc(2026, 3, 28, 3, 0);
        assert_eq!(day_start_ms(start + 3_600_000, &tz, FOUR).unwrap(), start);
        assert_eq!(
            next_day_start_ms(start + 3_600_000, &tz, FOUR).unwrap(),
            utc(2026, 3, 29, 2, 0)
        );
        // The day of 2026-10-24 (CEST) ends at 04:00 CET on the 25th,
        // 03:00 UTC: 25 hours after it began at 02:00 UTC.
        let start = utc(2026, 10, 24, 2, 0);
        assert_eq!(
            next_day_start_ms(start + 3_600_000, &tz, FOUR).unwrap(),
            utc(2026, 10, 25, 3, 0)
        );
    }

    #[test]
    fn the_local_next_day_start_is_within_the_next_day() {
        let now = rappel_core::time::now_ms();
        let next = local_next_day_start_ms(now, FOUR);
        assert!(next > now && next - now <= 25 * 3_600_000);
        assert_eq!(local_day_start_ms(next, FOUR), next);
    }

    #[test]
    fn other_start_hours() {
        let tz = paris(); // CEST (UTC+2) on 2026-10-15
        // Hour 0: 14:00 local is after the day began at 00:00 local.
        assert_eq!(
            day_start_ms(utc(2026, 10, 15, 12, 0), &tz, 0).unwrap(),
            utc(2026, 10, 14, 22, 0)
        );
        // Hour 23: the day began at 23:00 local the evening before...
        assert_eq!(
            day_start_ms(utc(2026, 10, 15, 12, 0), &tz, 23).unwrap(),
            utc(2026, 10, 14, 21, 0)
        );
        // ...and at 23:30 local it has just begun again.
        assert_eq!(
            day_start_ms(utc(2026, 10, 15, 21, 30), &tz, 23).unwrap(),
            utc(2026, 10, 15, 21, 0)
        );
        assert_eq!(
            next_day_start_ms(utc(2026, 10, 15, 21, 30), &tz, 23).unwrap(),
            utc(2026, 10, 16, 21, 0)
        );
        // Midnight exactly belongs to the new day.
        assert_eq!(
            day_start_ms(utc(2026, 10, 14, 22, 0), &tz, 0).unwrap(),
            utc(2026, 10, 14, 22, 0)
        );
        assert_eq!(
            day_start_ms(utc(2026, 10, 14, 21, 59), &tz, 0).unwrap(),
            utc(2026, 10, 13, 22, 0)
        );
    }

    /// 02:00 does not exist on 2026-03-29 in Paris (clocks go from 02:00
    /// to 03:00): `g_date_time_new` rounds it up to 03:00 CEST, 01:00 UTC.
    #[test]
    fn a_start_hour_skipped_by_daylight_saving_begins_the_day_an_hour_later() {
        let tz = paris();
        assert_eq!(
            day_start_ms(utc(2026, 3, 29, 12, 0), &tz, 2).unwrap(),
            utc(2026, 3, 29, 1, 0)
        );
        // Just before it, still the 28th's day (02:00 CET = 01:00 UTC).
        assert_eq!(
            day_start_ms(utc(2026, 3, 29, 0, 59), &tz, 2).unwrap(),
            utc(2026, 3, 28, 1, 0)
        );
        // The next day begins at 02:00 CEST = 00:00 UTC.
        assert_eq!(
            next_day_start_ms(utc(2026, 3, 29, 12, 0), &tz, 2).unwrap(),
            utc(2026, 3, 30, 0, 0)
        );
    }

    /// 02:00 happens twice on 2026-10-25 in Paris (02:00 CEST = 00:00 UTC,
    /// then 02:00 CET = 01:00 UTC): `g_date_time_new` takes the standard
    /// time occurrence, so the day begins at 01:00 UTC and is 25 hours long.
    #[test]
    fn a_start_hour_repeated_by_daylight_saving_begins_the_day_at_its_second_time() {
        let tz = paris();
        assert_eq!(
            day_start_ms(utc(2026, 10, 25, 12, 0), &tz, 2).unwrap(),
            utc(2026, 10, 25, 1, 0)
        );
        // The first 02:00 (00:00 UTC) does not start a day.
        assert_eq!(
            day_start_ms(utc(2026, 10, 25, 0, 30), &tz, 2).unwrap(),
            utc(2026, 10, 24, 0, 0)
        );
        assert_eq!(
            next_day_start_ms(utc(2026, 10, 25, 0, 30), &tz, 2).unwrap(),
            utc(2026, 10, 25, 1, 0)
        );
    }

    /// Every start hour, every few minutes across both daylight-saving days
    /// of three zones (one whose clock change is at midnight): the day
    /// start never goes backwards, `start <= now < next`, a start and a next
    /// start are fixed points, and a study day lasts 22 to 26 hours.
    #[test]
    fn the_day_start_is_consistent_across_daylight_saving_changes() {
        const H: i64 = 3_600_000;
        let zones = [
            // Central Europe.
            "CET-1CEST,M3.5.0,M10.5.0/3",
            // Australian east coast (southern hemisphere, the other way
            // round).
            "AEST-10AEDT,M10.1.0,M4.1.0/3",
            // Clocks change at midnight: 00:00 does not exist in spring.
            "EET-2EEST,M3.5.0/0,M10.5.0/1",
            // 23:00 does not exist on the spring day, so that day's start
            // rounds into the next calendar date.
            "XST-1XDT,M3.5.0/23,M10.5.0/1",
        ];
        for rule in zones {
            let tz = glib::TimeZone::from_identifier(Some(rule)).expect("a valid POSIX rule");
            for from in [
                utc(2026, 3, 27, 0, 0),
                utc(2026, 10, 23, 0, 0),
                utc(2026, 4, 3, 0, 0),
            ] {
                for hour in 0..=23_u8 {
                    let mut previous = i64::MIN;
                    let mut now = from;
                    while now < from + 5 * 24 * H {
                        let start = day_start_ms(now, &tz, hour).unwrap();
                        let next = next_day_start_ms(now, &tz, hour).unwrap();
                        let ctx = format!("{rule} hour {hour} now {now}");
                        assert!(start <= now && now < next, "{ctx}");
                        assert!((22 * H..=26 * H).contains(&(next - start)), "{ctx}");
                        assert_eq!(day_start_ms(start, &tz, hour).unwrap(), start, "{ctx}");
                        assert_eq!(day_start_ms(next, &tz, hour).unwrap(), next, "{ctx}");
                        assert_eq!(day_start_ms(next - 1, &tz, hour).unwrap(), start, "{ctx}");
                        assert!(start >= previous, "{ctx}");
                        previous = start;
                        now += 11 * 60_000;
                    }
                }
            }
        }
    }

    /// The midnight-change zone really skips 00:00 (the test above would
    /// otherwise not cover that case): on 2026-03-29 the clocks go from
    /// 00:00 to 01:00 EEST (UTC+3), so a day starting at hour 0 begins at
    /// 01:00 EEST = 22:00 UTC the evening before.
    #[test]
    fn a_start_hour_of_midnight_skipped_by_daylight_saving() {
        let tz = glib::TimeZone::from_identifier(Some("EET-2EEST,M3.5.0/0,M10.5.0/1"))
            .expect("a valid POSIX rule");
        assert_eq!(
            day_start_ms(utc(2026, 3, 29, 12, 0), &tz, 0).unwrap(),
            utc(2026, 3, 28, 22, 0)
        );
    }

    #[test]
    fn day_bounds_count_calendar_days_from_the_current_study_day() {
        let tz = glib::TimeZone::utc();
        // 2026-10-15 03:00 is still the 14th's study day.
        let now = utc(2026, 10, 15, 3, 0);
        let bounds = day_bounds_ms(now, &tz, FOUR, -2, 4).unwrap();
        assert_eq!(
            bounds,
            vec![
                utc(2026, 10, 12, 4, 0),
                utc(2026, 10, 13, 4, 0),
                utc(2026, 10, 14, 4, 0),
                utc(2026, 10, 15, 4, 0),
            ]
        );
        // The statistics grids: 30 past days with today last, 14 ahead.
        let past = day_bounds_ms(now, &tz, FOUR, -29, 31).unwrap();
        assert_eq!(past.len(), 31);
        assert_eq!(past[29], utc(2026, 10, 14, 4, 0));
        assert_eq!(past[30], utc(2026, 10, 15, 4, 0));
        let ahead = day_bounds_ms(now, &tz, FOUR, 0, 15).unwrap();
        assert_eq!((ahead.len(), ahead[0]), (15, utc(2026, 10, 14, 4, 0)));
    }

    #[test]
    fn day_bounds_follow_daylight_saving_changes() {
        let tz = paris();
        // Around 2026-10-25 (clocks back): the day before is 25 hours long.
        let now = utc(2026, 10, 25, 12, 0);
        let bounds = day_bounds_ms(now, &tz, FOUR, -1, 3).unwrap();
        assert_eq!(
            bounds,
            vec![
                utc(2026, 10, 24, 2, 0),
                utc(2026, 10, 25, 3, 0),
                utc(2026, 10, 26, 3, 0),
            ]
        );
        // And 2026-03-29 (clocks forward): a 23-hour day.
        let now = utc(2026, 3, 29, 12, 0);
        let bounds = day_bounds_ms(now, &tz, FOUR, -1, 3).unwrap();
        assert_eq!(bounds[1] - bounds[0], 23 * 3_600_000);
        assert_eq!(bounds[2] - bounds[1], 24 * 3_600_000);
    }

    #[test]
    fn day_bounds_increase_for_every_start_hour_across_changes() {
        for tz in [paris(), glib::TimeZone::utc()] {
            for hour in 0..24 {
                for now in [utc(2026, 3, 29, 12, 0), utc(2026, 10, 25, 12, 0)] {
                    let bounds = day_bounds_ms(now, &tz, hour, -29, 60).unwrap();
                    assert_eq!(bounds.len(), 60);
                    assert!(bounds.windows(2).all(|w| w[0] < w[1]), "hour {hour}");
                }
            }
        }
    }

    #[test]
    fn local_day_bounds_surround_now() {
        let now = rappel_core::time::now_ms();
        let bounds = local_day_bounds_ms(now, FOUR, -29, 31);
        assert_eq!(bounds.len(), 31);
        assert!(bounds[29] <= now && now < bounds[30]);
    }
}
