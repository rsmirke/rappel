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
//! loading it through the code's own loader, which returns `Err` with the
//! id of an object it looks up that is missing or of another type.
//!
//! [`CHECKS`] must list every `data/*.blp` file: a test fails when one is
//! added without a check.

use std::path::Path;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::dialogs::{browser_dialogs, card_editor, card_info, deck_name_dialog, preferences};
use crate::pages::{
    BrowserPage, DeckPage, DecksPage, RecoveryPage, ReviewPage, SettingPage, StatsPage,
};
use crate::ui::dialog;
use crate::widgets::{
    BackupRow, CardRow, DeckRow, HistoryRow, TagEntry, TagPills, TagSuggestions,
    chart_card::ChartCard,
};
use crate::window::RappelWindow;

/// Each `data/*.blp` file with how this test loads it.
const CHECKS: &[(&str, Load)] = &[
    ("backup_row.blp", Load::Template(template::<BackupRow>)),
    (
        "browser_dialogs.blp",
        Load::Plain(|| {
            loads("choose_dialog", browser_dialogs::choose_layout().err());
            loads("add_tags_dialog", browser_dialogs::add_tags_layout().err());
        }),
    ),
    ("browser_page.blp", Load::Template(template::<BrowserPage>)),
    (
        "card_editor.blp",
        Load::Template(template::<card_editor::CardEditor>),
    ),
    (
        "card_info_dialog.blp",
        Load::Plain(|| {
            loads("card_info_dialog", card_info::Layout::load().err());
        }),
    ),
    ("card_row.blp", Load::Template(template::<CardRow>)),
    ("chart_card.blp", Load::Template(template::<ChartCard>)),
    (
        "deck_name_dialog.blp",
        Load::Plain(|| {
            loads("deck_name_dialog", deck_name_dialog::layout().err());
        }),
    ),
    ("deck_page.blp", Load::Template(template::<DeckPage>)),
    ("deck_row.blp", Load::Template(template::<DeckRow>)),
    ("decks_page.blp", Load::Template(template::<DecksPage>)),
    (
        "clean_up_media_dialog.blp",
        Load::Plain(|| {
            loads("clean_up_media_dialog", dialog::CLEAN_UP_MEDIA.load().err());
        }),
    ),
    (
        "delete_card_dialog.blp",
        Load::Plain(|| {
            loads("delete_card_dialog", dialog::DELETE_CARD.load().err());
        }),
    ),
    (
        "discard_dialog.blp",
        Load::Plain(|| {
            loads("discard_dialog", dialog::DISCARD_CHANGES.load().err());
        }),
    ),
    ("history_row.blp", Load::Template(template::<HistoryRow>)),
    (
        "image_dialog.blp",
        Load::Plain(|| {
            loads("image_dialog", crate::render::full_size_layout().err());
        }),
    ),
    (
        "import_dialog.blp",
        Load::Plain(|| {
            loads("import_dialog", crate::transfer::confirm_layout().err());
        }),
    ),
    (
        "preferences_dialog.blp",
        Load::Plain(|| {
            loads("preferences_dialog", preferences::Rows::load().err());
        }),
    ),
    (
        "recovery_page.blp",
        Load::Template(template::<RecoveryPage>),
    ),
    (
        "restore_dialog.blp",
        Load::Plain(|| {
            loads("restore_dialog", dialog::RESTORE_BACKUP.load().err());
        }),
    ),
    (
        "retention_dialog.blp",
        Load::Plain(|| {
            loads("retention_dialog", preferences::retention_layout().err());
        }),
    ),
    ("review_page.blp", Load::Template(template::<ReviewPage>)),
    ("setting_page.blp", Load::Template(template::<SettingPage>)),
    (
        "shortcuts-dialog.blp",
        Load::Plain(|| {
            loads("shortcuts_dialog", crate::dialogs::shortcuts::load().err());
        }),
    ),
    (
        "start_fresh_dialog.blp",
        Load::Plain(|| {
            loads("start_fresh_dialog", dialog::START_FRESH.load().err());
        }),
    ),
    ("stats_page.blp", Load::Template(template::<StatsPage>)),
    ("tag_entry.blp", Load::Template(template::<TagEntry>)),
    ("tag_pills.blp", Load::Template(template::<TagPills>)),
    (
        "tag_suggestions.blp",
        Load::Template(template::<TagSuggestions>),
    ),
    ("window.blp", Load::Template(template::<RappelWindow>)),
];

/// How a file is loaded.
#[derive(Clone, Copy)]
enum Load {
    /// A composite template, checked by building its widget, which
    /// registers the types it names.
    Template(fn()),
    /// A plain layout, loaded with `ui::dialog::builder` by the code's own
    /// loader; nothing registers the types it names but
    /// `widgets::register_types`.
    Plain(fn()),
}

impl Load {
    fn run(self) {
        match self {
            Self::Template(check) | Self::Plain(check) => check(),
        }
    }
}

/// Builds a `T`, which checks its template; a window is destroyed again,
/// so it does not stay in GTK's list of toplevels.
fn template<T: IsA<glib::Object> + IsA<gtk::Widget> + glib::object::IsClass>() {
    let widget: T = glib::Object::new();
    if let Some(window) = widget.dynamic_cast_ref::<gtk::Window>() {
        window.destroy();
    }
}

/// Fails if the loader of `what` missed an object: `missing` is the id of
/// the first one it could not find with the type the code expects.
fn loads(what: &str, missing: Option<&'static str>) {
    if let Some(id) = missing {
        panic!("{what}: the .blp file has no object `{id}` of the type the code expects");
    }
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
            let path = dialog::resource_path(&ui);
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
        for (_, load) in CHECKS {
            load.run();
        }
    });
}

/// Set for the child process of `plain_layouts_load_in_a_fresh_process`.
const FRESH_PROCESS: &str = "RAPPEL_TEST_FRESH_PROCESS";

/// The plain layouts load in a process where nothing has built a widget
/// yet. In the test binary a type is registered once for all tests, so a
/// layout that names a custom widget (`$RappelTagEntry` in
/// `browser_dialogs.blp`) loads after any test built that widget, even if
/// its loader registers nothing; in the app it aborted the process
/// ("Invalid object type"). So this runs the check below in a new process
/// of this test binary, alone, and fails if it does not pass: a `GtkBuilder`
/// error is a `g_error`, which aborts that process.
#[test]
fn plain_layouts_load_in_a_fresh_process() {
    if std::env::var_os(FRESH_PROCESS).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("the test binary has a path");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "ui_files::plain_layouts_in_a_new_process",
            "--test-threads=1",
        ])
        .env(FRESH_PROCESS, "1")
        .output()
        .expect("the test binary runs again");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "the plain layouts failed in a fresh process ({}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The child of `plain_layouts_load_in_a_fresh_process`: loads every plain
/// layout, and no template first. Does nothing in a normal run.
#[test]
fn plain_layouts_in_a_new_process() {
    if std::env::var_os(FRESH_PROCESS).is_none() {
        return;
    }
    crate::testing::with_widgets(|| {
        for (_, load) in CHECKS {
            if let Load::Plain(check) = load {
                check();
            }
        }
    });
}
