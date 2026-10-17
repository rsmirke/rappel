//! Speed at the scale of issue #42: one deck of 5,000 cards, 2,000 of
//! them due, as `fill_scratch_db 5000 --due 2000` makes. The bounds are
//! for a debug test build, about 10 times what was measured (ADR 0025).
//! Each is on the least CPU time of the thread over its runs (`fastest`),
//! so a loaded machine does not fail them, while a blow-up of the order of
//! the bound (a quadratic loop, a query per card) still does (ADR 0049).
//! Set `RAPPEL_TIMINGS=1` to print the measurements.

use std::time::Duration;

use super::*;
use crate::domain::Grade;
use crate::scheduling::STEP_MS;
use crate::test_support::{DAY_MS, T0, assert_cpu_within, fastest};

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

const CARDS: usize = 5_000;
const DUE: usize = 2_000;

/// "Now" in the tests: the reviews are 40 and 20 days before it.
const NOW: i64 = T0 + 60 * DAY_MS;

/// A deck as `fill_scratch_db 5000 --due 2000` makes it: the first 2,000
/// cards learned 40 days before [`NOW`] (every third also lapsed 20 days
/// before), so they are all overdue; the rest new.
fn scratch_store() -> (Store, String) {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Scratch", T0).unwrap().id;
    // The default daily limit (200) would hide most of the 2,000 due cards.
    store.set_max_reviews_per_day(9_999).unwrap();
    for i in 0..CARDS {
        let card = store
            .add_card_at(&deck, &format!("Card {i}"), "back", T0)
            .unwrap()
            .id;
        if i < DUE {
            let learned = NOW - 40 * DAY_MS;
            let mut reviews = vec![(Grade::Pass, learned), (Grade::Pass, learned + STEP_MS)];
            if i % 3 == 0 {
                let lapsed = NOW - 20 * DAY_MS;
                reviews.extend([(Grade::Fail, lapsed), (Grade::Pass, lapsed + STEP_MS)]);
            }
            for (grade, at) in reviews {
                store
                    .record_review(&card, grade, Duration::from_secs(8), at)
                    .unwrap();
            }
        }
    }
    (store, deck)
}

#[test]
fn deck_reads_are_fast_at_five_thousand_cards() {
    let (store, deck) = scratch_store();
    let day_start = NOW - DAY_MS / 2;

    let (list, decks) = fastest("list_decks", 3, || {
        store.list_decks(NOW, day_start).unwrap()
    });
    assert_eq!(
        (decks[0].new_count, decks[0].due_count),
        (20, u32::try_from(DUE).unwrap())
    );
    // The rest of the row's summary, read in the same transaction.
    assert_eq!(
        (
            decks[0].card_count,
            decks[0].suspended_count,
            decks[0].held_back_count
        ),
        (
            u32::try_from(CARDS).unwrap(),
            0,
            u32::try_from(CARDS - DUE).unwrap() - 20
        )
    );
    assert_eq!(
        decks[0].next_change_at,
        store.next_study_change(&deck, NOW).unwrap()
    );
    let (input, study) = fastest("study_input", 3, || {
        store.study_input(&deck, NOW, day_start).unwrap()
    });
    assert_eq!(study.reviews.len(), DUE);
    let (next, _) = fastest("next_study_change", 3, || {
        store.next_study_change(&deck, NOW).unwrap()
    });
    let (cards, listed) = fastest("list_cards", 3, || store.list_cards(&deck).unwrap());
    assert_eq!(listed.len(), CARDS);
    // A hundred calls each: one takes about 0.01 ms, under the precision of
    // the CPU clock (one scheduler tick, `thread_cpu_time`).
    let (all, ()) = fastest("decks, 100 times", 3, || {
        for _ in 0..100 {
            store.decks().unwrap();
        }
    });
    let (one, ()) = fastest("deck, 100 times", 3, || {
        for _ in 0..100 {
            store.deck(&deck).unwrap();
        }
    });

    // What the deck page does on every change (#42 item 2).
    assert_cpu_within(
        input + next,
        ms(STUDY_MS),
        "study_input + next_study_change",
    );
    assert_cpu_within(list, ms(STUDY_MS), "list_decks");
    assert_cpu_within(cards, ms(LIST_CARDS_MS), "list_cards");
    // The point of `deck` / `decks` (#42 item 1): finding a deck or the
    // names does not count every card, so it is far cheaper than the
    // counted list whatever the machine's speed.
    // `x / 100 * 10 < list`: one call at least ten times cheaper.
    assert!(all < list * 10, "decks ×100 {all:?}, list_decks {list:?}");
    assert!(one < list * 10, "deck ×100 {one:?}, list_decks {list:?}");
}

