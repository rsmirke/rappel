use super::*;
use crate::test_support::T0;

const MISSING: &str = "01920000-0000-7000-8000-00000000dead";

fn store_with_cards() -> (Store, String, String) {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap().id;
    let a = store.add_card_at(&deck, "a", "", T0).unwrap().id;
    let b = store.add_card_at(&deck, "b", "", T0).unwrap().id;
    (store, a, b)
}

fn names(tags: &[Tag]) -> Vec<&str> {
    tags.iter().map(|t| t.name.as_str()).collect()
}

#[test]
fn a_tag_is_found_by_its_name_key() {
    let (mut store, a, b) = store_with_cards();
    let maths = store.add_tag_at(&a, "  Maths ", T0).unwrap();
    assert_eq!(maths.name, "Maths");
    // Another spelling of the same key reuses the tag and its spelling.
    let again = store.add_tag_at(&b, "maths", T0).unwrap();
    assert_eq!(again, maths);
    let upper = store.add_tag_at(&b, "MATHS", T0).unwrap();
    assert_eq!(upper, maths);
    let e1 = store.add_tag_at(&a, "\u{C9}conomie", T0).unwrap();
    let e2 = store.add_tag_at(&b, "E\u{301}conomie", T0).unwrap();
    assert_eq!(e1, e2);
    assert_eq!(names(&store.tags().unwrap()), ["Maths", "\u{C9}conomie"]);
    assert_eq!(
        names(&store.card_tags(&a).unwrap()),
        ["Maths", "\u{C9}conomie"]
    );
    assert_eq!(
        names(&store.card_tags(&b).unwrap()),
        ["Maths", "\u{C9}conomie"]
    );
}

#[test]
fn adding_a_tag_twice_changes_nothing_and_leaves_the_card_alone() {
    let (mut store, a, _) = store_with_cards();
    let before = store.card(&a).unwrap();
    store.add_tag_at(&a, "x", T0 + 5).unwrap();
    store.add_tag_at(&a, "x", T0 + 6).unwrap();
    assert_eq!(store.card_tags(&a).unwrap().len(), 1);
    assert_eq!(store.card(&a).unwrap(), before);
}

#[test]
fn removing_a_tag_keeps_the_tag_and_its_id() {
    let (mut store, a, b) = store_with_cards();
    let x = store.add_tag_at(&a, "x", T0).unwrap();
    store.add_tag_at(&b, "x", T0).unwrap();
    assert!(store.remove_tag(&a, &x.id).unwrap());
    assert!(!store.remove_tag(&a, &x.id).unwrap());
    assert_eq!(store.card_tags(&a).unwrap(), []);
    assert_eq!(store.card_tags(&b).unwrap(), std::slice::from_ref(&x));
    store.remove_tag(&b, &x.id).unwrap();
    // Unused, but still there: the same name gets the same id back.
    assert_eq!(store.tags().unwrap(), std::slice::from_ref(&x));
    assert_eq!(store.add_tag_at(&a, "X", T0).unwrap(), x);
}

#[test]
fn invalid_names_and_missing_rows_are_refused_without_writing() {
    let (mut store, a, _) = store_with_cards();
    assert!(matches!(
        store.add_tag_at(&a, " ", T0),
        Err(Error::EmptyTagName)
    ));
    assert!(matches!(
        store.add_tag_at(&a, "a,b", T0),
        Err(Error::TagNameHasComma)
    ));
    assert!(matches!(
        store.add_tag_at(&a, "a\0", T0),
        Err(Error::ContainsNul)
    ));
    assert!(matches!(
        store.add_tag_at(&a, &"x".repeat(257), T0),
        Err(Error::TagNameTooLong { .. })
    ));
    assert!(matches!(
        store.add_tag_at(MISSING, "x", T0),
        Err(Error::CardNotFound(_))
    ));
    assert_eq!(store.tags().unwrap(), []);
    assert!(matches!(
        store.remove_tag(&a, MISSING),
        Err(Error::TagNotFound(_))
    ));
    assert!(matches!(
        store.card_tags(MISSING),
        Err(Error::CardNotFound(_))
    ));
}

#[test]
fn a_deleted_card_takes_no_tags_and_keeps_its_own() {
    let (mut store, a, _) = store_with_cards();
    let x = store.add_tag_at(&a, "x", T0).unwrap();
    store.delete_card_at(&a, T0 + 1).unwrap();
    assert!(matches!(
        store.add_tag_at(&a, "y", T0),
        Err(Error::CardNotFound(_))
    ));
    assert!(matches!(
        store.remove_tag(&a, &x.id),
        Err(Error::CardNotFound(_))
    ));
    store.undelete_card(&a).unwrap();
    assert_eq!(store.card_tags(&a).unwrap(), [x]);
}

