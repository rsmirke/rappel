use std::time::Duration;

use super::*;
use crate::domain::Grade;
use crate::test_support::{DAY_MS, T0};

fn deleted_at(store: &Store, table: &str, id: &str) -> Option<i64> {
    store
        .conn
        .query_row(
            &format!("SELECT deleted_at FROM {table} WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

fn counts(store: &Store, now: i64) -> Vec<(String, u32, u32)> {
    store
        .list_decks(now, now)
        .unwrap()
        .into_iter()
        .map(|d| (d.deck.id, d.new_count, d.due_count))
        .collect()
}

fn names(store: &Store) -> Vec<String> {
    store
        .list_decks(T0, T0)
        .unwrap()
        .into_iter()
        .map(|d| d.deck.name)
        .collect()
}

#[test]
fn create_trims_the_name_and_lists_the_deck() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("  Maths \n", T0).unwrap();
    assert_eq!(deck.name, "Maths");
    assert_eq!((deck.created_at, deck.updated_at), (T0, T0));
    assert_eq!(deck.id.len(), 36);
    assert_eq!(
        store.list_decks(T0, T0).unwrap(),
        [DeckSummary {
            deck,
            new_count: 0,
            due_count: 0,
            card_count: 0,
            suspended_count: 0,
            held_back_count: 0,
            next_change_at: None,
        }]
    );
}

#[test]
fn empty_names_are_refused() {
    let mut store = Store::in_memory();
    for name in ["", "   ", "\t\n"] {
        assert!(matches!(
            store.create_deck_at(name, T0),
            Err(Error::EmptyDeckName)
        ));
    }
    let deck = store.create_deck_at("Maths", T0).unwrap();
    assert!(matches!(
        store.rename_deck_at(&deck.id, " ", T0),
        Err(Error::EmptyDeckName)
    ));
    assert_eq!(names(&store), ["Maths"]);
}

#[test]
fn names_longer_than_the_import_limit_are_refused() {
    use crate::domain::MAX_DECK_NAME_BYTES;
    let mut store = Store::in_memory();
    let longest = "a".repeat(MAX_DECK_NAME_BYTES);
    // Spaces around it are trimmed before measuring, as before storing.
    let deck = store.create_deck_at(&format!(" {longest} "), T0).unwrap();
    assert_eq!(deck.name, longest);

    // Bytes, not characters: 512 two-byte letters fit, one more does not.
    let accented = "é".repeat(MAX_DECK_NAME_BYTES / 2);
    store.create_deck_at(&accented, T0).unwrap();
    let too_long = format!("{accented}é");
    assert!(matches!(
        store.create_deck_at(&too_long, T0),
        Err(Error::DeckNameTooLong { limit }) if limit == MAX_DECK_NAME_BYTES
    ));
    assert!(matches!(
        store.rename_deck_at(&deck.id, &too_long, T0 + 5),
        Err(Error::DeckNameTooLong { .. })
    ));
    assert_eq!(names(&store).len(), 2);
    assert!(names(&store).contains(&longest));
}

#[test]
fn a_nul_in_a_name_is_refused() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap();
    assert!(matches!(
        store.create_deck_at("a\0b", T0),
        Err(Error::ContainsNul)
    ));
    assert!(matches!(
        store.rename_deck_at(&deck.id, "a\0b", T0 + 5),
        Err(Error::ContainsNul)
    ));
    assert_eq!(names(&store), ["Maths"]);
}

#[test]
fn rename_changes_name_and_updated_at() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap();
    store.rename_deck_at(&deck.id, " Algebra ", T0 + 5).unwrap();
    let listed = store.list_decks(T0, T0).unwrap().remove(0).deck;
    assert_eq!(listed.name, "Algebra");
    assert_eq!((listed.created_at, listed.updated_at), (T0, T0 + 5));
}

#[test]
fn rename_to_the_same_name_changes_nothing() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap();
    store.rename_deck_at(&deck.id, "Maths ", T0 + 5).unwrap();
    assert_eq!(store.list_decks(T0, T0).unwrap()[0].deck, deck);
}

#[test]
fn missing_or_deleted_decks_cannot_be_renamed_or_deleted() {
    let mut store = Store::in_memory();
    let missing = "01920000-0000-7000-8000-00000000dead";
    assert!(
        matches!(store.rename_deck_at(missing, "X", T0), Err(Error::DeckNotFound(id)) if id == missing)
    );
    assert!(matches!(
        store.delete_deck_at(missing, T0),
        Err(Error::DeckNotFound(_))
    ));

    let deck = store.create_deck_at("Maths", T0).unwrap();
    store.delete_deck_at(&deck.id, T0).unwrap();
    assert!(matches!(
        store.rename_deck_at(&deck.id, "X", T0),
        Err(Error::DeckNotFound(_))
    ));
    // Renaming to its own name does not reveal a deleted deck either.
    assert!(matches!(
        store.rename_deck_at(&deck.id, "Maths", T0),
        Err(Error::DeckNotFound(_))
    ));
    assert!(matches!(
        store.delete_deck_at(&deck.id, T0),
        Err(Error::DeckNotFound(_))
    ));
}