#[test]
fn listing_five_thousand_tagged_cards_is_fast() {
    let (mut store, deck) = scratch_store();
    let ids: Vec<String> = store
        .list_cards(&deck)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    for (i, id) in ids.iter().enumerate() {
        for name in [format!("tag {}", i % 40), format!("group {}", i % 7)] {
            store.add_tag_at(id, &name, T0).unwrap();
        }
    }
    let (tagged, listed) = fastest("list_tagged_cards", 3, || {
        store.list_tagged_cards(&deck).unwrap()
    });
    assert_eq!(listed.len(), CARDS);
    assert!(listed.iter().all(|c| c.tags.len() == 2));
    assert_cpu_within(tagged, ms(LIST_TAGGED_MS), "list_tagged_cards");
}

#[test]
fn planning_a_five_thousand_card_import_is_fast() {
    let (store, _) = scratch_store();
    let bytes = store.export_bytes_at(NOW).unwrap().0;
    let target = Store::in_memory();
    let (plan, summary) = fastest("plan_import (5,000 cards)", 3, || {
        target
            .plan_import_bytes(&bytes, &crate::interchange::Limits::DEFAULT)
            .unwrap()
            .summary()
            .clone()
    });
    assert_eq!(summary.cards.new, CARDS);
    assert_cpu_within(plan, ms(PLAN_IMPORT_MS), "plan_import");
}

/// A scratch store whose cards also have tags and a second deck, as the
/// All Cards page meets them.
fn tagged_scratch_store() -> (Store, String, Vec<String>) {
    let (mut store, deck) = scratch_store();
    let ids: Vec<String> = store
        .list_cards(&deck)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    for (i, id) in ids.iter().enumerate() {
        store
            .add_tag_at(id, &format!("tag {}", i % 40), T0)
            .unwrap();
    }
    (store, deck, ids)
}

#[test]
fn browsing_five_thousand_cards_is_fast() {
    use crate::storage::{BrowseQuery, StateFilter};

    let (mut store, deck, ids) = tagged_scratch_store();
    store.set_suspended(&ids[4_000..4_100], true).unwrap();
    let tag = store.tags().unwrap()[0].id.clone();
    let run = |what: &str, q: BrowseQuery| {
        let (t, rows) = fastest(what, 3, || store.browse(&q, NOW).unwrap());
        (t, rows.len())
    };
    let (all, n) = run("browse (all)", BrowseQuery::default());
    assert_eq!(n, CARDS);
    let (text, n) = run(
        "browse (text)",
        BrowseQuery {
            text: "card 12".to_owned(),
            ..BrowseQuery::default()
        },
    );
    assert_eq!(n, 111, "Card 12, 120-129, 1200-1299");
    let (rare, n) = run(
        "browse (one word)",
        BrowseQuery {
            text: "card 4999".to_owned(),
            ..BrowseQuery::default()
        },
    );
    assert_eq!(n, 1);
    let (filtered, n) = run(
        "browse (deck, tag, due)",
        BrowseQuery {
            deck_id: Some(deck),
            tag_id: Some(tag),
            state: Some(StateFilter::Due),
            ..BrowseQuery::default()
        },
    );
    assert!(n > 0);
    let (suspended, n) = run(
        "browse (suspended)",
        BrowseQuery {
            state: Some(StateFilter::Suspended),
            ..BrowseQuery::default()
        },
    );
    assert_eq!(n, 100);
    for t in [all, text, rare, filtered, suspended] {
        assert_cpu_within(t, ms(BROWSE_MS), "browse");
    }
}

