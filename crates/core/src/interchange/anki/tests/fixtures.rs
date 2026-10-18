//! The packages Anki itself made (`../fixtures/make_fixtures.py`): the
//! current format, the legacy one, Anki 2.0's (derived) and a collection
//! package. They hold the same collection.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::Error;
use crate::domain::CardState;
use crate::interchange::AnkiFormat;
use crate::test_support::DAY_MS;

const PACKAGES: [&str; 4] = [
    "current.apkg",
    "legacy.apkg",
    "anki2.apkg",
    "current.colpkg",
];

fn imported(name: &str) -> (Env, AnkiReport) {
    let mut env = Env::new();
    let report = env.import(&fixture(name)).unwrap();
    env.assert_no_temp();
    (env, report)
}

/// The id of the live card whose front is `front`.
fn card_id(env: &Env, front: &str) -> String {
    env.store
        .conn
        .query_row(
            "SELECT id FROM cards WHERE front_md = ?1 AND deleted_at IS NULL",
            [front],
            |r| r.get(0),
        )
        .unwrap()
}

/// The grades and durations of a card's reviews, and its other entries.
fn log(env: &Env, card: &str) -> Vec<(String, Option<String>, i64)> {
    let mut stmt = env
        .store
        .conn
        .prepare(
            "SELECT kind, grade, duration_ms FROM review_events WHERE card_id = ?1
             ORDER BY reviewed_at, id",
        )
        .unwrap();
    stmt.query_map([card], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn every_format_gives_the_same_cards() {
    let (reference, _) = imported("current.apkg");
    let cards = reference.cards();
    assert_eq!(cards.len(), 16);
    for name in &PACKAGES[1..] {
        let (env, _) = imported(name);
        assert_eq!(env.cards(), cards, "{name}");
    }
}

#[test]
fn the_formats_are_told_apart() {
    let env = Env::new();
    let formats: Vec<AnkiFormat> = PACKAGES
        .iter()
        .map(|name| env.plan(&fixture(name)).unwrap().summary().format)
        .collect();
    assert_eq!(
        formats,
        [
            AnkiFormat::Current,
            AnkiFormat::Anki21,
            AnkiFormat::Anki20,
            AnkiFormat::Current
        ]
    );
}

#[test]
fn the_plan_counts_what_is_imported_and_what_is_not() {
    for name in PACKAGES {
        let env = Env::new();
        let plan = env.plan(&fixture(name)).unwrap();
        let s = plan.summary();
        assert_eq!(s.cards, 16, "{name}");
        // Default, Lang, Lang › Français › Verbes, Science, Science › Geo.
        assert_eq!((s.new_decks, s.existing_decks), (5, 0), "{name}");
        assert_eq!(s.suspended, 1, "{name}");
        // 8 + 3 (the reversed note's front) + 1 (its back) + 2 + 1 (the
        // cloze) + 1 (the math card).
        assert_eq!(s.reviews, 16, "{name}");
        assert_eq!((s.new_media, s.present_media), (3, 0), "{name}");
        // geo::europe, 日本語, über, marked, leech.
        assert_eq!(s.tags, 5, "{name}");
        let k = &s.skipped;
        assert_eq!(count(k, Skip::EmptyFront), 1, "{name}");
        assert_eq!(examples(k, Skip::EmptyFront), ["poisson"]);
        assert_eq!(count(k, Skip::UnknownFilter), 2);
        assert_eq!(examples(k, Skip::UnknownFilter), ["myaddon"]);
        assert_eq!(count(k, Skip::RemoteImage), 1);
        assert_eq!(
            examples(k, Skip::RemoteImage),
            ["https://example.org/remote.png"]
        );
        assert_eq!(count(k, Skip::MissingMedia), 1);
        assert_eq!(examples(k, Skip::MissingMedia), ["gone.png"]);
        assert_eq!(count(k, Skip::UnsupportedMedia), 1);
        assert_eq!(examples(k, Skip::UnsupportedMedia), ["pic.svg"]);
        // "Set due date" and "Forget".
        assert_eq!(count(k, Skip::ManualEntry), 2);
        // Anki leaves unused files out of a deck package, not out of a
        // collection package.
        let unused = usize::from(name == "current.colpkg");
        assert_eq!(count(k, Skip::UnusedMedia), unused, "{name}");
        let reasons: Vec<Skip> = k.iter().map(|s| s.reason).collect();
        let mut sorted = reasons.clone();
        sorted.sort();
        assert_eq!(reasons, sorted, "in report order");
        assert_eq!(k.len(), 6 + unused, "{name}: {k:?}");
    }
}

#[test]
fn the_plan_writes_nothing() {
    let env = Env::new();
    env.plan(&fixture("current.apkg")).unwrap();
    assert_eq!(env.cards().len(), 0);
    assert_eq!(env.store.decks().unwrap().len(), 0);
    let media = env.data_dir().join("media");
    assert!(!media.exists() || std::fs::read_dir(media).unwrap().count() == 0);
    env.assert_no_temp();
}

#[test]
fn cards_become_plain_markdown_cards() {
    let (env, report) = imported("current.apkg");
    assert_eq!(report.cards, 16);
    assert!(report.backup.is_some());
    let cards = env.cards();
    let find = |front: &str| {
        cards
            .iter()
            .find(|c| c.1 == front)
            .unwrap_or_else(|| panic!("no card {front:?} in {cards:#?}"))
            .clone()
    };
    // HTML: bold, italic, underline as text, entities, lists, line breaks.
    let basic = find("Bonjour **bold** *italic* under \\& \\<x\\> café é😀\u{a0}!");
    assert_eq!(basic.0, "Default");
    assert_eq!(
        basic.2,
        "- one\n- two **2**\n\n1. first\n\nline\\\nbreak\\\na div\\\nred"
    );
    // Both directions of a reversed note, in the nested deck.
    assert_eq!(
        find("chat"),
        (
            "Lang › Français › Verbes".to_owned(),
            "chat".into(),
            "cat".into(),
            false
        )
    );
    assert_eq!(find("cat").2, "chat");
    // The optional reverse only where the note asks for it.
    assert_eq!(find("dog").2, "chien");
    assert!(cards.iter().all(|c| c.1 != "bird"));
    // Type in the answer: nothing on the front, the answer on the back.
    assert_eq!(find("Capital of Italy?").2, "Rome");
    // Cloze: each card hides its own clozes; the back shows them in bold
    // with the extra field. The filtered deck's cards are in their home deck.
    let c1 = find("\\[…\\] is the capital of France; the \\[…\\] runs through it.");
    assert_eq!(c1.0, "Science › Geo");
    assert_eq!(
        c1.2,
        "**Paris** is the capital of France; the **Seine** runs through it.\\\nGeography"
    );
    let c2 = find("Paris is the capital of \\[country\\]; the Seine runs through it.");
    assert_eq!(
        c2.2,
        "Paris is the capital of **France**; the Seine runs through it.\\\nGeography"
    );
    // A custom note type: conditionals, `text:`, an unknown filter, and
    // the special fields.
    assert_eq!(find("manger\\\nJe mange.").2, "to eat irregular manger");
    assert_eq!(find("boire").2, "to drink*no note* boire");
    assert_eq!(
        find("to eat (Info, Vocab)").2,
        "manger\\\nDeck Lang::Français::Verbes, subdeck Verbes, tags"
    );
    // Media, links, a script dropped, remote and missing files as text,
    // Markdown characters escaped.
    let html = cards
        .iter()
        .find(|c| c.1.starts_with("Script gone"))
        .unwrap();
    assert!(html.1.starts_with(
        "Script gone, [a link](<https://example.org/page>), ![](media:0c6665cf4dbd77d303d5d6bc5f28d892d19b0cb58aa22cf9b827d354dbc40b6c.png) and https://example.org/remote.png"
    ));
    assert!(!html.1.contains("alert"));
    assert!(html.2.starts_with("[clip.mp3](media:"), "{}", html.2);
    assert!(html.2.contains(" pic.svg ![a photo](media:"), "{}", html.2);
    assert!(
        html.2
            .ends_with(".jpg) gone.png Costs \\$5 and 3\\*4\\*5\\_6 \\# not a heading")
    );
    // Math, code blocks and code spans; the suspended card stays so.
    let math = find("Inline $x^2 < y$ and display $$\\int_0^1 x\\,dx$$");
    assert_eq!(
        math.2,
        "```\nfn main() {\n    println!(\"*hi*\");\n}\n```\n\nUse `a_b` here."
    );
    assert!(math.3, "suspended");
    // A buried card is not suspended.
    assert!(!html.3);
    // Every side parses back to what the converter meant: the escapes hold.
    for (_, front, back, _) in &cards {
        for side in [front, back] {
            assert!(crate::domain::card_side_fits(side));
            assert!(!side.contains('\0'));
        }
    }
}

#[test]
fn the_media_files_go_into_the_media_folder() {
    let (env, report) = imported("legacy.apkg");
    assert_eq!(report.new_media, 3);
    let folder = env.data_dir().join("media");
    let mut names: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let extensions: Vec<&str> = names
        .iter()
        .map(|n| n.rsplit('.').next().unwrap())
        .collect();
    assert_eq!(extensions, ["png", "mp3", "jpg"]);
    let rows: i64 = env
        .store
        .conn
        .query_row("SELECT count(*) FROM media", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 3);
    // Each is what the card names, and what the fixture's file is.
    let png = std::fs::read(
        folder.join(
            names
                .iter()
                .find(|n| n.rsplit('.').next() == Some("png"))
                .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(png, fixture("media/local.png"));
}

#[test]
fn the_history_is_kept_and_the_schedule_rebuilt_from_it() {
    let (mut env, _) = imported("current.apkg");
    let basic = card_id(
        &env,
        "Bonjour **bold** *italic* under \\& \\<x\\> café é😀\u{a0}!",
    );
    // Seven seconds each (the fixture's timer, to the second).
    let grades: Vec<(String, Option<String>, i64)> = log(&env, &basic)
        .into_iter()
        .map(|(kind, grade, ms)| (kind, grade, ms / 1000))
        .collect();
    // Again, Good, Good, Good, Again, Good, Hard, Easy.
    let expected: Vec<_> = [
        "fail", "pass", "pass", "pass", "fail", "pass", "pass", "pass",
    ]
    .iter()
    .map(|g| ("review".to_owned(), Some((*g).to_owned()), 7))
    .collect();
    assert_eq!(grades[..], expected[..]);
    assert!(matches!(
        env.store.card_state(&basic).unwrap(),
        CardState::Scheduled(_)
    ));
    // The forgotten cloze card: its reviews, then a reset; new again.
    let forgotten = card_id(
        &env,
        "\\[…\\] is the capital of France; the \\[…\\] runs through it.",
    );
    let kinds: Vec<String> = log(&env, &forgotten).into_iter().map(|e| e.0).collect();
    assert_eq!(kinds, ["review", "review", "reset"]);
    assert_eq!(env.store.card_state(&forgotten).unwrap(), CardState::New);
    // The rescheduled card keeps its one review, not the manual entry.
    let back = card_id(&env, "cat");
    assert_eq!(log(&env, &back).len(), 1);
    // New cards have no history.
    assert_eq!(log(&env, &card_id(&env, "Capital of Italy?")).len(), 0);
    // The cache is what a replay of the whole log gives.
    let before = crate::test_support::schedule_table(&env.store);
    let scheduler = crate::storage::scheduler_in(&env.store.conn).unwrap();
    env.store.rebuild_schedule_cache(&scheduler).unwrap();
    assert_eq!(crate::test_support::schedule_table(&env.store), before);
}

#[test]
fn tags_are_kept() {
    let (env, _) = imported("current.apkg");
    let names = |front: &str| -> Vec<String> {
        env.store
            .card_tags(&card_id(&env, front))
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect()
    };
    assert_eq!(
        names("Bonjour **bold** *italic* under \\& \\<x\\> café é😀\u{a0}!"),
        ["geo::europe", "über", "日本語"]
    );
    assert_eq!(names("chat"), ["marked"]);
    assert_eq!(
        names("\\[…\\] is the capital of France; the \\[…\\] runs through it."),
        ["leech"]
    );
    assert_eq!(names("Capital of Italy?").len(), 0);
}

#[test]
fn importing_twice_adds_nothing() {
    let (mut env, _) = imported("current.apkg");
    let before = env.cards();
    let plan = env.plan(&fixture("legacy.apkg")).unwrap();
    let s = plan.summary();
    assert_eq!(s.cards, 0);
    assert!(s.is_noop());
    assert_eq!(count(&s.skipped, Skip::AlreadyInRappel), 16);
    let report = env
        .store
        .apply_anki_at(&plan, T0 + 500 * DAY_MS, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.cards, 0);
    assert!(report.backup.is_none(), "nothing written, no backup");
    assert_eq!(env.cards(), before);
}

#[test]
fn cards_go_into_live_decks_of_the_same_name() {
    let mut env = Env::new();
    let lang = env.store.create_deck("lang").unwrap();
    let plan = env.plan(&fixture("current.apkg")).unwrap();
    assert_eq!(
        (plan.summary().new_decks, plan.summary().existing_decks),
        (4, 1)
    );
    env.import(&fixture("current.apkg")).unwrap();
    let in_lang = env.store.list_cards(&lang.id).unwrap();
    assert_eq!(in_lang.len(), 1);
    assert_eq!(in_lang[0].front_md, "Capital of Italy?");
}

#[test]
fn a_cancelled_import_writes_nothing() {
    let mut env = Env::new();
    let cancel = AtomicBool::new(true);
    let bytes = fixture("current.apkg");
    let source = crate::interchange::source::Source::Bytes(std::sync::Arc::from(&bytes[..]));
    let plan = env
        .store
        .plan_anki_source(source.clone(), &AnkiLimits::DEFAULT, &cancel);
    assert!(matches!(plan, Err(Error::ImportCancelled)), "{plan:?}");
    env.assert_no_temp();
    let plan = env.plan(&bytes).unwrap();
    cancel.store(true, Ordering::Relaxed);
    let applied = env.store.apply_anki_at(&plan, T0, &cancel);
    assert!(
        matches!(applied, Err(Error::ImportCancelled)),
        "{applied:?}"
    );
    assert_eq!(env.cards().len(), 0);
    assert_eq!(env.store.decks().unwrap().len(), 0);
    env.assert_no_temp();
}

#[test]
fn a_database_changed_since_the_plan_refuses_the_apply() {
    let mut env = Env::new();
    let plan = env.plan(&fixture("current.apkg")).unwrap();
    // The same card as one of the package's: the plan would add it, the
    // apply would not.
    let deck = env.store.create_deck("Lang").unwrap();
    env.store
        .add_card(&deck.id, "Capital of Italy?", "Rome")
        .unwrap();
    let applied = env.store.apply_anki_at(&plan, T0, &AtomicBool::new(false));
    assert!(
        matches!(applied, Err(Error::ImportPlanStale)),
        "{applied:?}"
    );
    assert_eq!(env.cards().len(), 1);
}

#[test]
fn a_file_changed_since_the_plan_refuses_the_apply() {
    let mut env = Env::new();
    let path = env.tmp.path().join("deck.apkg");
    std::fs::write(&path, fixture("current.apkg")).unwrap();
    let plan = env
        .store
        .plan_anki_import(&path, &AtomicBool::new(false))
        .unwrap();
    std::fs::write(&path, fixture("legacy.apkg")).unwrap();
    let applied = env.store.apply_anki_at(&plan, T0, &AtomicBool::new(false));
    assert!(
        matches!(applied, Err(Error::ImportFileChanged)),
        "{applied:?}"
    );
    assert_eq!(env.cards().len(), 0);
}

#[test]
fn files_are_told_apart_by_name_and_content() {
    use crate::interchange::{FileKind, file_kind};
    let env = Env::new();
    let dir = env.tmp.path();
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let anki = fixture("current.apkg");
    let rappel = env.store.export_bytes_at(T0).unwrap().0;
    assert_eq!(file_kind(&write("a.apkg", b"x")), Some(FileKind::Anki));
    assert_eq!(file_kind(&write("a.COLPKG", b"x")), Some(FileKind::Anki));
    assert_eq!(file_kind(&write("a.rappel", b"x")), Some(FileKind::Rappel));
    assert_eq!(file_kind(&write("a.cards", b"x")), Some(FileKind::Rappel));
    assert_eq!(file_kind(&write("anki.zip", &anki)), Some(FileKind::Anki));
    assert_eq!(
        file_kind(&write("export.zip", &rappel)),
        Some(FileKind::Rappel)
    );
    assert_eq!(file_kind(&write("other.zip", b"not a zip")), None);
    assert_eq!(file_kind(&dir.join("missing")), None);
}
