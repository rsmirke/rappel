//! What a tag entry holds, decided without GTK (SPEC §4.4, ADR 0031, ADR
//! 0043): the tags already made into pills, and the text being typed after
//! them. This decides which typed names become pills, which tags to suggest,
//! what the field's error says, and when two fields hold the same tags.
//!
//! The typed text is the entry's: normally one name, since a comma turns
//! what is before it into a pill ([`split_typed`]). It can hold commas only
//! around names that cannot be tags (too long), which stay to be fixed. The
//! cursor is a character offset, as `GtkEditable` has it.

use std::collections::HashSet;
use std::ops::Range;

use rappel_core::Error;
use rappel_core::domain::{
    MAX_TAG_NAME_BYTES, Tag, TagFieldError, check_tag_name, name_key, parse_tag_field, search_key,
};

use crate::i18n::{fill, gettext};
use crate::ui::text::group_digits;

/// The most suggestions shown at once.
pub const MAX_SUGGESTIONS: usize = 8;

/// The most tags the field lets a card have (ADR 0050, row 30; core's
/// limit, which the Anki import keeps too). The store has no limit, so a
/// card imported with more keeps them all, but the field adds none past
/// this: a pasted list of thousands of names would make a pill for each,
/// and no one tags a card a hundred times on purpose. A name past the
/// limit stays in the text, with an error under the field.
pub use rappel_core::domain::MAX_TAGS_PER_CARD;

/// The most tags the field lets a card have, which had `loaded` tags when
/// the editor opened: [`MAX_TAGS_PER_CARD`], or `loaded` if more (a card
/// imported with more keeps them, and removing some leaves room up to what
/// it had).
pub fn tag_limit(loaded: usize) -> usize {
    loaded.max(MAX_TAGS_PER_CARD)
}

/// The name being typed: the piece of `text` between the commas around
/// `cursor` (a character offset), without the spaces at its ends, as a byte
/// range of `text`. Empty (and placed at the cursor) when the piece is blank.
pub fn name_at(text: &str, cursor: usize) -> Range<usize> {
    let cursor_byte = text
        .char_indices()
        .nth(cursor)
        .map_or(text.len(), |(i, _)| i);
    let start = text[..cursor_byte].rfind(',').map_or(0, |i| i + 1);
    let end = text[cursor_byte..]
        .find(',')
        .map_or(text.len(), |i| cursor_byte + i);
    let piece = &text[start..end];
    let trimmed = piece.trim();
    if trimmed.is_empty() {
        return cursor_byte..cursor_byte;
    }
    let lead = piece.len() - piece.trim_start().len();
    let from = start + lead;
    from..from + trimmed.len()
}

/// Up to [`MAX_SUGGESTIONS`] of `all` for the name typed at `cursor` in
/// `text`, the entry after the `pills`. A tag matches when its
/// [`search_key`] contains the typed name's, so "eco" finds "Économie" in
/// any case. Best first: tags that start with the name, then those with a
/// word that does, then the rest; each group by key, then name. Tags already
/// there (a pill, or another piece of the text, by [`name_key`]) are left
/// out.
pub fn suggestions(all: &[Tag], pills: &[String], text: &str, cursor: usize) -> Vec<Tag> {
    let typed = name_at(text, cursor);
    let needle = search_key(&text[typed.clone()]);
    if needle.is_empty() {
        return Vec::new();
    }
    let typed_key = name_key(&text[typed.clone()]);
    // A set: a pasted list can hold thousands of pieces.
    let mut others: HashSet<String> = pills.iter().map(|p| name_key(p)).collect();
    let mut at = 0;
    for piece in text.split(',') {
        let end = at + piece.len();
        // The piece being typed is not "already there".
        if !(at <= typed.start && typed.end <= end) {
            others.insert(name_key(piece));
        }
        at = end + 1;
    }
    let mut found: Vec<(u8, String, &Tag)> = all
        .iter()
        .filter(|tag| !others.contains(&name_key(&tag.name)))
        .filter_map(|tag| {
            let key = search_key(&tag.name);
            // The tag the user typed in full comes first, so Tab cannot swap
            // it for a look-alike ("Économie" / "Economie" are two tags).
            let rank = if name_key(&tag.name) == typed_key {
                0
            } else if key.starts_with(&needle) {
                1
            } else if key
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word.starts_with(&needle))
            {
                2
            } else if key.contains(&needle) {
                3
            } else {
                return None;
            };
            Some((rank, key, tag))
        })
        .collect();
    found.sort_by(|a, b| (a.0, &a.1, &a.2.name).cmp(&(b.0, &b.1, &b.2.name)));
    found
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, _, tag)| tag.clone())
        .collect()
}