#[test]
fn duplicate_names_are_refused_on_create_and_rename() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    assert!(matches!(
        store.create_deck_at(" Maths ", T0),
        Err(Error::DuplicateDeckName(n)) if n == "Maths"
    ));
    let physics = store.create_deck_at("Physics", T0).unwrap();
    assert!(matches!(
        store.rename_deck_at(&physics.id, "Maths", T0),
        Err(Error::DuplicateDeckName(_))
    ));
    assert_eq!(names(&store), ["Maths", "Physics"]);
    assert_eq!(store.list_decks(T0, T0).unwrap()[0].deck, maths);
}

const COMPOSED: &str = "\u{C9}conomie"; // "Économie", É as one code point
const DECOMPOSED: &str = "E\u{301}conomie"; // E + combining acute accent

#[test]
fn names_differing_only_in_case_collide() {
    let mut store = Store::in_memory();
    store.create_deck_at("Maths", T0).unwrap();
    for name in ["maths", "MATHS", " mAtHs "] {
        assert!(
            matches!(
                store.create_deck_at(name, T0),
                Err(Error::DuplicateDeckName(_))
            ),
            "{name}"
        );
    }
    let physics = store.create_deck_at("Physics", T0).unwrap();
    assert!(matches!(
        store.rename_deck_at(&physics.id, "maths", T0),
        Err(Error::DuplicateDeckName(_))
    ));
    assert_eq!(names(&store), ["Maths", "Physics"]);
}

#[test]
fn composed_and_decomposed_accents_collide_in_any_case() {
    let mut store = Store::in_memory();
    store.create_deck_at(COMPOSED, T0).unwrap();
    for name in [
        DECOMPOSED,
        "\u{E9}conomie",
        "e\u{301}conomie",
        "\u{C9}CONOMIE",
    ] {
        assert!(
            matches!(
                store.create_deck_at(name, T0),
                Err(Error::DuplicateDeckName(_))
            ),
            "{name:?}"
        );
    }
    let other = store.create_deck_at("Other", T0).unwrap();
    assert!(matches!(
        store.rename_deck_at(&other.id, "e\u{301}conomie", T0),
        Err(Error::DuplicateDeckName(_))
    ));
    // Accents still count: "Economie" is another name.
    store.create_deck_at("Economie", T0).unwrap();
    assert_eq!(names(&store), ["Economie", "Other", COMPOSED]);
}

#[test]
fn a_deck_can_be_renamed_to_another_case_of_its_own_name() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at(DECOMPOSED, T0).unwrap();
    store
        .rename_deck_at(&deck.id, "\u{E9}CONOMIE", T0 + 1)
        .unwrap();
    let listed = store.list_decks(T0, T0).unwrap().remove(0).deck;
    assert_eq!(
        (listed.name.as_str(), listed.updated_at),
        ("\u{E9}CONOMIE", T0 + 1)
    );
}

#[test]
fn a_deleted_decks_name_is_reusable_in_any_case_or_form() {
    let mut store = Store::in_memory();
    let old = store.create_deck_at(COMPOSED, T0).unwrap();
    store.delete_deck_at(&old.id, T0).unwrap();
    let new = store.create_deck_at("e\u{301}conomie", T0).unwrap();
    store.delete_deck_at(&new.id, T0).unwrap();
    let other = store.create_deck_at("Other", T0).unwrap();
    store
        .rename_deck_at(&other.id, "\u{C9}CONOMIE", T0)
        .unwrap();
    assert_eq!(names(&store), ["\u{C9}CONOMIE"]);
}

#[test]
fn a_deck_without_a_name_key_is_refused() {
    let store = Store::in_memory();
    let err = store
        .conn
        .execute(
            "INSERT INTO decks (id, name, created_at, updated_at)
             VALUES ('01920000-0000-7000-8000-000000000001', 'Maths', 1, 1)",
            [],
        )
        .unwrap_err();
    assert!(err.to_string().contains("name_key is required"), "{err}");
}

