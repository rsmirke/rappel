# 39. Keyboard shortcuts, About, and the remembered window

- Status: Accepted
- Date: 2026-10-17
- Amends: ADR 0002 (the libadwaita floor)
- Amended by: ADR 0046 (the shortcuts test covers every key table), ADR 0048 (the window state is written in one step with mode 0600, and a wider existing file is narrowed)
- Refers to: SPEC §4.8 (phase 1, missing features), ADR 0004 (data directories), ADR 0021 and 0023 (the KaTeX fonts), ADR 0027 (settings), #53

## Context

Phase 1 step 2 of the roadmap (SPEC §4) adds three things:
- a keyboard shortcuts dialog (#53);
- an About dialog that credits the bundled KaTeX fonts (SIL OFL-1.1) and the main libraries;
- remembering the window size and the last deck across restarts.

## Decision

**libadwaita 1.8 is the new floor (GTK 4.20 with it).** The current GNOME shortcuts dialog is `AdwShortcutsDialog`, new in libadwaita 1.8. The older `GtkShortcutsWindow` is deprecated since GTK 4.18. libadwaita 1.8 needs GTK ≥ 4.19.4, so 4.20 in practice. Ubuntu 26.04, which CI uses, and Fedora 43 ship it, as do GNOME 49+ Flatpak runtimes. The cargo feature becomes `v1_8`. The gtk4 feature stays `v4_14`: no newer GTK call is used.

**The shortcuts dialog is a resource with no code.** `data/shortcuts-dialog.blp` defines an `AdwShortcutsDialog` with the id `shortcuts_dialog`. `AdwApplication` finds it at `shortcuts-dialog.ui` under the resource base path and sets up `app.shortcuts` and Ctrl+? itself (AdwApplication docs, "Shortcuts Dialog"). It lists every shortcut the app has, grouped by screen. The key tables in the code are named constants: `main.rs`'s `ACCELS`, the review page's `KEYS`, and the card editor's `SAVE_KEYS` and `FORMAT_KEYS`. A test checks that each key in them appears in the dialog and that every accelerator in the dialog parses. A key added later to one of those tables without a line in the dialog fails the gate. The keys handled in inline `match`es (All Cards, the tag suggestions, the statistics charts) are not checked by the test, so a change there needs the dialog updated by hand.

**Key hints in menus.**
- Preferences and Keyboard Shortcuts show their keys from the application's accelerators.
- The review page's Card Menu shows I, S and Delete through the menu items' `accel` attribute. The page handles those keys with `CallbackAction`s, not actions, because they carry guards: S must have been on screen a moment (ADR 0037), and nothing reacts while the editor or a question is open. A menu cannot see such callbacks.
- GTK reads the `accel` attribute before asking the action muxer (`gtk_menu_tracker_item_get_accel`). `GtkModelButton` also binds the key, but in managed scope, so it works only while the menu is open, where it acts like a click (`gtkmodelbutton.c`, `update_accel`). A test checks the hints match the page's keys and that GTK's menu shows them.

**About is built in code**, from constants (`dialogs/about.rs`):
- version from Cargo, GPL-3.0-or-later (`GTK_LICENSE_GPL_3_0`, the "or later" one), and the repository as the website and issue tracker;
- a "Built With" credit section for GTK, libadwaita, GtkSourceView, gtk-rs, RaTeX, SQLite and rusqlite, FSRS, and pulldown-cmark;
- a legal section for the KaTeX fonts, with the copyright from the fonts' own name table (Design Science, Khan Academy) and the OFL-1.1.

Both legal strings are shown as label markup by libadwaita, so they must stay valid markup; a test checks this. The links open in the browser only when clicked, and Rappel itself fetches nothing.

**The window state is a key file in the data directory, not GSettings.** It lives in `<data dir>/window-state.ini` (`glib::KeyFile`) and holds width, height, maximised and the open deck's id. GSettings was rejected for four reasons:
1. dconf is one store per user, so a debug build would read and write the real app's state. That breaks the separation ADR 0004 and CLAUDE.md demand. A file follows `RAPPEL_DATA_DIR` and the debug directory by itself.
2. GSettings needs an installed, compiled schema, or `g_settings_new` aborts. Every `cargo run` and test run would need a set-up step.
3. The state belongs to one device. It does not go into the database's `settings` table (ADR 0027), which is exported, imported and backed up.
4. No new dependency.

The file is written with mode `0600`, like the database, and atomically by `g_key_file_save_to_file` (through `g_file_set_contents`) when the window closes from the main page. A missing, unreadable or nonsensical value takes its default:
- a size outside 360 × 294 to 16,384 px;
- a deck id that is empty or over 64 bytes.

A failed save is logged at warning level. None of this can stop the app.

**Which deck is reopened.** The deck whose page was in the navigation stack when the window closed, the lowest one. So a review, statistics or All Cards page above a deck page reopens that deck page; a review session is never resumed. Closing on the deck list reopens nothing. At startup the deck page is pushed without the slide animation, before the window is shown. A deck deleted since leaves the deck list showing.

## Consequences

- Building needs libadwaita ≥ 1.8 and GTK ≥ 4.20 development files (README).
- A new shortcut needs a line in `data/shortcuts-dialog.blp`; for the named tables the test fails otherwise.
- `window-state.ini` is not in backups or exports, so a restore or import keeps the device's window as it was.
- The state is only restored at startup, not after a restore from the recovery page.
