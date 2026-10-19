//! What the statistics page says and draws, as pure functions of the
//! numbers (unit-tested; the page only puts the results in widgets). Day
//! names come in through [`DayNames`], so the tests need no time zone.

use crate::i18n::{fill, gettext, ngettext};
use crate::state::StatsData;
use crate::ui::text::{counted, group_digits, percent, studied_text};
use crate::ui::tile::Tile;
use crate::widgets::bar_chart::ChartBar;
use crate::widgets::bar_chart::layout::{Bar, nice_ceiling};
use crate::widgets::chart_card::ChartModel;

/// How a day, given by when it begins, is named.
pub struct DayNames<'a> {
    /// "17 Sep", on an axis.
    pub short: &'a dyn Fn(i64) -> String,
    /// "Tue 17 Sep", for a selected day.
    pub with_weekday: &'a dyn Fn(i64) -> String,
    /// "17 September", in a summary.
    pub long: &'a dyn Fn(i64) -> String,
}

pub fn reviews_title() -> String {
    gettext("Reviews per Day")
}

pub fn time_title() -> String {
    gettext("Time Studied per Day")
}

pub fn forecast_title() -> String {
    gettext("Due Forecast")
}

const NO_DATA: &str = "—";

/// The label of the empty state's button, which goes back to the page the
/// statistics were opened from: a deck's page, or the deck list.
pub fn back_label(one_deck: bool) -> String {
    if one_deck {
        gettext("_Back to Deck")
    } else {
        gettext("_Back to Decks")
    }
}

/// The three headline numbers.
pub fn headline(data: &StatsData) -> [Tile; 3] {
    let stats = &data.stats;
    let tile = |value: Option<String>, caption: String, none: String| {
        let shown = value.clone().unwrap_or_else(|| NO_DATA.to_owned());
        let said = value.unwrap_or(none);
        Tile {
            value: shown,
            // Translators: what a screen reader says for a headline figure:
            // {caption} is "Pass rate, last 30 days", {value} "91%".
            accessible: fill(
                &gettext("{caption}: {value}"),
                &[("caption", &caption), ("value", &said)],
            ),
            caption,
        }
    };
    let reviews = total_u32(stats.days.iter().map(|d| d.reviews));
    let studied_ms = total_ms(stats.days.iter().map(|d| d.duration_ms));
    [
        tile(
            stats.expected_retention.map(percent),
            gettext("Expected retention"),
            gettext("no reviewed cards"),
        ),
        tile(
            stats.pass_rate.map(percent),
            gettext("Pass rate, last 30 days"),
            gettext("no reviews"),
        ),
        tile(
            (reviews > 0).then(|| studied_text(studied_ms)),
            gettext("Time studied, last 30 days"),
            gettext("no reviews"),
        ),
    ]
}

/// Whether there is nothing at all to show: no review in the window, no
/// scheduled card and nothing due. The page is then a status page.
pub fn nothing_to_show(data: &StatsData) -> bool {
    let stats = &data.stats;
    stats.scheduled_cards == 0
        && stats.days.iter().all(|d| d.reviews == 0)
        && stats.forecast.iter().all(|&n| n == 0)
}

/// The sum of `counts`, stopping at `u32::MAX`: a day's counts come from
/// the database and may each be as large as that (ADR 0050, row 23), and a
/// plain sum would overflow.
fn total_u32(counts: impl IntoIterator<Item = u32>) -> u32 {
    counts.into_iter().fold(0, u32::saturating_add)
}

/// The sum of `durations` in milliseconds, stopping at the ends of `i64`,
/// as [`total_u32`] does.
fn total_ms(durations: impl IntoIterator<Item = i64>) -> i64 {
    durations.into_iter().fold(0, i64::saturating_add)
}

/// "3 reviews", "1 review".
fn reviews(n: u32) -> String {
    let n = n as usize;
    // Translators: {count} is a number of reviews (answers given to cards).
    counted(&ngettext("{count} review", "{count} reviews", n), n)
}

/// A count from the statistics, grouped.
fn number(n: u32) -> String {
    group_digits(n as usize)
}

