//! Format version 2: log entry kinds (ADR 0028), tags (ADR 0029).
//!
//! In the sample's file, the log sorted by id ends with q3's reset (5),
//! its review after the reset (6) and the undo of that review (7); the
//! tags are Algebra (0), Light (1) and Unused (2).

use super::*;

fn sample_doc() -> Value {
    let (env, _, _) = sample_env();
    json_of(&env.export())
}

fn plan_doc(doc: &Value) -> ImportProblem {
    refused(Env::new().plan(&archive_of(doc)))
}

fn field(
    table: &'static str,
    index: usize,
    name: &'static str,
    reason: FieldProblem,
) -> ImportProblem {
    ImportProblem::InvalidField {
        table,
        index,
        field: name,
        reason,
    }
}

/// A change made to a valid document.
type Mutation = fn(&mut Value);

/// The largest id a file may hold (its time is in the year 6429).
const LAST_ID: &str = "7fffffff-ffff-7fff-bfff-ffffffffffff";

#[test]
fn the_sample_has_each_kind_where_these_tests_expect_it() {
    let doc = sample_doc();
    let kinds: Vec<&str> = doc["review_events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "review", "review", "review", "review", "review", "reset", "review", "undo"
        ]
    );
    assert_eq!(
        doc["review_events"][7]["undoes"],
        doc["review_events"][6]["id"]
    );
    let tags: Vec<&str> = doc["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tags, ["Algebra", "Light", "Unused"]);
}

#[test]
fn broken_tags_and_card_tags_are_refused() {
    let cases: Vec<(Mutation, ImportProblem)> = vec![
        (
            |d| d["tags"][0]["id"] = json!("x"),
            field("tags", 0, "id", FieldProblem::NotUuidV7),
        ),
        (
            |d| d["tags"][1]["id"] = d["tags"][0]["id"].clone(),
            field("tags", 1, "id", FieldProblem::DuplicateId),
        ),
        (
            |d| d["tags"][0]["name"] = json!(" "),
            field("tags", 0, "name", FieldProblem::Empty),
        ),
        (
            |d| d["tags"][0]["name"] = json!("Algebra "),
            field("tags", 0, "name", FieldProblem::NotTrimmed),
        ),
        (
            |d| d["tags"][0]["name"] = json!("a".repeat(257)),
            field("tags", 0, "name", FieldProblem::TooLong),
        ),
        (
            |d| d["tags"][0]["name"] = json!("a\u{0}b"),
            field("tags", 0, "name", FieldProblem::ContainsNul),
        ),
        (
            |d| d["tags"][0]["name"] = json!("a,b"),
            field("tags", 0, "name", FieldProblem::ContainsComma),
        ),
        (
            |d| d["tags"][1]["name"] = json!("ALGEBRA"),
            field("tags", 1, "name", FieldProblem::DuplicateTagName),
        ),
        // One name in two Unicode forms (composed and decomposed) is one
        // tag, as in the database (`name_key`, NFC).
        (
            |d| {
                d["tags"][0]["name"] = json!("\u{c9}conomie");
                d["tags"][1]["name"] = json!("E\u{301}conomie");
            },
            field("tags", 1, "name", FieldProblem::DuplicateTagName),
        ),
        (
            |d| d["card_tags"][0]["card_id"] = json!(LAST_ID),
            field("card_tags", 0, "card_id", FieldProblem::UnknownReference),
        ),
        (
            |d| d["card_tags"][0]["tag_id"] = d["decks"][0]["id"].clone(),
            field("card_tags", 0, "tag_id", FieldProblem::UnknownReference),
        ),
        (
            |d| d["card_tags"][1] = d["card_tags"][0].clone(),
            field("card_tags", 1, "tag_id", FieldProblem::DuplicateRow),
        ),
        (
            |d| d["card_tags"].as_array_mut().unwrap().swap(0, 2),
            field("card_tags", 1, "card_id", FieldProblem::OutOfOrder),
        ),
    ];
    for (i, (mutate, expected)) in cases.into_iter().enumerate() {
        let mut doc = sample_doc();
        mutate(&mut doc);
        assert_eq!(plan_doc(&doc), expected, "case {i}");
    }
}

