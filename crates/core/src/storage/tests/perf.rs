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

/// Hostile text in the search entry (ADR 0050): thousands of words, FTS5
/// syntax repeated, a huge word, only quotes. Each search answers, finds
/// what its words find, and stays within a bound. Only the first
/// `MAX_QUERY_BYTES` are searched; before that cut, 20,000 hyphens took
/// 10 s of CPU and 5,000 distinct phrases 0.53 s.
#[test]
fn hostile_searches_at_five_thousand_cards_are_fast() {
    use crate::storage::BrowseQuery;

    let (store, _, _) = tagged_scratch_store();
    let words =
        |n: usize, f: &dyn Fn(usize) -> String| -> String { (0..n).map(|i| f(i) + " ").collect() };
    let cases: Vec<(&str, String, usize)> = vec![
        // Every card has "card": the same word 5,000 times (10 s of CPU
        // before the cut and before repeated words were left out).
        (
            "one word 5,000 times",
            words(5_000, &|_| "card".into()),
            CARDS,
        ),
        (
            "prefixes of one word",
            words(5_000, &|i| "card"[..=(i % 4)].into()),
            CARDS,
        ),
        // Distinct words that never start one another and each start a token.
        (
            "5,000 distinct numbers",
            words(5_000, &|i| i.to_string()),
            0,
        ),
        // A phrase per word, all sharing the token every card has.
        (
            "5,000 distinct phrases",
            words(5_000, &|i| format!("card-{i}")),
            0,
        ),
        (
            "5,000 phrases on the back",
            words(5_000, &|i| format!("back-{i}")),
            0,
        ),
        (
            "FTS5 syntax 2,000 times",
            words(2_000, &|_| {
                "NEAR(card back) OR \"card\"* ^card front:card ( ) AND NOT".into()
            }),
            0,
        ),
        ("a 1 MiB word", "c".repeat(1 << 20), 0),
        ("100,000 quotes", "\"".repeat(100_000), 0),
        ("20,000 hyphens", "card-".repeat(20_000), 0),
        // Every word is "card" once what comes before its first letter is
        // left out. U+10FFFF is private use to FTS5, which joined it to
        // "card" and found nothing; left out, the word requires less.
        (
            "NUL and odd Unicode",
            "card\0\u{301}\u{200b}\u{202e}\u{fffd}\u{10ffff}".repeat(1_000),
            CARDS,
        ),
    ];
    for (what, text, expected) in cases {
        let q = BrowseQuery {
            text,
            ..BrowseQuery::default()
        };
        let (t, rows) = fastest(what, 1, || store.browse(&q, NOW).unwrap());
        assert_eq!(rows.len(), expected, "{what}");
        assert_cpu_within(t, ms(HOSTILE_SEARCH_MS), what);
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
    let one = vec!["bulk".to_owned()];
    let (tagged, bulk) = fastest("add_tags_to_cards (1 tag, 5,000)", 1, || {
        store.add_tags_to_cards_at(&ids, &one, 100, T0).unwrap()
    });
    assert_eq!(bulk.cards.len(), CARDS);
    let bulk = bulk.tags[0].clone();
    let (untagged, had) = fastest("remove_tag_from_cards (5,000)", 1, || {
        store.remove_tag_from_cards(&ids, &bulk.id).unwrap()
    });
    assert_eq!(had.len(), CARDS);
    let (retagged, n) = fastest("restore_tag_on_cards (5,000)", 1, || {
        store.restore_tag_on_cards(&had, &bulk.id).unwrap()
    });
    assert_eq!(n, CARDS);
    let links: Vec<(String, String)> = had.iter().map(|id| (id.clone(), bulk.id.clone())).collect();
    let (released, n) = fastest("release_tags (5,000 links)", 1, || {
        store.release_tags(&links).unwrap()
    });
    assert_eq!(n, CARDS);
    let several: Vec<String> = ["bulk", "exam", "later"].map(String::from).to_vec();
    let (tagged_several, added) = fastest("add_tags_to_cards (3 tags, 5,000)", 1, || {
        store.add_tags_to_cards_at(&ids, &several, 100, T0).unwrap()
    });
    assert_eq!(added.cards.len(), CARDS);
    let (released_several, n) = fastest("release_tags (15,000 links)", 1, || {
        store.release_tags(&added.links).unwrap()
    });
    assert_eq!(n, CARDS);
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
        suspend,
        moved,
        unmove,
        tagged,
        untagged,
        retagged,
        released,
        tagged_several,
        released_several,
        reset,
        undo,
        deleted,
        undeleted,
    ] {
        assert_cpu_within(t, ms(BULK_MS), "a bulk write");
    }
}