/// `text` without the piece that holds `typed` (the name a suggestion
/// replaces, which becomes a pill), and its comma. Usually that leaves
/// nothing.
pub fn without_piece(text: &str, typed: &Range<usize>) -> String {
    let mut at = 0;
    let mut kept = Vec::new();
    for piece in text.split(',') {
        let end = at + piece.len();
        if !(at <= typed.start && typed.end <= end) {
            kept.push(piece);
        }
        at = end + 1;
    }
    kept.join(",")
}

/// What typing did to the entry: the names that become pills, and the text
/// left in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// New pills, trimmed, in the order typed.
    pub add: Vec<String>,
    /// What stays in the entry.
    pub rest: String,
}

/// Turns every name in `text` that a comma ends into a pill (Enter does it
/// for the last one too: `whole`). A blank name is dropped, and so is one
/// already a pill (by [`name_key`]; the pill keeps its spelling). A name
/// that cannot be a tag (too long), or that would take the card past
/// [`MAX_TAGS_PER_CARD`], stays in the entry, with its comma, so the error
/// under the field can say what to fix; nothing is lost. Splitting what is
/// left again changes nothing, so this can run on every change. Linear in
/// the text: the names seen are kept in a set.
pub fn split_typed(pills: &[String], text: &str, whole: bool, limit: usize) -> Split {
    let mut pieces: Vec<&str> = text.split(',').collect();
    let last = if whole { None } else { pieces.pop() };
    let mut keys: HashSet<String> = pills.iter().map(|p| name_key(p)).collect();
    let mut add = Vec::new();
    let mut kept = Vec::new();
    for piece in pieces {
        if piece.trim().is_empty() {
            continue;
        }
        let Ok(name) = check_tag_name(piece) else {
            kept.push(piece);
            continue;
        };
        let key = name_key(name);
        if keys.contains(&key) {
            continue;
        }
        if keys.len() >= limit {
            kept.push(piece);
            continue;
        }
        keys.insert(key);
        add.push(name.to_owned());
    }
    kept.extend(last);
    // The space typed after a comma is not part of the next name.
    Split {
        add,
        rest: kept.join(",").trim_start().to_owned(),
    }
}

/// What the field holds: the names to save, or the error to show under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagsCheck {
    /// The names, in the order typed, when they are all valid.
    pub names: Vec<String>,
    /// Why they cannot be saved.
    pub error: Option<String>,
}

/// Checks the field with the store's rules (`parse_tag_field`): the
/// `pills`, then the name being `typed`, which is saved too (a name typed
/// without a comma or Enter is not lost). The names are merged by
/// [`name_key`], keeping the first spelling. The pills were checked when
/// they were made, so an error is about the typed text: "this tag". The
/// typed text may not take the card past `limit` ([`tag_limit`]).
pub fn check(pills: &[String], typed: &str, limit: usize) -> TagsCheck {
    let mut text = pills.join(",");
    if !pills.is_empty() {
        text.push(',');
    }
    text.push_str(typed);
    match parse_tag_field(&text) {
        Ok(names) if names.len() > limit => TagsCheck {
            names: Vec::new(),
            error: Some(too_many_text()),
        },
        Ok(names) => TagsCheck { names, error: None },
        Err(err) => TagsCheck {
            names: Vec::new(),
            error: Some(error_text(&err)),
        },
    }
}