#[test]
fn entries_whose_fields_do_not_fit_their_kind_are_refused() {
    let at = |i, f, r| field("review_events", i, f, r);
    let cases: Vec<(Mutation, ImportProblem)> = vec![
        (
            |d| d["review_events"][5]["kind"] = json!("again"),
            at(5, "kind", FieldProblem::InvalidKind),
        ),
        (
            |d| d["review_events"][0]["grade"] = json!(null),
            at(0, "grade", FieldProblem::Missing),
        ),
        (
            |d| d["review_events"][0]["grade"] = json!("good"),
            at(0, "grade", FieldProblem::InvalidGrade),
        ),
        (
            |d| d["review_events"][5]["grade"] = json!("pass"),
            at(5, "grade", FieldProblem::NotAllowed),
        ),
        (
            |d| d["review_events"][7]["duration_ms"] = json!(5),
            at(7, "duration_ms", FieldProblem::NotAllowed),
        ),
        (
            |d| d["review_events"][5]["duration_ms"] = json!(-5),
            at(5, "duration_ms", FieldProblem::Negative),
        ),
        (
            |d| d["review_events"][7]["undoes"] = json!(null),
            at(7, "undoes", FieldProblem::Missing),
        ),
        (
            |d| d["review_events"][1]["undoes"] = d["review_events"][0]["id"].clone(),
            at(1, "undoes", FieldProblem::NotAllowed),
        ),
    ];
    for (i, (mutate, expected)) in cases.into_iter().enumerate() {
        let mut doc = sample_doc();
        mutate(&mut doc);
        assert_eq!(plan_doc(&doc), expected, "case {i}");
    }
}

#[test]
fn an_undo_must_cancel_one_earlier_review_or_reset_of_its_card() {
    let invalid = field("review_events", 7, "undoes", FieldProblem::InvalidUndo);
    let cases: Vec<Mutation> = vec![
        // Missing from the file.
        |d| d["review_events"][7]["undoes"] = json!(LAST_ID),
        // Another card's review.
        |d| d["review_events"][7]["undoes"] = d["review_events"][0]["id"].clone(),
        // Itself, an undo.
        |d| d["review_events"][7]["undoes"] = d["review_events"][7]["id"].clone(),
        // Dated before its target.
        |d| d["review_events"][7]["reviewed_at"] = json!(0),
    ];
    for (i, mutate) in cases.into_iter().enumerate() {
        let mut doc = sample_doc();
        mutate(&mut doc);
        assert_eq!(plan_doc(&doc), invalid, "case {i}");
    }
    // A second undo of the same entry.
    let mut doc = sample_doc();
    let mut second = doc["review_events"][7].clone();
    second["id"] = json!(LAST_ID);
    doc["review_events"].as_array_mut().unwrap().push(second);
    assert_eq!(
        plan_doc(&doc),
        field("review_events", 8, "undoes", FieldProblem::AlreadyUndone)
    );
    // An undo of an undo, an undo of a later entry, and two undos that
    // name each other (a cycle): each is refused, so a log's undos never
    // chain and replaying one never recurses (ADR 0050).
    let later = |doc: &mut Value, kind: &str| {
        let mut entry = doc["review_events"][7].clone();
        entry["id"] = json!(LAST_ID);
        entry["kind"] = json!(kind);
        entry["reviewed_at"] = json!(entry["reviewed_at"].as_i64().unwrap() + 1);
        if kind == "review" {
            entry["grade"] = json!("pass");
            entry["undoes"] = Value::Null;
            entry["duration_ms"] = json!(1000);
        }
        doc["review_events"].as_array_mut().unwrap().push(entry);
    };
    let mut doc = sample_doc();
    later(&mut doc, "undo");
    doc["review_events"][8]["undoes"] = doc["review_events"][7]["id"].clone();
    assert_eq!(
        plan_doc(&doc),
        field("review_events", 8, "undoes", FieldProblem::InvalidUndo)
    );
    let mut doc = sample_doc();
    later(&mut doc, "review");
    doc["review_events"][7]["undoes"] = json!(LAST_ID);
    assert_eq!(plan_doc(&doc), invalid);
    let mut doc = sample_doc();
    later(&mut doc, "undo");
    doc["review_events"][8]["undoes"] = doc["review_events"][7]["id"].clone();
    doc["review_events"][7]["undoes"] = json!(LAST_ID);
    assert_eq!(plan_doc(&doc), invalid);

    // An undo whose id sorts before its target's.
    let mut doc = sample_doc();
    let target = doc["review_events"][6].clone();
    let mut early = doc["review_events"][7].clone();
    early["id"] = doc["review_events"][0]["id"].clone();
    doc["review_events"] = json!([target, early]);
    assert_eq!(
        plan_doc(&doc),
        field("review_events", 1, "undoes", FieldProblem::InvalidUndo)
    );
}

