//! Tags: flat labels on cards, unique by their name key (ADR 0029).

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use super::{Card, has_nul, name_key};
use crate::error::{Error, Result};

/// The longest tag name, in UTF-8 bytes once trimmed.
pub const MAX_TAG_NAME_BYTES: usize = 256;

/// The most tags the editor's field lets a card have (ADR 0050, row 30),
/// and the most an Anki import keeps from a note (ADR 0061). The store has
/// no limit: a card imported from a `.rappel` file keeps all its tags.
pub const MAX_TAGS_PER_CARD: usize = 100;

/// A tag. Tags are never deleted: one no card uses any more stays, so the
/// same name later gets the same id back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Tag id (`UUIDv7` string).
    pub id: String,
    /// The name, trimmed, unique among tags by its
    /// [`name_key`](super::name_key).
    pub name: String,
}

/// A card with its tags, ordered by name key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedCard {
    /// The card.
    pub card: Card,
    /// Its tags.
    pub tags: Vec<Tag>,
}

/// The key suggestions are matched by: `name` without accents and in lower
/// case ("Économie", "economie" and "ECONOMIE" share one). Letters that do
/// not decompose into a base and a mark (ß, ø, ł) are left as they are.
/// This is for finding tags while typing only; whether two names are one tag
/// is decided by [`name_key`] (ADR 0029), which keeps accents.
#[must_use]
pub fn search_key(name: &str) -> String {
    name.trim()
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

/// A name in a tags field that cannot be a tag name.
#[derive(Debug, thiserror::Error)]
#[error("tag {position}: {error}")]
pub struct TagFieldError {
    /// Which name, counting from 1 in the order typed.
    pub position: usize,
    /// What is wrong with it.
    pub error: Error,
}

/// The tag names in `text`, a comma-separated list (SPEC §4.4): each trimmed
/// and checked by [`check_tag_name`], names equal by [`name_key`] merged
/// (the first spelling is kept), in the order typed. An empty piece after
/// the last comma is ignored, since "maths, " is what is left after typing a
/// comma or accepting a suggestion; any other empty piece is an error, so
/// "a,,b" is refused. Blank text gives no names.
///
/// # Errors
/// [`TagFieldError`] for the first name that is not valid.
pub fn parse_tag_field(text: &str) -> std::result::Result<Vec<String>, TagFieldError> {
    let pieces: Vec<&str> = text.split(',').collect();
    let last = pieces.len() - 1;
    let mut names: Vec<String> = Vec::new();
    // A set: a pasted list of thousands of names stays linear (ADR 0050).
    let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, piece) in pieces.into_iter().enumerate() {
        if i == last && piece.trim().is_empty() {
            break;
        }
        let name = check_tag_name(piece).map_err(|error| TagFieldError {
            position: i + 1,
            error,
        })?;
        if keys.insert(name_key(name)) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// `name` trimmed, if it can be a tag name: not empty, at most
/// [`MAX_TAG_NAME_BYTES`], no NUL and no comma. In the card editor's tags
/// field a comma ends a name and makes it a pill (SPEC §4.4, ADR 0043), and
/// [`parse_tag_field`] reads a comma-separated list, so a comma inside a
/// name could not be typed.
///
/// # Errors
/// [`Error::EmptyTagName`], [`Error::TagNameTooLong`],
/// [`Error::TagNameHasComma`], [`Error::ContainsNul`].
pub fn check_tag_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        Err(Error::EmptyTagName)
    } else if has_nul(name) {
        Err(Error::ContainsNul)
    } else if name.contains(',') {
        Err(Error::TagNameHasComma)
    } else if name.len() > MAX_TAG_NAME_BYTES {
        Err(Error::TagNameTooLong {
            limit: MAX_TAG_NAME_BYTES,
        })
    } else {
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_names_are_trimmed_and_checked() {
        assert_eq!(check_tag_name("  maths \n").unwrap(), "maths");
        assert!(matches!(check_tag_name(" \t"), Err(Error::EmptyTagName)));
        assert!(matches!(check_tag_name("a,b"), Err(Error::TagNameHasComma)));
        assert!(matches!(check_tag_name("a\0b"), Err(Error::ContainsNul)));
        let longest = "é".repeat(MAX_TAG_NAME_BYTES / 2);
        assert_eq!(check_tag_name(&longest).unwrap(), longest);
        assert!(matches!(
            check_tag_name(&format!("{longest}x")),
            Err(Error::TagNameTooLong { limit: 256 })
        ));
        // Trimmed before measuring.
        assert!(check_tag_name(&format!(" {longest} ")).is_ok());
    }

    #[test]
    fn search_keys_ignore_accents_and_case() {
        for same in ["Économie", "economie", "ECONOMIE", "E\u{301}conomie "] {
            assert_eq!(search_key(same), "economie");
        }
        assert_eq!(search_key("Ça va"), "ca va");
        assert_ne!(search_key("ß"), search_key("ss"));
    }

    fn field(text: &str) -> Vec<String> {
        parse_tag_field(text).unwrap()
    }

    #[test]
    fn a_tags_field_is_a_comma_separated_list() {
        assert_eq!(field(""), Vec::<String>::new());
        assert_eq!(field("  \n"), Vec::<String>::new());
        assert_eq!(field("maths"), ["maths"]);
        assert_eq!(field(" maths ,algebra"), ["maths", "algebra"]);
        // What typing a comma or accepting a suggestion leaves behind.
        assert_eq!(field("maths, "), ["maths"]);
        assert_eq!(field("maths,"), ["maths"]);
        // The same tag twice is one; the first spelling stays.
        assert_eq!(field("Maths, a, maths, MATHS"), ["Maths", "a"]);
    }

    #[test]
    fn a_tags_field_names_the_first_invalid_name() {
        let position = |text| parse_tag_field(text).unwrap_err().position;
        assert_eq!(position("a,,b"), 2);
        assert_eq!(position(",a"), 1);
        assert_eq!(position(","), 1);
        assert_eq!(position("a, b, \u{0}"), 3);
        let long = "x".repeat(MAX_TAG_NAME_BYTES + 1);
        let err = parse_tag_field(&format!("a, {long}")).unwrap_err();
        assert_eq!(err.position, 2);
        assert!(matches!(err.error, Error::TagNameTooLong { limit: 256 }));
        assert!(matches!(
            parse_tag_field("a,,b").unwrap_err().error,
            Error::EmptyTagName
        ));
    }
}
