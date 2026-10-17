# 44. The All Cards page's layout: search header, compact filters, shared rows, selection mode

- Status: Accepted
- Date: 2026-10-17
- Amends: ADR 0036 (the search field's key capture, the filter bar, the rows, the selection-mode header, the empty states)
- Amended by: ADR 0048 (the selection buttons are named by their own accessible labels)
- Refers to: ADR 0040 (shell), ADR 0042 (card rows and tag pills), `docs/briefs/phase-1-finish.md` ("Browser", "Tags", "Design rules"), GNOME HIG "Selection mode"

## Context

The phase 1 brief asks for the All Cards page: a search bar in the GNOME style, filters as compact controls that collapse at 360 px, rows consistent with the deck page's card list (ADR 0042), a selection mode with a bottom action bar that follows the HIG, and proper empty and no-results states. Before this, the three filter drop-downs stacked under 500 sp and took most of a 360×294 window, the rows had their own layout (with the tags as a dimmed text caption, and an odd indent from `rich-list` inside a 900 px clamp), and the action bar carried the "3 selected" count.

## Decision

### Search stays the header's title widget

A `GtkSearchEntry` in an `AdwClamp` (400 px) as the `AdwHeaderBar`'s title widget, always shown, not a `GtkSearchBar` under the header. A search bar is for a page where search is occasional and revealed on demand (Files, Settings' list); All Cards exists to search, so its field is always there and has the keyboard when the page opens. It costs no height, which matters at 294 px.