/// "Up to 40", the top of a chart's scale.
fn scale_text(ceiling: f64) -> String {
    // Translators: the top of a chart's scale; {value} is a number.
    fill(
        &gettext("Up to {value}"),
        &[("value", &group_digits(whole(ceiling)))],
    )
}

/// The day with the largest value, the earliest of equal ones; `None` if
/// every value is zero.
fn busiest(values: impl IntoIterator<Item = f64>) -> Option<(usize, f64)> {
    values
        .into_iter()
        .enumerate()
        .filter(|&(_, v)| v > 0.0)
        .fold(None, |best: Option<(usize, f64)>, (i, v)| match best {
            Some((_, b)) if b >= v => best,
            _ => Some((i, v)),
        })
}

/// A chart value as a whole number for display (values are counts, or
/// minutes of one day, so far below the limit).
fn whole(value: f64) -> usize {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and clamped to 0..=1e9 first"
    )]
    let n = value.round().clamp(0.0, 1e9) as usize;
    n
}

/// Milliseconds as minutes (one day is far below `u32::MAX` ms; more is
/// clamped).
fn minutes(ms: i64) -> f64 {
    let ms = u32::try_from(ms.clamp(0, i64::from(u32::MAX))).unwrap_or(0);
    f64::from(ms) / 60_000.0
}

/// The long name of day `i`, empty if there is no such day.
fn day_name(starts: &[i64], i: usize, names: &DayNames<'_>) -> String {
    starts.get(i).map_or_else(String::new, |&s| (names.long)(s))
}

fn ceiling_of(bars: &[ChartBar]) -> f64 {
    nice_ceiling(bars.iter().map(|b| b.bar.total()).fold(0.0, f64::max))
}

/// Reviews per day over the past 30 days: passes solid, fails hatched.
pub fn reviews_chart(data: &StatsData, names: &DayNames<'_>) -> ChartModel {
    let days = &data.stats.days;
    let bars: Vec<ChartBar> = days
        .iter()
        .zip(&data.past_starts)
        .map(|(day, &start)| {
            let failed = day.reviews.saturating_sub(day.passes);
            let when = (names.with_weekday)(start);
            let caption = if day.reviews == 0 {
                // Translators: a chart's day; {day} is "Tue 14 Oct".
                fill(&gettext("{day}: no reviews"), &[("day", &when)])
            } else {
                fill(
                    // Translators: a chart's day; {day} is "Tue 14 Oct",
                    // {reviews} "12 reviews", {passed} and {failed} numbers.
                    &gettext("{day}: {reviews}, {passed} passed, {failed} failed"),
                    &[
                        ("day", &when),
                        ("reviews", &reviews(day.reviews)),
                        ("passed", &number(day.passes)),
                        ("failed", &number(failed)),
                    ],
                )
            };
            ChartBar {
                bar: Bar {
                    solid: f64::from(day.passes),
                    hatched: f64::from(failed),
                },
                caption,
            }
        })
        .collect();
    let total = total_u32(days.iter().map(|d| d.reviews));
    let passed = total_u32(days.iter().map(|d| d.passes));
    let summary = match busiest(days.iter().map(|d| f64::from(d.reviews))) {
        None => gettext("Last 30 days: no reviews."),
        Some((i, most)) => fill(
            // Translators: a chart's summary for screen readers: {reviews}
            // is "120 reviews", {passed} a number, {day} "14 October",
            // {most} a number of reviews.
            &gettext("Last 30 days: {reviews}, {passed} passed, busiest day {day} with {most}."),
            &[
                ("reviews", &reviews(total)),
                ("passed", &number(passed)),
                ("day", &day_name(&data.past_starts, i, names)),
                ("most", &group_digits(whole(most))),
            ],
        ),
    };
    let ceiling = ceiling_of(&bars);
    ChartModel {
        scale: scale_text(ceiling),
        summary,
        first_day: data
            .past_starts
            .first()
            .map_or_else(String::new, |&s| (names.short)(s)),
        last_day: gettext("Today"),
        empty: (total == 0).then(|| gettext("No reviews in the last 30 days")),
        note: None,
        bars,
        ceiling,
    }
}

