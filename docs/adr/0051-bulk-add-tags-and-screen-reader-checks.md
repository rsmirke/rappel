# 51. Bulk Add Tags with the tag entry, and what screen readers get on the review page

- Status: Accepted
- Date: 2026-10-18
- Amends: ADR 0036 (the All Cards Add Tag dialog), ADR 0043 (the tag entry in a dialog), ADR 0025 row 3 (screen readers and the inserted breaks; the cut note), ADR 0045 (the card area's accessible role)
- Refers to: ADR 0046 (dialogs), ADR 0050 row 30 (at most 100 tags from the field), `docs/briefs/phase-1-finish.md` (slice 4)
- Closes: #65. Refs: #48, #47

## Context

Issue #65: the All Cards selection mode's "Add Tag…" was an `AdwEntryRow`, so a bulk add made a new tag from every typo, while the card editor's `TagEntry` (ADR 0043) suggests the tags there are. ADR 0043 left it to this dialog to decide whether it takes one tag or several. The core's bulk add had no limit on a card's tags, while the editor's field adds none past 100 (ADR 0050 row 30).

Issue #48 (from #47): confirm that screen readers do not get the zero-width spaces `render::breaks` puts into long runs, and that the review page's "only its start is shown" note is not read twice. GTK 4.24's source (`gtk/gtklabel.c`, `gtk/gtkatcontext.c`, `gtk/gtkaccessible.c`) and a read-only AT-SPI walk of the running app under Xvfb showed:

- A `GtkLabel` sets its accessible name to its text, breaks included, each time its text changes (`gtk_label_set_text_internal`), and its AT-SPI Text interface returns the same text (`gtk_label_accessible_text_get_contents`).
- The review page's card area is a `GtkBox`, whose default role is Generic. GTK exposes no name or description for a role whose naming is "prohibited" (step 1 of `gtk_at_context_get_text`), so the front, back and note the page set on it (ADR 0025 row 3) never reached a screen reader: the walk showed an unnamed panel. The note was exposed only by its own label, once.

## Decision

**Add Tags.** The dialog stays an `AdwAlertDialog` (ADR 0046: small question dialogs are alert dialogs), with a `TagEntry` as its extra child, every tag as a suggestion (the tag filter's list, which the page reads again on every change), and the editor's keys and behaviour unchanged.
- **Several tags at once, as pills**: the entry already makes pills, and taking only one would make the field behave differently from the editor's. The heading is **Add Tags** and the menu item **Add Tags…** (Remove Tag… stays singular: it takes one off). The body says how many cards and how a tag ends: "Choose the tags to put on 3 cards. Press Enter or type a comma after each one." (the editor's Tags group says the same).
- **Add** is the default and suggested response, enabled only with at least one tag and no error under the field (`tags_to_add`). The name being typed counts, as in the editor, so typing one name and clicking Add works. Enter on a typed name makes it a pill (the entry takes Enter first); Enter in the empty text, after the pills, is Add (`TagEntry::set_activates_default`: GtkText activates the dialog's default widget). Escape closes the suggestions if they show, else cancels (the entry's capture-phase controller, ADR 0043). The focus starts in the entry's text.
- **The limit**: `Store::add_tags_to_cards(ids, names, max_per_card)` puts every tag a card lacks on it in one transaction, or none of them if that would take it past the limit; such cards are reported (`TagsAdded::full`). The app passes the editor's `MAX_TAGS_PER_CARD` (100). All or nothing per card, not tag by tag: "which of my three tags did that card get?" has no good answer in a toast. A card imported with more than 100 keeps them and gets no more, as in the editor. If no card gets a tag, nothing is written, not even a new tag. The toast says what happened: "2 tags added to 3 cards; 1 card left out, as a card can have at most 100 tags", or "No tags added; a card can have at most 100 tags". The limit stays the app's, as in ADR 0050: the store still has none for an import or a single card's tags. The old `add_tag_to_cards` / `release_tag_from_cards` (one tag, no limit) are removed; their tests now run on the new calls.
- **Undo** is one step for all the tags: `Store::release_tags(links)` removes exactly the links the add made, on cards still live.
- **Remove Tag…** already lists only the tags the chosen cards have, in a searchable drop-down: left as it is. Suggestions there would add nothing, since the user picks from what is there.

**Screen readers.**
- **The card area's role is Group** (`review_page.blp`), which is named by its author, so the front's text (with the note when cut) is its name and the back's (with its note) its description, as ADR 0025 row 3 meant. Group rather than Article or Region: it is a focusable part of a window, not a document, and Region requires a name, which an empty side would not have.
- **The note's own label is a Presentation**, which GTK does not expose (`gtk_accessible_should_present`), so it is read once, with the side it is about.
- **Labels with breaks are named without them** (`render::name_without_breaks`, after every `set_label`), as the math paragraphs' text already was. Only Label and Heading roles get it; both are named from content.
- **Accepted: the AT-SPI Text interface still has the breaks.** A label's Text interface must match what is drawn: its offsets, extents and "text at offset" come from the Pango layout. Removing the breaks there would mean reimplementing `GtkAccessibleText` for these labels with offsets mapped both ways, for a character speech synthesizers do not speak (U+200B is a format character). What Orca does with it in flat review and braille is left to the maintainer's Orca check.

## Verification

- GTK tests: the Add Tags dialog applies the typed and suggested tags to exactly the chosen cards and Cancel adds nothing (`browser_page::tests::add_tags_puts_the_tags_of_its_entry_on_the_chosen_cards`); cards at the limit are left out with the right toast (`add_tags_leaves_out_cards_at_the_limit_of_the_tags_field`); the card area is a focusable Group with a label and the note a Presentation (`review_page::tests::screen_readers_get_the_card_from_the_card_area_and_the_note_once`); rendered labels with long runs have breaks in their text and get a name without them (`render::tests::labels_with_breaks_are_named_without_them`).
- GTK has no public getter for an accessible property's value, so these tests cannot read the name GTK exposes. The AT-SPI walk under Xvfb did, before and after the change; its output is in the PR (#48's checks).
- Core: `bulk::tests` for the limit, the merge of names by key, the Undo and the all-or-nothing errors; `perf` times both new writes on 5,000 cards.

## Consequences

- The All Cards tag dialog behaves like the editor's field; a later dialog that asks for tags can reuse it the same way.
- A plain layout may name the app's own widgets: `ui::dialog::builder` registers every one first (`widgets::register_types`, also called at startup). Without it, GtkBuilder aborted on `$RappelTagEntry` ("Invalid object type"), for the Move and Remove Tag dialogs too, which share the file; the screenshot run found it. The ordinary tests could not, since another test registers the type earlier in the same process, so `ui_files::plain_layouts_load_in_a_fresh_process` loads every plain layout in a new process of the test binary (checked to fail when the registration is removed).
- Orca still has to be heard: whether the focused card reads front, then note, then back; whether flat review or braille shows the breaks.
