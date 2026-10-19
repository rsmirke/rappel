//! Hostile packages, built by hand: each is refused, or imported with what
//! cannot be used left out and reported; none writes anything when
//! refused, leaves a temporary file behind, or takes long.

use std::time::Duration;

use serde_json::json;

use super::builder::{
    BASIC, Builder, CLOZE, Container, DEFAULT_DECK, entries, zip_of, zstd, zstd_bomb,
    zstd_bomb_after,
};
use super::*;
use crate::Error;
use crate::interchange::AnkiProblem;
use crate::test_support::{png_bytes, within};

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

#[test]
fn both_containers_of_a_hand_made_package_import() {
    for container in [Container::Legacy, Container::Current] {
        let mut b = one_basic();
        b.media("a.png", &png_bytes(3));
        b.note(11, BASIC, &["<img src=\"a.png\">", "x"], "");
        b.card(101, 11, DEFAULT_DECK, 0);
        let mut env = Env::new();
        let report = env.import(&b.build(container)).unwrap();
        assert_eq!((report.cards, report.new_media), (2, 1), "{container:?}");
    }
}

#[test]
fn not_a_package_is_refused() {
    let env = Env::new();
    assert!(matches!(
        refused(&env, b"not a zip"),
        AnkiProblem::InvalidArchive(_)
    ));
    assert_eq!(
        refused(&env, &zip_of(&[("other".into(), b"x".to_vec())])),
        AnkiProblem::NoCollection
    );
    // A Rappel export is not an Anki package.
    let export = env.store.export_bytes_at(T0).unwrap().0;
    assert_eq!(refused(&env, &export), AnkiProblem::NoCollection);
}

#[test]
fn unknown_versions_are_refused() {
    let env = Env::new();
    let mut b = one_basic();
    b.raw("meta", &[0x08, 99]);
    assert_eq!(
        refused(&env, &b.build(Container::Legacy)),
        AnkiProblem::NewerVersion(99)
    );
    let mut b = one_basic();
    b.version(99);
    assert_eq!(
        refused(&env, &b.build(Container::Legacy)),
        AnkiProblem::UnsupportedSchema(99)
    );
    let mut b = one_basic();
    b.raw("meta", &[0x08]);
    assert!(matches!(
        refused(&env, &b.build(Container::Legacy)),
        AnkiProblem::Damaged(_)
    ));
}

#[test]
fn a_zstd_bomb_stops_at_the_cap() {
    within(Duration::from_secs(10), "zstd bomb", || {
        let env = Env::new();
        // The collection: 8 GiB of zeros in 256 KiB.
        let mut b = one_basic();
        b.raw("meta", &[0x08, 3])
            .raw("collection.anki21b", &zstd_bomb(8 << 30));
        let bytes = b.build(Container::Current);
        let limits = AnkiLimits {
            collection: 16 << 20,
            ..AnkiLimits::DEFAULT
        };
        let result = env.store.plan_anki_bytes(&bytes, &limits, PLAN_NOW);
        env.assert_no_temp();
        assert!(
            matches!(
                result,
                Err(Error::AnkiRefused(AnkiProblem::EntryTooLarge { .. }))
            ),
            "{result:?}"
        );
        // The media list.
        let mut b = one_basic();
        b.raw("media", &zstd_bomb(1 << 30));
        let problem = refused(&env, &b.build(Container::Current));
        assert!(
            matches!(problem, AnkiProblem::EntryTooLarge { .. }),
            "{problem:?}"
        );
        // A media file: left out, the card kept.
        let mut b = one_basic();
        b.note(11, BASIC, &["<img src=\"bomb.png\">", "x"], "")
            .card(101, 11, DEFAULT_DECK, 0);
        b.media("bomb.png", b"");
        let mut parts = entries(&b.build(Container::Current));
        for (name, data) in &mut parts {
            if name == "0" {
                *data = zstd_bomb_after(&png_bytes(1), 4 << 30);
            }
        }
        let plan = env.plan(&zip_of(&parts)).unwrap();
        assert_eq!(count(&plan.summary().skipped, Skip::MediaTooLarge), 1);
        assert_eq!(plan.summary().cards, 2);
    });
}