/// Entries in the log of `a_card_with_a_long_log_is_fast`'s card.
const LONG_LOG: usize = 20_000;

/// The store of `a_card_with_a_long_log_is_fast`: its deck, its one
/// card, how many entries the log has, and the time of the last one.
fn long_log_store() -> (Store, String, String, usize, i64) {
    use crate::domain::{EventKind, ReviewEvent};
    use crate::scheduling::Scheduler;

    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Long", T0).unwrap().id;
    let card = store.add_card_at(&deck, "front", "back", T0).unwrap().id;
    let start = NOW - 30 * DAY_MS;
    let mut ids = store.ids.clone();
    let mut events = Vec::new();
    let mut push = |at: i64, kind: EventKind| {
        let id = ids.next(at).unwrap();
        let duration_ms = if matches!(kind, EventKind::Review(_)) {
            4_000
        } else {
            0
        };
        events.push(ReviewEvent {
            id: id.clone(),
            card_id: card.clone(),
            reviewed_at: at,
            kind,
            duration_ms,
        });
        id
    };
    let mut at = start;
    for k in 0..LONG_LOG {
        at = start + i64::try_from(k).unwrap() * 60_000;
        let grade = if k % 7 == 0 { Grade::Fail } else { Grade::Pass };
        let id = push(at, EventKind::Review(grade));
        if k % 10 == 9 {
            push(at + 1, EventKind::Undo { undoes: id });
        }
    }
    let reset = push(at + 2, EventKind::Reset);
    push(at + 3, EventKind::Undo { undoes: reset });
    store.ids = ids;
    let tx = store.conn.transaction().unwrap();
    for e in &events {
        crate::storage::schedule::insert_event(&tx, e).unwrap();
    }
    tx.commit().unwrap();
    store.rebuild_schedule_cache(&Scheduler::default()).unwrap();
    (store, deck, card, events.len(), at + 3)
}

/// One card's log as an import may bring it (ADR 0050): 20,000 reviews a
/// minute apart, every tenth undone, then a reset that is undone too, so
/// no surviving reset ends the reviews the daily limits count. Before
/// `COUNTED_REVIEWS` found each card's last reset once, deciding that took
/// time quadratic in the log: 7 s of CPU for 8,000 reviews.
#[test]
fn a_card_with_a_long_log_is_fast() {
    use crate::stats::DayGrid;
    use crate::storage::{BrowseQuery, Scope};

    let (mut store, deck, card, entries, at) = long_log_store();
    let now = at + DAY_MS;
    let day_start = now - DAY_MS / 2;
    let (list, decks) = fastest("list_decks (long log)", 3, || {
        store.list_decks(now, day_start).unwrap()
    });
    assert_eq!(decks[0].card_count, 1);
    let (study, _) = fastest("study_input (long log)", 3, || {
        store.study_input(&deck, now, day_start).unwrap()
    });
    let (info, card_info) = fastest("card_info (long log)", 3, || {
        store.card_info(&card, now).unwrap()
    });
    assert_eq!(
        card_info.reviews,
        u32::try_from(LONG_LOG - LONG_LOG / 10).unwrap()
    );
    let past = DayGrid::new((0..=40).map(|d| now - (40 - d) * DAY_MS).collect()).unwrap();
    let ahead = DayGrid::new((0..=14).map(|d| now + d * DAY_MS).collect()).unwrap();
    let (stats, _) = fastest("stats (long log)", 3, || {
        store.stats(Scope::All, now, &past, &ahead).unwrap()
    });
    let (browse, _) = fastest("browse (long log)", 3, || {
        store.browse(&BrowseQuery::default(), now).unwrap()
    });
    let latest = crate::scheduling::surviving(&store.review_events(&card).unwrap())
        .unwrap()
        .last()
        .map(|e| e.id.clone())
        .unwrap();
    let (undo, _) = fastest("undo (long log)", 1, || {
        store.undo(&card, &latest, now).unwrap()
    });
    let (reset, _) = fastest("reset_card (long log)", 1, || {
        store.reset_card(&card, now).unwrap()
    });
    let bytes = store.export_bytes_at(now).unwrap().0;
    // An import backs the database up first, so it needs a data directory.
    let tmp = tempfile::tempdir().unwrap();
    let mut target = Store::open(tmp.path()).unwrap();
    let (import, ()) = fastest("plan and apply the import (long log)", 1, || {
        let plan = target
            .plan_import_bytes(&bytes, &crate::interchange::Limits::DEFAULT)
            .unwrap();
        target.apply_import_at(&plan, now).unwrap();
    });
    assert_eq!(target.review_events(&card).unwrap().len(), entries + 2);
    let (target_list, _) = fastest("list_decks after the import (long log)", 3, || {
        target.list_decks(now, day_start).unwrap()
    });

    for (t, what) in [
        (list, "list_decks"),
        (study, "study_input"),
        (target_list, "list_decks after the import"),
    ] {
        assert_cpu_within(t, ms(STUDY_MS), what);
    }
    for (t, what) in [
        (info, "card_info"),
        (stats, "stats"),
        (browse, "browse"),
        (undo, "undo"),
        (reset, "reset_card"),
    ] {
        assert_cpu_within(t, ms(LONG_LOG_MS), what);
    }
    assert_cpu_within(import, ms(LONG_LOG_IMPORT_MS), "import");
}

