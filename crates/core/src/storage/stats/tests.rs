use std::time::Duration;

use proptest::prelude::*;

use super::*;
use crate::Error;
use crate::domain::{CardState, EventKind, Grade, ReviewEvent};
use crate::scheduling::{Scheduler, surviving};
use crate::stats::{self, DayActivity, HistoryEntry};
use crate::test_support::{DAY_MS, T0, assert_cpu_within, fastest};

/// "Now" in these tests: 40 days after the first review of the property
/// tests, so some of their reviews are in the 30-day window and some not.
const NOW: i64 = T0 + 40 * DAY_MS;

/// The 31 bounds of the 30 days ending half a day after [`NOW`].
fn past() -> DayGrid {
    let end = NOW + DAY_MS / 2;
    DayGrid::new((0..=30).map(|i| end - (30 - i) * DAY_MS).collect()).unwrap()
}

/// The 15 bounds of the 14 days from the start of the day of [`NOW`].
fn ahead() -> DayGrid {
    let start = NOW - DAY_MS / 2;
    DayGrid::new((0..=14).map(|i| start + i * DAY_MS).collect()).unwrap()
}

fn stats_of(store: &Store, scope: Scope<'_>) -> Stats {
    store.stats(scope, NOW, &past(), &ahead()).unwrap()
}

/// Equal, the mean retention within rounding (a sum in another order).
fn assert_same(a: &Stats, b: &Stats) {
    let (ra, rb) = (a.expected_retention, b.expected_retention);
    match (ra, rb) {
        (Some(x), Some(y)) => assert!((x - y).abs() < 1e-9, "{x} vs {y}"),
        (x, y) => assert_eq!(x, y),
    }
    assert_eq!(
        Stats {
            expected_retention: None,
            ..a.clone()
        },
        Stats {
            expected_retention: None,
            ..b.clone()
        }
    );
}

fn answer(store: &mut Store, card: &str, grade: Grade, at: i64, secs: u64) -> ReviewEvent {
    store
        .record_review(card, grade, Duration::from_secs(secs), at)
        .unwrap()
}

fn store_with_decks(decks: usize, cards_per_deck: usize) -> (Store, Vec<String>, Vec<String>) {
    let mut store = Store::in_memory();
    let deck_ids: Vec<String> = (0..decks)
        .map(|i| store.create_deck_at(&format!("Deck {i}"), T0).unwrap().id)
        .collect();
    let mut cards = Vec::new();
    for i in 0..cards_per_deck {
        for deck in &deck_ids {
            cards.push(
                store
                    .add_card_at(deck, &format!("q{i}"), "a", T0)
                    .unwrap()
                    .id,
            );
        }
    }
    (store, deck_ids, cards)
}

#[test]
fn an_empty_store_has_empty_stats() {
    let store = Store::in_memory();
    let s = stats_of(&store, Scope::All);
    assert_eq!(s.days.len(), 30);
    assert!(s.days.iter().all(|d| d.reviews == 0));
    assert_eq!((s.pass_rate, s.expected_retention), (None, None));
    assert_eq!(
        (s.forecast, s.overdue, s.scheduled_cards),
        (vec![0; 14], 0, 0)
    );
}

#[test]
fn a_missing_deck_is_refused() {
    let store = Store::in_memory();
    let r = store.stats(Scope::Deck("nope"), NOW, &past(), &ahead());
    assert!(matches!(r, Err(Error::DeckNotFound(_))));
}

