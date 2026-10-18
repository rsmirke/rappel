//! Round trip.

use super::*;

#[test]
fn export_import_export_gives_identical_bytes_and_schedules() {
    let (source, _, _) = sample_env();
    let bytes = source.export();

    let mut target = Env::new();
    let plan = target.plan(&bytes).unwrap();
    assert_eq!(
        *plan.summary(),
        ImportSummary {
            decks: TableCounts {
                new: 2,
                present: 0,
                revived: 0
            },
            cards: TableCounts {
                new: 4,
                present: 0,
                revived: 0
            },
            tags: TableCounts {
                new: 3,
                present: 0,
                revived: 0
            },
            card_tags: TableCounts {
                new: 3,
                present: 0,
                revived: 0
            },
            review_events: TableCounts {
                new: 8,
                present: 0,
                revived: 0
            },
            media: TableCounts::default(),
            settings: SettingsImport::Unchanged,
            conflicts: vec![],
        }
    );
    let report = target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(
        (report.decks, report.cards, report.review_events),
        (2, 4, 8)
    );
    assert!(report.backup.as_ref().unwrap().exists());

    assert_eq!(target.export(), bytes);
    assert_eq!(schedule_table(&target.store), schedule_table(&source.store));
}

#[test]
fn the_longest_names_and_sides_the_app_accepts_can_be_imported() {
    use crate::domain::{MAX_CARD_SIDE_BYTES, MAX_DECK_NAME_BYTES};
    let mut source = Env::new();
    // Two-byte letters, so the limits are met in bytes, not characters.
    let name = "é".repeat(MAX_DECK_NAME_BYTES / 2);
    let side = "ü".repeat(MAX_CARD_SIDE_BYTES / 2);
    let deck = source.store.create_deck(&name).unwrap();
    source.store.add_card(&deck.id, &side, &side).unwrap();
    let bytes = export(&source.store);

    let mut target = Env::new();
    let report = target.import(&bytes).unwrap();
    assert_eq!((report.decks, report.cards), (1, 1));
    assert_eq!(export(&target.store), bytes);
}

#[test]
fn importing_the_same_file_twice_is_a_noop() {
    let (source, _, _) = sample_env();
    let bytes = source.export();
    let mut target = Env::new();
    target.import(&bytes).unwrap();
    let after_first = target.export();
    let backups = target.backups();

    let plan = target.plan(&bytes).unwrap();
    assert_eq!(
        *plan.summary(),
        ImportSummary {
            decks: TableCounts {
                new: 0,
                present: 2,
                revived: 0
            },
            cards: TableCounts {
                new: 0,
                present: 4,
                revived: 0
            },
            tags: TableCounts {
                new: 0,
                present: 3,
                revived: 0
            },
            card_tags: TableCounts {
                new: 0,
                present: 3,
                revived: 0
            },
            review_events: TableCounts {
                new: 0,
                present: 8,
                revived: 0
            },
            media: TableCounts::default(),
            settings: SettingsImport::Unchanged,
            conflicts: vec![],
        }
    );
    assert!(plan.summary().is_noop());
    let report = target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(report.backup, None);
    assert_eq!(target.export(), after_first);
    assert_eq!(target.backups(), backups);

    // Importing a database's own export into itself is a no-op too.
    let plan = source.plan(&bytes).unwrap();
    assert!(plan.summary().is_noop());
}

#[test]
fn a_later_export_adds_only_what_is_new_and_rebuilds_those_schedules() {
    let (mut source, deck, card) = sample_env();
    let older = source.export();
    let mut target = Env::new();
    target.import(&older).unwrap();

    // More history on an existing card, a new card with a review, and
    // a new deck.
    source
        .store
        .record_review(&card, Grade::Pass, Duration::from_secs(2), T0 + 30 * DAY_MS)
        .unwrap();
    let new_card = source.store.add_card(&deck, "e ?", "2.718").unwrap().id;
    source
        .store
        .record_review(
            &new_card,
            Grade::Fail,
            Duration::from_secs(3),
            T0 + 31 * DAY_MS,
        )
        .unwrap();
    source.store.create_deck("Chemistry").unwrap();
    let newer = source.export();

    let plan = target.plan(&newer).unwrap();
    assert_eq!(
        plan.summary().decks,
        TableCounts {
            new: 1,
            present: 2,
            revived: 0
        }
    );
    assert_eq!(
        plan.summary().cards,
        TableCounts {
            new: 1,
            present: 4,
            revived: 0
        }
    );
    assert_eq!(
        plan.summary().review_events,
        TableCounts {
            new: 2,
            present: 8,
            revived: 0
        }
    );
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(target.export(), newer);
    assert_eq!(schedule_table(&target.store), schedule_table(&source.store));
}

#[test]
fn diverged_copies_merge_with_reviews_interleaved_by_time() {
    // Two copies of one history, each reviewing the same card later at
    // different times; then each imports the other's export.
    let (mut a, _, card) = sample_env();
    let mut b = Env::new();
    b.import(&a.export()).unwrap();
    let late = T0 + 40 * DAY_MS;
    let review = |env: &mut Env, t: i64| {
        env.store
            .record_review(&card, Grade::Pass, Duration::from_secs(1), t)
            .unwrap();
    };
    review(&mut a, late);
    review(&mut b, late + DAY_MS);
    review(&mut a, late + 2 * DAY_MS);
    let (from_a, from_b) = (a.export(), b.export());
    let plan = b.plan(&from_a).unwrap();
    assert_eq!(
        plan.summary().review_events,
        TableCounts {
            new: 2,
            present: 8,
            revived: 0
        }
    );
    b.store.apply_import_at(&plan, T_EXPORT).unwrap();
    a.import(&from_b).unwrap();
    // Both now hold the same log and the same cache, equal to a rebuild.
    assert_eq!(a.export(), b.export());
    assert_eq!(schedule_table(&a.store), schedule_table(&b.store));
    let events = a.store.review_events(&card).unwrap();
    assert_eq!(events.len(), 5);
    let rebuilt = Scheduler::default().rebuild_state(&card, &events).unwrap();
    assert_eq!(a.store.card_state(&card).unwrap(), rebuilt);
}

#[test]
fn files_from_two_databases_merge() {
    let (a, _, _) = sample_env();
    let mut b = Env::new();
    let deck = b.store.create_deck("Languages").unwrap().id;
    let card = b.store.add_card(&deck, "chat", "cat").unwrap().id;
    b.store
        .record_review(&card, Grade::Pass, Duration::from_secs(1), T0)
        .unwrap();

    let mut target = Env::new();
    target.import(&a.export()).unwrap();
    target.import(&b.export()).unwrap();
    let doc = json_of(&target.export());
    assert_eq!(doc["decks"].as_array().unwrap().len(), 3);
    assert_eq!(doc["cards"].as_array().unwrap().len(), 5);
    assert_eq!(doc["review_events"].as_array().unwrap().len(), 9);
    let mut expected = schedule_table(&a.store);
    expected.extend(schedule_table(&b.store));
    expected.sort();
    assert_eq!(schedule_table(&target.store), expected);
}

#[test]
fn an_in_memory_store_refuses_to_import_without_a_backup() {
    let (source, _, _) = sample_env();
    let mut store = Store::in_memory();
    let plan = store
        .plan_import_bytes(&source.export(), &Limits::DEFAULT)
        .unwrap();
    assert!(matches!(
        store.apply_import_at(&plan, T_EXPORT),
        Err(Error::NoBackupDir)
    ));
    assert_eq!(json_of(&export(&store))["decks"], json!([]));
}