/// Time studied per day over the past 30 days, in minutes.
pub fn time_chart(data: &StatsData, names: &DayNames<'_>) -> ChartModel {
    let days = &data.stats.days;
    let bars: Vec<ChartBar> = days
        .iter()
        .zip(&data.past_starts)
        .map(|(day, &start)| {
            let when = (names.with_weekday)(start);
            let caption = if day.duration_ms <= 0 {
                // Translators: a chart's day; {day} is "Tue 14 Oct".
                fill(&gettext("{day}: no time studied"), &[("day", &when)])
            } else {
                fill(
                    // Translators: a chart's day; {day} is "Tue 14 Oct",
                    // {time} "12 min".
                    &gettext("{day}: {time} studied"),
                    &[("day", &when), ("time", &studied_text(day.duration_ms))],
                )
            };
            ChartBar {
                bar: Bar {
                    solid: minutes(day.duration_ms),
                    hatched: 0.0,
                },
                caption,
            }
        })
        .collect();
    let total = total_ms(days.iter().map(|d| d.duration_ms));
    let summary = match busiest(days.iter().map(|d| minutes(d.duration_ms))) {
        None => gettext("Last 30 days: no time studied."),
        Some((i, _)) => fill(
            // Translators: a chart's summary for screen readers: {time} is
            // "3 h 12 min", {day} "14 October", {most} "25 min".
            &gettext("Last 30 days: {time} studied, longest day {day} with {most}."),
            &[
                ("time", &studied_text(total)),
                ("day", &day_name(&data.past_starts, i, names)),
                ("most", &studied_text(days[i].duration_ms)),
            ],
        ),
    };
    let ceiling = ceiling_of(&bars);
    ChartModel {
        scale: fill(
            // Translators: the top of a chart's scale; {value} is a number
            // of minutes.
            &gettext("Up to {value} min"),
            &[("value", &group_digits(whole(ceiling)))],
        ),
        summary,
        first_day: data
            .past_starts
            .first()
            .map_or_else(String::new, |&s| (names.short)(s)),
        last_day: gettext("Today"),
        empty: (total <= 0).then(|| gettext("No time studied in the last 30 days")),
        note: None,
        bars,
        ceiling,
    }
}