#[test]
fn reviews_time_pass_rate_and_forecast_of_a_small_deck() {
    let (mut store, decks, cards) = store_with_decks(1, 2);
    let day = NOW - 2 * DAY_MS;
    // Outside the window: 31 days back (first, as a review is never dated
    // before the card's last entry).
    answer(&mut store, &cards[1], Grade::Pass, NOW - 31 * DAY_MS, 1);
    answer(&mut store, &cards[0], Grade::Pass, day, 4);
    answer(&mut store, &cards[0], Grade::Pass, day + 600_000, 6);
    answer(&mut store, &cards[1], Grade::Fail, day + 1, 10);
    let s = stats_of(&store, Scope::Deck(&decks[0]));
    let d = s.days[27];
    assert_eq!((d.reviews, d.passes, d.duration_ms), (3, 2, 20_000));
    assert_eq!(s.days.iter().map(|d| d.reviews).sum::<u32>(), 3);
    assert_eq!(s.pass_rate, Some(2.0 / 3.0));
    assert_eq!(s.scheduled_cards, 2);
    // Both cards are overdue (a few days old), the passed one by FSRS days.
    assert!(s.overdue >= 1 && s.forecast[0] >= s.overdue);
    assert!(s.expected_retention.unwrap() < 1.0);
}

#[test]
fn deleted_cards_and_decks_do_not_count() {
    let (mut store, decks, cards) = store_with_decks(2, 1);
    for card in &cards {
        answer(&mut store, card, Grade::Pass, NOW - DAY_MS, 3);
    }
    let both = stats_of(&store, Scope::All);
    assert_eq!(both.days[28].reviews, 2);
    store.delete_card(&cards[0]).unwrap();
    assert_eq!(stats_of(&store, Scope::All).days[28].reviews, 1);
    store.delete_deck(&decks[1]).unwrap();
    let none = stats_of(&store, Scope::All);
    assert_eq!((none.days[28].reviews, none.scheduled_cards), (0, 0));
    assert!(matches!(
        store.stats(Scope::Deck(&decks[1]), NOW, &past(), &ahead()),
        Err(Error::DeckNotFound(_))
    ));
}

fn review_totals(s: &Stats) -> (u32, u32, i64) {
    s.days.iter().fold((0, 0, 0), |a, d| {
        (a.0 + d.reviews, a.1 + d.passes, a.2 + d.duration_ms)
    })
}

#[test]
fn a_reset_does_not_change_the_past_days_but_an_undo_does() {
    let (mut store, _, cards) = store_with_decks(1, 1);
    let at = NOW - 3 * DAY_MS;
    answer(&mut store, &cards[0], Grade::Pass, at, 2);
    answer(&mut store, &cards[0], Grade::Fail, at + 1, 3);
    let before = stats_of(&store, Scope::All);
    assert_eq!(review_totals(&before), (2, 1, 5_000));
    assert_eq!(before.scheduled_cards, 1);

    // The reset restarts the memory side only.
    store.reset_card(&cards[0], at + 2).unwrap();
    let reset = stats_of(&store, Scope::All);
    assert_eq!(reset.days, before.days);
    assert_eq!(reset.pass_rate, before.pass_rate);
    assert_eq!(
        (
            reset.scheduled_cards,
            reset.expected_retention,
            reset.overdue
        ),
        (0, None, 0)
    );
    assert_eq!(reset.forecast, vec![0; 14]);

    // Reviews after the reset add to the activity.
    answer(&mut store, &cards[0], Grade::Pass, at + 3, 4);
    let after = stats_of(&store, Scope::All);
    assert_eq!(review_totals(&after), (3, 2, 9_000));
    assert_eq!(after.scheduled_cards, 1);

    // An undo removes the review everywhere.
    let log = store.review_events(&cards[0]).unwrap();
    let last = surviving(&log).unwrap().last().unwrap().id.clone();
    store.undo(&cards[0], &last, at + 4).unwrap();
    let undone = stats_of(&store, Scope::All);
    assert_eq!(review_totals(&undone), (2, 1, 5_000));
    assert_eq!(undone.scheduled_cards, 0);
}