#[test]
fn a_deleted_decks_name_can_be_reused() {
    let mut store = Store::in_memory();
    let old = store.create_deck_at("Maths", T0).unwrap();
    store.delete_deck_at(&old.id, T0).unwrap();
    let new = store.create_deck_at("Maths", T0).unwrap();
    assert_ne!(old.id, new.id);

    // And by rename, after the new one is deleted too.
    store.delete_deck_at(&new.id, T0).unwrap();
    let other = store.create_deck_at("Physics", T0).unwrap();
    store.rename_deck_at(&other.id, "Maths", T0).unwrap();
    assert_eq!(names(&store), ["Maths"]);
}

#[test]
fn list_is_sorted_by_name_ignoring_case() {
    let mut store = Store::in_memory();
    for name in ["physics", "Algebra", "Écologie", "biology"] {
        store.create_deck_at(name, T0).unwrap();
    }
    // Lower-casing, not locale collation: "é" sorts after ASCII letters.
    assert_eq!(names(&store), ["Algebra", "biology", "physics", "Écologie"]);
}

#[test]
fn decks_and_deck_read_live_decks_without_counts() {
    let mut store = Store::in_memory();
    for name in ["physics", "Algebra", "Écologie", "biology"] {
        store.create_deck_at(name, T0).unwrap();
    }
    let gone = store.create_deck_at("Gone", T0).unwrap();
    store.delete_deck_at(&gone.id, T0).unwrap();

    let listed: Vec<Deck> = store
        .list_decks(T0, T0)
        .unwrap()
        .into_iter()
        .map(|s| s.deck)
        .collect();
    let decks = store.decks().unwrap();
    assert_eq!(decks, listed);
    assert_eq!(decks.len(), 4);
    for deck in &decks {
        assert_eq!(&store.deck(&deck.id).unwrap(), deck);
    }

    let missing = "01920000-0000-7000-8000-00000000dead";
    assert!(matches!(store.deck(missing), Err(Error::DeckNotFound(id)) if id == missing));
    assert!(matches!(store.deck(&gone.id), Err(Error::DeckNotFound(_))));
    store.undelete_deck(&gone.id).unwrap();
    assert_eq!(store.deck(&gone.id).unwrap().name, "Gone");
}

#[test]
fn counts_new_and_due_live_cards() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let empty = store.create_deck_at("Empty", T0).unwrap();
    let cards: Vec<_> = (0..5)
        .map(|i| {
            store
                .add_card_at(&maths.id, &format!("q{i}"), "a", T0)
                .unwrap()
                .id
        })
        .collect();
    // cards[0], cards[1] reviewed: in the 10-minute step. cards[2] deleted.
    for c in &cards[..2] {
        store
            .record_review(c, Grade::Fail, Duration::ZERO, T0)
            .unwrap();
    }
    store.delete_card_at(&cards[2], T0).unwrap();

    // Step cards due within 20 minutes count as due, as on the deck page
    // (ADR 0014).
    assert_eq!(
        counts(&store, T0),
        [(empty.id.clone(), 0, 0), (maths.id.clone(), 2, 2)]
    );
    assert_eq!(
        counts(&store, T0 + DAY_MS),
        [(empty.id.clone(), 0, 0), (maths.id.clone(), 2, 2)]
    );

    // A deleted reviewed card is not due.
    store.delete_card_at(&cards[0], T0).unwrap();
    assert_eq!(
        counts(&store, T0 + DAY_MS),
        [(empty.id, 0, 0), (maths.id, 2, 1)]
    );
}

#[test]
fn deleting_a_deck_hides_it_and_its_cards_and_keeps_their_history() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let other = store.create_deck_at("Other", T0).unwrap();
    let card = store.add_card_at(&maths.id, "q", "a", T0).unwrap();
    let kept = store.add_card_at(&other.id, "q", "a", T0).unwrap();
    store
        .record_review(&card.id, Grade::Pass, Duration::from_secs(3), T0)
        .unwrap();

    store.delete_deck_at(&maths.id, T0 + 1).unwrap();

    assert_eq!(names(&store), ["Other"]);
    assert!(matches!(
        store.list_cards(&maths.id),
        Err(Error::DeckNotFound(_))
    ));
    assert!(matches!(
        store.study_input(&maths.id, T0, T0),
        Err(Error::DeckNotFound(_))
    ));
    assert!(matches!(store.card(&card.id), Err(Error::CardNotFound(_))));
    assert!(matches!(
        store.add_card_at(&maths.id, "q", "a", T0),
        Err(Error::DeckNotFound(_))
    ));
    assert!(matches!(
        store.record_review(&card.id, Grade::Pass, Duration::ZERO, T0 + 2),
        Err(Error::CardNotFound(_))
    ));
    // History kept; the other deck untouched.
    assert_eq!(store.review_events(&card.id).unwrap().len(), 1);
    assert_eq!(store.card(&kept.id).unwrap(), kept);
}