#[test]
fn a_window_larger_than_the_limit_is_refused() {
    let env = Env::new();
    // A frame asking for a 2 GiB window (exponent 21).
    let frame = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 21 << 3, 0x01, 0, 0];
    let mut b = one_basic();
    b.raw("meta", &[0x08, 3]).raw("collection.anki21b", &frame);
    assert!(matches!(
        refused(&env, &b.build(Container::Current)),
        AnkiProblem::Damaged(_)
    ));
    // Data after the frame.
    let mut data = zstd(&b.collection());
    data.extend_from_slice(b"trailing");
    let mut b = one_basic();
    b.raw("meta", &[0x08, 3]).raw("collection.anki21b", &data);
    assert!(matches!(
        refused(&env, &b.build(Container::Current)),
        AnkiProblem::Damaged(_)
    ));
}

#[test]
fn too_many_entries_or_media_files_are_refused() {
    let env = Env::new();
    let mut b = one_basic();
    for i in 0..20 {
        b.media(&format!("{i}.png"), &png_bytes(i));
    }
    let bytes = b.build(Container::Legacy);
    let mut limits = AnkiLimits::DEFAULT;
    limits.archive.entries = 10;
    let result = env.store.plan_anki_bytes(&bytes, &limits, PLAN_NOW);
    assert!(matches!(
        result,
        Err(Error::AnkiRefused(AnkiProblem::TooManyEntries { .. }))
    ));
    let mut limits = AnkiLimits::DEFAULT;
    limits.media_files = 10;
    let result = env.store.plan_anki_bytes(&bytes, &limits, PLAN_NOW);
    assert!(matches!(
        result,
        Err(Error::AnkiRefused(AnkiProblem::TooManyMediaFiles { .. }))
    ));
}

#[test]
fn broken_sqlite_is_refused() {
    let env = Env::new();
    let mut b = one_basic();
    let good = b.collection();
    for damaged in [
        b"not a database at all".to_vec(),
        good[..good.len() / 2].to_vec(),
        {
            let mut d = good.clone();
            // Garble every page but the first.
            for byte in d.iter_mut().skip(4096) {
                *byte ^= 0x5a;
            }
            d
        },
        Vec::new(),
    ] {
        let mut b = one_basic();
        b.raw("collection.anki21", &damaged);
        let problem = refused(&env, &b.build(Container::Legacy));
        assert!(matches!(problem, AnkiProblem::Damaged(_)), "{problem:?}");
    }
}

#[test]
fn views_are_refused_and_triggers_never_run() {
    let env = Env::new();
    let mut b = one_basic();
    b.sql("ALTER TABLE notes RENAME TO real_notes; CREATE VIEW notes AS SELECT * FROM real_notes;");
    let problem = refused(&env, &b.build(Container::Legacy));
    assert!(matches!(problem, AnkiProblem::Damaged(_)), "{problem:?}");

    // A trigger would write if anything wrote; the import only reads.
    let mut b = one_basic();
    b.sql(
        "CREATE TABLE log (x); CREATE TRIGGER t AFTER INSERT ON cards BEGIN INSERT INTO log VALUES (1); END;
         CREATE TRIGGER u AFTER UPDATE ON notes BEGIN INSERT INTO log VALUES (2); END;",
    );
    let mut env = Env::new();
    let report = env.import(&b.build(Container::Legacy)).unwrap();
    assert_eq!(report.cards, 1);
}

#[test]
fn notes_that_do_not_match_their_note_type_are_left_out() {
    let mut b = one_basic();
    // Three fields for Basic's two; one; an unknown note type; a card of
    // a missing note; a template ord Basic has not.
    b.note(11, BASIC, &["a", "b", "c"], "")
        .card(101, 11, DEFAULT_DECK, 0);
    b.note(12, BASIC, &["only"], "")
        .card(102, 12, DEFAULT_DECK, 0);
    b.note(13, 999, &["a", "b"], "")
        .card(103, 13, DEFAULT_DECK, 0);
    b.card(104, 77, DEFAULT_DECK, 0);
    b.note(14, BASIC, &["a", "b"], "")
        .card(105, 14, DEFAULT_DECK, 5);
    // Ids and kinds of the wrong type.
    b.sql("INSERT INTO cards VALUES ('x', 10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, '');");
    b.sql("INSERT INTO cards VALUES (106, 10, 'deck', 'ord', 0, 0, 'type', X'00', 0, 0, 0, 0, 0, 0, 0, 0, 0, '');");
    let env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    let s = plan.summary();
    assert_eq!(s.cards, 1, "{s:#?}");
    assert_eq!(count(&s.skipped, Skip::BrokenNote), 7, "{s:#?}");
}

