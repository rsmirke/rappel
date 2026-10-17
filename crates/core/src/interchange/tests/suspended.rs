//! The `suspended` flag in the file (ADR 0035) and the search index of
//! imported cards.

use super::*;
use crate::storage::BrowseQuery;

fn env_with_cards() -> (Env, Vec<String>) {
    let mut env = Env::new();
    let deck = env.store.create_deck("Maths").unwrap().id;
    let ids = ["Électron **libre**", "second", "third"]
        .iter()
        .map(|f| env.store.add_card(&deck, f, "back").unwrap().id)
        .collect();
    (env, ids)
}

fn suspended_in(json: &Value) -> Vec<bool> {
    json["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["suspended"].as_bool().unwrap())
        .collect()
}

#[test]
fn the_flag_round_trips() {
    let (mut source, ids) = env_with_cards();
    source.store.set_suspended(&ids[1..2], true).unwrap();
    let bytes = source.export();
    let flags = suspended_in(&json_of(&bytes));
    assert_eq!(flags.iter().filter(|f| **f).count(), 1);

    let mut target = Env::new();
    target.import(&bytes).unwrap();
    assert!(target.store.card(&ids[1]).unwrap().suspended);
    assert!(!target.store.card(&ids[0]).unwrap().suspended);
    assert_eq!(target.export(), bytes);
}

#[test]
fn a_file_made_before_suspension_existed_imports_as_not_suspended() {
    let (source, ids) = env_with_cards();
    let mut value = json_of(&source.export());
    for card in value["cards"].as_array_mut().unwrap() {
        card.as_object_mut().unwrap().remove("suspended").unwrap();
    }
    let mut target = Env::new();
    target.import(&archive_of(&value)).unwrap();
    for id in &ids {
        assert!(!target.store.card(id).unwrap().suspended);
    }
}

#[test]
fn a_card_that_differs_only_in_the_flag_is_already_present() {
    let (mut env, ids) = env_with_cards();
    let unsuspended = env.export();
    env.store.set_suspended(&ids[..1], true).unwrap();
    let suspended = env.export();

    // The database keeps its own flag, whichever way the file has it.
    for (file, db_flag) in [(&unsuspended, true), (&suspended, true)] {
        let plan = env.plan(file).unwrap();
        assert_eq!(plan.summary().cards.new, 0);
        assert_eq!(plan.summary().cards.present, 3);
        assert_eq!(plan.summary().conflicts.len(), 0);
        env.import(file).unwrap();
        assert_eq!(env.store.card(&ids[0]).unwrap().suspended, db_flag);
    }
    env.store.set_suspended(&ids[..1], false).unwrap();
    env.import(&suspended).unwrap();
    assert!(!env.store.card(&ids[0]).unwrap().suspended);
}

#[test]
fn imported_cards_are_searchable() {
    let (source, _) = env_with_cards();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    let found = target
        .store
        .browse(
            &BrowseQuery {
                text: "electron libre".to_owned(),
                ..BrowseQuery::default()
            },
            T_EXPORT,
        )
        .unwrap();
    assert_eq!(found.len(), 1);
}

/// A flag that is not `true` or `false` is a JSON error, never read as a
/// suspension (ADR 0050).
#[test]
fn a_flag_of_another_type_is_refused() {
    let (source, _) = env_with_cards();
    let base = json_of(&source.export());
    for flag in [
        json!(1),
        json!(0),
        json!("true"),
        json!(null),
        json!([true]),
    ] {
        let mut value = base.clone();
        value["cards"][0]["suspended"] = flag.clone();
        assert!(
            matches!(
                refused(Env::new().plan(&archive_of(&value))),
                ImportProblem::Json(_)
            ),
            "{flag}"
        );
    }
}