#[test]
fn deck_delete_stamps_the_deck_and_its_live_cards_with_one_value() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let other = store.create_deck_at("Other", T0).unwrap();
    let a = store.add_card_at(&maths.id, "a", "", T0).unwrap();
    let b = store.add_card_at(&maths.id, "b", "", T0).unwrap();
    let earlier = store.add_card_at(&maths.id, "c", "", T0).unwrap();
    let elsewhere = store.add_card_at(&other.id, "d", "", T0).unwrap();
    store.delete_card_at(&earlier.id, T0 + 10).unwrap();

    store.delete_deck_at(&maths.id, T0 + 20).unwrap();

    assert_eq!(deleted_at(&store, "decks", &maths.id), Some(T0 + 20));
    assert_eq!(deleted_at(&store, "cards", &a.id), Some(T0 + 20));
    assert_eq!(deleted_at(&store, "cards", &b.id), Some(T0 + 20));
    // Deleted before: keeps its own stamp, so it is not part of the batch.
    assert_eq!(deleted_at(&store, "cards", &earlier.id), Some(T0 + 10));
    assert_eq!(deleted_at(&store, "cards", &elsewhere.id), None);
    assert_eq!(deleted_at(&store, "decks", &other.id), None);
}

#[test]
fn deck_delete_stamp_is_after_earlier_card_deletes_in_the_same_millisecond() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let card = store.add_card_at(&maths.id, "a", "", T0).unwrap();
    let earlier = store.add_card_at(&maths.id, "b", "", T0).unwrap();
    // Same millisecond, and a clock that went backwards: the batch stamp
    // must still differ from the earlier single delete.
    store.delete_card_at(&earlier.id, T0 + 10).unwrap();
    store.delete_deck_at(&maths.id, T0 + 5).unwrap();

    let stamp = deleted_at(&store, "decks", &maths.id).unwrap();
    assert_eq!(stamp, T0 + 11);
    assert_eq!(deleted_at(&store, "cards", &card.id), Some(stamp));
    assert_eq!(deleted_at(&store, "cards", &earlier.id), Some(T0 + 10));
}

// --- Undelete -------------------------------------------------------------

#[test]
fn undelete_restores_the_deck_and_exactly_its_batch() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let a = store.add_card_at(&maths.id, "a", "", T0).unwrap();
    let b = store.add_card_at(&maths.id, "b", "", T0).unwrap();
    let earlier = store.add_card_at(&maths.id, "c", "", T0).unwrap();
    store
        .record_review(&a.id, Grade::Pass, Duration::from_secs(2), T0)
        .unwrap();
    let state_before = store.card_state(&a.id).unwrap();
    // Deleted on its own before the deck, with a clock that then went
    // backwards: the deck's stamp is still later, so not part of its batch.
    store.delete_card_at(&earlier.id, T0 + 10).unwrap();
    store.delete_deck_at(&maths.id, T0 + 5).unwrap();

    let restored = store.undelete_deck(&maths.id).unwrap();

    assert_eq!(restored, maths, "same deck, updated_at unchanged");
    assert_eq!(names(&store), ["Maths"]);
    assert_eq!(deleted_at(&store, "decks", &maths.id), None);
    assert_eq!(deleted_at(&store, "cards", &a.id), None);
    assert_eq!(deleted_at(&store, "cards", &b.id), None);
    assert_eq!(deleted_at(&store, "cards", &earlier.id), Some(T0 + 10));
    let live: Vec<String> = store
        .list_cards(&maths.id)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(live, [a.id.clone(), b.id.clone()]);
    // History, schedule and counts are back as they were.
    assert_eq!(store.review_events(&a.id).unwrap().len(), 1);
    assert_eq!(store.card_state(&a.id).unwrap(), state_before);
    assert_eq!(counts(&store, T0 + DAY_MS * 365), [(maths.id, 1, 1)]);
}

#[test]
fn undelete_after_a_second_delete_restores_the_second_batch() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let a = store.add_card_at(&maths.id, "a", "", T0).unwrap();
    store.delete_deck_at(&maths.id, T0 + 1).unwrap();
    store.undelete_deck(&maths.id).unwrap();
    let b = store.add_card_at(&maths.id, "b", "", T0 + 2).unwrap();
    store.delete_card_at(&a.id, T0 + 3).unwrap();
    store.delete_deck_at(&maths.id, T0 + 4).unwrap();

    store.undelete_deck(&maths.id).unwrap();

    assert_eq!(deleted_at(&store, "cards", &a.id), Some(T0 + 3));
    assert_eq!(deleted_at(&store, "cards", &b.id), None);
}