#[test]
fn hostile_templates_are_left_out_quickly() {
    within(Duration::from_secs(10), "hostile templates", || {
        let deep = "{{#Front}}".repeat(500) + &"{{/Front}}".repeat(500);
        let huge = "x".repeat(300 << 10);
        let mut b = Builder::new();
        for (id, qfmt) in [
            (5, deep.as_str()),
            (6, huge.as_str()),
            (7, "{{#Front}}never closed"),
            (8, "{{Front}}".repeat(20_000).as_str()),
        ] {
            b.model(
                id,
                json!({"name": "T", "type": 0,
                       "flds": [{"name": "Front", "ord": 0}],
                       "tmpls": [{"name": "C", "ord": 0, "qfmt": qfmt, "afmt": "{{Front}}"}]}),
            );
        }
        let big = "y".repeat(100_000);
        for (n, mid) in [5, 6, 7, 8].into_iter().enumerate() {
            let id = i64::try_from(n).unwrap() + 20;
            let field = if mid == 8 { big.as_str() } else { "f" };
            b.note(id, mid, &[field], "")
                .card(id * 10, id, DEFAULT_DECK, 0);
        }
        // Templates that are not text, field lists with gaps.
        b.model(
            9,
            json!({"name": "N", "flds": [{"name": "A", "ord": 1}], "tmpls": []}),
        );
        b.model(10, json!({"name": "N", "flds": [], "tmpls": [{"name": "C", "ord": 0, "qfmt": 5, "afmt": null}]}));
        b.note(30, 9, &["a"], "").card(300, 30, DEFAULT_DECK, 0);
        b.note(31, 10, &[], "").card(310, 31, DEFAULT_DECK, 0);
        let env = Env::new();
        let plan = env.plan(&b.build(Container::Legacy)).unwrap();
        let s = plan.summary();
        assert_eq!(s.cards, 0, "{s:#?}");
        assert_eq!(count(&s.skipped, Skip::BadTemplate), 3, "{s:#?}");
        assert_eq!(count(&s.skipped, Skip::TooLong), 1, "{s:#?}");
        assert_eq!(count(&s.skipped, Skip::BrokenNote), 2, "{s:#?}");
    });
}

#[test]
fn giant_fields_are_left_out() {
    let mut b = one_basic();
    let giant = "z".repeat(2 << 20);
    b.note(11, BASIC, &[&giant, "b"], "")
        .card(101, 11, DEFAULT_DECK, 0);
    b.note(12, BASIC, &["a", "b"], &"t ".repeat(40_000))
        .card(102, 12, DEFAULT_DECK, 0);
    let env = Env::new();
    let limits = AnkiLimits {
        note: 1 << 20,
        note_tags: 1 << 10,
        ..AnkiLimits::DEFAULT
    };
    let plan = env
        .store
        .plan_anki_bytes(&b.build(Container::Legacy), &limits, PLAN_NOW)
        .unwrap();
    assert_eq!(plan.summary().cards, 1);
    assert_eq!(count(&plan.summary().skipped, Skip::TooLong), 2);
    // A side over Rappel's 1 MiB once converted.
    let mut b = one_basic();
    let big = "<b>w</b> ".repeat(200_000);
    b.note(11, BASIC, &[&big, "b"], "")
        .card(101, 11, DEFAULT_DECK, 0);
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    assert_eq!(count(&plan.summary().skipped, Skip::TooLong), 1);
}

