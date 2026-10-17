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

/// Every item of the dialog as (title, accelerators).
fn items() -> Vec<(String, Vec<String>)> {
    let builder = dialog::builder(FILE);
    let _: adw::ShortcutsDialog = dialog::object(&builder, FILE, "shortcuts_dialog")
        .expect("AdwApplication needs the id shortcuts_dialog");
    builder
        .objects()
        .into_iter()
        .filter_map(|o| o.downcast::<adw::ShortcutsItem>().ok())
        .map(|item| {
            let accels = item
                .accelerator()
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            (item.title().to_string(), accels)
        })
        .collect()
}

#[test]
fn every_accelerator_parses_and_every_bound_key_is_listed() {
    crate::testing::with_gtk(|| {
        adw::init().unwrap();
        gio::resources_register_include!("rappel.gresource").unwrap();
        let items = items();
        assert!(items.len() >= 20, "the dialog lost its items");
        let mut listed = Vec::new();
        for (title, accels) in &items {
            assert!(!accels.is_empty(), "{title} has no accelerator");
            for accel in accels {
                listed.push(normalised(accel));
            }
        }
        let bound = crate::ACCELS
            .iter()
            .map(|(_, accel)| (*accel).to_owned())
            .chain(["F10", "<Control>question"].map(str::to_owned))
            .chain(crate::pages::review_key_triggers().map(str::to_owned))
            .chain(SAVE_KEYS.iter().map(|(trigger, _)| (*trigger).to_owned()))
            .chain(FORMAT_KEYS.iter().map(|(trigger, _)| (*trigger).to_owned()))
            .chain(tag_entry::KEYS.iter().map(|(key, _)| plain(*key)))
            .chain(tag_pills::REMOVE_KEYS.iter().map(|(key, _)| plain(*key)))
            .chain(bar_chart::KEYS.iter().map(|(key, _)| plain(*key)))
            .chain(browser_text::LIST_KEYS.iter().map(|&(key, ctrl, _)| {
                if ctrl {
                    format!("<Control>{}", plain(key))
                } else {
                    plain(key)
                }
            }))
            .chain([browser_text::TO_LIST_KEY, browser_text::ESCAPE_KEY].map(plain));
        for trigger in bound {
            let trigger = trigger.as_str();
            assert!(
                listed.contains(&normalised(trigger)),
                "{trigger} is bound but not in the Keyboard Shortcuts dialog"
            );
        }
    });
}