#[test]
fn ids_of_tags_increase_and_differ_from_other_ids() {
    let (mut store, a, _) = store_with_cards();
    let x = store.add_tag_at(&a, "x", T0).unwrap();
    let y = store.add_tag_at(&a, "y", T0).unwrap();
    assert!(crate::id::is_uuid_v7(&x.id));
    assert!(y.id > x.id);
    assert_ne!(x.id, a);
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn a_card_and_its_tags_are_written_together() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap().id;
    let added = store
        .add_card_with_tags_at(&deck, "f", "b", &strings(&["Maths", "algebra"]), T0)
        .unwrap();
    assert_eq!(names(&added.tags), ["algebra", "Maths"]);
    assert_eq!(added.tags, store.card_tags(&added.card.id).unwrap());
    assert_eq!(added.card, store.card(&added.card.id).unwrap());
}

#[test]
fn a_bad_tag_name_writes_neither_the_card_nor_any_tag() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap().id;
    for bad in ["", "a,b", "a\0b", &"x".repeat(257)] {
        let result = store.add_card_with_tags_at(&deck, "f", "", &strings(&["ok", bad]), T0);
        assert!(result.is_err(), "{bad:?}");
    }
    assert_eq!(store.list_cards(&deck).unwrap(), []);
    assert_eq!(store.tags().unwrap(), []);

    let card = store.add_card_at(&deck, "f", "b", T0).unwrap();
    let before = store.card(&card.id).unwrap();
    let result = store.edit_card_with_tags_at(
        &card.id,
        "changed",
        "b",
        Some(&strings(&["new", "a,b"])),
        T0 + 1,
    );
    assert!(matches!(result, Err(Error::TagNameHasComma)));
    assert_eq!(store.card(&card.id).unwrap(), before);
    assert_eq!(store.tags().unwrap(), []);
    // A missing deck writes no tag either.
    let result = store.add_card_with_tags_at(MISSING, "f", "", &strings(&["x"]), T0);
    assert!(matches!(result, Err(Error::DeckNotFound(_))));
    assert_eq!(store.tags().unwrap(), []);
}

#[test]
fn an_edit_makes_the_tags_exactly_the_names_and_never_deletes_a_tag() {
    let (mut store, a, b) = store_with_cards();
    let first = store
        .edit_card_with_tags_at(&a, "a", "", Some(&strings(&["x", "y"])), T0 + 1)
        .unwrap();
    store.add_tag_at(&b, "y", T0).unwrap();
    let second = store
        .edit_card_with_tags_at(&a, "a", "", Some(&strings(&["Y", "z"])), T0 + 2)
        .unwrap();
    // "Y" is the existing "y": its spelling and id are kept.
    assert_eq!(names(&second.tags), ["y", "z"]);
    assert_eq!(second.tags[0], first.tags[1]);
    assert_eq!(second.tags, store.card_tags(&a).unwrap());
    // "x" was unlinked, not deleted; the other card keeps "y".
    assert_eq!(names(&store.tags().unwrap()), ["x", "y", "z"]);
    assert_eq!(names(&store.card_tags(&b).unwrap()), ["y"]);
    let cleared = store
        .edit_card_with_tags_at(&a, "a", "", Some(&[]), T0 + 3)
        .unwrap();
    assert_eq!(cleared.tags, []);
    assert_eq!(names(&store.tags().unwrap()), ["x", "y", "z"]);
}

#[test]
fn no_names_leaves_the_tags_alone() {
    let (mut store, a, _) = store_with_cards();
    let tagged = store
        .edit_card_with_tags_at(&a, "a", "", Some(&strings(&["x"])), T0 + 1)
        .unwrap();
    let kept = store
        .edit_card_with_tags_at(&a, "edited", "", None, T0 + 2)
        .unwrap();
    assert_eq!(kept.tags, tagged.tags);
    assert_eq!(kept.card.front_md, "edited");
}

#[test]
fn only_a_changed_side_bumps_updated_at() {
    let (mut store, a, _) = store_with_cards();
    let tagged = store
        .edit_card_with_tags_at(&a, "a", "", Some(&strings(&["x"])), T0 + 5)
        .unwrap();
    assert_eq!(tagged.card.updated_at, T0);
    let edited = store
        .edit_card_with_tags_at(&a, "a2", "", Some(&strings(&["x"])), T0 + 6)
        .unwrap();
    assert_eq!(edited.card.updated_at, T0 + 6);
    assert_eq!(edited.card, store.card(&a).unwrap());
}

#[test]
fn editing_a_missing_or_deleted_card_with_tags_fails() {
    let (mut store, a, _) = store_with_cards();
    store.delete_card_at(&a, T0 + 1).unwrap();
    for id in [a.as_str(), MISSING] {
        let result = store.edit_card_with_tags_at(id, "x", "", Some(&strings(&["t"])), T0 + 2);
        assert!(matches!(result, Err(Error::CardNotFound(_))));
    }
    assert_eq!(store.tags().unwrap(), []);
}