/// Cards due on each of the next 14 days; today also holds the overdue ones.
pub fn forecast_chart(data: &StatsData, names: &DayNames<'_>) -> ChartModel {
    let stats = &data.stats;
    let cards = |n: u32| {
        let n = n as usize;
        // Translators: {count} is a number of cards due on a day.
        counted(&ngettext("{count} card due", "{count} cards due", n), n)
    };
    let bars: Vec<ChartBar> = stats
        .forecast
        .iter()
        .zip(&data.ahead_starts)
        .enumerate()
        .map(|(i, (&n, &start))| {
            let when = if i == 0 {
                gettext("Today")
            } else {
                (names.with_weekday)(start)
            };
            let caption = if n == 0 {
                // Translators: a chart's day; {day} is "Tue 14 Oct".
                fill(&gettext("{day}: nothing due"), &[("day", &when)])
            } else if i == 0 && stats.overdue > 0 {
                fill(
                    // Translators: a chart's day; {day} is "Today", {cards}
                    // "12 cards due", {overdue} a number.
                    &gettext("{day}: {cards}, {overdue} of them overdue"),
                    &[
                        ("day", &when),
                        ("cards", &cards(n)),
                        ("overdue", &number(stats.overdue)),
                    ],
                )
            } else {
                // Translators: a chart's day; {day} is "Tue 14 Oct", {cards}
                // "12 cards due".
                fill(
                    &gettext("{day}: {cards}"),
                    &[("day", &when), ("cards", &cards(n))],
                )
            };
            ChartBar {
                bar: Bar {
                    solid: f64::from(n),
                    hatched: 0.0,
                },
                caption,
            }
        })
        .collect();
    let total = total_u32(stats.forecast.iter().copied());
    let summary = if total == 0 {
        gettext("Next 14 days: nothing due.")
    } else {
        let (i, most) = busiest(stats.forecast.iter().map(|&n| f64::from(n))).unwrap_or((0, 0.0));
        let day = if i == 0 {
            gettext("today")
        } else {
            day_name(&data.ahead_starts, i, names)
        };
        let values = [
            ("cards", cards(total)),
            ("overdue", number(stats.overdue)),
            ("day", day),
            ("most", group_digits(whole(most))),
        ];
        let values: Vec<(&str, &str)> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
        if stats.overdue > 0 {
            fill(
                // Translators: a chart's summary for screen readers:
                // {cards} is "40 cards due", {overdue} a number, {day}
                // "today" or "14 October", {most} a number of cards.
                &gettext(
                    "Next 14 days: {cards}, {overdue} of them overdue, busiest day {day} with {most}.",
                ),
                &values,
            )
        } else {
            fill(
                // Translators: a chart's summary for screen readers:
                // {cards} is "40 cards due", {day} "today" or "14
                // October", {most} a number of cards.
                &gettext("Next 14 days: {cards}, busiest day {day} with {most}."),
                &values,
            )
        }
    };
    let ceiling = ceiling_of(&bars);
    ChartModel {
        scale: scale_text(ceiling),
        summary,
        first_day: gettext("Today"),
        last_day: data
            .ahead_starts
            .last()
            .map_or_else(String::new, |&s| (names.short)(s)),
        empty: (total == 0).then(|| gettext("Nothing due in the next 14 days")),
        note: (stats.overdue > 0).then(|| {
            let n = stats.overdue as usize;
            counted(
                // Translators: under the forecast chart; {count} is a
                // number of cards.
                &ngettext(
                    "{count} overdue card is counted under Today",
                    "{count} overdue cards are counted under Today",
                    n,
                ),
                n,
            )
        }),
        bars,
        ceiling,
    }
}

#[cfg(test)]
mod tests {
    use rappel_core::stats::{DayActivity, Stats};

    use super::*;