#[test]
fn undelete_refuses_a_name_taken_meanwhile_and_changes_nothing() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let card = store.add_card_at(&maths.id, "a", "", T0).unwrap();
    store.delete_deck_at(&maths.id, T0 + 1).unwrap();
    let other = store.create_deck_at("MATHS", T0 + 2).unwrap();

    let err = store.undelete_deck(&maths.id).unwrap_err();

    assert!(
        matches!(&err, Error::DuplicateDeckName(n) if n == "Maths"),
        "{err:?}"
    );
    assert_eq!(deleted_at(&store, "decks", &maths.id), Some(T0 + 1));
    assert_eq!(deleted_at(&store, "cards", &card.id), Some(T0 + 1));
    assert_eq!(names(&store), [other.name]);
}

#[test]
fn undelete_of_a_live_deck_changes_nothing() {
    let mut store = Store::in_memory();
    let maths = store.create_deck_at("Maths", T0).unwrap();
    let earlier = store.add_card_at(&maths.id, "a", "", T0).unwrap();
    store.delete_card_at(&earlier.id, T0 + 1).unwrap();

    assert_eq!(store.undelete_deck(&maths.id).unwrap(), maths);
    assert_eq!(deleted_at(&store, "cards", &earlier.id), Some(T0 + 1));
}

#[test]
fn undelete_of_an_unknown_deck_is_not_found() {
    let mut store = Store::in_memory();
    assert!(matches!(
        store.undelete_deck("01920000-0000-7000-8000-00000000ffff"),
        Err(Error::DeckNotFound(_))
    ));
}

// --- The deck list counts like the deck page ------------------------------

mod study_counts {
    use super::{DAY_MS, T0};
    use crate::domain::Grade;
    /// The default `max_new_per_day`.
    const NEW_PER_DAY: u32 = 20;
    use crate::storage::Store;
    use crate::test_support::answer;

    /// "Today" began two hours before `T0`.
    const DAY_START: i64 = T0 - 2 * 3_600_000;

    fn deck_with_cards(store: &mut Store, name: &str, n: usize) -> (String, Vec<String>) {
        let deck = store.create_deck_at(name, T0).unwrap().id;
        let cards = (0..n)
            .map(|i| {
                store
                    .add_card_at(&deck, &format!("q{i}"), "a", T0)
                    .unwrap()
                    .id
            })
            .collect();
        (deck, cards)
    }

    /// Every deck's `(new, due)` from `list_decks` equals the lengths of
    /// its `study_input` at the same time.
    fn assert_list_matches_study_input(store: &Store, now: i64, day_start: i64) {
        for summary in store.list_decks(now, day_start).unwrap() {
            let input = store.study_input(&summary.deck.id, now, day_start).unwrap();
            assert_eq!(
                (summary.new_count as usize, summary.due_count as usize),
                (input.new.len(), input.reviews.len() + input.steps.len()),
                "deck {}",
                summary.deck.name
            );
        }
    }

    #[test]
    fn list_counts_equal_the_study_session_at_any_time() {
        let mut store = Store::in_memory();
        let (_, maths) = deck_with_cards(&mut store, "Maths", 30);
        let (_, physics) = deck_with_cards(&mut store, "Physics", 3);
        // Yesterday's new cards, now in review; one failed today, in the step.
        answer(&mut store, &maths[0], Grade::Pass, DAY_START - DAY_MS);
        answer(
            &mut store,
            &maths[0],
            Grade::Pass,
            DAY_START - DAY_MS + 600_000,
        );
        answer(&mut store, &maths[1], Grade::Fail, T0);
        // Today's first reviews use the budget; a deleted one still does.
        answer(&mut store, &maths[2], Grade::Pass, T0);
        answer(&mut store, &maths[3], Grade::Pass, T0);
        store.delete_card_at(&maths[3], T0).unwrap();
        answer(&mut store, &physics[0], Grade::Fail, T0);

        for now in [T0, T0 + 5 * 60_000, T0 + DAY_MS, T0 + 40 * DAY_MS] {
            assert_list_matches_study_input(&store, now, DAY_START);
        }
        assert_list_matches_study_input(&store, T0 + DAY_MS, DAY_START + DAY_MS);
    }

