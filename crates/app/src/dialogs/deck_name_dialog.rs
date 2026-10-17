//! The dialog that asks for a deck name, to create or rename a deck.
//! Layout in `data/deck_name_dialog.blp`.
//!
//! The name is checked as it is typed, with the core's own name key and
//! length limit, so an empty, duplicate or too long name disables the
//! confirm button and the last two are explained under the entry. The
//! store still has the final say.

use adw::prelude::*;
use gtk::glib;
use rappel_core::domain::{Deck, MAX_DECK_NAME_BYTES, deck_name_fits, deck_name_key};

use crate::ui::dialog;
use crate::ui::text::group_digits;

/// What the dialog is for.
#[derive(Debug, Clone)]
pub enum Purpose {
    Create,
    Rename(Deck),
}

/// Result of checking a typed name against the live decks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameCheck {
    /// Empty or only whitespace.
    Empty,
    /// Another live deck has this name, ignoring case and Unicode
    /// composition; holds that deck's name as shown.
    Duplicate(String),
    /// Longer than the store accepts (and an import reads back).
    TooLong,
    Valid,
}

/// Checks `input` against `decks`, skipping the deck with id `exclude`
/// (the deck being renamed). Same rule as the store (ADR 0010).
pub fn check_deck_name(input: &str, decks: &[Deck], exclude: Option<&str>) -> NameCheck {
    if input.trim().is_empty() {
        return NameCheck::Empty;
    }
    if !deck_name_fits(input) {
        return NameCheck::TooLong;
    }
    let key = deck_name_key(input);
    decks
        .iter()
        .filter(|deck| Some(deck.id.as_str()) != exclude)
        .find(|deck| deck_name_key(&deck.name) == key)
        .map_or(NameCheck::Valid, |deck| {
            NameCheck::Duplicate(deck.name.clone())
        })
}

/// The error shown under the entry, if any.
pub fn name_error(check: &NameCheck) -> Option<String> {
    match check {
        NameCheck::Duplicate(name) => Some(format!("A deck named “{name}” already exists")),
        NameCheck::TooLong => Some(format!(
            "The name is too long (the limit is {} bytes)",
            group_digits(MAX_DECK_NAME_BYTES)
        )),
        NameCheck::Empty | NameCheck::Valid => None,
    }
}

/// The dialog, its name entry and its error label, from
/// `data/deck_name_dialog.blp`; `None` after logging which is missing.
pub(crate) fn layout() -> Option<(adw::AlertDialog, adw::EntryRow, gtk::Label)> {
    const FILE: &str = "deck_name_dialog.ui";
    let builder = dialog::builder(FILE);
    Some((
        dialog::object(&builder, FILE, "deck_name_dialog")?,
        dialog::object(&builder, FILE, "name_entry")?,
        dialog::object(&builder, FILE, "error_label")?,
    ))
}

/// Shows the dialog over `parent` and returns the name typed, or `None`
/// if it was cancelled. `decks` are the live decks, for the duplicate check.
pub async fn ask(
    parent: &impl IsA<gtk::Widget>,
    purpose: Purpose,
    decks: Vec<Deck>,
) -> Option<String> {
    let (dialog, entry, error_label) = layout()?;

    let exclude = match &purpose {
        Purpose::Create => {
            dialog.set_heading(Some("New Deck"));
            None
        }
        Purpose::Rename(deck) => {
            dialog.set_heading(Some("Rename Deck"));
            dialog.set_response_label("confirm", "_Rename");
            entry.set_text(&deck.name);
            Some(deck.id.clone())
        }
    };
    // Screen readers read the error with the entry.
    // libadwaita-rs lists `Accessible` for `AdwEntryRow` only with its
    // `gtk_v4_10` feature, which we do not enable; every widget is one.
    entry
        .upcast_ref::<gtk::Widget>()
        .update_relation(&[gtk::accessible::Relation::DescribedBy(&[
            error_label.upcast_ref()
        ])]);

    let update = glib::clone!(
        #[weak]
        dialog,
        #[weak]
        error_label,
        move |entry: &adw::EntryRow| {
            let check = check_deck_name(&entry.text(), &decks, exclude.as_deref());
            show_check(&dialog, entry, &error_label, &check);
        }
    );
    update(&entry);
    entry.connect_changed(update);
    dialog.set_focus(Some(&entry));

    let response = dialog.choose_future(Some(parent)).await;
    (response == "confirm").then(|| entry.text().to_string())
}