#[test]
fn undoing_the_latest_reset_brings_the_memory_back_and_keeps_the_activity() {
    let (mut store, _, cards) = store_with_decks(1, 1);
    let total = |store: &Store| review_totals(&stats_of(store, Scope::All)).0;
    let at = NOW - 3 * DAY_MS;
    answer(&mut store, &cards[0], Grade::Pass, at, 1);
    let reset = store.reset_card(&cards[0], at + 1).unwrap();
    assert_eq!(total(&store), 1);
    let info = store.card_info(&cards[0], NOW).unwrap();
    assert_eq!(
        (info.reviews, info.history.len(), info.next_due),
        (0, 2, None)
    );
    store.undo(&cards[0], &reset.id, at + 2).unwrap();
    assert_eq!(total(&store), 1);
    let info = store.card_info(&cards[0], NOW).unwrap();
    assert_eq!((info.reviews, info.history.len()), (1, 1));
    assert!(info.next_due.is_some());
}

#[test]
fn card_info_of_a_live_card_matches_its_cache() {
    let (mut store, _, cards) = store_with_decks(1, 1);
    let c = &cards[0];
    answer(&mut store, c, Grade::Pass, T0, 2);
    answer(&mut store, c, Grade::Pass, T0 + 600_000, 3);
    answer(&mut store, c, Grade::Fail, T0 + 20 * DAY_MS, 9);
    let info = store.card_info(c, NOW).unwrap();
    let CardState::Scheduled(s) = store.card_state(c).unwrap() else {
        panic!("scheduled")
    };
    assert_eq!((info.reviews, info.passes, info.lapses), (3, 2, 1));
    assert_eq!(info.next_due, Some(s.due_at));
    assert_eq!(info.stability, Some(s.stability));
    assert_eq!(info.difficulty, Some(s.difficulty));
    assert_eq!(info.history.len(), 3);
    assert!(matches!(
        info.history[2],
        HistoryEntry::Review {
            grade: Grade::Fail,
            duration_ms: 9_000,
            ..
        }
    ));
    assert_eq!(store.card(c).unwrap().created_at, info.created_at);
}

#[test]
fn card_info_refuses_a_deleted_or_missing_card() {
    let (mut store, _, cards) = store_with_decks(1, 1);
    store.delete_card(&cards[0]).unwrap();
    assert!(matches!(
        store.card_info(&cards[0], NOW),
        Err(Error::CardNotFound(_))
    ));
    assert!(matches!(
        store.card_info("nope", NOW),
        Err(Error::CardNotFound(_))
    ));
}

// --- random logs ---

#[derive(Debug, Clone)]
enum Op {
    Review {
        card: usize,
        pass: bool,
        minutes: i64,
        secs: u64,
    },
    Undo {
        card: usize,
        minutes: i64,
    },
    Reset {
        card: usize,
        minutes: i64,
    },
    DeleteCard {
        card: usize,
    },
}

fn op(cards: usize) -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..cards, any::<bool>(), 1_i64..2_000, 0_u64..60)
            .prop_map(|(card, pass, minutes, secs)| Op::Review { card, pass, minutes, secs }),
        2 => (0..cards, 1_i64..2_000).prop_map(|(card, minutes)| Op::Undo { card, minutes }),
        1 => (0..cards, 1_i64..2_000).prop_map(|(card, minutes)| Op::Reset { card, minutes }),
        1 => (0..cards).prop_map(|card| Op::DeleteCard { card }),
    ]
}

/// Applies `op`; an operation the store refuses (a deleted card, nothing to
/// undo) is skipped, as the app would show an error and change nothing.
fn apply(store: &mut Store, cards: &[String], clock: &mut i64, op: &Op) {
    match *op {
        Op::Review {
            card,
            pass,
            minutes,
            secs,
        } => {
            *clock += minutes * 60_000;
            let grade = if pass { Grade::Pass } else { Grade::Fail };
            store
                .record_review(&cards[card], grade, Duration::from_secs(secs), *clock)
                .ok();
        }
        Op::Undo { card, minutes } => {
            *clock += minutes * 60_000;
            let log = store.review_events(&cards[card]).unwrap();
            if let Some(last) = surviving(&log).unwrap().last() {
                store.undo(&cards[card], &last.id.clone(), *clock).ok();
            }
        }
        Op::Reset { card, minutes } => {
            *clock += minutes * 60_000;
            store.reset_card(&cards[card], *clock).ok();
        }
        Op::DeleteCard { card } => {
            store.delete_card(&cards[card]).ok();
        }
    }
}