    #[test]
    fn list_new_counts_are_capped_per_day_and_renew_the_next_day() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 30);
        let new_count =
            |store: &Store, now, day_start| store.list_decks(now, day_start).unwrap()[0].new_count;
        assert_eq!(new_count(&store, T0, DAY_START), NEW_PER_DAY);

        for card in &cards[..5] {
            answer(&mut store, card, Grade::Pass, T0);
        }
        assert_eq!(new_count(&store, T0, DAY_START), NEW_PER_DAY - 5);
        for card in &cards[5..NEW_PER_DAY as usize] {
            answer(&mut store, card, Grade::Pass, T0);
        }
        assert_eq!(
            new_count(&store, T0, DAY_START),
            0,
            "today's budget is used"
        );
        // The next study day: 10 unreviewed cards left, all allowed.
        assert_eq!(new_count(&store, T0 + DAY_MS, DAY_START + DAY_MS), 10);
    }

    // --- What the deck list says when there is nothing to study now -----

    /// `(card_count, suspended_count, held_back_count, next_change_at)` of
    /// the only deck.
    fn extra(store: &Store, now: i64, day_start: i64) -> (u32, u32, u32, Option<i64>) {
        let d = store.list_decks(now, day_start).unwrap().remove(0);
        (
            d.card_count,
            d.suspended_count,
            d.held_back_count,
            d.next_change_at,
        )
    }

    /// Every deck's `next_change_at` equals `next_study_change`.
    fn assert_next_matches(store: &Store, now: i64) {
        for d in store.list_decks(now, DAY_START).unwrap() {
            assert_eq!(
                d.next_change_at,
                store.next_study_change(&d.deck.id, now).unwrap(),
                "deck {}",
                d.deck.name
            );
        }
    }

    #[test]
    fn a_deck_without_cards_has_nothing_counted_and_no_next_change() {
        let mut store = Store::in_memory();
        deck_with_cards(&mut store, "Empty", 0);
        assert_eq!(extra(&store, T0, DAY_START), (0, 0, 0, None));
    }

    #[test]
    fn new_cards_have_no_next_change_and_suspended_ones_are_counted() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 3);
        assert_eq!(extra(&store, T0, DAY_START), (3, 0, 0, None));
        store.set_suspended(&cards[..1], true).unwrap();
        assert_eq!(extra(&store, T0, DAY_START), (3, 1, 0, None));
        // Every card suspended, one of them reviewed: no change to come.
        answer(&mut store, &cards[1], Grade::Pass, T0);
        store.set_suspended(&cards, true).unwrap();
        let d = store.list_decks(T0, DAY_START).unwrap().remove(0);
        assert_eq!((d.new_count, d.due_count), (0, 0));
        assert_eq!(extra(&store, T0, DAY_START), (3, 3, 0, None));
        assert_next_matches(&store, T0);
    }

    #[test]
    fn deleted_cards_are_not_counted() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 3);
        store.set_suspended(&cards[..1], true).unwrap();
        store.delete_card_at(&cards[0], T0).unwrap();
        store.delete_card_at(&cards[1], T0).unwrap();
        assert_eq!(extra(&store, T0, DAY_START), (1, 0, 0, None));
    }

    #[test]
    fn a_step_card_changes_twenty_minutes_before_it_is_due() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 1);
        // Failed at T0: due again in the 10-minute step, so offered at once.
        answer(&mut store, &cards[0], Grade::Fail, T0);
        let d = store.list_decks(T0, DAY_START).unwrap().remove(0);
        assert_eq!((d.due_count, d.next_change_at), (1, None));
        assert_next_matches(&store, T0);
        // Seen from 30 minutes earlier, the step (due at T0 + 10 min) is
        // more than 20 minutes away: it changes the deck 20 minutes early.
        let earlier = T0 - 30 * 60_000;
        let d = store.list_decks(earlier, DAY_START).unwrap().remove(0);
        let due = T0 + 10 * 60_000;
        assert_eq!(
            (d.due_count, d.next_change_at),
            (0, Some(due - 20 * 60_000))
        );
        assert_next_matches(&store, earlier);
    }

    #[test]
    fn suspended_cards_are_never_held_back() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 30);
        store.set_suspended(&cards[25..], true).unwrap();
        // 25 new not suspended, 20 allowed: 5 held back, not 10.
        let d = store.list_decks(T0, DAY_START).unwrap().remove(0);
        assert_eq!(
            (d.new_count, d.held_back_count, d.suspended_count),
            (NEW_PER_DAY, 5, 5)
        );

        // Three overdue reviews, one suspended, a limit of 1: one held back.
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Physics", 3);
        for card in &cards {
            answer(&mut store, card, Grade::Pass, T0 - 3 * DAY_MS);
            answer(&mut store, card, Grade::Pass, T0 - 3 * DAY_MS + 600_000);
        }
        store.set_suspended(&cards[..1], true).unwrap();
        store.set_max_reviews_per_day(1).unwrap();
        let later = T0 + 400 * DAY_MS;
        let d = store.list_decks(later, later).unwrap().remove(0);
        assert_eq!((d.due_count, d.held_back_count), (1, 1));
    }

    #[test]
    fn a_reset_card_counts_as_new_again() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 1);
        answer(&mut store, &cards[0], Grade::Pass, T0 - 3 * DAY_MS);
        answer(
            &mut store,
            &cards[0],
            Grade::Pass,
            T0 - 3 * DAY_MS + 600_000,
        );
        let before = store.list_decks(T0, DAY_START).unwrap().remove(0);
        assert_eq!(before.new_count, 0);
        store.reset_card(&cards[0], T0).unwrap();
        let d = store.list_decks(T0, DAY_START).unwrap().remove(0);
        assert_eq!(
            (
                d.new_count,
                d.due_count,
                d.held_back_count,
                d.next_change_at
            ),
            (1, 0, 0, None)
        );
        assert_list_matches_study_input(&store, T0, DAY_START);
    }

    #[test]
    fn a_learned_card_changes_when_it_is_due_and_not_before() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 1);
        answer(&mut store, &cards[0], Grade::Pass, T0);
        answer(&mut store, &cards[0], Grade::Pass, T0 + 600_000);
        let due = store.next_study_change(&cards_deck(&store), T0 + 600_000);
        let due = due.unwrap().expect("a reviewed card becomes due");
        assert!(due > T0 + 600_000);
        assert_eq!(extra(&store, T0 + 600_000, DAY_START), (1, 0, 0, Some(due)));
        // One millisecond before: still the next change. At the due time it
        // is studied now, so there is no later change.
        assert_eq!(extra(&store, due - 1, DAY_START).3, Some(due));
        let at_due = store.list_decks(due, due).unwrap().remove(0);
        assert_eq!((at_due.due_count, at_due.next_change_at), (1, None));
        for now in [T0, T0 + 600_000, due - 1, due, due + DAY_MS] {
            assert_next_matches(&store, now);
        }
    }

    fn cards_deck(store: &Store) -> String {
        store.decks().unwrap().remove(0).id
    }

    #[test]
    fn the_daily_limits_hold_cards_back() {
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Maths", 30);
        // 20 new allowed today, 10 held back.
        assert_eq!(extra(&store, T0, DAY_START).2, 10);
        for card in &cards[..NEW_PER_DAY as usize] {
            answer(&mut store, card, Grade::Pass, T0);
        }
        let d = store.list_decks(T0, DAY_START).unwrap().remove(0);
        assert_eq!((d.new_count, d.held_back_count), (0, 10));
        // The next day the ten are allowed; nothing is held back.
        let tomorrow = store
            .list_decks(T0 + DAY_MS, DAY_START + DAY_MS)
            .unwrap()
            .remove(0);
        assert_eq!((tomorrow.new_count, tomorrow.held_back_count), (10, 0));

        // A review limit of 2 with 3 cards due holds one back.
        let mut store = Store::in_memory();
        let (_, cards) = deck_with_cards(&mut store, "Physics", 3);
        for card in &cards {
            answer(&mut store, card, Grade::Pass, T0 - 3 * DAY_MS);
            answer(&mut store, card, Grade::Pass, T0 - 3 * DAY_MS + 600_000);
        }
        store.set_max_reviews_per_day(2).unwrap();
        let later = T0 + 400 * DAY_MS;
        let d = store.list_decks(later, later).unwrap().remove(0);
        assert_eq!((d.due_count, d.held_back_count), (2, 1));
        store.set_max_reviews_per_day(0).unwrap();
        let d = store.list_decks(later, later).unwrap().remove(0);
        assert_eq!((d.due_count, d.held_back_count), (0, 3));
        assert_next_matches(&store, later);
    }
}

