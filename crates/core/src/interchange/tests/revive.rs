//! Revive.

use super::*;

fn live_card_ids(store: &Store, deck: &str) -> Vec<String> {
    store
        .list_cards(deck)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect()
}

#[test]
fn only_a_difference_in_deleted_at_can_be_merged() {
    use super::import::{Outcome, compare};
    assert_eq!(compare(true, None, None), Outcome::Present);
    assert_eq!(compare(true, Some(5), Some(5)), Outcome::Present);
    assert_eq!(compare(true, None, Some(5)), Outcome::Present);
    assert_eq!(compare(true, Some(5), None), Outcome::Revive(5));
    assert_eq!(compare(true, Some(5), Some(6)), Outcome::Present);
    for (stored, file) in [
        (None, None),
        (Some(5), None),
        (None, Some(5)),
        (Some(5), Some(5)),
    ] {
        assert_eq!(compare(false, stored, file), Outcome::Conflict);
    }
}

#[test]
fn export_delete_all_import_revives_everything() {
    let (mut env, deck, _) = sample_env();
    let bytes = env.export();
    let schedules = schedule_table(&env.store);
    let cards = live_card_ids(&env.store, &deck);
    assert_eq!(cards.len(), 2);
    // One card on its own first, so it has its own stamp, then every
    // live deck ("Physics" is already deleted in the file).
    env.store.delete_card(&cards[1]).unwrap();
    env.store.delete_deck(&deck).unwrap();
    assert_eq!(env.store.list_decks(T_EXPORT, T_EXPORT).unwrap(), []);

    let plan = env.plan(&bytes).unwrap();
    assert_eq!(
        *plan.summary(),
        ImportSummary {
            decks: TableCounts {
                new: 0,
                present: 1,
                revived: 1
            },
            cards: TableCounts {
                new: 0,
                present: 2,
                revived: 2
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
    let report = env.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(
        (report.decks, report.cards, report.review_events),
        (0, 0, 0)
    );
    assert_eq!((report.revived_decks, report.revived_cards), (1, 2));
    assert!(report.backup.as_ref().unwrap().exists());

    // Every field, deleted rows and the review log as before the deletes;
    // schedules and card order (seq) kept.
    assert_eq!(env.export(), bytes);
    assert_eq!(schedule_table(&env.store), schedules);
    assert_eq!(live_card_ids(&env.store, &deck), cards);

    // The same file again changes nothing and makes no backup.
    let backups = env.backups();
    let plan = env.plan(&bytes).unwrap();
    assert!(plan.summary().is_noop(), "{:?}", plan.summary());
    let report = env.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(report.backup, None);
    assert_eq!(env.export(), bytes);
    assert_eq!(env.backups(), backups);
}

#[test]
fn a_mixed_import_revives_adds_and_skips() {
    let (mut source, deck, _) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    let old_cards = live_card_ids(&target.store, &deck);

    // The source gets a new card and a new deck after the first export.
    let added = source.store.add_card(&deck, "e ?", "2.718").unwrap().id;
    source.store.create_deck("Chemistry").unwrap();
    // The target gets a card the file never had, then deletes the deck:
    // that card is in the deck's delete batch and comes back with it.
    let only_here = target.store.add_card(&deck, "φ ?", "1.618").unwrap().id;
    target
        .store
        .record_review(&only_here, Grade::Pass, Duration::from_secs(1), T0)
        .unwrap();
    let schedules = schedule_table(&target.store);
    target.store.delete_deck(&deck).unwrap();

    let plan = target.plan(&source.export()).unwrap();
    assert_eq!(
        *plan.summary(),
        ImportSummary {
            decks: TableCounts {
                new: 1,
                present: 1,
                revived: 1
            },
            cards: TableCounts {
                new: 1,
                present: 2,
                revived: 3
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
    let report = target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(
        (report.decks, report.cards, report.review_events),
        (1, 1, 0)
    );
    assert_eq!((report.revived_decks, report.revived_cards), (1, 3));

    let mut expected = old_cards;
    expected.extend([only_here, added]);
    assert_eq!(live_card_ids(&target.store, &deck), expected);
    let names: Vec<String> = target
        .store
        .list_decks(T_EXPORT, T_EXPORT)
        .unwrap()
        .into_iter()
        .map(|d| d.deck.name)
        .collect();
    assert_eq!(names, ["Chemistry", "Mathématiques"]);
    assert_eq!(schedule_table(&target.store), schedules);
}

#[test]
fn a_card_deleted_in_the_file_stays_deleted_when_its_deck_comes_back() {
    let (source, deck, card) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    let cards = live_card_ids(&target.store, &deck);
    target.store.delete_deck(&deck).unwrap();
    let stamp = json_of(&target.export())["decks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == deck.as_str())
        .unwrap()["deleted_at"]
        .clone();
    assert!(stamp.is_i64());

    // A file where the deck is live but one of its cards was deleted at
    // exactly the deck's delete stamp here: the card's own row decides.
    let mut doc = json_of(&source.export());
    for c in doc["cards"].as_array_mut().unwrap() {
        if c["id"] == cards[1].as_str() {
            c["deleted_at"] = stamp.clone();
        }
    }
    let plan = target.plan(&archive_of(&doc)).unwrap();
    assert_eq!(
        plan.summary().cards,
        TableCounts {
            new: 0,
            present: 3,
            revived: 1
        }
    );
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(live_card_ids(&target.store, &deck), [card]);
}

#[test]
fn a_row_deleted_in_the_file_but_live_here_is_present() {
    let (mut source, deck, _) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    let before = target.export();
    let cards = live_card_ids(&target.store, &deck);
    source.store.delete_card(&cards[0]).unwrap();
    source.store.delete_deck(&deck).unwrap();
    let bytes = source.export();

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
    // An import never deletes.
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(target.export(), before);

    // With something new in the file, the import is applied, and still
    // deletes nothing.
    source.store.create_deck("Chemistry").unwrap();
    let report = target.import(&source.export()).unwrap();
    assert_eq!((report.decks, report.revived_decks), (1, 0));
    assert_eq!(live_card_ids(&target.store, &deck), cards);
}

#[test]
fn a_plan_whose_deck_was_brought_back_meanwhile_is_stale() {
    let (source, deck, _) = sample_env();
    let bytes = source.export();
    let mut target = Env::new();
    target.import(&bytes).unwrap();
    target.store.delete_deck(&deck).unwrap();
    let plan = target.plan(&bytes).unwrap();
    assert_eq!(plan.summary().decks.revived, 1);
    target.store.undelete_deck(&deck).unwrap();
    let before = target.export();
    assert!(matches!(
        target.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::ImportPlanStale)
    ));
    assert_eq!(target.export(), before);
}

#[test]
fn only_the_cards_deleted_with_the_deck_come_back() {
    let (source, deck, _) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    let cards = live_card_ids(&target.store, &deck);
    // Added and deleted on its own here, before the deck: its own stamp.
    let alone = target.store.add_card(&deck, "τ ?", "6.283").unwrap().id;
    target.store.delete_card(&alone).unwrap();
    target.store.delete_deck(&deck).unwrap();

    let plan = target.plan(&source.export()).unwrap();
    assert_eq!(plan.summary().cards.revived, 2);
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(live_card_ids(&target.store, &deck), cards);
}

#[test]
fn a_plan_whose_deck_was_deleted_meanwhile_is_stale() {
    let (source, mut target, deck, _) = diverged();
    let plan = target.plan(&source.export()).unwrap();
    assert_eq!(plan.summary().decks.revived, 0);
    target.store.delete_deck(&deck).unwrap();
    let before = target.export();
    assert!(matches!(
        target.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::ImportPlanStale)
    ));
    assert_eq!(target.export(), before);
}

#[test]
fn a_plan_is_stale_even_if_the_changes_cancel_out_in_the_counts() {
    let mut source = Env::new();
    let a = source.store.create_deck("A").unwrap().id;
    let b = source.store.create_deck("B").unwrap().id;
    source.store.add_card(&a, "a", "").unwrap();
    source.store.add_card(&b, "b", "").unwrap();
    let bytes = source.export();
    let mut target = Env::new();
    target.import(&bytes).unwrap();
    target.store.delete_deck(&a).unwrap();
    let plan = target.plan(&bytes).unwrap();

    // A comes back here and B goes: the same counts, other rows.
    target.store.undelete_deck(&a).unwrap();
    target.store.delete_deck(&b).unwrap();
    assert_eq!(target.plan(&bytes).unwrap().summary(), plan.summary());
    let before = target.export();
    assert!(matches!(
        target.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::ImportPlanStale)
    ));
    assert_eq!(target.export(), before);
}

#[test]
fn a_row_deleted_on_both_sides_at_different_times_is_already_present() {
    let (source, deck, _) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    let card = live_card_ids(&target.store, &deck)[1].clone();
    target.store.delete_card(&card).unwrap();
    target.store.delete_deck(&deck).unwrap();
    let mut doc = json_of(&source.export());
    for c in doc["cards"].as_array_mut().unwrap() {
        // The file's deck is deleted, so all its cards are, at stamps
        // that differ from the database's.
        if c["deck_id"] == deck.as_str() {
            c["deleted_at"] = json!(if c["id"] == card.as_str() { T0 } else { T0 + 2 });
        }
    }
    for d in doc["decks"].as_array_mut().unwrap() {
        if d["id"] == deck.as_str() {
            d["deleted_at"] = json!(T0 + 1);
        }
    }
    let bytes = archive_of(&doc);
    let before = target.export();
    let backups = target.backups();

    let plan = target.plan(&bytes).unwrap();
    let summary = plan.summary();
    assert!(summary.conflicts.is_empty(), "{:?}", summary.conflicts);
    assert_eq!((summary.decks.new, summary.decks.revived), (0, 0));
    assert_eq!((summary.cards.new, summary.cards.revived), (0, 0));
    assert_eq!(summary.decks.present, 2);
    assert_eq!(
        summary.cards.present,
        doc["cards"].as_array().unwrap().len()
    );
    // Nothing to write: no change and no backup, and the database keeps
    // its own delete stamps.
    target.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(target.export(), before);
    assert_eq!(target.backups(), backups);
}