fn run(ops: &[Op]) -> (Store, Vec<String>, Vec<String>, i64) {
    let (mut store, decks, cards) = store_with_decks(3, 2);
    let mut clock = T0;
    for o in ops {
        apply(&mut store, &cards, &mut clock, o);
    }
    (store, decks, cards, clock)
}

/// The statistics computed from the logs with the pure functions only:
/// the oracle for the SQL.
fn oracle(store: &Store, deck: Option<&str>) -> Stats {
    let scheduler = Scheduler::default();
    let (mut reviews, mut cards) = (Vec::new(), Vec::new());
    for d in store.decks().unwrap() {
        if deck.is_some_and(|id| id != d.id) {
            continue;
        }
        for card in store.list_cards(&d.id).unwrap() {
            let log = store.review_events(&card.id).unwrap();
            reviews.extend(
                stats::activity_reviews(&log)
                    .unwrap()
                    .into_iter()
                    .filter_map(stats::as_activity),
            );
            if let CardState::Scheduled(s) = scheduler.rebuild_state(&card.id, &log).unwrap() {
                cards.push(stats::ScheduledCard {
                    due_at: s.due_at,
                    stability: s.stability,
                    difficulty: s.difficulty,
                    last_review_at: s.last_review_at,
                });
            }
        }
    }
    Stats::compute(&past(), &ahead(), NOW, reviews, cards)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// The SQL equals the pure functions over the logs, globally and per
    /// deck, whatever reviews, undos, resets and deletions happened.
    #[test]
    fn sql_equals_the_pure_oracle(ops in prop::collection::vec(op(6), 0..120)) {
        let (store, decks, ..) = run(&ops);
        assert_same(&stats_of(&store, Scope::All), &oracle(&store, None));
        for deck in &decks {
            assert_same(&stats_of(&store, Scope::Deck(deck)), &oracle(&store, Some(deck)));
        }
    }

    /// The decks' counts add up to the global ones.
    #[test]
    fn per_deck_totals_sum_to_the_global_total(ops in prop::collection::vec(op(6), 0..120)) {
        let (store, decks, ..) = run(&ops);
        let all = stats_of(&store, Scope::All);
        let parts: Vec<Stats> = decks.iter().map(|d| stats_of(&store, Scope::Deck(d))).collect();
        for i in 0..30 {
            let sum = parts.iter().fold(DayActivity::default(), |a, p| DayActivity {
                reviews: a.reviews + p.days[i].reviews,
                passes: a.passes + p.days[i].passes,
                duration_ms: a.duration_ms + p.days[i].duration_ms,
            });
            prop_assert_eq!(sum, all.days[i]);
        }
        for i in 0..14 {
            prop_assert_eq!(parts.iter().map(|p| p.forecast[i]).sum::<u32>(), all.forecast[i]);
        }
        prop_assert_eq!(parts.iter().map(|p| p.overdue).sum::<u32>(), all.overdue);
        prop_assert_eq!(parts.iter().map(|p| p.scheduled_cards).sum::<u32>(), all.scheduled_cards);
        let weighted: f64 = parts
            .iter()
            .map(|p| p.expected_retention.unwrap_or(0.0) * f64::from(p.scheduled_cards))
            .sum();
        let expected = all.expected_retention.unwrap_or(0.0) * f64::from(all.scheduled_cards);
        prop_assert!((weighted - expected).abs() < 1e-6);
    }

    /// Undoing the latest review or reset leaves the statistics and the
    /// card's information exactly as they were before it.
    #[test]
    fn undoing_an_entry_changes_nothing(
        ops in prop::collection::vec(op(6), 0..80),
        card in 0_usize..6,
        review in any::<bool>(),
        pass in any::<bool>(),
    ) {
        let (mut store, decks, cards, clock) = run(&ops);
        prop_assume!(store.card(&cards[card]).is_ok());
        let before_all = stats_of(&store, Scope::All);
        let before_decks: Vec<Stats> = decks.iter().map(|d| stats_of(&store, Scope::Deck(d))).collect();
        let before_info = store.card_info(&cards[card], NOW).unwrap();
        let at = clock + 60_000;
        let entry = if review {
            let grade = if pass { Grade::Pass } else { Grade::Fail };
            store.record_review(&cards[card], grade, Duration::from_secs(5), at).unwrap()
        } else {
            store.reset_card(&cards[card], at).unwrap()
        };
        store.undo(&cards[card], &entry.id, at + 1).unwrap();
        assert_same(&before_all, &stats_of(&store, Scope::All));
        for (deck, before) in decks.iter().zip(&before_decks) {
            assert_same(before, &stats_of(&store, Scope::Deck(deck)));
        }
        let after_info = store.card_info(&cards[card], NOW).unwrap();
        prop_assert_eq!(before_info, after_info);
    }

    /// A card's information agrees with its cache and with the counted
    /// rule, and its history lists exactly the surviving entries.
    #[test]
    fn card_info_agrees_with_the_cache(ops in prop::collection::vec(op(6), 0..120)) {
        let (store, _, cards, _) = run(&ops);
        for card in cards.iter().filter(|c| store.card(c).is_ok()) {
            let info = store.card_info(card, NOW).unwrap();
            let log = store.review_events(card).unwrap();
            prop_assert_eq!(info.reviews as usize, stats::counted_reviews(&log).unwrap().len());
            prop_assert_eq!(info.history.len(), surviving(&log).unwrap().len());
            match store.card_state(card).unwrap() {
                CardState::New => prop_assert_eq!(info.next_due, None),
                CardState::Scheduled(s) => {
                    prop_assert_eq!(info.next_due, Some(s.due_at));
                    prop_assert_eq!(info.stability, Some(s.stability));
                    prop_assert_eq!(info.lapses, s.lapses);
                    prop_assert_eq!(info.difficulty, Some(s.difficulty));
                    prop_assert_eq!(
                        info.retrievability,
                        Some(stats::retrievability(s.stability, s.difficulty, s.last_review_at, NOW))
                    );
                }
            }
            // The history is the surviving entries, in order.
            for (entry, kept) in info.history.iter().zip(surviving(&log).unwrap()) {
                match (entry, &kept.kind) {
                    (HistoryEntry::Review { at, grade, duration_ms, interval_ms }, EventKind::Review(g)) => {
                        prop_assert_eq!((*at, *grade, *duration_ms), (kept.reviewed_at, *g, kept.duration_ms));
                        prop_assert!(*interval_ms >= 0);
                    }
                    (HistoryEntry::Reset { at }, EventKind::Reset) => prop_assert_eq!(*at, kept.reviewed_at),
                    other => prop_assert!(false, "{other:?}"),
                }
            }
        }
    }
}