/// Shows `check` in the dialog: the confirm button only for a valid name,
/// and the error under the entry for a duplicate or too long name.
fn show_check(
    dialog: &adw::AlertDialog,
    entry: &adw::EntryRow,
    error_label: &gtk::Label,
    check: &NameCheck,
) {
    dialog.set_response_enabled("confirm", *check == NameCheck::Valid);
    if let Some(message) = name_error(check) {
        if !error_label.is_visible() || error_label.label() != message {
            // Screen readers hear the error when it appears.
            entry
                .upcast_ref::<gtk::Widget>()
                .announce(&message, gtk::AccessibleAnnouncementPriority::Medium);
        }
        error_label.set_label(&message);
        error_label.set_visible(true);
        entry.add_css_class("error");
    } else {
        error_label.set_visible(false);
        entry.remove_css_class("error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck(id: &str, name: &str) -> Deck {
        Deck {
            id: id.to_owned(),
            name: name.to_owned(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn decks() -> Vec<Deck> {
        vec![deck("1", "Maths"), deck("2", "\u{C9}conomie")]
    }

    #[test]
    fn empty_and_blank_names_are_empty() {
        assert_eq!(check_deck_name("", &decks(), None), NameCheck::Empty);
        assert_eq!(check_deck_name(" \t ", &decks(), None), NameCheck::Empty);
    }

    #[test]
    fn duplicates_ignore_case_spaces_and_composition() {
        let dup = |name: &str| NameCheck::Duplicate(name.to_owned());
        assert_eq!(check_deck_name("maths", &decks(), None), dup("Maths"));
        assert_eq!(check_deck_name("  MATHS ", &decks(), None), dup("Maths"));
        // E + combining acute accent equals the precomposed É.
        assert_eq!(
            check_deck_name("e\u{301}conomie", &decks(), None),
            dup("\u{C9}conomie")
        );
    }

    #[test]
    fn accents_still_count() {
        assert_eq!(
            check_deck_name("Economie", &decks(), None),
            NameCheck::Valid
        );
    }

    #[test]
    fn rename_skips_the_deck_itself() {
        assert_eq!(
            check_deck_name("MATHS", &decks(), Some("1")),
            NameCheck::Valid
        );
        assert_eq!(
            check_deck_name("économie", &decks(), Some("1")),
            NameCheck::Duplicate("\u{C9}conomie".to_owned())
        );
    }

    #[test]
    fn names_over_the_limit_are_too_long() {
        let longest = "é".repeat(MAX_DECK_NAME_BYTES / 2);
        assert_eq!(check_deck_name(&longest, &decks(), None), NameCheck::Valid);
        // Bytes, not letters; the spaces around are not counted.
        assert_eq!(
            check_deck_name(&format!("  {longest}  "), &decks(), None),
            NameCheck::Valid
        );
        assert_eq!(
            check_deck_name(&format!("{longest}e"), &decks(), None),
            NameCheck::TooLong
        );
    }

    #[test]
    fn errors_explain_duplicates_and_long_names_only() {
        assert_eq!(
            name_error(&NameCheck::Duplicate("Maths".to_owned())).as_deref(),
            Some("A deck named “Maths” already exists")
        );
        assert_eq!(
            name_error(&NameCheck::TooLong).as_deref(),
            Some("The name is too long (the limit is 1,024 bytes)")
        );
        assert_eq!(name_error(&NameCheck::Empty), None);
        assert_eq!(name_error(&NameCheck::Valid), None);
    }
}