#[test]
fn tagged_cards_are_listed_in_creation_order_with_their_tags() {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap().id;
    let other = store.create_deck_at("Other", T0).unwrap().id;
    let a = store
        .add_card_with_tags_at(&deck, "a", "", &strings(&["b", "A"]), T0)
        .unwrap();
    let plain = store.add_card_at(&deck, "plain", "", T0).unwrap();
    let c = store
        .add_card_with_tags_at(&deck, "c", "", &strings(&["b"]), T0)
        .unwrap();
    store
        .add_card_with_tags_at(&other, "o", "", &strings(&["b"]), T0)
        .unwrap();
    let gone = store
        .add_card_with_tags_at(&deck, "gone", "", &strings(&["b"]), T0)
        .unwrap();
    store.delete_card_at(&gone.card.id, T0 + 1).unwrap();

    let listed = store.list_tagged_cards(&deck).unwrap();
    assert_eq!(
        listed,
        [
            a,
            TaggedCard {
                card: plain,
                tags: vec![]
            },
            c
        ]
    );
    assert_eq!(names(&listed[0].tags), ["A", "b"]);
    assert!(matches!(
        store.list_tagged_cards(MISSING),
        Err(Error::DeckNotFound(_))
    ));
}

mod property {
    use proptest::prelude::*;

    use super::*;
    use crate::domain::parse_tag_field;

    proptest! {
        /// After any run of edits, the card's tags are the names of the last
        /// field typed, and the set of tags only ever grows.
        #[test]
        fn the_tags_are_the_last_field_typed(
            fields in proptest::collection::vec("[a-cA-CÉé ,]{0,12}", 1..8),
        ) {
            let (mut store, a, _) = store_with_cards();
            let mut known = 0;
            for (i, field) in fields.iter().enumerate() {
                let Ok(names) = parse_tag_field(field) else { continue };
                let now = T0 + 1 + i64::try_from(i).unwrap();
                let tagged = store
                    .edit_card_with_tags_at(&a, "a", "", Some(&names), now)
                    .unwrap();
                let wanted: Vec<_> = names.iter().map(|n| name_key(n)).collect();
                let mut got: Vec<_> = tagged.tags.iter().map(|t| name_key(&t.name)).collect();
                let mut wanted_sorted = wanted.clone();
                wanted_sorted.sort();
                got.sort();
                prop_assert_eq!(got, wanted_sorted);
                let all = store.tags().unwrap().len();
                prop_assert!(all >= known);
                known = all;
            }
        }
    }
}

#[test]
fn the_same_tag_named_twice_is_one_tag() {
    let (mut store, a, _) = store_with_cards();
    let tagged = store
        .edit_card_with_tags_at(&a, "a", "", Some(&strings(&["x", "X", " x "])), T0 + 1)
        .unwrap();
    assert_eq!(names(&tagged.tags), ["x"]);
    assert_eq!(store.tags().unwrap().len(), 1);
    let deck = store.create_deck_at("D", T0).unwrap().id;
    let added = store
        .add_card_with_tags_at(&deck, "f", "", &strings(&["Y", "y"]), T0)
        .unwrap();
    assert_eq!(names(&added.tags), ["Y"]);
}

/// The suggestions and the tag filter offer only the tags a live card has
/// (ADR 0052): a deleted card or deck, or a tag taken off, hides one; a
/// suspended card keeps it; an undo brings it back.
#[test]
fn used_tags_are_those_a_live_card_has() {
    let (mut store, a, b) = store_with_cards();
    let other = store.create_deck_at("Physics", T0).unwrap().id;
    let c = store.add_card_at(&other, "c", "", T0).unwrap().id;
    let x = store.add_tag_at(&a, "x", T0).unwrap();
    store.add_tag_at(&b, "suspended", T0).unwrap();
    store.add_tag_at(&c, "physics", T0).unwrap();
    let typo = store.add_tag_at(&a, "algebar", T0).unwrap();
    store.set_suspended(std::slice::from_ref(&b), true).unwrap();
    assert_eq!(
        names(&store.used_tags().unwrap()),
        ["algebar", "physics", "suspended", "x"]
    );

    // A typo taken off: still a tag, no longer offered.
    assert!(store.remove_tag(&a, &typo.id).unwrap());
    assert_eq!(names(&store.tags().unwrap()).len(), 4);
    assert_eq!(
        names(&store.used_tags().unwrap()),
        ["physics", "suspended", "x"]
    );

    // A deleted deck hides its cards' tags; undeleting it brings them back.
    store.delete_deck_at(&other, T0 + 1).unwrap();
    assert_eq!(names(&store.used_tags().unwrap()), ["suspended", "x"]);
    store.undelete_deck(&other).unwrap();
    assert_eq!(
        names(&store.used_tags().unwrap()),
        ["physics", "suspended", "x"]
    );

    // A deleted card, likewise.
    store.delete_card_at(&a, T0 + 2).unwrap();
    assert_eq!(names(&store.used_tags().unwrap()), ["physics", "suspended"]);
    store.undelete_card(&a).unwrap();
    assert!(names(&store.used_tags().unwrap()).contains(&"x"));

    // A removal from All Cards and its Undo.
    let ids = vec![a.clone()];
    let removed = store.remove_tag_from_cards(&ids, &x.id).unwrap();
    assert!(!names(&store.used_tags().unwrap()).contains(&"x"));
    store.restore_tag_on_cards(&removed, &x.id).unwrap();
    assert!(names(&store.used_tags().unwrap()).contains(&"x"));
}