#[test]
fn browsing_twenty_thousand_cards_is_fast() {
    use crate::storage::BrowseQuery;

    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Big", T0).unwrap().id;
    for i in 0..20_000 {
        store
            .add_card_at(&deck, &format!("Card {i}"), "back", T0)
            .unwrap();
    }
    let (all, rows) = fastest("browse 20,000 (all)", 3, || {
        store.browse(&BrowseQuery::default(), NOW).unwrap()
    });
    assert_eq!(rows.len(), 20_000);
    let (text, rows) = fastest("browse 20,000 (text)", 3, || {
        store
            .browse(
                &BrowseQuery {
                    text: "card 19999".to_owned(),
                    ..BrowseQuery::default()
                },
                NOW,
            )
            .unwrap()
    });
    assert_eq!(rows.len(), 1);
    assert_cpu_within(all, ms(4 * BROWSE_MS), "browse 20,000 (all)");
    assert_cpu_within(text, ms(4 * BROWSE_MS), "browse 20,000 (text)");
}

#[test]
fn bulk_writes_on_five_thousand_cards_are_fast() {
    let (mut store, deck, ids) = tagged_scratch_store();
    let other = store.create_deck_at("Other", T0).unwrap().id;
    let (suspend, changed) = fastest("set_suspended (5,000)", 1, || {
        store.set_suspended(&ids, true).unwrap()
    });
    assert_eq!(changed.len(), CARDS);
    let (moved, m) = fastest("move_cards (5,000)", 1, || {
        store.move_cards(&ids, &other).unwrap()
    });
    assert_eq!(m.len(), CARDS);
    let (unmove, back) = fastest("unmove_cards (5,000)", 1, || {
        store.unmove_cards(&m).unwrap()
    });
    assert_eq!(back, CARDS);
    let (tagged, _) = fastest("add_tag_to_cards (5,000)", 1, || {
        store.add_tag_to_cards_at(&ids, "bulk", T0).unwrap()
    });
    let (reset, events) = fastest("reset_cards (5,000)", 1, || {
        store.reset_cards(&ids, NOW).unwrap()
    });
    let (undo, skipped) = fastest("undo_entries (5,000)", 1, || {
        store.undo_entries(&events, NOW + 1).unwrap()
    });
    assert_eq!(skipped.len(), 0);
    let (deleted, ()) = fastest("delete_cards (5,000)", 1, || {
        store.delete_cards_at(&ids, NOW + 2).unwrap();
    });
    let (undeleted, back) = fastest("undelete_cards (5,000)", 1, || {
        store.undelete_cards(&ids).unwrap()
    });
    assert_eq!(back, CARDS);
    assert_eq!(store.list_cards(&deck).unwrap().len(), CARDS);
    for t in [
        suspend, moved, unmove, tagged, reset, undo, deleted, undeleted,
    ] {
        assert_cpu_within(t, ms(BULK_MS), "a bulk write");
    }
}

#[test]
fn rebuilding_the_search_index_of_five_thousand_cards_is_fast() {
    let (mut store, _) = scratch_store();
    let (t, ()) = fastest("rebuild_search_index (5,000)", 3, || {
        let tx = store.conn.transaction().unwrap();
        crate::storage::search::rebuild_search_index(&tx).unwrap();
        tx.commit().unwrap();
    });
    assert_cpu_within(t, ms(REBUILD_INDEX_MS), "rebuild_search_index");
}

/// `study_input` + `next_study_change`, and `list_decks`, each. Measured
/// side by side on one loaded machine (debug build): `study_input` 27 ms,
/// `next_study_change` 3 ms, `list_decks` 28 ms. `list_decks` counts
/// without reading card ids (`deck_counts_in`, ADR 0041); before that it
/// took 42 ms there, and 52 to 56 ms with a separate count and next-change
/// query per deck.
const STUDY_MS: u64 = 150;
/// `list_cards` of 5,000 cards (measured 6 ms).
const LIST_CARDS_MS: u64 = 100;
/// `list_tagged_cards` of 5,000 cards with two tags each (measured 30 ms).
const LIST_TAGGED_MS: u64 = 300;
/// Planning the import of the 5,000-card export into an empty store
/// (measured 80 ms).
const PLAN_IMPORT_MS: u64 = 1_000;
/// `browse` of 5,000 cards with tags, each query (measured 13 to 28 ms; 20,000
/// cards: 48 ms, with a bound four times as long).
const BROWSE_MS: u64 = 500;
/// Each bulk write on 5,000 cards. The slowest is `undo_entries` (measured
/// 0.9 s: it replays every card's log); the others take 25 to 420 ms.
const BULK_MS: u64 = 9_000;
/// The migration's index backfill of 5,000 cards (measured 87 ms).
const REBUILD_INDEX_MS: u64 = 1_000;