#[test]
fn a_tag_with_another_name_is_a_conflict() {
    let (source, _, _) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    let mut doc = json_of(&source.export());
    doc["tags"][0]["name"] = json!("Geometry");
    let id = doc["tags"][0]["id"].as_str().unwrap().to_owned();
    let plan = target.plan(&archive_of(&doc)).unwrap();
    assert_eq!(plan.summary().conflicts, [Conflict::TagDiffers(id)]);
    assert!(matches!(
        target.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::ImportConflicts(1))
    ));
}

/// Two databases that each created "maths" (two ids) merge cleanly: the
/// file's tag is mapped onto the database's, counted as present, and the
/// file's cards get the database's tag, with the database's spelling.
#[test]
fn two_databases_that_each_created_maths_merge_cleanly() {
    let tagged = |name: &str, tag: &str| {
        let mut env = Env::new();
        let deck = env.store.create_deck(name).unwrap().id;
        let card = env.store.add_card(&deck, name, "a").unwrap().id;
        let tag = env.store.add_tag(&card, tag).unwrap();
        (env, card, tag)
    };
    let (mut a, a_card, a_tag) = tagged("A", "maths");
    let (mut b, b_card, b_tag) = tagged("B", "Maths");
    assert_ne!(a_tag.id, b_tag.id);
    let (from_a, from_b) = (a.export(), b.export());

    let plan = a.plan(&from_b).unwrap();
    let summary = plan.summary();
    assert_eq!(summary.conflicts, []);
    assert_eq!(
        (summary.tags, summary.card_tags),
        (
            TableCounts {
                new: 0,
                present: 1,
                revived: 0
            },
            TableCounts {
                new: 1,
                present: 0,
                revived: 0
            }
        )
    );
    let report = a.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!((report.tags, report.card_tags), (0, 1));
    assert_eq!(a.store.tags().unwrap(), std::slice::from_ref(&a_tag));
    assert_eq!(
        a.store.card_tags(&b_card).unwrap(),
        std::slice::from_ref(&a_tag)
    );
    assert_eq!(
        a.store.card_tags(&a_card).unwrap(),
        std::slice::from_ref(&a_tag)
    );

    // Re-importing changes nothing: the mapped tag and its card tag are
    // already present.
    let after = a.export();
    let again = a.plan(&from_b).unwrap();
    assert!(again.summary().is_noop());
    assert_eq!(
        (again.summary().tags, again.summary().card_tags),
        (
            TableCounts {
                new: 0,
                present: 1,
                revived: 0
            },
            TableCounts {
                new: 0,
                present: 1,
                revived: 0
            }
        )
    );
    a.store.apply_import_at(&again, T_EXPORT).unwrap();
    assert_eq!(a.export(), after);

    // The other way round too, onto B's own tag; then each imports the
    // other's merged export and nothing changes.
    b.import(&from_a).unwrap();
    assert_eq!(b.store.tags().unwrap(), std::slice::from_ref(&b_tag));
    assert_eq!(
        b.store.card_tags(&a_card).unwrap(),
        std::slice::from_ref(&b_tag)
    );
    assert!(a.plan(&b.export()).unwrap().summary().is_noop());
    assert!(b.plan(&a.export()).unwrap().summary().is_noop());
}

#[test]
fn tags_merge_with_those_of_the_database() {
    let (source, _, card) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    // The target tags a card with a new tag and with the file's.
    let geometry = target.store.add_tag(&card, "Geometry").unwrap();
    let unused = target.store.add_tag(&card, "unused").unwrap();
    let mut source = source;
    // The source tags the same card with "Light".
    source.store.add_tag(&card, "light").unwrap();
    let plan = target.plan(&source.export()).unwrap();
    assert_eq!(
        (plan.summary().tags, plan.summary().card_tags),
        (
            TableCounts {
                new: 0,
                present: 3,
                revived: 0
            },
            TableCounts {
                new: 1,
                present: 3,
                revived: 0
            }
        )
    );
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    let names: Vec<String> = target
        .store
        .card_tags(&card)
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["Algebra", "Geometry", "Light", "Unused"]);
    assert!(target.store.tags().unwrap().contains(&geometry));
    assert!(target.store.tags().unwrap().contains(&unused));
}

