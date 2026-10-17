//! Tests of the Keyboard Shortcuts dialog (`data/shortcuts-dialog.blp`).
//! It has no code: `AdwApplication` loads it and sets up `app.shortcuts`
//! and Ctrl+? itself. These tests keep it honest: every accelerator in it
//! parses, and every key the code binds is listed: the application's
//! accelerators, the review page's keys, the card editor's save and format
//! keys, the tag entry's and the tag pills' keys, the All Cards list's keys
//! and the charts' keys, each from its named table.

use adw::prelude::*;
use gtk::gio;

use crate::dialogs::card_editor::{FORMAT_KEYS, SAVE_KEYS};
use crate::pages::browser_text;
use crate::ui::dialog;
use crate::widgets::{bar_chart, tag_entry, tag_pills};

const FILE: &str = "shortcuts-dialog.ui";

/// A key and its modifiers, the way GTK compares accelerators.
type Accel = (gtk::gdk::Key, gtk::gdk::ModifierType);

/// The main-keyboard key a keypad key stands for (`KP_1` is `1`,
/// `KP_Left` is `Left`), and `ISO_Enter` for Return; other keys as they are.
fn main_key(key: gtk::gdk::Key) -> gtk::gdk::Key {
    use gtk::gdk::Key;
    match key {
        Key::KP_Enter | Key::ISO_Enter => Key::Return,
        Key::KP_Space => Key::space,
        other => other
            .name()
            .and_then(|name| name.strip_prefix("KP_").and_then(Key::from_name))
            .unwrap_or(other),
    }
}

/// `trigger` as an accelerator, ignoring Shift on a letter (the review
/// page binds `e` and `<Shift>e` alike) and the keypad (`KP_1` is `1`).
fn normalised(trigger: &str) -> Accel {
    let (key, mods) = gtk::accelerator_parse(trigger)
        .unwrap_or_else(|| panic!("{trigger:?} is not an accelerator"));
    let key = main_key(key).to_lower();
    let mods = if key.to_unicode().is_some_and(char::is_alphabetic)
        && !mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
    {
        mods - gtk::gdk::ModifierType::SHIFT_MASK
    } else {
        mods
    };
    (key, mods)
}

/// `key` without modifiers, as a trigger string.
fn plain(key: gtk::gdk::Key) -> String {
    key.name()
        .unwrap_or_else(|| panic!("{key:?} has no name"))
        .to_string()
}

/// The dialog, under the id `AdwApplication` looks for ("a resource
/// located at `shortcuts-dialog.ui` which defines an `AdwShortcutsDialog`
/// with the ID `shortcuts_dialog`", libadwaita docs, `AdwApplication`),
/// with the builder that holds the rest of the file; `Err` with the id
/// after logging that it is missing.
pub(crate) fn load() -> Result<(gtk::Builder, adw::ShortcutsDialog), &'static str> {
    let builder = dialog::builder(FILE);
    let dialog = dialog::object(&builder, FILE, "shortcuts_dialog")?;
    Ok((builder, dialog))
}

/// The items of one section of the dialog, as (title, accelerators).
type Items = Vec<(String, Vec<String>)>;

/// Every section of the dialog by title, with its items. An
/// `AdwShortcutsSection` is a `GListModel` of its `AdwShortcutsItem`s
/// (libadwaita docs, `AdwShortcutsSection`).
fn sections() -> Vec<(String, Items)> {
    let (builder, _) = load().expect("AdwApplication needs the id shortcuts_dialog");
    builder
        .objects()
        .into_iter()
        .filter_map(|o| o.downcast::<adw::ShortcutsSection>().ok())
        .map(|section| {
            let items = section
                .upcast_ref::<gio::ListModel>()
                .iter::<adw::ShortcutsItem>()
                .map(|item| {
                    let item = item.expect("the section is not changed while it is read");
                    let accels = item
                        .accelerator()
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect();
                    (item.title().to_string(), accels)
                })
                .collect();
            (section.title().unwrap_or_default().to_string(), items)
        })
        .collect()
}

/// `key`, with Ctrl when `ctrl`, as a trigger string.
fn trigger(key: gtk::gdk::Key, ctrl: bool) -> String {
    if ctrl {
        format!("<Control>{}", plain(key))
    } else {
        plain(key)
    }
}

/// Every key the code binds, by the section of the dialog that must list
/// it.
fn bound_keys() -> Vec<(&'static str, Vec<String>)> {
    vec![
        (
            "General",
            crate::ACCELS
                .iter()
                .map(|(_, accel)| (*accel).to_owned())
                .chain(["F10", "<Control>question"].map(str::to_owned))
                .collect(),
        ),
        (
            "Studying",
            crate::pages::review_key_triggers()
                .map(str::to_owned)
                .collect(),
        ),
        (
            "Card Editor",
            SAVE_KEYS
                .iter()
                .map(|(trigger, _)| (*trigger).to_owned())
                .chain(FORMAT_KEYS.iter().map(|(trigger, _)| (*trigger).to_owned()))
                .collect(),
        ),
        (
            "Tags",
            tag_entry::KEYS
                .iter()
                .map(|(key, _)| plain(*key))
                .chain(tag_pills::REMOVE_KEYS.iter().map(|(key, _)| plain(*key)))
                .collect(),
        ),
        (
            "All Cards",
            browser_text::LIST_KEYS
                .iter()
                .map(|&(key, ctrl, _)| trigger(key, ctrl))
                .chain([browser_text::TO_LIST_KEY, browser_text::ESCAPE_KEY].map(plain))
                .collect(),
        ),
        (
            "Statistics",
            bar_chart::KEYS.iter().map(|(key, _)| plain(*key)).collect(),
        ),
    ]
}

#[test]
fn every_accelerator_parses_and_every_bound_key_is_listed_in_its_section() {
    crate::testing::with_widgets(|| {
        let sections = sections();
        let count: usize = sections.iter().map(|(_, items)| items.len()).sum();
        assert!(count >= 20, "the dialog lost its items");
        for (section, keys) in bound_keys() {
            let Some((_, items)) = sections.iter().find(|(title, _)| title == section) else {
                panic!("the Keyboard Shortcuts dialog has no section {section}");
            };
            let mut listed = Vec::new();
            for (title, accels) in items {
                assert!(!accels.is_empty(), "{title} has no accelerator");
                listed.extend(accels.iter().map(|accel| normalised(accel)));
            }
            for trigger in &keys {
                assert!(
                    listed.contains(&normalised(trigger)),
                    "{trigger} is bound but not in the {section} section of Keyboard Shortcuts"
                );
            }
        }
        // Every section is checked against the code.
        for (title, _) in &sections {
            assert!(
                bound_keys().iter().any(|(section, _)| section == title),
                "section {title} is not checked against any key table"
            );
        }
    });
}
