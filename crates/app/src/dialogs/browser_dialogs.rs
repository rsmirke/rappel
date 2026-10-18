//! The two small dialogs of the All Cards page's bulk actions (ADR 0036):
//! choose one of a list (a deck to move cards to, a tag to take off them)
//! and ask for tags to add, in the card editor's tag entry, with its
//! suggestions and keys (ADR 0051). Layout in `data/browser_dialogs.blp`.

use adw::prelude::*;
use gtk::glib;
use rappel_core::domain::Tag;

use crate::ui::{dialog, dropdown};
use crate::widgets::TagEntry;
use crate::widgets::tag_entry::field::TagsCheck;

const FILE: &str = "browser_dialogs.ui";

/// The tags Add Tags would put on the cards: the field's names, if it has
/// at least one and no error. `None` keeps the Add button off.
pub fn tags_to_add(check: &TagsCheck) -> Option<&[String]> {
    (check.error.is_none() && !check.names.is_empty()).then_some(check.names.as_slice())
}

/// A builder over this file. Both dialogs are in it, and the builder makes
/// every object of the file, so the `RappelTagEntry` type of Add Tags must
/// be registered first even for the choice dialog: `GtkBuilder` fails on an
/// unknown type name ("Invalid object type").
fn builder() -> gtk::Builder {
    TagEntry::ensure_type();
    dialog::builder(FILE)
}

/// The choice dialog and its dropdown; `Err` with the first missing
/// object's id, after logging every missing one.
pub(crate) fn choose_layout() -> Result<(adw::AlertDialog, gtk::DropDown), &'static str> {
    let builder = builder();
    let (dialog, dropdown) = (
        dialog::object(&builder, FILE, "choose_dialog"),
        dialog::object(&builder, FILE, "choice_dropdown"),
    );
    Ok((dialog?, dropdown?))
}

/// The Add Tags dialog and its tag entry; `Err` with the first missing
/// object's id, after logging every missing one.
pub(crate) fn add_tags_layout() -> Result<(adw::AlertDialog, TagEntry), &'static str> {
    let builder = builder();
    let (dialog, entry) = (
        dialog::object(&builder, FILE, "add_tags_dialog"),
        dialog::object(&builder, FILE, "add_tags_entry"),
    );
    Ok((dialog?, entry?))
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
    let (dialog, dropdown) = choose_layout().ok()?;
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

/// Shows the Add Tags dialog over `parent`, with `body` and the tags
/// there are (`all`) as suggestions, and calls `on_add` with the names to
/// add (checked with the store's rules, at most
/// `field::MAX_TAGS_PER_CARD`) if the user chooses Add. Returns the dialog
/// (tests answer it).
pub fn add_tags(
    parent: &impl IsA<gtk::Widget>,
    body: &str,
    all: Vec<Tag>,
    on_add: impl Fn(Vec<String>) + 'static,
) -> Option<adw::AlertDialog> {
    let (dialog, entry) = add_tags_layout().ok()?;
    dialog.set_body(body);
    connect(&dialog, &entry, all);
    dialog.connect_response(
        None,
        glib::clone!(
            #[weak]
            entry,
            move |_, response| {
                if response == "confirm"
                    && let Some(names) = tags_to_add(&entry.check())
                {
                    on_add(names.to_vec());
                }
            }
        ),
    );
    dialog.present(Some(parent));
    Some(dialog)
}

/// Sets up the Add Tags dialog's entry: the suggestions, Enter on an empty
/// name for Add, Add enabled only with a tag to add, and the focus on the
/// entry.
fn connect(dialog: &adw::AlertDialog, entry: &TagEntry, all: Vec<Tag>) {
    entry.set_all_tags(all);
    // Enter in an empty name, after the last pill, is Add: the entry takes
    // Enter itself only while a name is typed or a suggestion is showing
    // (`keys::key_action`), and passes it on otherwise, so the text's
    // `activate` activates the dialog's default widget, the `confirm`
    // button (AdwAlertDialog:default-response, AdwDialog:default-widget:
    // "activated when the user presses Enter"). A disabled response's
    // button is insensitive and is not activated.
    entry.set_activates_default(true);
    let update = glib::clone!(
        #[weak]
        dialog,
        #[weak]
        entry,
        move || {
            dialog.set_response_enabled("confirm", tags_to_add(&entry.check()).is_some());
        }
    );
    update();
    entry.set_on_changed(update);
    dialog.set_focus(Some(&entry.text()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(names: &[&str], error: Option<&str>) -> TagsCheck {
        TagsCheck {
            names: names.iter().map(|&n| n.to_owned()).collect(),
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn add_is_offered_only_for_at_least_one_valid_tag() {
        assert_eq!(tags_to_add(&check(&[], None)), None);
        assert_eq!(
            tags_to_add(&check(&["maths", "exam"], None)),
            Some(&["maths".to_owned(), "exam".to_owned()][..])
        );
        // An error under the field (too long, past the limit) keeps Add off,
        // even with pills.
        assert_eq!(
            tags_to_add(&check(&["maths"], Some("This tag is too long"))),
            None
        );
    }
}
