use std::time::Duration;

use super::*;
use crate::domain::Grade;
use crate::test_support::{DAY_MS, T0, schedule_table};

const MISSING: &str = "01920000-0000-7000-8000-00000000dead";

/// A store with decks "Maths" and "Art" and three cards in Maths.
fn setup() -> (Store, String, String, Vec<String>) {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap().id;
    let art = store.create_deck_at("Art", T0).unwrap().id;
    let ids = ["a", "b", "c"]
        .iter()
        .map(|f| store.add_card_at(&maths, f, "", T0).unwrap().id)
        .collect();
    (store, maths, art, ids)
}

fn deck_of(store: &Store, id: &str) -> String {
    store.card(id).unwrap().deck_id
}

#[test]
fn a_move_reports_what_moved_and_undoes_exactly_that() {
    let (mut store, maths, art, ids) = setup();
    let in_art = store.add_card_at(&art, "d", "", T0).unwrap().id;
    let mut all = ids.clone();
    all.push(in_art.clone());
    all.push(ids[0].clone()); // a repeat is one card

    let moved = store.move_cards(&all, &art).unwrap();
    assert_eq!(
        moved,
        ids.iter()
            .map(|id| Moved {
                card_id: id.clone(),
                from: maths.clone(),
                to: art.clone()
            })
            .collect::<Vec<_>>()
    );
    for id in &ids {
        assert_eq!(deck_of(&store, id), art);
    }
    // A move is not an edit.
    assert_eq!(store.card(&ids[0]).unwrap().updated_at, T0);

    assert_eq!(store.unmove_cards(&moved).unwrap(), 3);
    for id in &ids {
        assert_eq!(deck_of(&store, id), maths);
    }
    assert_eq!(deck_of(&store, &in_art), art);
}

#[test]
fn an_undo_of_a_move_leaves_cards_whose_world_changed() {
    let (mut store, maths, art, ids) = setup();
    let moved = store.move_cards(&ids, &art).unwrap();
    store.delete_card_at(&ids[0], T0 + 1).unwrap();
    assert_eq!(store.unmove_cards(&moved).unwrap(), 2);
    assert_eq!(deck_of(&store, &ids[1]), maths);

    let moved = store.move_cards(&ids[1..], &art).unwrap();
    store.delete_deck_at(&maths, T0 + 2).unwrap();
    assert_eq!(store.unmove_cards(&moved).unwrap(), 0);
    assert_eq!(deck_of(&store, &ids[1]), art);
}

#[test]
fn a_move_to_a_missing_or_deleted_deck_changes_nothing() {
    let (mut store, maths, art, ids) = setup();
    assert!(matches!(
        store.move_cards(&ids, MISSING),
        Err(Error::DeckNotFound(_))
    ));
    store.delete_deck_at(&art, T0 + 1).unwrap();
    assert!(matches!(
        store.move_cards(&ids, &art),
        Err(Error::DeckNotFound(_))
    ));
    assert_eq!(deck_of(&store, &ids[0]), maths);
}

#[test]
fn one_card_that_is_not_live_refuses_the_whole_write() {
    let (mut store, _, art, ids) = setup();
    store.delete_card_at(&ids[2], T0 + 1).unwrap();
    let bad = ids.clone();
    assert!(matches!(
        store.set_suspended(&bad, true),
        Err(Error::CardNotFound(_))
    ));
    assert!(matches!(
        store.move_cards(&bad, &art),
        Err(Error::CardNotFound(_))
    ));
    assert!(matches!(
        store.add_tag_to_cards_at(&bad, "x", T0),
        Err(Error::CardNotFound(_))
    ));
    assert!(matches!(
        store.reset_cards(&bad, T0 + 2),
        Err(Error::CardNotFound(_))
    ));
    assert!(matches!(
        store.delete_cards_at(&bad, T0 + 2),
        Err(Error::CardNotFound(_))
    ));
    let mut with_missing = ids[..2].to_vec();
    with_missing.push(MISSING.to_owned());
    assert!(matches!(
        store.set_suspended(&with_missing, true),
        Err(Error::CardNotFound(_))
    ));
    assert!(!store.card(&ids[0]).unwrap().suspended);
    assert!(store.tags().unwrap().is_empty(), "no tag was created");
}