    fn names() -> DayNames<'static> {
        DayNames {
            short: &|t| format!("d{t}"),
            with_weekday: &|t| format!("Day {t}"),
            long: &|t| format!("day {t}"),
        }
    }

    fn day(reviews: u32, passes: u32, duration_ms: i64) -> DayActivity {
        DayActivity {
            reviews,
            passes,
            duration_ms,
        }
    }

    /// 30 past days, starting at 100, and 14 days ahead, starting at 1000.
    fn data(days: Vec<DayActivity>, forecast: Vec<u32>, overdue: u32) -> StatsData {
        let n = i64::try_from(days.len()).unwrap();
        let a = i64::try_from(forecast.len()).unwrap();
        let scheduled = total_u32(forecast.iter().copied());
        StatsData {
            stats: Stats {
                days,
                pass_rate: None,
                forecast,
                overdue,
                scheduled_cards: scheduled,
                expected_retention: None,
            },
            past_starts: (0..n).map(|i| 100 + i).collect(),
            ahead_starts: (0..a).map(|i| 1000 + i).collect(),
        }
    }

    fn quiet() -> StatsData {
        data(vec![day(0, 0, 0); 30], vec![0; 14], 0)
    }

    #[test]
    fn the_empty_state_goes_back_where_it_came_from() {
        assert_eq!(back_label(true), "_Back to Deck");
        assert_eq!(back_label(false), "_Back to Decks");
    }

    #[test]
    fn the_headline_shows_dashes_without_data() {
        let tiles = headline(&quiet());
        assert_eq!(tiles[0].value, "—");
        assert_eq!(tiles[0].accessible, "Expected retention: no reviewed cards");
        assert_eq!(tiles[1].accessible, "Pass rate, last 30 days: no reviews");
        assert_eq!(tiles[2].value, "—");
        assert_eq!(
            tiles[2].accessible,
            "Time studied, last 30 days: no reviews"
        );
    }

    #[test]
    fn the_headline_shows_percentages_and_time() {
        let mut d = quiet();
        d.stats.expected_retention = Some(0.912);
        d.stats.pass_rate = Some(0.874);
        d.stats.days[3] = day(10, 9, 3_600_000);
        d.stats.days[29] = day(2, 2, 12 * 60_000);
        let tiles = headline(&d);
        assert_eq!(tiles[0].value, "91%");
        assert_eq!(tiles[1].value, "87%");
        assert_eq!(tiles[2].value, "1 h 12 min");
        assert_eq!(
            tiles[2].accessible,
            "Time studied, last 30 days: 1 h 12 min"
        );
    }

    #[test]
    fn nothing_to_show_needs_no_reviews_no_schedule_and_nothing_due() {
        assert!(nothing_to_show(&quiet()));
        let mut d = quiet();
        d.stats.days[0] = day(1, 1, 5_000);
        assert!(!nothing_to_show(&d));
        let mut d = quiet();
        d.stats.scheduled_cards = 1;
        assert!(!nothing_to_show(&d));
        let mut d = quiet();
        d.stats.forecast[4] = 2;
        assert!(!nothing_to_show(&d));
    }

    #[test]
    fn the_reviews_summary_names_the_busiest_day() {
        let mut days = vec![day(0, 0, 0); 30];
        days[2] = day(38, 35, 0);
        days[9] = day(38, 30, 0);
        days[29] = day(5, 5, 0);
        let m = reviews_chart(&data(days, vec![0; 14], 0), &names());
        // 81 reviews, 70 passed; the earliest of the equal busiest days.
        assert_eq!(
            m.summary,
            "Last 30 days: 81 reviews, 70 passed, busiest day day 102 with 38."
        );
        assert_eq!(m.empty, None);
        assert_eq!(m.ceiling, 50.0);
        assert_eq!(m.scale, "Up to 50");
        assert_eq!(m.first_day, "d100");
        assert_eq!(m.last_day, "Today");
        assert_eq!(m.bars.len(), 30);
        assert_eq!(
            m.bars[2].caption,
            "Day 102: 38 reviews, 35 passed, 3 failed"
        );
        assert_eq!(
            m.bars[2].bar,
            Bar {
                solid: 35.0,
                hatched: 3.0
            }
        );
        assert_eq!(m.bars[0].caption, "Day 100: no reviews");
    }

    #[test]
    fn one_review_is_singular_and_an_empty_window_is_said() {
        let mut days = vec![day(0, 0, 0); 30];
        days[5] = day(1, 0, 4_000);
        let m = reviews_chart(&data(days, vec![0; 14], 0), &names());
        assert_eq!(m.bars[5].caption, "Day 105: 1 review, 0 passed, 1 failed");
        assert_eq!(
            m.summary,
            "Last 30 days: 1 review, 0 passed, busiest day day 105 with 1."
        );
        let m = reviews_chart(&quiet(), &names());
        assert_eq!(m.summary, "Last 30 days: no reviews.");
        assert_eq!(m.empty.as_deref(), Some("No reviews in the last 30 days"));
        assert_eq!(m.ceiling, 1.0);
    }

    #[test]
    fn the_time_chart_is_in_minutes() {
        let mut days = vec![day(0, 0, 0); 30];
        days[1] = day(10, 8, 90_000);
        days[7] = day(40, 30, 25 * 60_000);
        let m = time_chart(&data(days, vec![0; 14], 0), &names());
        assert_eq!(m.bars[7].bar.solid, 25.0);
        assert_eq!(m.bars[1].caption, "Day 101: 1 min studied");
        assert_eq!(m.bars[0].caption, "Day 100: no time studied");
        assert_eq!(
            m.summary,
            "Last 30 days: 26 min studied, longest day day 107 with 25 min."
        );
        assert_eq!(m.scale, "Up to 50 min");
        let m = time_chart(&quiet(), &names());
        assert_eq!(m.summary, "Last 30 days: no time studied.");
        assert_eq!(
            m.empty.as_deref(),
            Some("No time studied in the last 30 days")
        );
    }

    #[test]
    fn the_forecast_counts_overdue_cards_under_today() {
        let mut forecast = vec![0; 14];
        forecast[0] = 12;
        forecast[5] = 40;
        forecast[6] = 3;
        let m = forecast_chart(&data(vec![day(0, 0, 0); 30], forecast, 9), &names());
        assert_eq!(
            m.summary,
            "Next 14 days: 55 cards due, 9 of them overdue, busiest day day 1005 with 40."
        );
        assert_eq!(m.bars[0].caption, "Today: 12 cards due, 9 of them overdue");
        assert_eq!(m.bars[5].caption, "Day 1005: 40 cards due");
        assert_eq!(m.bars[1].caption, "Day 1001: nothing due");
        assert_eq!(
            m.note.as_deref(),
            Some("9 overdue cards are counted under Today")
        );
        assert_eq!(
            (m.first_day.as_str(), m.last_day.as_str()),
            ("Today", "d1013")
        );
    }

    #[test]
    fn the_forecast_busiest_day_can_be_today_and_can_be_empty() {
        let mut forecast = vec![0; 14];
        forecast[0] = 1;
        let m = forecast_chart(&data(vec![day(0, 0, 0); 30], forecast, 1), &names());
        assert_eq!(
            m.summary,
            "Next 14 days: 1 card due, 1 of them overdue, busiest day today with 1."
        );
        assert_eq!(
            m.note.as_deref(),
            Some("1 overdue card is counted under Today")
        );
        let m = forecast_chart(&quiet(), &names());
        assert_eq!(m.summary, "Next 14 days: nothing due.");
        assert_eq!(m.empty.as_deref(), Some("Nothing due in the next 14 days"));
        assert_eq!(m.note, None);
    }

    /// Counts and durations as large as the database can hold, negative
    /// durations and far dates: nothing panics or overflows, and every
    /// number stays sane (ADR 0050, rows 23 and 34).
    #[test]
    fn hostile_numbers_never_panic_and_stay_sane() {
        let hostile = [
            day(u32::MAX, u32::MAX, i64::MAX),
            day(u32::MAX, 0, i64::MIN),
            day(5, 9, -1),
        ];
        let mut days: Vec<DayActivity> = hostile.iter().copied().cycle().take(30).collect();
        days[0] = day(u32::MAX, u32::MAX, i64::MAX);
        let mut all = data(days, vec![u32::MAX; 14], u32::MAX);
        all.stats.pass_rate = Some(f64::NAN);
        all.stats.expected_retention = Some(f64::INFINITY);
        all.past_starts = vec![i64::MIN; 30];
        all.ahead_starts = vec![i64::MAX; 14];
        let names = DayNames {
            short: &crate::ui::time::day_short,
            with_weekday: &crate::ui::time::day_with_weekday,
            long: &crate::ui::time::day_long,
        };
        let tiles = headline(&all);
        for tile in &tiles {
            assert!(
                !tile.value.contains("NaN") && !tile.value.contains("inf"),
                "{tile:?}"
            );
        }
        for chart in [
            reviews_chart(&all, &names),
            time_chart(&all, &names),
            forecast_chart(&all, &names),
        ] {
            assert!(chart.ceiling.is_finite() && chart.ceiling >= 1.0);
            for bar in &chart.bars {
                assert!(bar.bar.solid.is_finite() && bar.bar.solid >= 0.0);
                assert!(bar.bar.hatched.is_finite() && bar.bar.hatched >= 0.0);
            }
            assert!(!chart.summary.contains('-'), "{}", chart.summary);
            assert!(chart.summary.len() < 300, "{}", chart.summary);
        }
        assert!(
            reviews_chart(&all, &names)
                .summary
                .starts_with("Last 30 days: 4,294,967,295 reviews")
        );
    }

    #[test]
    fn every_caption_is_one_per_day() {
        let d = quiet();
        assert_eq!(reviews_chart(&d, &names()).bars.len(), 30);
        assert_eq!(time_chart(&d, &names()).bars.len(), 30);
        assert_eq!(forecast_chart(&d, &names()).bars.len(), 14);
    }
}