// --- The deck list's counts are a second implementation of study_input ---

mod counts_equal_study_input {
    use std::time::Duration;

    use proptest::prelude::*;

    use super::{DAY_MS, T0};
    use crate::domain::{CardState, Grade, Phase};
    use crate::storage::Store;
    use crate::storage::review::deck_counts_in;

    const CARDS: usize = 12;

    /// One random write: an answer, a suspension, an undo of the card's
    /// latest entry, a reset or a deletion.
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Pass,
        Fail,
        Suspend,
        Unsuspend,
        UndoLatest,
        Reset,
        Delete,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => Just(Op::Pass),
            2 => Just(Op::Fail),
            1 => Just(Op::Suspend),
            1 => Just(Op::Unsuspend),
            1 => Just(Op::UndoLatest),
            1 => Just(Op::Reset),
            1 => Just(Op::Delete),
        ]
    }

    /// The deck's cards that would be studied now without the daily
    /// limits, counted card by card through the public API: new cards and
    /// due reviews, neither suspended.
    fn uncut_totals(store: &Store, deck: &str, now: i64) -> (usize, usize) {
        let mut new = 0;
        let mut due = 0;
        for card in store.list_cards(deck).unwrap() {
            if card.suspended {
                continue;
            }
            match store.card_state(&card.id).unwrap() {
                CardState::New => new += 1,
                CardState::Scheduled(s) if s.phase == Phase::Review && s.due_at <= now => {
                    due += 1;
                }
                CardState::Scheduled(_) => {}
            }
        }
        (new, due)
    }

    proptest! {
        // About 0.1 s per 16 cases in a debug build.
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// `deck_counts_in` (the deck list) and `study_input` (the deck
        /// page) agree after any history, at any time, under any limits
        /// (ADR 0014, 0041).
        #[test]
        fn deck_counts_equal_study_input(
            ops in prop::collection::vec((op(), 0..CARDS, 0_i64..3_000), 0..60),
            max_new in 0_u32..6,
            max_reviews in 0_u32..6,
            probes in prop::collection::vec((0_i64..40_000, 0_i64..24), 1..6),
        ) {
            let mut store = Store::in_memory();
            store.set_max_new_per_day(max_new).unwrap();
            store.set_max_reviews_per_day(max_reviews).unwrap();
            let a = store.create_deck_at("A", T0).unwrap().id;
            let b = store.create_deck_at("B", T0).unwrap().id;
            let cards: Vec<String> = (0..CARDS)
                .map(|i| {
                    let deck = if i % 3 == 0 { &b } else { &a };
                    store.add_card_at(deck, &format!("q{i}"), "a", T0).unwrap().id
                })
                .collect();
            let mut latest: Vec<Option<String>> = vec![None; CARDS];
            let mut t = T0;
            for (op, i, minutes) in ops {
                t += minutes * 60_000;
                let card = &cards[i];
                let one = std::slice::from_ref(card);
                // Writes on a deleted card fail; that is part of the
                // history too.
                match op {
                    Op::Pass | Op::Fail => {
                        let grade = if matches!(op, Op::Pass) { Grade::Pass } else { Grade::Fail };
                        if let Ok(e) = store.record_review(card, grade, Duration::from_secs(1), t) {
                            latest[i] = Some(e.id);
                        }
                    }
                    Op::Suspend => { let _ = store.set_suspended(one, true); }
                    Op::Unsuspend => { let _ = store.set_suspended(one, false); }
                    Op::UndoLatest => {
                        if let Some(e) = latest[i].take() {
                            let _ = store.undo(card, &e, t);
                        }
                    }
                    Op::Reset => {
                        if let Ok(e) = store.reset_card(card, t) {
                            latest[i] = Some(e.id);
                        }
                    }
                    Op::Delete => { let _ = store.delete_card_at(card, t); }
                }
            }
            for (minutes, hours_ago) in probes {
                let now = T0 + minutes * 60_000;
                let day_start = (now - (now - T0).rem_euclid(DAY_MS) - hours_ago * 3_600_000).min(now);
                for deck in [&a, &b] {
                    let input = store.study_input(deck, now, day_start).unwrap();
                    let counts = deck_counts_in(&store.conn, deck, now, day_start).unwrap();
                    prop_assert_eq!(counts.new as usize, input.new.len());
                    prop_assert_eq!(counts.reviews as usize, input.reviews.len());
                    prop_assert_eq!(counts.steps as usize, input.steps.len());
                    prop_assert_eq!(counts.next_change_at, store.next_study_change(deck, now).unwrap());
                    if let Some(at) = counts.next_change_at {
                        prop_assert!(at > now);
                    }
                    let live = store.list_cards(deck).unwrap();
                    prop_assert_eq!(counts.live as usize, live.len());
                    prop_assert_eq!(counts.suspended as usize, live.iter().filter(|c| c.suspended).count());
                    let (new_total, due_total) = uncut_totals(&store, deck, now);
                    prop_assert_eq!(
                        counts.held_back as usize,
                        (new_total - input.new.len()) + (due_total - input.reviews.len())
                    );
                }
                // And `list_decks` passes them on unchanged.
                for s in store.list_decks(now, day_start).unwrap() {
                    let input = store.study_input(&s.deck.id, now, day_start).unwrap();
                    prop_assert_eq!(s.new_count as usize, input.new.len());
                    prop_assert_eq!(s.due_count as usize, input.reviews.len() + input.steps.len());
                }
            }
        }
    }
}
