# 46. Dialogs, problem pages and how the app writes

- Status: Accepted
- Date: 2026-10-17
- Amends: ADR 0027 (Preferences), ADR 0032 (Statistics' empty state), ADR 0039 (the shortcuts test covers every key table)
- Refers to: ADR 0016, 0026 (recovery), `docs/briefs/phase-1-finish.md` ("Dialogs", "Design rules")

## Context

The last slice of the phase 1 UI overhaul is a pass over the screens no earlier slice covered: Preferences, Card Information, About, Keyboard Shortcuts, every alert dialog, Statistics and the startup problem pages, and the wording of toasts. The brief asks for standard libadwaita dialogs, Header Capitalisation for titles and buttons, sentence case for descriptions, "…" for actions that need more input, plain words, spacing in multiples of 6 and empty states with one pill button. The reviewer of #71 also asked that the keys of All Cards, the tag suggestions and the charts move into named tables, so the Keyboard Shortcuts test covers them.

## Decision

**Punctuation.** One rule for the whole app, applied where texts differed:
- Titles, headings, buttons, menu items and shortcut names use Header Capitalisation (as before).
- Alert dialog bodies are prose and end with a full stop, even when they are one sentence ("The text you typed will be lost.").
- Row subtitles, group descriptions, status page descriptions and toasts that are one sentence have no full stop ("Limits count for each deck separately"); two sentences keep both stops. Where a subtitle had two short sentences, they became one with a semicolon, so a row reads as one phrase.
- Percentages are written "90%" everywhere (`ui::text::percent`); the retention row said "90 %".

**Preferences.** Already an `AdwPreferencesDialog` with an `AdwButtonRow` for Open Backup Folder. The Data group now says when backups are made ("each day it opens, and before every import"), which is what the folder holds. The retention row's value is `dimmed numeric`. The retention question no longer repeats the row's "higher means more reviews".

**Card Information.** The rows stay libadwaita `property` rows (the title above the value, as in Files' Properties): they keep long values such as "09/09/26 15:50 · overdue by 38 days" readable at 360 px, where a value beside its title would wrap. Rows with figures or dates, the history rows and the backup rows have `numeric`, so the digits line up.

**Statistics.** The empty state gets the brief's single pill button: **Back to Deck** or **Back to Decks**, whichever page opened it (`text::back_label`), through `navigation.pop` (an action `AdwNavigationView` defines), and the focus goes to it when the page is shown. A button that starts studying was considered, but it would have to build a session from the statistics page for a scope (all decks) that has no study action. The icon is `document-open-recent-symbolic` (a clock: the history that is not there yet), replacing `view-continuous-symbolic`, which reads as a layout control. The headline values and the charts' scale, day labels and caption are `numeric`; the chart's inner spacing is 6 px (was 4 and 4 + 4).

**Alert dialogs.** Bodies became full sentences; the bulk Move and Remove Tag dialogs say "Choose the deck to move 3 cards to." instead of ending on a colon; the restore question's heading is "Restore This Backup?" with the date in the body, so the heading stays short at 360 px. Destructive style stays on Delete, Discard, Restore and Start Fresh: each replaces or removes what the user sees now, even where the old file is kept. Cancel (or Keep Editing) is the default and close response wherever the other answer is destructive.

**Problem pages.** On the "too new" and "could not start" pages the selectable path label is the only focusable widget, so GTK gave it the focus when the window was shown and, with `gtk-label-select-on-focus` on (the default), selected all of it: the page opened with its path highlighted. `ui::focus::unselect_on_first_focus` drops that first selection in an idle; Tab later selects it as GTK does. The recovery page's text for "no backups" is a pure, tested function and no longer says "Do not copy files there by hand", which sounded like a warning about something the user had not done.

**`dim-label` → `dimmed`.** libadwaita 1.7 renamed the class; every use is now `dimmed`.

**Keyboard Shortcuts.** The tag entry (`widgets::tag_entry::KEYS`), the tag pills (`REMOVE_KEYS`), All Cards (`LIST_KEYS`, `TO_LIST_KEY`, `ESCAPE_KEY` in `browser_page/text.rs`, with the pure `list_key`) and the charts (`bar_chart::KEYS`) take their keys from named tables, and `dialogs/shortcuts.rs` checks each against the dialog; a keypad key counts as its main-keyboard key. The dialog gains a **Tags** section (Add Tag, the suggestion keys, Backspace to the last tag, Backspace / Delete on a tag) and the editor's **Alt+E / Alt+P** (Edit or Preview, on a narrow window). "Statistics Charts" is now "Statistics". Typing to search on All Cards has no accelerator to list, so it is not in the dialog.

**About** is unchanged: `AdwAboutDialog` already follows the HIG.

## Consequences

- New texts follow the punctuation rule; a reviewer can check a text against it.
- Mnemonics are not tested against the dialog (they live in Blueprint labels); Alt+E / Alt+P are listed by hand.
- The startup problem pages could not be screenshotted at 360 px: they are shown before the window state is applied, so the harness window keeps its default size.