**Typing in the list searches.** A key that types a character, pressed while the focus is in the list or on the status page, goes into the search field, which takes the focus without selecting its text (`grab_focus_without_selecting`, caret at the end), so the next letters, Backspace and the arrows act on the text. A key controller on the content (bubble phase, so the list's own Ctrl+A, Space and arrows come first; the header and the filters keep their keys) decides with the pure `browser_text::types_into_search`. It takes a key that types a character, and a dead key or Compose (`Multi_key`): the field then has the focus, so its input method composes the key that follows (Compose s s gives ß). It also takes Backspace while the field has text, as GTK's own capture does. It skips Ctrl and Alt shortcuts, Space, Enter, Menu and every other key that makes no character (Tab, the arrows, Escape). It also skips keys typed while a popover is open, such as a row's ⋮ menu or a drop-down's popup: the event then comes from the popover's own surface. It is off while cards are being chosen, because the field is hidden then. GTK's `gtk_search_entry_set_key_capture_widget` (a GTK 4.0 method; ADR 0036 confused it with the 4.22 property) is not used. It forwards keys from popovers under the capture widget too, it has no check that the entry is shown, and it leaves the focus where it was (gtksearchentry.c, `capture_widget_key_handled`, read for GTK 4.20). Its list of keys is the model for ours: it ignores Space, Enter and Menu, and `gtk_search_entry_is_keynav` covers the movement keys and Ctrl or Alt. Space on the list is decided by `browser_text::space_action`: a focused row in selection mode is chosen or dropped, and otherwise the key goes to the focused widget, so a row's ⋮ button still opens its menu.

Ctrl+F still focuses the field; in selection mode it does nothing (Cancel or Escape first), so a shortcut never drops a choice of cards silently.

### Filters on one line, always

Deck, tag and state stay `GtkDropDown`s (the first two with a search field), side by side in one row under the header, in a 600 px clamp so they line up with the list, with the card count ("336 cards", `.numeric`, dimmed) at the end. They share the width equally and never wrap or stack: at 360 px each is about 105 px, and the chosen entry ends in "…" if it does not fit (`ui::dropdown::shrinkable`: GTK's default factory makes a label that cannot ellipsize; the popup keeps whole names and GTK's check mark). Under 500 sp the count is hidden to leave the width to the filters. A screen reader still hears it after each search, because the page announces it, not the label. The bar is 46 px high at every width. It hides while there is no card at all (`browser_text::filters_shown`), because there is nothing to narrow. The Select button and Ctrl+A do nothing while the list is empty.

Not a filter button with a popover: a hidden filter silently narrows the list (ADR 0036), and three drop-downs fit on one line even on a phone. Not an `AdwToggleGroup` for the state: five values (Any, New, Learning, Due, Suspended) do not fit 360 px as toggles.

### One row for both lists

The All Cards rows are the deck page's `CardRow` (ADR 0042), not a copy: `CardRow::for_browser()` makes a row whose menu forwards to `browser.*` instead of `deck.*` (the action names are the same), whose third line (`caption_label`, `.caption` `.dimmed`) says the deck and status ("Maths · Due in 3 days"), and which has a long-press gesture (touch only) that starts choosing. A check button before the text shows while `selecting` is on, in place of the ⋮ menu. `bind_browse` binds a `BrowseItem` and follows its `selected`; the list item's accessible label is "front, back: …, tags: …, deck: Maths, Due in 3 days[, selected / not selected]" (`browser_text::row_label` around `card_row::text::row_label`). `BrowserRow` and `data/browser_row.blp` are gone.

So the rows look and cost the same: two lines like the deck page's (front; back with the tag pills), plus the deck and status line; the pause icon for a suspended card; the menu made on first use (`ui::rows::menu_on_demand`); the pills relabelled, never remade. The list is clamped to 600 px with `separators` and without `rich-list`, as on the deck page, which removes the old indent. The narrow rows show one pill, as on the deck page; they keep the back (the old compact rows dropped it), so a row is the same at every width. A test binds one row to 4,000 of 5,000 `BrowseItem`s and checks that no pill label or popover is made and only those cards get previews (about 120 ms CPU in a debug build, bounded at 2 s as for the deck rows).

### Selection mode as the HIG describes it

The HIG: "The header bar should show a cancel button and the heading should indicate the number of selected items", and the actions go in an action bar at the bottom. So in selection mode:

- the header's title is an `AdwWindowTitle` "3 Selected" ("Select Cards" before the first), in place of the search field (a `GtkStack` as the title widget);
- **Cancel** (a text button) replaces the Select button at the end, and **Select All** (icon and label; the icon alone under 500 sp, with its tooltip and accessible label) comes at the start;
- the page's `can-pop` is off, which hides Back and keeps libadwaita's Escape and swipe from popping the page (`AdwNavigationPage:can-pop` docs), so Escape leaves selection mode, as ADR 0036 meant (its consequence about `pop-on-escape` is settled);
- the bottom `GtkActionBar` has Move…, Tags and Suspend at the start, Reset and Delete at the end, away from the others; under 500 sp they keep only their icons. The count moved from the action bar to the header.

A header button that hides while it has the keyboard (Cancel, Select All) hands the focus to the Select button that comes back; Select moves it to the list. The filters stay usable while choosing; as before, a card that leaves the list is no longer chosen.

Not done: the HIG's check box on hover in normal mode. The list's click opens the editor (ADR 0036), the Select button, Ctrl+A and a long press already start choosing, and a hover-only control would fail the phone rule (SPEC §4.8).

### Empty and no-results states

One `AdwStatusPage` whose icon, title, sentence and button come from `browser_text::empty_page`, `GO_TO_DECKS` and `clear_label` (unit-tested). Which one shows is `browser_text::empty_state(rows, filtered, collection_empty)`. The last argument says whether any live card exists, whatever the filters (the decks' card counts). It is read only when a filtered search finds nothing. So All Cards opened from an empty deck in an empty collection says "No Cards Yet" at once, not "No Results Found" first.

- no card at all: "No Cards Yet", "Cards you add to your decks will show up here", **Go to Decks** (back to the deck list, where decks are made and cards added);
- nothing matches: "No Results Found", "Try a different search or other filters", and a button that says what it clears: **Clear Search**, **Clear Filters** or **Clear Search and Filters**.

Both buttons are `.pill .suggested-action` (brief, design rules). Escape in an empty search field moves to the list or to this button.

## Consequences

- `CardRow` serves two pages. A change to it shows on both; the deck page's rows never show the check button or the third line.
- The page's minimum size is the window's (360×294), like the deck page's; at 294 px about two rows show under the filters, or one and a half with the action bar.
- At 360 px a long deck name in the deck filter is cut with "…"; its full name is in the popup.
- Typing while the focus is on the header, the filters or the action bar does not search; only the list and the status page pass keys to the field.
- Dead keys and Compose typed in the list rely on the field's input method composing the next key once the field has the focus. The unit tests cover the decision, not the composing: the Xvfb keymap has no dead keys. An input method that composes from a preedit (ibus with a Japanese or Chinese engine) may not start a search from the list. In the search field itself, input methods work as in any entry. The manual checklist covers both.
- The drop-downs' popup check mark follows the drop-down's choice by item (as in GTK's own factory, `selected_item_changed`), not `GtkListItem:selected`, which follows the pointer and the arrow keys in the popup. It is an expression bound once per popup row (the drop-down's `selected-item` through a weak `GtkObjectExpression`, and the row's `item`), so a rebind adds no handler, and a row that is dropped takes its watch with it (tested).