#[test]
fn tags_are_added_and_removed_in_bulk_with_exact_reports() {
    let (mut store, _, _, ids) = setup();
    let first = store.add_tag_at(&ids[0], "Hard", T0).unwrap();

    let (tag, changed) = store.add_tag_to_cards_at(&ids, " hard ", T0).unwrap();
    assert_eq!(tag, first, "found by name key, not created again");
    assert_eq!(changed, &ids[1..]);
    assert_eq!(store.tags().unwrap().len(), 1);
    assert_eq!(store.card(&ids[1]).unwrap().updated_at, T0);

    // Undo of the add: take it off exactly the cards that got it.
    assert_eq!(
        store.remove_tag_from_cards(&changed, &tag.id).unwrap(),
        changed
    );
    assert_eq!(
        store.card_tags(&ids[0]).unwrap(),
        std::slice::from_ref(&first)
    );
    assert_eq!(store.card_tags(&ids[1]).unwrap().len(), 0);

    // Remove from all: only the card that had it is reported, and the
    // undo gives it back.
    let removed = store.remove_tag_from_cards(&ids, &tag.id).unwrap();
    assert_eq!(removed, &ids[..1]);
    assert_eq!(store.restore_tag_on_cards(&removed, &tag.id).unwrap(), 1);
    assert_eq!(store.card_tags(&ids[0]).unwrap(), [first]);
    assert_eq!(store.tags().unwrap().len(), 1, "tags are never deleted");
}

#[test]
fn bad_tag_names_and_unknown_tags_are_refused() {
    let (mut store, _, _, ids) = setup();
    for name in ["", "  ", "a,b"] {
        assert!(
            store.add_tag_to_cards_at(&ids, name, T0).is_err(),
            "{name:?}"
        );
    }
    assert!(matches!(
        store.remove_tag_from_cards(&ids, MISSING),
        Err(Error::TagNotFound(_))
    ));
    assert_eq!(store.tags().unwrap().len(), 0);
}

#[test]
fn a_bulk_reset_makes_cards_new_and_its_undo_brings_them_back() {
    let (mut store, _, _, ids) = setup();
    for id in &ids[..2] {
        for at in [T0, T0 + DAY_MS] {
            store
                .record_review(id, Grade::Pass, Duration::ZERO, at)
                .unwrap();
        }
    }
    let before = schedule_table(&store);
    assert_eq!(before.len(), 2);

    let events = store.reset_cards(&ids, T0 + 2 * DAY_MS).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(schedule_table(&store).len(), 0);
    for id in &ids {
        assert!(matches!(
            store.card_state(id).unwrap(),
            crate::domain::CardState::New
        ));
    }

    let skipped = store.undo_entries(&events, T0 + 3 * DAY_MS).unwrap();
    assert_eq!(skipped.len(), 0);
    assert_eq!(schedule_table(&store), before);
}

#[test]
fn a_card_studied_since_its_reset_is_skipped_by_the_undo() {
    let (mut store, _, _, ids) = setup();
    let events = store.reset_cards(&ids, T0 + 1).unwrap();
    store
        .record_review(&ids[1], Grade::Pass, Duration::ZERO, T0 + 2)
        .unwrap();
    store.delete_card_at(&ids[2], T0 + 3).unwrap();
    let skipped = store.undo_entries(&events, T0 + 4).unwrap();
    assert_eq!(skipped, &ids[1..]);
    // The card studied since keeps its review.
    assert_eq!(store.review_events(&ids[1]).unwrap().len(), 2);
    // The first card's reset is cancelled.
    assert_eq!(store.review_events(&ids[0]).unwrap().len(), 2);
}