/// The error under the field when the typed names would take the card past
/// its limit. It names [`MAX_TAGS_PER_CARD`]: a card over it came so from an
/// import, and gets no more.
fn too_many_text() -> String {
    fill(
        // Translators: {limit} is the most tags a card can have (100).
        &gettext("A card can have at most {limit} tags"),
        &[("limit", &group_digits(MAX_TAGS_PER_CARD))],
    )
}

fn error_text(err: &TagFieldError) -> String {
    match &err.error {
        // Not reached from the tag entry: `split_typed` drops blank names
        // before a comma, and `parse_tag_field` ignores a blank last one.
        // Kept so every error of the store has a text.
        Error::EmptyTagName => "This tag is empty: remove the extra comma".to_owned(),
        Error::TagNameTooLong { .. } => format!(
            "This tag is too long (the limit is {} bytes)",
            group_digits(MAX_TAG_NAME_BYTES)
        ),
        Error::ContainsNul => "This tag contains a null character".to_owned(),
        Error::TagNameHasComma => "This tag contains a comma".to_owned(),
        other => format!("This tag cannot be saved: {other}"),
    }
}

/// The names of `names` as keys, in a fixed order, to tell whether two fields
/// hold the same tags whatever their spelling, spacing or order.
pub fn tag_keys(names: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = names.iter().map(|n| name_key(n)).collect();
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// [`super::split_typed`] for a card loaded with `pills`.
    fn split_typed(pills: &[String], text: &str, whole: bool) -> Split {
        super::split_typed(pills, text, whole, tag_limit(pills.len()))
    }

    /// [`super::check`] for a card loaded with `pills`.
    fn check(pills: &[String], typed: &str) -> TagsCheck {
        super::check(pills, typed, tag_limit(pills.len()))
    }

    fn tag(name: &str) -> Tag {
        Tag {
            id: format!("id-{name}"),
            name: name.to_owned(),
        }
    }

    fn tags(names: &[&str]) -> Vec<Tag> {
        names.iter().map(|n| tag(n)).collect()
    }

    fn names(found: &[Tag]) -> Vec<&str> {
        found.iter().map(|t| t.name.as_str()).collect()
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&s| s.to_owned()).collect()
    }

    /// The name under the cursor, where `|` marks the cursor.
    fn typed(marked: &str) -> String {
        let cursor = marked.chars().position(|c| c == '|').unwrap();
        let text = marked.replacen('|', "", 1);
        text[name_at(&text, cursor)].to_owned()
    }

    #[test]
    fn the_name_is_the_piece_around_the_cursor() {
        assert_eq!(typed("|"), "");
        assert_eq!(typed("mat|"), "mat");
        assert_eq!(typed("ma|ths"), "maths");
        assert_eq!(typed("a, b|c , d"), "bc");
        assert_eq!(typed("a,| b"), "b");
        assert_eq!(typed("a, |"), "");
        assert_eq!(typed("a,|,b"), "");
        assert_eq!(typed("é, Éco|"), "Éco");
    }

    #[test]
    fn a_blank_piece_is_an_empty_range_at_the_cursor() {
        assert_eq!(name_at("a, ", 3), 3..3);
        assert_eq!(name_at("a,  ,b", 3), 3..3);
        assert_eq!(name_at("", 0), 0..0);
    }

    #[test]
    fn a_cursor_past_the_end_is_the_end() {
        assert_eq!(name_at("ab", 99), 0..2);
    }

    #[test]
    fn suggestions_ignore_accents_and_case() {
        let all = tags(&["Économie", "Ecologie", "history", "maths"]);
        for typed in ["eco", "ECO", "éco", "Éco"] {
            let found = suggestions(&all, &[], typed, typed.chars().count());
            assert_eq!(names(&found), ["Ecologie", "Économie"], "{typed}");
        }
        assert_eq!(names(&suggestions(&all, &[], "économie", 8)), ["Économie"]);
        assert_eq!(suggestions(&all, &[], "", 0), []);
        assert_eq!(suggestions(&all, &[], " ", 1), []);
        assert_eq!(suggestions(&all, &[], "zzz", 3), []);
    }

    #[test]
    fn prefix_matches_come_before_word_starts_and_the_rest() {
        let all = tags(&["quartic", "linear algebra", "algebra", "Algèbre", "malgré"]);
        let found = suggestions(&all, &[], "alg", 3);
        assert_eq!(
            names(&found),
            ["algebra", "Algèbre", "linear algebra", "malgré"]
        );
    }

    #[test]
    fn tags_already_there_are_left_out_but_not_the_one_typed() {
        let all = tags(&["maths", "Mathématiques", "math-art"]);
        // A pill, in another spelling.
        let found = suggestions(&all, &strings(&["MATHS"]), "mat", 3);
        assert_eq!(names(&found), ["math-art", "Mathématiques"]);
        // Another piece of the text.
        let found = suggestions(&all, &[], "MATHS, mat", 10);
        assert_eq!(names(&found), ["math-art", "Mathématiques"]);
        // Typing the full name of a tag still offers it.
        let found = suggestions(&all, &[], "maths", 5);
        assert_eq!(names(&found), ["maths"]);
        // Editing an earlier piece: only the other pieces are excluded.
        let found = suggestions(&all, &[], "mat, maths", 3);
        assert_eq!(names(&found), ["math-art", "Mathématiques"]);
        // And the pills.
        let found = suggestions(&all, &strings(&["math-art"]), "mat, maths", 3);
        assert_eq!(names(&found), ["Mathématiques"]);
    }

    #[test]
    fn the_name_typed_in_full_comes_first() {
        // Two tags that differ only by an accent are two tags.
        let all = tags(&["Economie", "Économie", "Économies"]);
        let found = suggestions(&all, &[], "Économie", 8);
        assert_eq!(names(&found), ["Économie", "Economie", "Économies"]);
        let found = suggestions(&all, &[], "economie", 8);
        assert_eq!(names(&found), ["Economie", "Économie", "Économies"]);
    }

    #[test]
    fn at_most_eight_are_suggested() {
        let all: Vec<Tag> = (0..20).map(|i| tag(&format!("tag {i:02}"))).collect();
        let found = suggestions(&all, &[], "tag", 3);
        assert_eq!(found.len(), MAX_SUGGESTIONS);
        assert_eq!(found[0].name, "tag 00");
    }

    #[test]
    fn a_taken_suggestion_takes_the_typed_piece_away() {
        let take = |text: &str, cursor: usize| without_piece(text, &name_at(text, cursor));
        assert_eq!(take("alg", 3), "");
        assert_eq!(take(" alg ", 4), "");
        // Next to a name that cannot be a tag, which stays.
        let long = "x".repeat(300);
        let text = format!("{long},alg");
        assert_eq!(take(&text, text.chars().count()), long);
    }

    #[test]
    fn a_comma_turns_the_names_before_it_into_pills() {
        let split = |pills: &[&str], text: &str| split_typed(&strings(pills), text, false);
        assert_eq!(
            split(&[], "maths"),
            Split {
                add: vec![],
                rest: "maths".to_owned()
            }
        );
        assert_eq!(
            split(&[], " maths ,"),
            Split {
                add: strings(&["maths"]),
                rest: String::new()
            }
        );
        // A pasted list: everything but the last name, which is still
        // being typed.
        assert_eq!(
            split(&[], "a, b ,c, d"),
            Split {
                add: strings(&["a", "b", "c"]),
                rest: "d".to_owned()
            }
        );
        // Blank names and names already there are dropped.
        assert_eq!(
            split(&["Maths"], "maths,, ,a,A,"),
            Split {
                add: strings(&["a"]),
                rest: String::new()
            }
        );
    }

    #[test]
    fn enter_turns_the_last_name_into_a_pill_too() {
        let enter = |pills: &[&str], text: &str| split_typed(&strings(pills), text, true);
        assert_eq!(
            enter(&[], " Algèbre "),
            Split {
                add: strings(&["Algèbre"]),
                rest: String::new()
            }
        );
        assert_eq!(
            enter(&["algèbre"], "ALGÈBRE"),
            Split {
                add: vec![],
                rest: String::new()
            }
        );
        assert_eq!(
            enter(&[], "  "),
            Split {
                add: vec![],
                rest: String::new()
            }
        );
    }

    #[test]
    fn a_name_that_cannot_be_a_tag_stays_to_be_fixed() {
        let long = "x".repeat(300);
        let text = format!("a,{long},b");
        let split = split_typed(&[], &text, false);
        assert_eq!(split.add, strings(&["a"]));
        assert_eq!(split.rest, format!("{long},b"));
        // Splitting again changes nothing: it runs on every change.
        assert_eq!(
            split_typed(&split.add, &split.rest, false),
            Split {
                add: vec![],
                rest: split.rest.clone()
            }
        );
        // Enter keeps it too, and makes the rest pills.
        let split = split_typed(&strings(&["a"]), &format!("{long},b"), true);
        assert_eq!(split.add, strings(&["b"]));
        assert_eq!(split.rest, long);
    }

    #[test]
    fn the_field_is_the_pills_and_the_name_typed() {
        let ok = check(&strings(&["maths", "algebra"]), " Maths ");
        assert_eq!(ok.names, ["maths", "algebra"]);
        assert_eq!(ok.error, None);
        let ok = check(&strings(&["maths"]), " geo ");
        assert_eq!(ok.names, ["maths", "geo"]);
        assert_eq!(check(&[], "").names, Vec::<String>::new());
        assert_eq!(check(&[], " ").error, None);
        assert_eq!(check(&strings(&["a"]), "").names, ["a"]);

        let long = "x".repeat(257);
        let bad = check(&strings(&["a"]), &long);
        assert_eq!(bad.names, Vec::<String>::new());
        assert_eq!(
            bad.error.as_deref(),
            Some("This tag is too long (the limit is 256 bytes)")
        );
        assert_eq!(
            check(&[], &long).error.as_deref(),
            Some("This tag is too long (the limit is 256 bytes)")
        );
        assert_eq!(
            check(&[], "a\0").error.as_deref(),
            Some("This tag contains a null character")
        );
    }

    #[test]
    fn no_empty_name_reaches_the_check() {
        // Blank names are dropped by the split, and a blank typed name is
        // ignored, so "This tag is empty" is never shown by the entry.
        let split = split_typed(&[], "a,, ,b,", false);
        assert_eq!(split.add, strings(&["a", "b"]));
        assert_eq!(check(&split.add, &split.rest).error, None);
        assert_eq!(check(&strings(&["a"]), "  ").error, None);
    }

    /// `n` distinct names, "tag 0" to "tag n-1", joined by commas.
    fn pasted(n: usize) -> String {
        (0..n)
            .map(|i| format!("tag {i}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    #[test]
    fn the_field_adds_no_tag_past_the_limit_and_says_so() {
        let text = format!("{},", pasted(MAX_TAGS_PER_CARD + 5));
        let split = split_typed(&[], &text, false);
        assert_eq!(split.add.len(), MAX_TAGS_PER_CARD);
        assert_eq!(split.add[0], "tag 0");
        // The names past the limit stay in the text, nothing is dropped.
        assert_eq!(split.rest, "tag 100,tag 101,tag 102,tag 103,tag 104,");
        let over = check(&split.add, &split.rest);
        assert_eq!(
            over.error.as_deref(),
            Some("A card can have at most 100 tags")
        );
        assert_eq!(over.names, Vec::<String>::new());
        // Splitting again changes nothing.
        assert_eq!(
            split_typed(&split.add, &split.rest, false),
            Split {
                add: vec![],
                rest: split.rest.clone()
            }
        );
        // A name already there is no new tag, and blank text adds none.
        assert_eq!(check(&split.add, "tag 7").error, None);
        assert_eq!(check(&split.add, " ").error, None);
        assert_eq!(check(&split.add, "").names.len(), MAX_TAGS_PER_CARD);
        // One name typed at the limit, without a comma: not added either.
        let at_limit = check(&split.add, "one more");
        assert_eq!(at_limit.error, over.error);
        let enter = split_typed(&split.add, "one more", true);
        assert_eq!(enter.add, Vec::<String>::new());
        assert_eq!(enter.rest, "one more");
    }

    #[test]
    fn a_card_with_more_tags_than_the_limit_keeps_them_but_gets_no_more() {
        // Imported: the store has no limit.
        let pills: Vec<String> = (0..150).map(|i| format!("tag {i}")).collect();
        let kept = check(&pills, "");
        assert_eq!(kept.error, None);
        assert_eq!(kept.names.len(), 150);
        assert!(check(&pills, "new").error.is_some());
        assert_eq!(split_typed(&pills, "new,", false).add, Vec::<String>::new());
        // Removing some makes room up to what the card had, never past it.
        let limit = tag_limit(pills.len());
        assert_eq!(super::check(&pills[..120], "new", limit).error, None);
        assert_eq!(
            super::split_typed(&pills[..120], "new,", false, limit).add,
            ["new"]
        );
        assert!(super::check(&pills, "new", limit).error.is_some());
        assert_eq!(super::check(&pills[..99], "new", tag_limit(0)).error, None);
        assert!(
            super::check(&pills[..100], "new", tag_limit(0))
                .error
                .is_some()
        );
    }

    #[test]
    fn a_huge_paste_is_split_and_checked_in_linear_time() {
        let text = format!("{},last", pasted(20_000));
        crate::testing::within(
            Duration::from_secs(2),
            "split and check 20,000 names",
            move || {
                let split = split_typed(&[], &text, false);
                assert_eq!(split.add.len(), MAX_TAGS_PER_CARD);
                assert!(check(&split.add, &split.rest).error.is_some());
                // What the next key does with that text left in the field: the
                // split, the check and the suggestions, each once.
                let again = split_typed(&split.add, &split.rest, false);
                assert_eq!(again.add, Vec::<String>::new());
                let all: Vec<Tag> = (0..5_000).map(|i| tag(&format!("tag {i}"))).collect();
                let cursor = split.rest.chars().count();
                let _ = suggestions(&all, &split.add, &split.rest, cursor);
            },
        );
    }

    #[test]
    fn a_huge_single_name_is_refused_quickly_and_kept() {
        let long = "é".repeat(1 << 16);
        crate::testing::within(
            Duration::from_millis(500),
            "a 128 KiB tag name",
            move || {
                let text = format!("{long},b");
                let split = split_typed(&[], &text, true);
                assert_eq!(split.add, strings(&["b"]));
                assert_eq!(split.rest, long);
                assert_eq!(
                    check(&split.add, &split.rest).error.as_deref(),
                    Some("This tag is too long (the limit is 256 bytes)")
                );
                let _ = suggestions(&tags(&["éé"]), &[], &split.rest, 3);
            },
        );
    }

    #[test]
    fn keys_ignore_spelling_spacing_and_order() {
        let keys =
            |names: &[&str]| tag_keys(&names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>());
        assert_eq!(keys(&["Maths", "a"]), keys(&["a", "maths"]));
        assert_ne!(keys(&["a"]), keys(&["a", "b"]));
    }
}