#[test]
fn review_entries_out_of_range_are_left_out() {
    let mut b = one_basic();
    for (id, ease, kind) in [
        (i64::MIN, 3, 1),
        (-1, 3, 1),
        (0, 3, 1),
        (253_402_300_800_000, 3, 1),
        (i64::MAX, 3, 1),
        (1_000, 9, 1),
        (1_001, -1, 1),
        (1_002, 3, 7),
        (1_003, 3, 3),
        (1_004, 0, 1),
        (1_005, 2, 5),
        (1_006, 4, 2),
    ] {
        b.rev(id, 100, ease, 5_000_000_000, kind);
    }
    let mut env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    let s = plan.summary();
    // 0 (1970) and 1_006 (an Easy relearn) are reviews.
    assert_eq!(s.reviews, 2, "{s:#?}");
    assert_eq!(count(&s.skipped, Skip::BadReview), 7);
    assert_eq!(count(&s.skipped, Skip::FilteredReview), 1);
    assert_eq!(count(&s.skipped, Skip::ManualEntry), 2);
    env.import(&b.build(Container::Legacy)).unwrap();
    let longest: i64 = env
        .store
        .conn
        .query_row("SELECT max(duration_ms) FROM review_events", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(longest, 3_600_000, "an hour at most");
    // The import exports and imports again.
    let export = env.store.export_bytes_at(T0).unwrap().0;
    let other = Env::new();
    other
        .store
        .plan_import_bytes(&export, &crate::interchange::Limits::DEFAULT)
        .unwrap();
}

#[test]
fn deck_names_become_rappel_names() {
    let mut b = Builder::new();
    let names = [
        "::A::::B::",
        "  ",
        "A:::B",
        "Tab\there\u{1}",
        "x\u{1f}y",
        "",
    ];
    for (i, name) in names.iter().enumerate() {
        let id = i64::try_from(i).unwrap() + 10;
        b.deck(id, name);
        b.note(id, BASIC, &[&format!("card {i}"), "b"], "")
            .card(id * 10, id, id, 0);
    }
    let long = "n".repeat(2000);
    b.deck(50, &long);
    b.note(50, BASIC, &["long", "b"], "").card(500, 50, 50, 0);
    // A deck id that is not there, and a filtered deck's card.
    b.note(51, BASIC, &["lost", "b"], "").card(510, 51, 999, 0);
    b.deck(52, "Home");
    b.note(52, BASIC, &["filtered", "b"], "")
        .card_with(520, 52, 999, 0, 52, 0, 0);
    let mut env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    assert_eq!(count(&plan.summary().skipped, Skip::DeckNameShortened), 1);
    env.import(&b.build(Container::Legacy)).unwrap();
    let deck_of = |front: &str| {
        env.cards()
            .into_iter()
            .find(|c| c.1 == front)
            .map(|c| c.0)
            .unwrap()
    };
    assert_eq!(deck_of("card 0"), "A › B");
    assert_eq!(deck_of("card 1"), "Default");
    assert_eq!(deck_of("card 2"), "A › :B");
    assert_eq!(deck_of("card 3"), "Tab here");
    assert_eq!(deck_of("card 4"), "x › y");
    assert_eq!(deck_of("card 5"), "Default");
    let shortened = deck_of("long");
    assert!(shortened.len() <= crate::domain::MAX_DECK_NAME_BYTES && shortened.ends_with('…'));
    assert_eq!(deck_of("lost"), "Default");
    assert_eq!(deck_of("filtered"), "Home");
}

#[test]
fn bad_media_names_are_never_used() {
    let mut b = Builder::new();
    for (i, name) in [
        "../escape.png",
        "dir/a.png",
        "a\\b.png",
        "..",
        "dup.png",
        "dup.png",
        "ok.png",
    ]
    .iter()
    .enumerate()
    {
        b.media(name, &png_bytes(u32::try_from(i).unwrap()));
        let id = i64::try_from(i).unwrap() + 10;
        b.note(id, BASIC, &[&format!("<img src=\"{name}\"> {i}"), "b"], "")
            .card(id * 10, id, DEFAULT_DECK, 0);
    }
    let mut env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    let s = plan.summary();
    assert_eq!(s.cards, 7);
    assert_eq!(s.new_media, 1, "only ok.png");
    // Four names that cannot be file names, and one used twice.
    assert_eq!(count(&s.skipped, Skip::BadMediaName), 5, "{s:#?}");
    env.import(&b.build(Container::Legacy)).unwrap();
    // Nothing was written outside the media folder.
    let outside: Vec<_> = std::fs::read_dir(env.tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(outside, ["data"]);
}

#[test]
fn a_malformed_media_list_is_refused() {
    let env = Env::new();
    for list in [
        &b"not json"[..],
        b"{\"x\": \"a.png\"}",
        b"{\"0\": 5}",
        b"[1, 2]",
    ] {
        let mut b = one_basic();
        b.raw("media", list);
        assert!(matches!(
            refused(&env, &b.build(Container::Legacy)),
            AnkiProblem::Damaged(_)
        ));
    }
    let mut b = one_basic();
    b.raw("media", &zstd(&[0x0a, 0x05, 0x0a]));
    assert!(matches!(
        refused(&env, &b.build(Container::Current)),
        AnkiProblem::Damaged(_)
    ));
}

#[test]
fn media_of_other_kinds_are_left_out() {
    let mut b = one_basic();
    b.media(
        "pic.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
    );
    b.media("page.html", b"<html><body>x</body></html>");
    b.media("fake.mp3", b"ID3\x04\0\0\0\0\0\0junk");
    b.media("empty.png", b"");
    b.media("sound.png", &png_bytes(1));
    b.note(
        11,
        BASIC,
        &["<img src=\"pic.svg\"><img src=\"page.html\"> [sound:fake.mp3] <img src=\"empty.png\"> [sound:sound.png]", "b"],
        "",
    )
    .card(101, 11, DEFAULT_DECK, 0);
    let env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    let k = &plan.summary().skipped;
    assert_eq!(count(k, Skip::UnsupportedMedia), 3, "{k:#?}");
    assert_eq!(count(k, Skip::DamagedMedia), 2, "{k:#?}");
    assert_eq!(plan.summary().new_media, 0);
}

#[test]
fn cloze_cards_without_their_cloze_are_empty() {
    let mut b = Builder::new();
    b.note(10, CLOZE, &["{{c1::a}} b", ""], "")
        .card(100, 10, DEFAULT_DECK, 0)
        .card(101, 10, DEFAULT_DECK, 1);
    let env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    assert_eq!(plan.summary().cards, 1);
    assert_eq!(count(&plan.summary().skipped, Skip::EmptyFront), 1);
}

#[test]
fn tags_rappel_cannot_have_are_left_out() {
    let many: Vec<String> = (0..150).map(|i| format!("t{i}")).collect();
    let mut b = Builder::new();
    b.note(
        10,
        BASIC,
        &["a", "b"],
        &format!(" ok a,b {} T0 ", "x".repeat(300)),
    )
    .card(100, 10, DEFAULT_DECK, 0);
    b.note(11, BASIC, &["c", "d"], &many.join(" "))
        .card(110, 11, DEFAULT_DECK, 0);
    let mut env = Env::new();
    let plan = env.plan(&b.build(Container::Legacy)).unwrap();
    let k = &plan.summary().skipped;
    assert_eq!(count(k, Skip::InvalidTag), 2, "{k:#?}");
    assert_eq!(examples(k, Skip::InvalidTag)[0], "a,b");
    assert_eq!(count(k, Skip::TooManyTags), 1);
    env.import(&b.build(Container::Legacy)).unwrap();
    let ids: Vec<String> = env.cards().iter().map(|c| c.1.clone()).collect();
    assert_eq!(ids, ["a", "c"]);
    let id: String = env
        .store
        .conn
        .query_row("SELECT id FROM cards WHERE front_md = 'c'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(env.store.card_tags(&id).unwrap().len(), 100);
    let id: String = env
        .store
        .conn
        .query_row("SELECT id FROM cards WHERE front_md = 'a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    // "ok" and "T0".
    assert_eq!(env.store.card_tags(&id).unwrap().len(), 2);
}

#[test]
fn a_failed_import_leaves_no_temporary_file() {
    let mut b = one_basic();
    b.version(3);
    let env = Env::new();
    // A leftover of a crash is removed by the next import.
    let stale = env.data_dir().join(".anki-import-0123456789abcdef");
    std::fs::write(&stale, b"old").unwrap();
    let keep = env.data_dir().join(".anki-import-notours");
    std::fs::write(&keep, b"keep").unwrap();
    let _ = env.plan(&b.build(Container::Legacy));
    assert!(!stale.exists());
    assert!(keep.exists(), "only names an import makes are removed");
    std::fs::remove_file(keep).unwrap();
    env.assert_no_temp();
}

#[test]
fn a_package_of_20000_cards_imports_quickly() {
    within(Duration::from_secs(60), "20,000 cards", || {
        let mut b = Builder::new();
        b.sql("BEGIN");
        for i in 0..10_000_i64 {
            let id = 1_000_000 + i;
            b.note(
                id,
                BASIC,
                &[
                    &format!("<b>front</b> {i} &amp; \\(x^{i}\\)"),
                    &format!("back {i}<br>line"),
                ],
                "tag::a leech",
            );
            b.card(id * 2, id, DEFAULT_DECK, 0);
            b.rev(1_700_000_000_000 + i * 10, id * 2, 3, 4000, 0);
            b.rev(1_700_000_000_001 + i * 10, id * 2, 1, 4000, 1);
        }
        let reversed = json!({
            "name": "Rev", "type": 0,
            "flds": [{"name": "Front", "ord": 0}, {"name": "Back", "ord": 1}],
            "tmpls": [{"name": "1", "ord": 0, "qfmt": "{{Front}}", "afmt": "{{Back}}"},
                      {"name": "2", "ord": 1, "qfmt": "{{Back}}", "afmt": "{{Front}}"}]
        });
        b.model(3, reversed);
        for i in 0..5_000_i64 {
            let id = 2_000_000 + i;
            b.note(id, 3, &[&format!("q {i}"), &format!("a {i}")], "");
            b.card(id * 2, id, DEFAULT_DECK, 0)
                .card(id * 2 + 1, id, DEFAULT_DECK, 1);
        }
        b.sql("COMMIT");
        let bytes = b.build(Container::Current);
        let mut env = Env::new();
        let report = env.import(&bytes).unwrap();
        assert_eq!(report.cards, 20_000);
        assert_eq!(report.reviews, 20_000);
        env.assert_no_temp();
    });
}

// ------------------------------------------------- round 3 (ADR 0063)

/// The largest SQLite pages (64 KiB), a header that claims two billion
/// pages, the WAL format flags, unknown format flags, and 5,000 tables:
/// each read or refused as damaged, quickly, nothing written.
#[test]
fn sqlite_files_at_their_extremes_are_read_or_refused_quickly() {
    // 64 KiB pages: an ordinary database.
    let mut b = one_basic();
    b.sql("PRAGMA journal_mode = DELETE; PRAGMA page_size = 65536; VACUUM;");
    let big_pages = b.build(Container::Legacy);
    let mut env = Env::new();
    assert_eq!(env.import(&big_pages).unwrap().cards, 1);

    let good = one_basic().collection();
    let with_header = |at: usize, bytes: &[u8]| {
        let mut c = good.clone();
        c[at..at + bytes.len()].copy_from_slice(bytes);
        let mut b = one_basic();
        b.raw("collection.anki21", &c);
        b.build(Container::Legacy)
    };
    // The page count in the header (offset 28): SQLite trusts it only
    // with a matching change counter; else the file's size.
    let lying = with_header(28, &0x7FFF_FFFF_u32.to_be_bytes());
    // Read and write versions 2 (WAL): set back to a rollback journal in
    // the copy (ADR 0061), so the database reads.
    let wal = with_header(18, &[2, 2]);
    // An unknown file format: refused as damaged.
    let unknown = with_header(18, &[9, 9]);
    for (what, package) in [
        ("lying page count", lying),
        ("WAL", wal),
        ("unknown format", unknown),
    ] {
        let env = Env::new();
        let planned = within(Duration::from_secs(10), "an odd SQLite header", move || {
            env.plan(&package).map(|p| p.summary().cards)
        });
        match what {
            "WAL" => assert!(planned.is_ok(), "{what}: {planned:?}"),
            // Read or refused as damaged, never anything else.
            _ => assert!(
                matches!(
                    planned,
                    Ok(_) | Err(Error::AnkiRefused(AnkiProblem::Damaged(_)))
                ),
                "{what}: {planned:?}"
            ),
        }
    }

    // 5,000 tables beside Anki's: the schema and the check stay quick.
    let mut b = one_basic();
    let mut tables = String::new();
    for i in 0..5_000 {
        tables.push_str("CREATE TABLE t");
        tables.push_str(&i.to_string());
        tables.push_str(" (a, b, c);");
    }
    b.sql(&tables);
    let package = b.build(Container::Legacy);
    let env = Env::new();
    let planned = within(Duration::from_secs(20), "5,000 tables", move || {
        env.plan(&package).map(|p| p.summary().cards)
    });
    // Past the collection's table limit: refused, from the schema.
    assert!(
        matches!(&planned, Err(Error::AnkiRefused(AnkiProblem::Damaged(why))) if why.contains("too many tables")),
        "{planned:?}"
    );
}