#[test]
fn a_bulk_delete_hides_the_cards_and_its_undo_restores_them() {
    let (mut store, maths, _, ids) = setup();
    store
        .record_review(&ids[0], Grade::Pass, Duration::ZERO, T0)
        .unwrap();
    store.delete_cards_at(&ids[..2], T0 + 1).unwrap();
    assert_eq!(store.list_cards(&maths).unwrap().len(), 1);
    assert_eq!(
        store.review_events(&ids[0]).unwrap().len(),
        1,
        "history kept"
    );

    assert_eq!(store.undelete_cards(&ids[..2]).unwrap(), 2);
    assert_eq!(store.list_cards(&maths).unwrap().len(), 3);
    // Coming back twice is harmless.
    assert_eq!(store.undelete_cards(&ids[..2]).unwrap(), 0);
}

#[test]
fn cards_do_not_come_back_into_a_deleted_deck() {
    let (mut store, maths, _, ids) = setup();
    store.delete_cards_at(&ids, T0 + 1).unwrap();
    store.delete_deck_at(&maths, T0 + 2).unwrap();
    assert_eq!(store.undelete_cards(&ids).unwrap(), 0);
}

#[test]
fn an_undo_of_a_move_does_not_undo_a_later_move() {
    let (mut store, maths, art, ids) = setup();
    let history = store.create_deck_at("History", T0).unwrap().id;
    let first = store.move_cards(&ids, &art).unwrap();
    store.move_cards(&ids[..1], &history).unwrap();
    // Only the two cards still in Art go back.
    assert_eq!(store.unmove_cards(&first).unwrap(), 2);
    assert_eq!(deck_of(&store, &ids[0]), history);
    assert_eq!(deck_of(&store, &ids[1]), maths);
    assert_eq!(deck_of(&store, &ids[2]), maths);
}

#[test]
fn the_undo_of_suspend_and_of_add_tag_skips_cards_deleted_since() {
    let (mut store, _, _, ids) = setup();
    store.set_suspended(&ids, true).unwrap();
    let (tag, added) = store.add_tag_to_cards_at(&ids, "x", T0).unwrap();
    store.delete_card_at(&ids[0], T0 + 1).unwrap();

    assert_eq!(store.restore_suspended(&ids, false).unwrap(), 2);
    assert!(!store.card(&ids[1]).unwrap().suspended);
    assert_eq!(store.release_tag_from_cards(&added, &tag.id).unwrap(), 2);
    assert_eq!(store.card_tags(&ids[1]).unwrap().len(), 0);
    assert!(matches!(
        store.release_tag_from_cards(&added, MISSING),
        Err(Error::TagNotFound(_))
    ));
}

#[test]
fn restoring_a_tag_skips_deleted_cards_and_refuses_an_unknown_tag() {
    let (mut store, _, _, ids) = setup();
    let tag = store.add_tag_at(&ids[0], "x", T0).unwrap();
    let removed = store.remove_tag_from_cards(&ids, &tag.id).unwrap();
    store.delete_card_at(&ids[0], T0 + 1).unwrap();
    assert_eq!(store.restore_tag_on_cards(&removed, &tag.id).unwrap(), 0);
    assert!(matches!(
        store.restore_tag_on_cards(&removed, MISSING),
        Err(Error::TagNotFound(_))
    ));
}

#[test]
fn undoing_an_entry_twice_skips_it_the_second_time() {
    let (mut store, _, _, ids) = setup();
    let events = store.reset_cards(&ids, T0 + 1).unwrap();
    assert_eq!(store.undo_entries(&events, T0 + 2).unwrap().len(), 0);
    assert_eq!(store.undo_entries(&events, T0 + 3).unwrap(), ids);
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|&s| s.to_owned()).collect()
}