// --- speed at 5,000 cards and 100,000 reviews ---

const CARDS: usize = 5_000;
const ROUNDS: i64 = 20;
const HEAVY: i64 = 2_000;

/// 5,000 cards in 5 decks with 20 reviews each (100,000), spread over the
/// 25 days before [`NOW`]; every 50th card reset after its 11th review,
/// every 25th card's 13th review undone; one card with 2,000 more.
fn big_store() -> (Store, Vec<String>, String) {
    let mut store = Store::in_memory();
    let decks: Vec<String> = (0..5)
        .map(|i| store.create_deck_at(&format!("Deck {i}"), T0).unwrap().id)
        .collect();
    let cards: Vec<String> = (0..CARDS)
        .map(|i| {
            store
                .add_card_at(&decks[i % 5], &format!("q{i}"), "a", T0)
                .unwrap()
                .id
        })
        .collect();
    let base = NOW - 25 * DAY_MS;
    let mut events = Vec::new();
    let mut ids = store.ids.clone();
    let mut push = |card: &str, at: i64, kind: EventKind, duration_ms: i64| -> String {
        let id = ids.next(at).unwrap();
        events.push(ReviewEvent {
            id: id.clone(),
            card_id: card.to_owned(),
            reviewed_at: at,
            kind,
            duration_ms,
        });
        id
    };
    for round in 0..ROUNDS {
        for (i, card) in cards.iter().enumerate() {
            let at = base + round * DAY_MS + i64::try_from(i).unwrap() * 1_000;
            let grade = if (i + usize::try_from(round).unwrap()) % 7 == 0 {
                Grade::Fail
            } else {
                Grade::Pass
            };
            let id = push(card, at, EventKind::Review(grade), 5_000);
            if round == 10 && i % 50 == 0 {
                push(card, at + 1, EventKind::Reset, 0);
            }
            if round == 12 && i % 25 == 0 {
                push(card, at + 1, EventKind::Undo { undoes: id }, 0);
            }
        }
    }
    for k in 0..HEAVY {
        push(
            &cards[1],
            base + ROUNDS * DAY_MS + k * 60_000,
            EventKind::Review(Grade::Pass),
            3_000,
        );
    }
    store.ids = ids;
    let tx = store.conn.transaction().unwrap();
    for e in &events {
        super::super::schedule::insert_event(&tx, e).unwrap();
    }
    tx.commit().unwrap();
    store.rebuild_schedule_cache(&Scheduler::default()).unwrap();
    let heavy = cards[1].clone();
    (store, decks, heavy)
}

