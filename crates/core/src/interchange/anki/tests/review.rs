//! The attacks and cases of the slice's review, each a test: bounded JSON,
//! values measured not loaded, schemas that would run code, template work,
//! the plan's clock, and the history, media, deck and report details.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;

use super::builder::{
    BASIC, Builder, CLOZE, Container, DEFAULT_DECK, entries, proto_bytes, proto_varint, zip_of,
    zstd,
};
use super::*;
use crate::Error;
use crate::interchange::AnkiProblem;
use crate::test_support::{DAY_MS, png_bytes, within};

fn refused(env: &Env, bytes: &[u8]) -> AnkiProblem {
    let result = env.plan(bytes);
    env.assert_no_temp();
    match result {
        Err(Error::AnkiRefused(problem)) => problem,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn one_basic() -> Builder {
    let mut b = Builder::new();
    b.note(10, BASIC, &["front", "back"], "")
        .card(100, 10, DEFAULT_DECK, 0);
    b
}

/// The kinds of a card's log, in order.
fn kinds(env: &Env, front: &str) -> Vec<String> {
    let mut stmt = env
        .store
        .conn
        .prepare(
            "SELECT e.kind FROM review_events e JOIN cards c ON c.id = e.card_id
             WHERE c.front_md = ?1 ORDER BY e.reviewed_at, e.id",
        )
        .unwrap();
    stmt.query_map([front], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn legacy_json_past_its_counts_is_refused_quickly() {
    within(Duration::from_secs(20), "bounded JSON", || {
        let env = Env::new();
        // A note type with more fields than one may have, then note types
        // with more fields together than all may have.
        let mut b = one_basic();
        b.model(
            5,
            json!({"name": "Many", "flds": vec![json!({}); 1_001], "tmpls": []}),
        );
        let problem = refused(&env, &b.build(Container::Legacy));
        assert!(matches!(problem, AnkiProblem::Damaged(_)), "{problem:?}");
        let mut b = one_basic();
        for id in 100..302 {
            b.model(id, json!({"flds": vec![json!({}); 1_000]}));
        }
        let problem = refused(&env, &b.build(Container::Legacy));
        assert!(matches!(problem, AnkiProblem::Damaged(_)), "{problem:?}");
        // More note types, decks or media entries than allowed.
        let mut b = one_basic();
        for id in 100..10_101 {
            b.model(id, json!({}));
        }
        assert!(matches!(
            refused(&env, &b.build(Container::Legacy)),
            AnkiProblem::Damaged(_)
        ));
        let mut b = one_basic();
        for id in 100..100_101 {
            b.deck(id, "d");
        }
        assert!(matches!(
            refused(&env, &b.build(Container::Legacy)),
            AnkiProblem::Damaged(_)
        ));
        let list: serde_json::Map<String, serde_json::Value> = (0..100_001)
            .map(|i| (i.to_string(), json!("a.png")))
            .collect();
        let mut b = one_basic();
        b.raw(
            "media",
            serde_json::Value::Object(list).to_string().as_bytes(),
        );
        assert!(matches!(
            refused(&env, &b.build(Container::Legacy)),
            AnkiProblem::TooManyMediaFiles { .. }
        ));
    });
}

#[test]
fn names_past_their_budget_are_refused() {
    let env = Env::new();
    let mut b = one_basic();
    // 2,000 decks with 4 KiB names: 8 MiB, over a 1 MiB budget.
    let name = "n".repeat(4000);
    for id in 100..2_100 {
        b.deck(id, &name);
    }
    let limits = AnkiLimits {
        names: 1 << 20,
        ..AnkiLimits::DEFAULT
    };
    let result = env
        .store
        .plan_anki_bytes(&b.build(Container::Legacy), &limits, PLAN_NOW);
    assert!(
        matches!(result, Err(Error::AnkiRefused(AnkiProblem::Damaged(_)))),
        "{result:?}"
    );
}

#[test]
fn values_larger_than_the_length_limit_are_measured_not_read() {
    // A field of 3 MiB with a note limit of 1 MiB (SQLite's length limit is
    // then 66 MiB): measured by `octet_length`, never fetched.
    let mut b = one_basic();
    let big = "x".repeat(3 << 20);
    b.note(11, BASIC, &[&big, "b"], "")
        .card(101, 11, DEFAULT_DECK, 0);
    let env = Env::new();
    let limits = AnkiLimits {
        note: 1 << 20,
        legacy_json: 1 << 20,
        ..AnkiLimits::DEFAULT
    };
    let plan = env
        .store
        .plan_anki_bytes(&b.build(Container::Legacy), &limits, PLAN_NOW)
        .unwrap();
    assert_eq!(plan.summary().cards, 1);
    assert_eq!(count(&plan.summary().skipped, Skip::TooLong), 1);
}

#[test]
fn virtual_tables_and_generated_columns_are_refused() {
    let env = Env::new();
    for text in [
        "CREATE VIRTUAL TABLE search USING fts5(x)",
        // SQLite stores its own spelling, but a file may hold any: this one
        // hides the keyword from a reading of words.
        "CREATE/**/VIRTUAL/**/TABLE search USING fts5(x)",
    ] {
        let mut b = one_basic();
        // Refused by the first look at the schema, before anything could
        // connect the module (a later guard says "virtual or shadow").
        b.sql(&format!(
            "CREATE VIRTUAL TABLE search USING fts5(x); INSERT INTO search VALUES ('a');
             PRAGMA writable_schema = ON;
             UPDATE sqlite_schema SET sql = '{text}' WHERE name = 'search';
             PRAGMA writable_schema = OFF;"
        ));
        let problem = refused(&env, &b.build(Container::Legacy));
        assert_eq!(
            problem,
            AnkiProblem::Damaged(
                "the collection cannot be read: it has a virtual table".to_owned()
            ),
            "{text}"
        );
    }
    let mut b = one_basic();
    b.sql("ALTER TABLE notes ADD COLUMN g AS (printf('%.*c', 999999999, 'x'));");
    let problem = refused(&env, &b.build(Container::Legacy));
    assert_eq!(
        problem,
        AnkiProblem::Damaged("the collection cannot be read: it has a generated column".to_owned())
    );
    let mut b = one_basic();
    b.sql("CREATE TABLE other (a, b AS (a * 2) STORED);");
    assert!(matches!(
        refused(&env, &b.build(Container::Legacy)),
        AnkiProblem::Damaged(_)
    ));
}

#[test]
fn template_work_is_bounded_on_a_large_field() {
    within(Duration::from_secs(30), "template work", || {
        // No cloze in it: nothing but the work budget stops a filter that
        // reads the whole field at each of its nodes (each output is
        // empty, so the output cap never does).
        let field = "y ".repeat(3_800_000); // about 7.2 MiB
        let mut b = Builder::new();
        let templates = [
            // Each under the 256 KiB a template may have.
            "{{#F}}x{{/F}}".repeat(15_000),
            "{{^F}}x{{/F}}{{#Tags}}t{{/Tags}}".repeat(6_000),
            "{{cloze-only:F}}".repeat(15_000),
            "{{type:F}}".repeat(20_000),
        ];
        for (i, qfmt) in templates.iter().enumerate() {
            let id = 50 + i64::try_from(i).unwrap();
            b.model(
                id,
                json!({"name": "T", "type": 0,
                       "flds": [{"name": "F", "ord": 0}],
                       "tmpls": [{"name": "C", "ord": 0, "qfmt": qfmt, "afmt": "x"}]}),
            );
            b.note(id, id, &[&field], "a b")
                .card(id * 10, id, DEFAULT_DECK, 0);
        }
        let env = Env::new();
        let limits = AnkiLimits {
            note: 16 << 20,
            ..AnkiLimits::DEFAULT
        };
        let plan = env
            .store
            .plan_anki_bytes(&b.build(Container::Legacy), &limits, PLAN_NOW)
            .unwrap();
        let s = plan.summary();
        // The sections only write "x": imported. `cloze-only` reads the
        // field at each node: cut by the work budget. `type:` on the
        // question writes nothing: an empty front.
        assert_eq!(s.cards, 2, "{s:#?}");
        assert_eq!(count(&s.skipped, Skip::TooLong), 1, "{s:#?}");
        assert_eq!(count(&s.skipped, Skip::EmptyFront), 1, "{s:#?}");
    });
}

#[test]
fn cancel_is_seen_between_any_two_cards() {
    let mut b = Builder::new();
    for i in 0..300 {
        b.note(1_000 + i, BASIC, &[&format!("q{i}"), "a"], "").card(
            10_000 + i,
            1_000 + i,
            DEFAULT_DECK,
            0,
        );
    }
    let bytes = b.build(Container::Legacy);
    let env = Env::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (flag, count) = (Arc::clone(&cancel), Arc::clone(&seen));
    // Cancelled once the fifth card is done: the sixth is never read.
    let hook = crate::test_support::set_hook(&super::super::import::AFTER_CARD, move || {
        if count.fetch_add(1, Ordering::Relaxed) + 1 == 5 {
            flag.store(true, Ordering::Relaxed);
        }
    });
    let source = crate::interchange::source::Source::Bytes(Arc::from(&bytes[..]));
    let result = env
        .store
        .plan_anki_source(source, &AnkiLimits::DEFAULT, &cancel, PLAN_NOW);
    drop(hook);
    assert!(matches!(result, Err(Error::ImportCancelled)), "{result:?}");
    assert_eq!(seen.load(Ordering::Relaxed), 5);
    env.assert_no_temp();
}

#[test]
fn a_cancel_at_the_last_moment_writes_nothing() {
    let mut b = one_basic();
    b.media("a.png", &png_bytes(7));
    b.note(11, BASIC, &["<img src=\"a.png\">", "x"], "")
        .card(101, 11, DEFAULT_DECK, 0);
    b.rev(1_700_000_000_000, 101, 3, 1000, 0);
    let mut env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    // After the media are stored and the schedules rebuilt, just before
    // the commit.
    let hook = crate::test_support::set_hook(&super::super::import::BEFORE_COMMIT, move || {
        flag.store(true, Ordering::Relaxed);
    });
    let applied = env.store.apply_anki_at(&plan, APPLY_NOW, &cancel);
    drop(hook);
    assert!(
        matches!(applied, Err(Error::ImportCancelled)),
        "{applied:?}"
    );
    assert_eq!(env.cards().len(), 0);
    let (media, events, schedules): (i64, i64, i64) = env
        .store
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM media), (SELECT count(*) FROM review_events),
                    (SELECT count(*) FROM card_schedule)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((media, events, schedules), (0, 0, 0));
}

#[test]
fn cards_dated_in_the_future_apply() {
    // Ids (creation times) in 2100: the plan dates them at its own clock,
    // and the apply decides the same, later.
    let future = 4_102_444_800_000;
    let mut b = Builder::new();
    b.note(future, BASIC, &["future", "b"], "")
        .card(future, future, DEFAULT_DECK, 0)
        .card_with(i64::MAX, future, DEFAULT_DECK, 0, 0, 0, 0);
    b.rev(PLAN_NOW - DAY_MS, future, 3, 1000, 0);
    // A review after the plan's clock is left out.
    b.rev(PLAN_NOW + DAY_MS, future, 3, 1000, 1);
    let mut env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    assert_eq!(plan.summary().reviews, 1);
    assert_eq!(count(&plan.summary().skipped, Skip::BadReview), 1);
    let report = env
        .store
        .apply_anki_at(&plan, PLAN_NOW + 30 * DAY_MS, &AtomicBool::new(false))
        .unwrap();
    // The card dated 2100 and the one whose id is past any date.
    assert_eq!(report.cards, 2);
    let created: Vec<i64> = env
        .store
        .conn
        .prepare("SELECT created_at FROM cards")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(created, [PLAN_NOW, PLAN_NOW]);
}

#[test]
fn filtered_reviews_that_rescheduled_are_kept() {
    // As Anki 26.9 logs them: a review answered early in a filtered deck
    // that reschedules is type 3 with an ease factor; a preview or cram is
    // type 3 with factor 0.
    let mut b = one_basic();
    let t = 1_700_000_000_000;
    b.rev_full(t, 100, 3, 1, 2500, 1000, 0)
        .rev_full(t + 1, 100, 3, 4, 2500, 1000, 3)
        .rev_full(t + 2, 100, 3, 0, 0, 1000, 3)
        .rev_full(t + 3, 100, 1, 0, 0, 1000, 3);
    let env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    assert_eq!(plan.summary().reviews, 2);
    assert_eq!(count(&plan.summary().skipped, Skip::FilteredReview), 2);
}

#[test]
fn a_forget_in_the_history_is_a_reset_at_its_time() {
    let mut b = one_basic();
    let t = 1_700_000_000_000;
    // Reviewed, forgotten (manual, ease, interval and factor 0), reviewed
    // again; "Set Due Date" (an interval) stays a left-out manual entry.
    b.card_with(101, 10, DEFAULT_DECK, 1, 0, 2, 2);
    b.rev_full(t, 101, 3, 1, 2500, 1000, 0)
        .rev_full(t + DAY_MS, 101, 0, 0, 0, 0, 4)
        .rev_full(t + 2 * DAY_MS, 101, 3, 1, 2500, 1000, 0)
        .rev_full(t + 3 * DAY_MS, 101, 0, 7, 2500, 0, 4);
    b.model(
        3,
        json!({"name": "R", "type": 0,
               "flds": [{"name": "Front", "ord": 0}, {"name": "Back", "ord": 1}],
               "tmpls": [{"name": "1", "ord": 0, "qfmt": "{{Front}}", "afmt": "{{Back}}"},
                         {"name": "2", "ord": 1, "qfmt": "{{Back}}", "afmt": "{{Front}}"}]}),
    );
    b.sql("UPDATE notes SET mid = 3 WHERE id = 10;");
    let mut env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    assert_eq!(count(&plan.summary().skipped, Skip::ManualEntry), 1);
    env.import(&b.build(Container::Legacy)).unwrap();
    assert_eq!(kinds(&env, "back"), ["review", "reset", "review"]);
    // A forget with no review before it changes nothing.
    assert_eq!(kinds(&env, "front"), Vec::<String>::new());
}

#[test]
fn a_bad_zstd_checksum_is_refused() {
    let mut b = one_basic();
    let mut frame = zstd(&b.collection());
    // The encoder writes a content checksum (bit 2 of the frame header
    // descriptor), as Anki's does.
    assert_ne!(frame[4] & 0x04, 0);
    let n = frame.len();
    frame[n - 1] ^= 0xff;
    let mut b = one_basic();
    b.raw("meta", &[0x08, 3]).raw("collection.anki21b", &frame);
    let env = Env::new();
    assert!(matches!(
        refused(&env, &b.build(Container::Current)),
        AnkiProblem::Damaged(_)
    ));
}

#[test]
fn media_files_are_numbered_by_their_entries_only() {
    let mut b = one_basic();
    b.media("a.png", &png_bytes(1));
    b.note(11, BASIC, &["<img src=\"a.png\">", "x"], "")
        .card(101, 11, DEFAULT_DECK, 0);
    // A list with another field before its one entry.
    let mut entry = Vec::new();
    proto_bytes(&mut entry, 1, b"a.png");
    let mut list = Vec::new();
    proto_varint(&mut list, 2, 7);
    proto_bytes(&mut list, 1, &entry);
    let mut parts = entries(&b.build(Container::Current));
    for (name, data) in &mut parts {
        if name == "media" {
            *data = zstd(&list);
        }
    }
    let env = Env::new();
    let plan = env.plan(&zip_of(&parts)).unwrap();
    assert_eq!(plan.summary().new_media, 1, "{:#?}", plan.summary());
}

#[test]
fn where_cards_go_is_part_of_the_plan() {
    let mut env = Env::new();
    let lang = env.store.create_deck("Lang").unwrap();
    let other = env.store.create_deck("Other").unwrap();
    let plan = env.plan(&fixture("current.apkg")).unwrap();
    // Swapped names: "Lang" is now another deck.
    env.store.rename_deck(&lang.id, "Tmp").unwrap();
    env.store.rename_deck(&other.id, "Lang").unwrap();
    env.store.rename_deck(&lang.id, "Other").unwrap();
    let applied = env
        .store
        .apply_anki_at(&plan, APPLY_NOW, &AtomicBool::new(false));
    assert!(
        matches!(applied, Err(Error::ImportPlanStale)),
        "{applied:?}"
    );
}

#[test]
fn null_columns_are_damaged_notes_and_bad_tags_count_once() {
    let mut b = one_basic();
    b.sql("INSERT INTO notes VALUES (11, 'g', 1, 0, 0, '', NULL, '', 0, 0, '');");
    b.card(101, 11, DEFAULT_DECK, 0);
    b.sql("INSERT INTO notes VALUES (12, 'g', 1, 0, 0, NULL, 'a\u{1f}b', '', 0, 0, '');");
    b.card(102, 12, DEFAULT_DECK, 0);
    b.note(13, BASIC, &["c", "d"], "a,b x,y")
        .card(103, 13, DEFAULT_DECK, 0);
    b.note(14, BASIC, &["e", "f"], "a,b")
        .card(104, 14, DEFAULT_DECK, 0);
    let env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    let k = &plan.summary().skipped;
    assert_eq!(count(k, Skip::BrokenNote), 2, "{k:#?}");
    assert_eq!(count(k, Skip::TooLong), 0, "{k:#?}");
    assert_eq!(count(k, Skip::InvalidTag), 2, "{k:#?}");
}

#[test]
fn a_local_name_with_a_colon_is_a_file() {
    let mut b = one_basic();
    b.media("12:30.png", &png_bytes(2));
    b.media("a:b.png", &png_bytes(3));
    b.note(
        11,
        BASIC,
        &[
            "<img src=\"12:30.png\"><img src=\"a:b.png\"><img src=\"http://x.org/c.png\">",
            "x",
        ],
        "",
    )
    .card(101, 11, DEFAULT_DECK, 0);
    let env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    assert_eq!(plan.summary().new_media, 2, "{:#?}", plan.summary());
    assert_eq!(count(&plan.summary().skipped, Skip::RemoteImage), 1);
    let _ = CLOZE;
}

/// A cancel during a long SQLite statement (here the integrity check of a
/// collection with a large indexed table) stops the statement: after the
/// cancel, the plan uses a fraction of the CPU time a whole plan uses from
/// the same point, and returns `ImportCancelled` (ADR 0063).
#[test]
fn a_cancel_stops_a_long_sqlite_statement() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut b = one_basic();
    b.sql(
        "CREATE TABLE padding (x); CREATE INDEX padding_x ON padding (x);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n LIMIT 400000)
         INSERT INTO padding SELECT randomblob(24) FROM n;",
    );
    let bytes = b.build(Container::Legacy);
    let env = Env::new();
    // The CPU time a plan uses from the start of the integrity check on,
    // cancelled there or not.
    let after_check = |cancelling: bool| {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let start = Rc::new(RefCell::new(None));
        let at = Rc::clone(&start);
        let hook =
            crate::test_support::set_hook(&super::super::collection::BEFORE_CHECK, move || {
                if cancelling {
                    flag.store(true, Ordering::Relaxed);
                    // Long enough for the cancel to reach the statement's flag.
                    std::thread::sleep(Duration::from_millis(60));
                }
                *at.borrow_mut() = Some((
                    crate::test_support::thread_cpu_time(),
                    std::time::Instant::now(),
                ));
            });
        let source = crate::interchange::source::Source::Bytes(Arc::from(&bytes[..]));
        let result = env
            .store
            .plan_anki_source(source, &AnkiLimits::DEFAULT, &cancel, PLAN_NOW)
            .map(|_| ());
        let end = crate::test_support::thread_cpu_time();
        drop(hook);
        let (cpu, wall) = start.borrow_mut().take().expect("the check was reached");
        (
            crate::test_support::cpu_time_between(cpu, end, wall.elapsed()),
            result,
        )
    };
    let (whole, planned) = after_check(false);
    assert!(planned.is_ok(), "{planned:?}");
    let (part, cancelled) = after_check(true);
    eprintln!("after the check began: {whole:?} of CPU, cancelled there: {part:?}");
    assert!(
        matches!(cancelled, Err(Error::ImportCancelled)),
        "{cancelled:?}"
    );
    assert!(
        part * 3 < whole,
        "cancelled: {part:?} of CPU, whole: {whole:?}"
    );
    env.assert_no_temp();
}