/// Undo on one machine, the other imports it: the entry is cancelled
/// there too, and the schedules agree.
#[test]
fn an_imported_undo_cancels_the_entry_here_too() {
    let (mut a, _, card) = sample_env();
    let mut b = Env::new();
    let review = a
        .store
        .record_review(&card, Grade::Fail, Duration::from_secs(1), T0 + 40 * DAY_MS)
        .unwrap();
    b.import(&a.export()).unwrap();
    a.store.undo(&card, &review.id, T0 + 40 * DAY_MS).unwrap();
    let plan = b.plan(&a.export()).unwrap();
    assert_eq!(plan.summary().review_events.new, 1);
    b.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(b.export(), a.export());
    assert_eq!(schedule_table(&b.store), schedule_table(&a.store));
}

/// Both machines undo the same review, each with its own undo: an entry
/// is undone once, so the merge is refused.
#[test]
fn two_undos_of_one_entry_are_a_conflict() {
    let (mut a, _, card) = sample_env();
    let mut b = Env::new();
    let review = a
        .store
        .record_review(&card, Grade::Fail, Duration::from_secs(1), T0 + 40 * DAY_MS)
        .unwrap();
    b.import(&a.export()).unwrap();
    let from_a = a.store.undo(&card, &review.id, T0 + 40 * DAY_MS).unwrap();
    let from_b = b.store.undo(&card, &review.id, T0 + 41 * DAY_MS).unwrap();
    let plan = b.plan(&a.export()).unwrap();
    assert_eq!(
        plan.summary().conflicts,
        [Conflict::AlreadyUndone {
            event_id: from_a.id,
            existing_id: from_b.id,
        }]
    );
}

/// Entries of a card interleave on import as reviews always did: a reset
/// made on one machine and reviews made on the other replay in time order.
#[test]
fn a_reset_from_another_machine_replays_in_time_order() {
    let (mut a, _, card) = sample_env();
    let mut b = Env::new();
    b.import(&a.export()).unwrap();
    let late = T0 + 40 * DAY_MS;
    a.store.reset_card(&card, late).unwrap();
    b.store
        .record_review(&card, Grade::Pass, Duration::from_secs(1), late + DAY_MS)
        .unwrap();
    b.import(&a.export()).unwrap();
    a.import(&b.export()).unwrap();
    assert_eq!(a.export(), b.export());
    let events = a.store.review_events(&card).unwrap();
    let state = Scheduler::default().rebuild_state(&card, &events).unwrap();
    assert_eq!(a.store.card_state(&card).unwrap(), state);
    // The review came after the reset, so the card counts it alone.
    assert_eq!(crate::test_support::scheduled(state).reps, 1);
}

/// An undo made between plan and apply, of the entry the file undoes:
/// the plan would now be a conflict, so it is stale.
#[test]
fn a_plan_whose_entry_was_undone_meanwhile_is_stale() {
    let (mut a, _, card) = sample_env();
    let mut b = Env::new();
    let review = a
        .store
        .record_review(&card, Grade::Fail, Duration::from_secs(1), T0 + 40 * DAY_MS)
        .unwrap();
    b.import(&a.export()).unwrap();
    a.store.undo(&card, &review.id, T0 + 40 * DAY_MS).unwrap();
    let plan = b.plan(&a.export()).unwrap();
    assert_eq!(plan.summary().conflicts, []);
    b.store.undo(&card, &review.id, T0 + 41 * DAY_MS).unwrap();
    let before = b.export();
    assert!(matches!(
        b.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::ImportPlanStale)
    ));
    assert_eq!(b.export(), before);
}

/// A tag of the file's name created between plan and apply turns a new
/// tag into a mapped one: the plan is stale.
#[test]
fn a_plan_whose_tag_name_was_created_meanwhile_is_stale() {
    let (source, _, _) = sample_env();
    let mut target = Env::new();
    let deck = target.store.create_deck("Other").unwrap().id;
    let card = target.store.add_card(&deck, "q", "a").unwrap().id;
    let plan = target.plan(&source.export()).unwrap();
    assert_eq!(plan.summary().conflicts, []);
    target.store.add_tag(&card, "light").unwrap();
    assert!(matches!(
        target.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::ImportPlanStale)
    ));
    assert_eq!(target.store.decks().unwrap().len(), 1);
}