#[test]
fn stats_are_fast_at_five_thousand_cards_and_a_hundred_thousand_reviews() {
    let (store, decks, heavy) = big_store();
    // 100,000 reviews and the heavy card's, less the 200 undone ones; the
    // 100 resets take nothing from the activity.
    let expected = 100_000 + HEAVY - 200;

    let (all_time, all) = fastest("stats, all decks", 3, || stats_of(&store, Scope::All));
    assert_eq!(
        all.days.iter().map(|d| i64::from(d.reviews)).sum::<i64>(),
        expected
    );
    assert_eq!(all.scheduled_cards, u32::try_from(CARDS).unwrap());
    let (deck_time, deck) = fastest("stats, one deck", 3, || {
        stats_of(&store, Scope::Deck(&decks[0]))
    });
    assert!(
        deck.days.iter().map(|d| d.reviews).sum::<u32>()
            < all.days.iter().map(|d| d.reviews).sum::<u32>()
    );
    let (card_time, info) = fastest("card_info, 2,000 reviews", 3, || {
        store.card_info(&heavy, NOW).unwrap()
    });
    assert!(info.reviews >= 2_000);

    assert_cpu_within(all_time, LIMIT, "global stats");
    assert_cpu_within(deck_time, LIMIT, "deck stats");
    assert_cpu_within(card_time, CARD_LIMIT, "card info");
}

/// About 10 times what was measured on a debug build (ADR 0025): 0.3 s for
/// the statistics, 10 ms for one card. In CPU time, checked against the
/// machine's slowdown (`assert_cpu_within`, ADR 0049).
const LIMIT: Duration = Duration::from_secs(2);
const CARD_LIMIT: Duration = Duration::from_millis(100);

/// 9999-12-31T23:59:59.999Z, the latest time an import accepts.
const LATEST_IMPORTED: i64 = 253_402_300_799_999;

