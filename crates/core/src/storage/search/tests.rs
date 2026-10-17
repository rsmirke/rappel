use std::time::Duration;

use proptest::prelude::*;

use super::*;
use crate::domain::Grade;
use crate::scheduling::STEP_MS;
use crate::test_support::{DAY_MS, T0};

const NOW: i64 = T0 + 60 * DAY_MS;

fn store_with_deck() -> (Store, String) {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap().id;
    (store, deck)
}

fn query(text: &str) -> BrowseQuery {
    BrowseQuery {
        text: text.to_owned(),
        ..BrowseQuery::default()
    }
}

/// The fronts found by `q`, newest first.
fn fronts(store: &Store, q: &BrowseQuery) -> Vec<String> {
    store
        .browse(q, NOW)
        .unwrap()
        .into_iter()
        .map(|r| r.card.front_md)
        .collect()
}

fn review_twice(store: &mut Store, id: &str) {
    for at in [NOW - 40 * DAY_MS, NOW - 40 * DAY_MS + STEP_MS] {
        store
            .record_review(id, Grade::Pass, Duration::ZERO, at)
            .unwrap();
    }
}

/// The index as stored, by rowid.
fn index(conn: &Connection) -> Vec<(i64, String, String)> {
    let mut stmt = conn
        .prepare("SELECT rowid, front, back FROM cards_fts ORDER BY rowid")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn assert_index_is_a_rebuild(store: &mut Store) {
    let before = index(&store.conn);
    let tx = store.conn.transaction().unwrap();
    rebuild_search_index(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(index(&store.conn), before);
}

#[test]
fn a_query_is_folded_quoted_prefix_words() {
    assert_eq!(fts_query("Économie"), Some("\"economie\"*".to_owned()));
    assert_eq!(
        fts_query("  two   WORDS "),
        Some("\"two\"* \"words\"*".to_owned())
    );
    assert_eq!(fts_query(""), None);
    assert_eq!(fts_query("** - \""), None);
}

#[test]
fn nothing_typed_is_search_syntax() {
    let (mut store, deck) = store_with_deck();
    store.add_card_at(&deck, "alpha beta", "gamma", T0).unwrap();
    for text in [
        "alpha OR gamma",
        "NEAR(alpha beta)",
        "alpha*",
        "\"alpha",
        "col:alpha",
        "front:alpha",
        "-alpha",
        "alpha AND",
        "(alpha",
        "^alpha",
        "²",
        "\u{301}",
        "a\0b",
    ] {
        store.browse(&query(text), NOW).unwrap();
    }
    // "OR" is a word, not an operator: all words must match.
    assert_eq!(fronts(&store, &query("alpha OR gamma")).len(), 0);
    assert_eq!(fronts(&store, &query("alpha*")), ["alpha beta"]);
}

proptest! {
    #[test]
    fn any_input_is_a_valid_search(text in any::<String>()) {
        let (mut store, deck) = store_with_deck();
        store.add_card_at(&deck, "alpha beta", "gamma", T0).unwrap();
        store.browse(&query(&text), NOW).unwrap();
    }
}

#[test]
fn search_ignores_case_and_accents_on_both_sides() {
    let (mut store, deck) = store_with_deck();
    store
        .add_card_at(&deck, "Économie politique", "Résumé", T0)
        .unwrap();
    store.add_card_at(&deck, "cafe", "plain", T0).unwrap();
    for text in [
        "economie",
        "ÉCONOMIE",
        "économie",
        "polit",
        "resume",
        "RÉSUMÉ",
    ] {
        assert_eq!(
            fronts(&store, &query(text)),
            ["Économie politique"],
            "{text}"
        );
    }
    assert_eq!(fronts(&store, &query("café")), ["cafe"]);
    // Every word must match, in any order.
    assert_eq!(fronts(&store, &query("politique economie")).len(), 1);
    assert_eq!(fronts(&store, &query("economie cafe")).len(), 0);
}

#[test]
fn search_sees_what_is_read_not_the_markdown() {
    let (mut store, deck) = store_with_deck();
    store
        .add_card_at(
            &deck,
            "**bold** and [a link](https://example.org/path)",
            "$x^2$ and `code`",
            T0,
        )
        .unwrap();
    assert_eq!(fronts(&store, &query("bold")).len(), 1);
    assert_eq!(fronts(&store, &query("link")).len(), 1);
    assert_eq!(fronts(&store, &query("code")).len(), 1);
    assert_eq!(fronts(&store, &query("x")).len(), 1);
    // The address behind a link is not read; neither are the markers.
    assert_eq!(fronts(&store, &query("example")).len(), 0);
    assert_eq!(fronts(&store, &query("https")).len(), 0);
    assert_eq!(fronts(&store, &query("**")).len(), 0);
    assert_eq!(fronts(&store, &query("](")).len(), 0);
}

#[test]
fn an_edit_changes_what_is_found() {
    let (mut store, deck) = store_with_deck();
    let card = store.add_card_at(&deck, "before", "x", T0).unwrap();
    store.edit_card_at(&card.id, "after", "x", T0 + 1).unwrap();
    assert_eq!(fronts(&store, &query("before")).len(), 0);
    assert_eq!(fronts(&store, &query("after")), ["after"]);
    store
        .edit_card_with_tags_at(&card.id, "later", "x", None, T0 + 2)
        .unwrap();
    assert_eq!(fronts(&store, &query("after")).len(), 0);
    assert_eq!(fronts(&store, &query("later")), ["later"]);
    assert_index_is_a_rebuild(&mut store);
}

#[test]
fn deleted_cards_and_decks_are_not_found_until_they_come_back() {
    let (mut store, deck) = store_with_deck();
    let other = store.create_deck_at("Other", T0).unwrap().id;
    let a = store.add_card_at(&deck, "needle one", "", T0).unwrap();
    store.add_card_at(&other, "needle two", "", T0).unwrap();
    store.delete_card_at(&a.id, T0 + 1).unwrap();
    assert_eq!(fronts(&store, &query("needle")), ["needle two"]);
    store.undelete_card(&a.id).unwrap();
    assert_eq!(fronts(&store, &query("needle")).len(), 2);
    store.delete_deck_at(&other, T0 + 2).unwrap();
    assert_eq!(fronts(&store, &query("needle")), ["needle one"]);
    store.undelete_deck(&other).unwrap();
    assert_eq!(fronts(&store, &query("needle")).len(), 2);
}

#[test]
fn an_empty_query_lists_every_live_card_newest_first() {
    let (mut store, deck) = store_with_deck();
    for f in ["one", "two", "three"] {
        store.add_card_at(&deck, f, "", T0).unwrap();
    }
    assert_eq!(
        fronts(&store, &BrowseQuery::default()),
        ["three", "two", "one"]
    );
}

#[test]
fn filters_by_deck_and_tag_alone_and_together() {
    let (mut store, maths) = store_with_deck();
    let art = store.create_deck_at("Art", T0).unwrap().id;
    let a = store.add_card_at(&maths, "a", "", T0).unwrap();
    let b = store.add_card_at(&maths, "b", "", T0).unwrap();
    let c = store.add_card_at(&art, "c", "", T0).unwrap();
    let hard = store.add_tag_at(&a.id, "Hard", T0).unwrap();
    store.add_tag_at(&c.id, "Hard", T0).unwrap();
    store.add_tag_at(&b.id, "Easy", T0).unwrap();
    let q = |deck: Option<&String>, tag: Option<&str>| BrowseQuery {
        deck_id: deck.cloned(),
        tag_id: tag.map(str::to_owned),
        ..BrowseQuery::default()
    };
    assert_eq!(fronts(&store, &q(Some(&maths), None)), ["b", "a"]);
    assert_eq!(fronts(&store, &q(None, Some(&hard.id))), ["c", "a"]);
    assert_eq!(fronts(&store, &q(Some(&maths), Some(&hard.id))), ["a"]);
    let both = BrowseQuery {
        text: "a".to_owned(),
        ..q(Some(&maths), Some(&hard.id))
    };
    assert_eq!(fronts(&store, &both), ["a"]);
    // Tags come with the rows.
    let rows = store.browse(&q(None, Some(&hard.id)), NOW).unwrap();
    assert_eq!(rows[0].deck_name, "Art");
    assert_eq!(rows[0].tags, [hard]);
}

#[test]
fn state_filters_and_statuses() {
    let (mut store, deck) = store_with_deck();
    let new = store.add_card_at(&deck, "new", "", T0).unwrap();
    let learning = store.add_card_at(&deck, "learning", "", T0).unwrap();
    let due = store.add_card_at(&deck, "due", "", T0).unwrap();
    let later = store.add_card_at(&deck, "later", "", T0).unwrap();
    let held = store.add_card_at(&deck, "held", "", T0).unwrap();
    store
        .record_review(&learning.id, Grade::Pass, Duration::ZERO, NOW)
        .unwrap();
    review_twice(&mut store, &due.id);
    for at in [NOW - STEP_MS * 2, NOW - STEP_MS] {
        store
            .record_review(&later.id, Grade::Pass, Duration::ZERO, at)
            .unwrap();
    }
    review_twice(&mut store, &held.id);
    store
        .set_suspended(std::slice::from_ref(&held.id), true)
        .unwrap();
    let state = |s| BrowseQuery {
        state: Some(s),
        ..BrowseQuery::default()
    };
    assert_eq!(fronts(&store, &state(StateFilter::New)), ["new"]);
    assert_eq!(fronts(&store, &state(StateFilter::Learning)), ["learning"]);
    assert_eq!(fronts(&store, &state(StateFilter::Due)), ["due"]);
    assert_eq!(fronts(&store, &state(StateFilter::Suspended)), ["held"]);

    let rows = store.browse(&BrowseQuery::default(), NOW).unwrap();
    let status = |id: &str| rows.iter().find(|r| r.card.id == id).unwrap().status;
    assert_eq!(status(&new.id), CardStatus::New);
    assert!(matches!(status(&learning.id), CardStatus::Learning(_)));
    assert!(matches!(status(&due.id), CardStatus::Due(_)));
    assert!(matches!(
        status(&later.id),
        CardStatus::Scheduled(_) | CardStatus::Learning(_)
    ));
    assert_eq!(status(&held.id), CardStatus::Suspended);
    assert_eq!(
        store.browse_row(&held.id, NOW).unwrap().unwrap().status,
        CardStatus::Suspended
    );
    store.delete_card_at(&held.id, T0 + 1).unwrap();
    assert_eq!(store.browse_row(&held.id, NOW).unwrap(), None);
}

#[test]
fn a_suspended_card_is_not_studied_and_comes_back_unsuspended() {
    let (mut store, deck) = store_with_deck();
    let new = store.add_card_at(&deck, "new", "", T0).unwrap();
    let due = store.add_card_at(&deck, "due", "", T0).unwrap();
    review_twice(&mut store, &due.id);
    let ids = [new.id.clone(), due.id.clone()];
    let studied = |store: &Store| {
        let input = store.study_input(&deck, NOW, NOW - DAY_MS / 2).unwrap();
        (input.new.len(), input.reviews.len())
    };
    assert_eq!(studied(&store), (1, 1));
    let counts = |store: &Store| {
        let d = &store.list_decks(NOW, NOW - DAY_MS / 2).unwrap()[0];
        (d.new_count, d.due_count)
    };
    assert_eq!(counts(&store), (1, 1));

    assert_eq!(store.set_suspended(&ids, true).unwrap(), ids);
    assert_eq!(studied(&store), (0, 0));
    assert_eq!(counts(&store), (0, 0));
    assert_eq!(store.next_study_change(&deck, T0).unwrap(), None);
    assert!(store.card_info(&due.id, NOW).unwrap().suspended);
    // Nothing else about the card changed.
    assert_eq!(store.card(&due.id).unwrap().updated_at, T0);
    assert_eq!(store.review_events(&due.id).unwrap().len(), 2);

    // Already as asked: nothing changed, nothing returned.
    assert_eq!(store.set_suspended(&ids, true).unwrap().len(), 0);
    assert_eq!(store.set_suspended(&ids, false).unwrap().len(), 2);
    assert_eq!(studied(&store), (1, 1));
}

#[test]
fn a_suspended_card_leaves_the_forecast_and_retention() {
    use crate::stats::DayGrid;
    use crate::storage::Scope;

    let (mut store, deck) = store_with_deck();
    let card = store.add_card_at(&deck, "c", "", T0).unwrap();
    review_twice(&mut store, &card.id);
    let past = DayGrid::new((0..=30).map(|d| NOW - (30 - d) * DAY_MS).collect()).unwrap();
    let ahead = DayGrid::new((0..=14).map(|d| NOW + d * DAY_MS).collect()).unwrap();
    let scheduled = |store: &Store| {
        store
            .stats(Scope::All, NOW, &past, &ahead)
            .unwrap()
            .scheduled_cards
    };
    assert_eq!(scheduled(&store), 1);
    store
        .set_suspended(std::slice::from_ref(&card.id), true)
        .unwrap();
    assert_eq!(scheduled(&store), 0);
    store.set_suspended(&[card.id], false).unwrap();
    assert_eq!(scheduled(&store), 1);
}

#[derive(Debug, Clone)]
enum Op {
    Add(String, String),
    Edit(usize, String),
    Delete(usize),
    Undelete(usize),
    Move(usize),
    Suspend(usize),
}

fn op() -> impl Strategy<Value = Op> {
    let text = "[a-zA-Zéèü*_$# ]{0,12}";
    prop_oneof![
        3 => (text, text).prop_map(|(a, b)| Op::Add(a, b)),
        3 => (0..8usize, text).prop_map(|(i, t)| Op::Edit(i, t)),
        1 => (0..8usize).prop_map(Op::Delete),
        1 => (0..8usize).prop_map(Op::Undelete),
        1 => (0..8usize).prop_map(Op::Move),
        1 => (0..8usize).prop_map(Op::Suspend),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Whatever was written, the index equals one rebuilt from the cards.
    #[test]
    fn the_index_equals_a_rebuild(ops in proptest::collection::vec(op(), 1..30)) {
        let (mut store, deck) = store_with_deck();
        let other = store.create_deck_at("Other", T0).unwrap().id;
        let mut ids: Vec<String> = Vec::new();
        let mut t = T0;
        for op in ops {
            t += 1;
            match op {
                Op::Add(f, b) => {
                    if let Ok(card) = store.add_card_at(&deck, &f, &b, t) {
                        ids.push(card.id);
                    }
                }
                Op::Edit(i, f) => {
                    if let Some(id) = ids.get(i % ids.len().max(1)) {
                        let _ = store.edit_card_at(id, &f, "back", t);
                    }
                }
                Op::Delete(i) => {
                    if let Some(id) = ids.get(i % ids.len().max(1)) {
                        let _ = store.delete_card_at(id, t);
                    }
                }
                Op::Undelete(i) => {
                    if let Some(id) = ids.get(i % ids.len().max(1)) {
                        let _ = store.undelete_card(id);
                    }
                }
                Op::Move(i) => {
                    if let Some(id) = ids.get(i % ids.len().max(1)) {
                        let _ = store.move_cards(std::slice::from_ref(id), &other);
                    }
                }
                Op::Suspend(i) => {
                    if let Some(id) = ids.get(i % ids.len().max(1)) {
                        let _ = store.set_suspended(std::slice::from_ref(id), true);
                    }
                }
            }
        }
        assert_index_is_a_rebuild(&mut store);
    }
}

/// The tokens of folded text as FTS5's `unicode61` reads ASCII: runs of
/// letters and digits. The generated text below only folds to ASCII.
fn ascii_tokens(folded: &str) -> Vec<&str> {
    folded
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Whether `word` (one word of a query) is found in `side`, by text alone:
/// its tokens appear in a row in the side's search text, the last one as
/// the start of a token.
fn word_in_side(word: &[&str], side: &[&str]) -> bool {
    let Some((last, exact)) = word.split_last() else {
        return true;
    };
    side.windows(word.len())
        .any(|w| w[..exact.len()] == *exact && w[exact.len()].starts_with(last))
}

/// What a search for `text` should find among `cards` (front, back), by
/// text alone: every word of the query in the front or the back.
fn found_by_text(text: &str, cards: &[(String, String)]) -> Vec<usize> {
    if text.trim().is_empty() {
        return (0..cards.len()).collect();
    }
    let folded: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == '\0')
        .map(search_key)
        .collect();
    let words: Vec<Vec<&str>> = folded
        .iter()
        .map(|w| ascii_tokens(w))
        .filter(|t| !t.is_empty())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    cards
        .iter()
        .enumerate()
        .filter(|(_, (front, back))| {
            let (front, back) = (search_text(front), search_text(back));
            let (front, back) = (ascii_tokens(&front), ascii_tokens(&back));
            words
                .iter()
                .all(|w| word_in_side(w, &front) || word_in_side(w, &back))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Pieces of FTS5 syntax and of words, for hostile queries.
fn query_text() -> impl Strategy<Value = String> {
    let piece = prop::sample::select(vec![
        "a",
        "b",
        "ab",
        "ba",
        "abc",
        "A",
        "É",
        "é",
        "e",
        "1",
        "10",
        "\"",
        "\"\"",
        "*",
        "^",
        "(",
        ")",
        ":",
        "-",
        "+",
        ".",
        ",",
        "OR",
        "AND",
        "NOT",
        "NEAR",
        "NEAR(",
        "front:",
        "back:",
        "{front back}:",
        "\0",
        " ",
        " ",
        " ",
        "\t",
    ]);
    prop::collection::vec(piece, 0..12).prop_map(|p| p.concat())
}

/// A side made of words that the queries above can find, with separators
/// that are no Markdown syntax at the start of a line.
fn side_text() -> impl Strategy<Value = String> {
    let word = prop::sample::select(vec![
        "a", "b", "ab", "abc", "bca", "e", "é", "É", "1", "10", "or", "and", "not", "near",
        "front", "back",
    ]);
    let sep = prop::sample::select(vec![" ", " ", "-", "\n", ", "]);
    prop::collection::vec((word, sep), 1..8)
        .prop_map(|ws| ws.into_iter().map(|(w, s)| w.to_owned() + s).collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the query holds (FTS5 operators, quotes, column filters,
    /// stars, carets, NUL, repeated words), a search finds exactly the
    /// cards whose text has every word, never more or fewer because of
    /// its syntax (ADR 0050).
    #[test]
    fn a_search_finds_by_text_never_by_syntax(
        cards in prop::collection::vec((side_text(), side_text()), 1..8),
        text in query_text(),
    ) {
        let (mut store, deck) = store_with_deck();
        let mut ids = Vec::new();
        for (front, back) in &cards {
            // Distinct fronts are not needed: rows are matched by id.
            ids.push(store.add_card_at(&deck, front, back, T0).unwrap().id);
        }
        let mut found: Vec<usize> = store
            .browse(&query(&text), NOW)
            .unwrap()
            .into_iter()
            .map(|r| ids.iter().position(|id| *id == r.card.id).unwrap())
            .collect();
        found.sort_unstable();
        prop_assert_eq!(found, found_by_text(&text, &cards), "query {:?}", text);
    }
}

#[test]
fn a_word_that_starts_another_is_left_out() {
    assert_eq!(fts_query("card ca card"), Some("\"card\"*".to_owned()));
    // Not the start of the other: both stay, in sorted order.
    assert_eq!(fts_query("rd card"), Some("\"card\"* \"rd\"*".to_owned()));
    // The start of a phrase's first token, or of the phrase itself.
    assert_eq!(fts_query("card-1 card"), Some("\"card-1\"*".to_owned()));
    assert_eq!(fts_query("CARD Card card"), Some("\"card\"*".to_owned()));
    // A quote is doubled after the comparison.
    assert_eq!(fts_query("a\" a\"b"), Some("\"a\"\"b\"*".to_owned()));
}

#[test]
fn only_the_start_of_a_long_query_is_searched() {
    let (mut store, deck) = store_with_deck();
    store.add_card_at(&deck, "alpha beta", "gamma", T0).unwrap();
    // The words after the limit are not searched: "zzz" would find nothing.
    let long = format!("{}alpha zzz", " ".repeat(MAX_QUERY_BYTES - 6));
    assert_eq!(fronts(&store, &query(&long)), ["alpha beta"]);
    // A word cut by the limit is searched as its start.
    let cut = format!("{}gammazzz", " ".repeat(MAX_QUERY_BYTES - 5));
    assert_eq!(fronts(&store, &query(&cut)), ["alpha beta"]);
    // The cut falls between two characters: é is two bytes.
    let accents = format!("{}éé", " ".repeat(MAX_QUERY_BYTES - 3));
    assert_eq!(query_start(&accents).len(), MAX_QUERY_BYTES - 1);
    assert_eq!(fts_query(&accents), Some("\"e\"*".to_owned()));
}