/// Tags on one card, as many as a pasted list or an import can bring
/// (ADR 0050): 5,000 names typed in the tags field, then 5,000 others in
/// their place, so the database holds 10,000 tags. Linking them was
/// quadratic in the card's tags (0.6 s for 5,000, lists instead of sets).
#[test]
fn a_card_with_thousands_of_tags_is_fast() {
    use crate::domain::parse_tag_field;
    use crate::storage::BrowseQuery;

    const TAGS: usize = 5_000;
    let (mut store, deck, ids) = tagged_scratch_store();
    let card = ids[0].clone();
    let field = |prefix: &str| -> String {
        (0..TAGS)
            .map(|i| prefix.to_owned() + " " + &i.to_string() + ", ")
            .collect()
    };
    let (parse, names) = fastest("parse_tag_field (5,000 names)", 3, || {
        parse_tag_field(&field("name")).unwrap()
    });
    assert_eq!(names.len(), TAGS);
    let (set, tags) = fastest("set 5,000 tags", 1, || {
        store
            .edit_card_with_tags_at(&card, "front", "back", Some(&names), T0 + 1)
            .unwrap()
            .tags
    });
    assert_eq!(tags.len(), TAGS);
    let others = parse_tag_field(&field("other")).unwrap();
    let (replace, tags) = fastest("replace 5,000 tags", 1, || {
        store
            .edit_card_with_tags_at(&card, "front", "back", Some(&others), T0 + 2)
            .unwrap()
            .tags
    });
    assert_eq!(tags.len(), TAGS);
    assert_eq!(store.tags().unwrap().len(), 40 + 2 * TAGS);
    let (list, listed) = fastest("list_tagged_cards (a card with 5,000 tags)", 3, || {
        store.list_tagged_cards(&deck).unwrap()
    });
    assert_eq!(listed[0].tags.len(), TAGS);
    let filter = BrowseQuery {
        tag_id: Some(tags[TAGS - 1].id.clone()),
        ..BrowseQuery::default()
    };
    let (browse, rows) = fastest("browse by one of 10,000 tags", 3, || {
        store.browse(&filter, NOW).unwrap()
    });
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tags.len(), TAGS);
    let (all_tags, _) = fastest("tags (10,040)", 3, || store.tags().unwrap());

    assert_cpu_within(parse, ms(TAGS_MS), "parse_tag_field");
    assert_cpu_within(set, ms(TAGS_MS), "setting 5,000 tags");
    assert_cpu_within(replace, ms(TAGS_MS), "replacing 5,000 tags");
    assert_cpu_within(list, ms(LIST_TAGGED_MS), "list_tagged_cards");
    assert_cpu_within(browse, ms(BROWSE_MS), "browse by tag");
    assert_cpu_within(all_tags, ms(BROWSE_MS), "tags");
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
/// Each hostile search of `hostile_searches_at_five_thousand_cards_are_fast`
/// (measured 63 ms at most, for 20,000 hyphens cut to 1 KiB).
const HOSTILE_SEARCH_MS: u64 = 600;
/// Each bulk write on 5,000 cards, Add Tags with one and with three tags and
/// its Undo (`release_tags`) included. The slowest is `undo_entries`
/// (measured 0.9 s: it replays every card's log); the others take 25 to
/// 420 ms (`add_tags_to_cards` of three tags 85 ms, `release_tags` of their
/// 15,000 links 64 ms, in a debug build).
const BULK_MS: u64 = 9_000;
/// Each read or write of the card with a 20,000-entry log other than the
/// deck reads, which keep `STUDY_MS` (measured 36 ms; the others at most
/// 153 ms, for an undo, which replays the log).
const LONG_LOG_MS: u64 = 1_500;
/// Planning and applying the import of that card (measured 0.6 s).
const LONG_LOG_IMPORT_MS: u64 = 6_000;
/// Parsing, setting or replacing 5,000 tags on one card (measured 15 ms,
/// 185 ms and 209 ms).
const TAGS_MS: u64 = 2_000;
/// The migration's index backfill of 5,000 cards (measured 87 ms).
const REBUILD_INDEX_MS: u64 = 1_000;