/// Histories an import can bring (ADR 0050): reviews at the first and last
/// times a file may hold, a clock that ran backwards for 2,000 reviews
/// (each id later, each time earlier), 2,000 reviews in the same
/// millisecond with the largest durations, and reviews a year ahead of
/// now. The cache rebuilds, the statistics and card information stay
/// within their ranges, and each read is fast.
#[test]
fn hostile_histories_give_sound_statistics_quickly() {
    const RUN: i64 = 2_000;
    let (mut store, _, cards) = store_with_decks(1, 4);
    let mut ids = store.ids.clone();
    let mut events = Vec::new();
    let mut push = |card: &str, at: i64, grade: Grade, duration_ms: i64| {
        events.push(ReviewEvent {
            id: ids.next(at.clamp(0, LATEST_IMPORTED)).unwrap(),
            card_id: card.to_owned(),
            reviewed_at: at,
            kind: EventKind::Review(grade),
            duration_ms,
        });
    };
    push(&cards[0], 0, Grade::Fail, 0);
    push(&cards[0], 1, Grade::Pass, 0);
    push(&cards[0], LATEST_IMPORTED, Grade::Pass, i64::MAX);
    for k in 0..RUN {
        let grade = if k % 3 == 0 { Grade::Fail } else { Grade::Pass };
        push(&cards[1], NOW - k * DAY_MS / 7, grade, 1_000);
        push(&cards[2], NOW - DAY_MS, grade, i64::MAX);
    }
    for k in 0..10 {
        push(
            &cards[3],
            NOW + 365 * DAY_MS + k * DAY_MS,
            Grade::Pass,
            1_000,
        );
    }
    store.ids = ids;
    let tx = store.conn.transaction().unwrap();
    for e in &events {
        super::super::schedule::insert_event(&tx, e).unwrap();
    }
    tx.commit().unwrap();
    store.rebuild_schedule_cache(&Scheduler::default()).unwrap();

    let (time, all) = fastest("stats of hostile histories", 3, || {
        stats_of(&store, Scope::All)
    });
    // Every review inside the 30 days is counted once, the huge durations
    // saturate instead of wrapping.
    let window = past();
    let in_window = events
        .iter()
        .filter(|e| window.day_of(e.reviewed_at).is_some())
        .count();
    assert_eq!(
        all.days.iter().map(|d| u64::from(d.reviews)).sum::<u64>(),
        u64::try_from(in_window).unwrap()
    );
    assert!(
        all.days
            .iter()
            .all(|d| d.duration_ms >= 0 && d.passes <= d.reviews)
    );
    assert!(all.pass_rate.is_some_and(|r| (0.0..=1.0).contains(&r)));
    assert!(
        all.expected_retention
            .is_some_and(|r| (0.0..=1.0).contains(&r)),
        "{:?}",
        all.expected_retention
    );
    assert_eq!(all.scheduled_cards, 4);
    assert_cpu_within(time, CARD_LIMIT, "stats of hostile histories");

    for card in &cards {
        let (time, info) = fastest("card_info of a hostile history", 3, || {
            store.card_info(card, NOW).unwrap()
        });
        assert!(info.passes <= info.reviews);
        assert!(info.stability.is_some_and(|s| s.is_finite() && s > 0.0));
        assert!(info.difficulty.is_some_and(f32::is_finite));
        assert!(
            info.retrievability
                .is_some_and(|r| (0.0..=1.0).contains(&r)),
            "{:?}",
            info.retrievability
        );
        assert!(info.next_due.is_some());
        assert_cpu_within(time, CARD_LIMIT, "card_info of a hostile history");
    }
    let (time, _) = fastest("list_decks of hostile histories", 3, || {
        store.list_decks(NOW, NOW - DAY_MS / 2).unwrap()
    });
    assert_cpu_within(time, CARD_LIMIT, "list_decks of hostile histories");
}
