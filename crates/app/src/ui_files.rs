//! Tests that every Blueprint file in `data/` is bundled and loads with what
//! the code takes from it (#62), so a `.blp` file and its Rust disagreeing
//! fails `cargo test` instead of the app.
//!
//! A composite template is checked by building its widget. gtk4-rs checks
//! each `#[template_child]` as the widget is built (`init_template` then
//! `check_template_children`): a child whose id the file lacks, or whose
//! type differs, panics there. That code runs inside `GObject`'s instance
//! initialisation, a C callback the panic cannot unwind through, so it
//! aborts the test binary with the panic's message rather than failing
//! this one test. A file loaded with `ui::dialog::builder` is checked by
//! loading it through the code's own loader, which returns `None` when an
//! object it looks up is missing or of another type.
//!
//! [`CHECKS`] must list every `data/*.blp` file: a test fails when one is
//! added without a check.

use std::path::Path;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::dialogs::{browser_dialogs, card_editor, card_info, deck_name_dialog, preferences};
use crate::pages::{BrowserPage, DeckPage, DecksPage, RecoveryPage, ReviewPage, StatsPage};
use crate::ui::dialog;
use crate::widgets::{
    BackupRow, CardRow, DeckRow, HistoryRow, TagEntry, TagPills, TagSuggestions,
    chart_card::ChartCard,
};
use crate::window::RappelWindow;

/// Each `data/*.blp` file with how this test loads it.
const CHECKS: &[(&str, fn())] = &[
    ("backup_row.blp", template::<BackupRow>),
    ("browser_dialogs.blp", || {
        loads("choose_dialog", browser_dialogs::choose_layout().is_some());
        loads(
            "tag_name_dialog",
            browser_dialogs::tag_name_layout().is_some(),
        );
    }),
    ("browser_page.blp", template::<BrowserPage>),
    ("card_editor.blp", template::<card_editor::CardEditor>),
    ("card_info_dialog.blp", || {
        loads("card_info_dialog", card_info::Layout::load().is_some());
    }),
    ("card_row.blp", template::<CardRow>),
    ("chart_card.blp", template::<ChartCard>),
    ("deck_name_dialog.blp", || {
        loads("deck_name_dialog", deck_name_dialog::layout().is_some());
    }),
    ("deck_page.blp", template::<DeckPage>),
    ("deck_row.blp", template::<DeckRow>),
    ("decks_page.blp", template::<DecksPage>),
    ("delete_card_dialog.blp", || {
        loads("delete_card_dialog", dialog::DELETE_CARD.load().is_some());
    }),
    ("discard_dialog.blp", || {
        loads("discard_dialog", dialog::DISCARD_CHANGES.load().is_some());
    }),
    ("history_row.blp", template::<HistoryRow>),
    ("import_dialog.blp", || {
        loads("import_dialog", crate::transfer::confirm_layout().is_some());
    }),
    ("preferences_dialog.blp", || {
        loads("preferences_dialog", preferences::Rows::load().is_some());
    }),
    ("recovery_page.blp", template::<RecoveryPage>),
    ("restore_dialog.blp", || {
        loads("restore_dialog", dialog::RESTORE_BACKUP.load().is_some());
    }),
    ("retention_dialog.blp", || {
        loads(
            "retention_dialog",
            preferences::retention_layout().is_some(),
        );
    }),
    ("review_page.blp", template::<ReviewPage>),
    ("shortcuts-dialog.blp", || {
        loads(
            "shortcuts_dialog",
            crate::dialogs::shortcuts::load().is_some(),
        );
    }),
    ("start_fresh_dialog.blp", || {
        loads("start_fresh_dialog", dialog::START_FRESH.load().is_some());
    }),
    ("stats_page.blp", template::<StatsPage>),
    ("tag_entry.blp", template::<TagEntry>),
    ("tag_pills.blp", template::<TagPills>),
    ("tag_suggestions.blp", template::<TagSuggestions>),
    ("window.blp", template::<RappelWindow>),
];

/// Builds a `T`, which checks its template; a window is destroyed again,
/// so it does not stay in GTK's list of toplevels.
fn template<T: IsA<glib::Object> + IsA<gtk::Widget> + glib::object::IsClass>() {
    let widget: T = glib::Object::new();
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        window.destroy();
    }
}

/// Fails unless the loader of `what` found every object it looks up.
fn loads(what: &str, found: bool) {
    assert!(
        found,
        "{what}: an object its loader looks up with `ui::dialog::object` is missing from \
         the .blp file or of another type"
    );
}

/// The `.blp` files in `data/`, sorted.
fn blueprint_files() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "blp"))
        .collect();
    files.sort();
    files
}

#[test]
fn every_blueprint_file_has_a_check() {
    let mut checked: Vec<&str> = CHECKS.iter().map(|(file, _)| *file).collect();
    checked.sort_unstable();
    assert_eq!(
        blueprint_files(),
        checked,
        "CHECKS in ui_files.rs must list every data/*.blp file, and no other"
    );
}

#[test]
fn every_blueprint_file_is_bundled() {
    crate::testing::with_widgets(|| {
        for (file, _) in CHECKS {
            let ui = file.replace(".blp", ".ui");
            let path = format!("/io/github/rsmirke/Rappel/{ui}");
            assert!(
                gio::resources_lookup_data(&path, gio::ResourceLookupFlags::NONE).is_ok(),
                "{ui} is not in data/resources.gresource.xml"
            );
        }
    });
}

#[test]
fn every_blueprint_file_loads_with_what_the_code_takes_from_it() {
    crate::testing::with_widgets(|| {
        for (_, check) in CHECKS {
            check();
        }
    });
}
