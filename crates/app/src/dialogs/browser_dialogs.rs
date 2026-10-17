//! The two small dialogs of the All Cards page's bulk actions (ADR 0036):
//! choose one of a list (a deck to move cards to, a tag to take off them)
//! and ask for the name of a tag to add. Layout in
//! `data/browser_dialogs.blp`.

use adw::prelude::*;
use gtk::glib;
use rappel_core::Error;
use rappel_core::domain::{MAX_TAG_NAME_BYTES, check_tag_name};

use crate::ui::text::group_digits;
use crate::ui::{dialog, dropdown};

const FILE: &str = "browser_dialogs.ui";

/// Result of checking a typed tag name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagNameCheck {
    /// Nothing typed yet: no error, but nothing to confirm.
    Empty,
    /// Not a tag name; holds the explanation.
    Invalid(String),
    /// A name the store accepts (trimmed).
    Valid(String),
}

/// Checks `input` with the store's rule ([`check_tag_name`]).
pub fn check_tag_input(input: &str) -> TagNameCheck {
    match check_tag_name(input) {
        Ok(name) => TagNameCheck::Valid(name.to_owned()),
        Err(Error::EmptyTagName) => TagNameCheck::Empty,
        Err(Error::TagNameTooLong { .. }) => TagNameCheck::Invalid(format!(
            "The name is too long (the limit is {} bytes)",
            group_digits(MAX_TAG_NAME_BYTES)
        )),
        Err(Error::TagNameHasComma) => {
            TagNameCheck::Invalid("A tag name cannot contain a comma".to_owned())
        }
        Err(Error::ContainsNul) => {
            TagNameCheck::Invalid("The name contains a null character".to_owned())
        }
        Err(other) => TagNameCheck::Invalid(other.to_string()),
    }
}

/// The choice dialog and its dropdown; `None` after logging which is
/// missing.
pub(crate) fn choose_layout() -> Option<(adw::AlertDialog, gtk::DropDown)> {
    let builder = dialog::builder(FILE);
    Some((
        dialog::object(&builder, FILE, "choose_dialog")?,
        dialog::object(&builder, FILE, "choice_dropdown")?,
    ))
}

/// The Add Tag dialog, its entry and its error label; `None` after logging
/// which is missing.
pub(crate) fn tag_name_layout() -> Option<(adw::AlertDialog, adw::EntryRow, gtk::Label)> {
    let builder = dialog::builder(FILE);
    Some((
        dialog::object(&builder, FILE, "tag_name_dialog")?,
        dialog::object(&builder, FILE, "tag_entry")?,
        dialog::object(&builder, FILE, "error_label")?,
    ))
}

/// Shows a list of `labels` over `parent` and returns the index chosen, or
/// `None` if cancelled. `labels` are the user's own names: the dropdown
/// shows them as plain text.
pub async fn choose(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    confirm_label: &str,
    labels: &[String],
) -> Option<usize> {
    let (dialog, dropdown) = choose_layout()?;
    dialog.set_heading(Some(heading));
    dialog.set_body(body);
    dialog.set_response_label("confirm", confirm_label);
    dropdown::set_labels(&dropdown, labels);
    dropdown.update_property(&[gtk::accessible::Property::Label(heading)]);
    dialog.set_focus(Some(&dropdown));

    let response = dialog.choose_future(Some(parent)).await;
    (response == "confirm")
        .then(|| usize::try_from(dropdown.selected()).ok())
        .flatten()
}

/// Shows the Add Tag dialog over `parent` and returns the name typed
/// (trimmed), or `None` if cancelled.
pub async fn ask_tag_name(parent: &impl IsA<gtk::Widget>) -> Option<String> {
    let (dialog, entry, error_label) = tag_name_layout()?;
    // Screen readers read the error with the entry (see deck_name_dialog).
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
            let check = check_tag_input(&entry.text());
            dialog.set_response_enabled("confirm", matches!(check, TagNameCheck::Valid(_)));
            match check {
                TagNameCheck::Invalid(message) => {
                    if error_label.label() != message || !error_label.is_visible() {
                        entry
                            .upcast_ref::<gtk::Widget>()
                            .announce(&message, gtk::AccessibleAnnouncementPriority::Medium);
                    }
                    error_label.set_label(&message);
                    error_label.set_visible(true);
                }
                TagNameCheck::Empty | TagNameCheck::Valid(_) => error_label.set_visible(false),
            }
        }
    );
    update(&entry);
    entry.connect_changed(update);
    dialog.set_focus(Some(&entry));

    let response = dialog.choose_future(Some(parent)).await;
    if response != "confirm" {
        return None;
    }
    match check_tag_input(&entry.text()) {
        TagNameCheck::Valid(name) => Some(name),
        TagNameCheck::Empty | TagNameCheck::Invalid(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_name_is_checked_with_the_stores_rule() {
        assert_eq!(check_tag_input(""), TagNameCheck::Empty);
        assert_eq!(check_tag_input("  \n"), TagNameCheck::Empty);
        assert_eq!(
            check_tag_input("  maths "),
            TagNameCheck::Valid("maths".to_owned())
        );
        assert!(matches!(check_tag_input("a,b"), TagNameCheck::Invalid(_)));
        assert!(matches!(check_tag_input("a\0b"), TagNameCheck::Invalid(_)));
        assert!(matches!(
            check_tag_input(&"x".repeat(MAX_TAG_NAME_BYTES + 1)),
            TagNameCheck::Invalid(m) if m.contains("256")
        ));
    }
}