#[test]
fn several_tags_are_added_at_once_and_their_undo_takes_off_exactly_those() {
    let (mut store, _, _, ids) = setup();
    let hard = store.add_tag_at(&ids[0], "Hard", T0).unwrap();
    let added = store
        .add_tags_to_cards_at(&ids, &names(&[" hard ", "Exam", "EXAM", "new"]), 100, T0)
        .unwrap();
    // "EXAM" is "Exam"; "hard" is the tag the first card has.
    let tag_names: Vec<&str> = added.tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tag_names, ["Hard", "Exam", "new"]);
    assert_eq!(added.tags[0], hard);
    assert_eq!(added.cards, ids);
    assert_eq!(added.full.len(), 0);
    // The first card lacked two of them, the others three.
    assert_eq!(added.links.len(), 2 + 3 + 3);
    assert_eq!(store.card_tags(&ids[1]).unwrap().len(), 3);
    assert_eq!(store.card(&ids[1]).unwrap().updated_at, T0);

    // Adding them again changes nothing.
    let again = store
        .add_tags_to_cards_at(&ids, &names(&["hard", "exam"]), 100, T0)
        .unwrap();
    assert_eq!(again, TagsAdded::default());

    // The undo leaves the tag the first card had before, and skips a card
    // deleted since.
    store.delete_card_at(&ids[2], T0 + 1).unwrap();
    assert_eq!(store.release_tags(&added.links).unwrap(), 2);
    assert_eq!(
        store.card_tags(&ids[0]).unwrap(),
        std::slice::from_ref(&hard)
    );
    assert_eq!(store.card_tags(&ids[1]).unwrap().len(), 0);
    assert_eq!(store.tags().unwrap().len(), 3, "tags are never deleted");
}

#[test]
fn a_card_that_would_pass_the_limit_gets_none_of_the_tags() {
    let (mut store, _, _, ids) = setup();
    // Card 0 has 2 tags, card 1 has 1, card 2 none; the limit is 3.
    store.add_tag_at(&ids[0], "a", T0).unwrap();
    store.add_tag_at(&ids[0], "b", T0).unwrap();
    store.add_tag_at(&ids[1], "a", T0).unwrap();
    let added = store
        .add_tags_to_cards_at(&ids, &names(&["a", "c", "d"]), 3, T0)
        .unwrap();
    // Card 0 lacks c and d: 4 > 3. Card 1 lacks c and d: 3. Card 2 lacks
    // all three: 3.
    assert_eq!(added.full, &ids[..1]);
    assert_eq!(added.cards, &ids[1..]);
    assert_eq!(store.card_tags(&ids[0]).unwrap().len(), 2);
    assert_eq!(store.card_tags(&ids[1]).unwrap().len(), 3);
    assert_eq!(store.card_tags(&ids[2]).unwrap().len(), 3);

    // A card already past the limit (an import) gets no more.
    let added = store
        .add_tags_to_cards_at(&ids[..1], &names(&["e"]), 1, T0)
        .unwrap();
    assert_eq!(added.full, &ids[..1]);
    assert_eq!(added.links.len(), 0);
}

#[test]
fn when_no_card_gets_a_tag_no_tag_is_created() {
    let (mut store, _, _, ids) = setup();
    store.add_tag_at(&ids[0], "a", T0).unwrap();
    let added = store
        .add_tags_to_cards_at(&ids[..1], &names(&["brand new"]), 1, T0)
        .unwrap();
    assert_eq!(added.full, &ids[..1]);
    assert_eq!(added.tags.len(), 0);
    assert_eq!(store.tags().unwrap().len(), 1);
}

#[test]
fn a_bad_name_or_card_among_several_writes_nothing() {
    let (mut store, _, _, ids) = setup();
    for bad in [
        names(&["ok", ""]),
        names(&["ok", "a,b"]),
        names(&["ok", "a\0b"]),
    ] {
        assert!(
            store.add_tags_to_cards_at(&ids, &bad, 100, T0).is_err(),
            "{bad:?}"
        );
    }
    let mut with_missing = ids.clone();
    with_missing.push(MISSING.to_owned());
    assert!(matches!(
        store.add_tags_to_cards_at(&with_missing, &names(&["ok"]), 100, T0),
        Err(Error::CardNotFound(_))
    ));
    assert_eq!(store.tags().unwrap().len(), 0);
    assert_eq!(store.card_tags(&ids[0]).unwrap().len(), 0);
}
