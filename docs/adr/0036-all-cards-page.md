# 36. The All Cards page

- Status: Accepted
- Date: 2026-10-17
- Amends: SPEC §4.6 (the page), §4.5 (Suspend in the deck page's card menu)
- Amended by: ADR 0044 (typing in the list searches, filters on one line, the deck page's rows, the selection-mode header, the empty states)
- Refers to: ADR 0013 (the virtualised card list), ADR 0030 (Undo that finds the world changed), ADR 0034 (the browser comes first), ADR 0035 (the index, suspension and bulk writes in core)

## Context

ADR 0035 put search, filters, suspension and bulk writes in core. This is the screen on top of them (SPEC §4.6): fast at 5,000 cards and more, rich rows on a desktop and compact ones at 360 px, usable by keyboard and by screen reader, with an Undo for each action.

## Decision

**Entry.** A search button at the end of the deck list's header and **Ctrl+F** (`win.search`). From the deck list it opens All Cards; from a deck page, All Cards filtered to that deck; on All Cards it puts the keyboard in the search field; while studying, on the statistics page or under an open dialog (the editor) it does nothing, so the page never opens behind a dialog. The page is guarded against key repeat like the other pages.

**One query path.** Every search, filter change and write reads the rows again with `Store::browse` and replaces the model with one `splice`. The list is a `GtkListView`, which builds rows only for what is on screen (ADR 0013). `BrowseItem` makes the front and back previews the first time a row asks, so a keystroke parses nothing for the rows nobody sees. Measured in a debug build with 5,000 cards: the page opens in 53 ms, a search takes 20 ms, choosing all cards 8 ms, and the list builds about 200 rows (the ones in view and a few the view measures), never one per card.

**Search field.** A `GtkSearchEntry` with its own 150 ms `search-delay`, so there is no hand-made debounce. `stop-search` (Escape) has no default action, so the page clears the text, or, if there is none, moves to the list. Down moves from the field to the list. Typing anywhere to search (`key-capture-widget` on the entry) is GTK 4.22 API (the GIR says "since 4.22"; the Rust bindings do not gate it, so it would compile and then fail on older GTK); the app targets 4.14 (the `v4_14` feature), so it is not used.

**Filters are always shown**, in a bar under the header: deck, tag and state drop-downs (the first two with a search field; `GtkDropDown` searches by an expression, which `ui::dropdown::set_labels` sets). They are not behind a toggle: three drop-downs are small, and a hidden filter would silently narrow the list. At 360 px they stack.

**Rows.** A wide row has the front, the back (dimmed) and a caption "Maths · Due in 3 days · algebra, hard". A compact row (an `AdwBreakpoint` at 500 sp sets the page's `compact` property, which each row is bound to) has the front and "Maths · Due", without the back and the tags. A screen reader always hears the full caption. Suspended cards are dimmed and read as such.

**Choosing cards is the page's own** ([`Selection`], a set of ids), not a `GtkSelectionModel`. Outside selection mode a click opens the editor, as on the deck page; a list view with a selection model selects on a click, and its single-click-activate mode selects on hover. Selection mode starts with the Select button, **Ctrl+A** (capture phase on the list, so the search field keeps its own Ctrl+A), or a long press on a touch screen (claimed, so its release does not activate the row). In it a click, **Enter** or **Space** chooses a row, a check button shows the state, and the row's accessible label says "selected". **Escape** leaves it first, then clears the search and filters; with neither to do it is not handled and the navigation view goes back.

**Actions.** A bottom action bar shows "3 selected" with Move…, Tags (Add Tag…, Remove Tag…), Suspend or Unsuspend (Unsuspend only when every chosen card is suspended), Reset and Delete. Under 500 sp the buttons lose their words (tooltips and accessible labels stay). A row's ⋮ menu does the same for one card; it has Suspend or Unsuspend as two items of which the row disables the one that does nothing and the menu hides it (`hidden-when: action-disabled`), so the menu model stays static. Every action runs at once (Delete too, as on the deck page), leaves selection mode and shows an Undo toast; the toast text counts what really changed. A card that was already as asked is not counted, and if nothing changed a notice says so. The Undo calls are the core's lenient ones (ADR 0035): they do less when the world changed, and a note says how much ("2 of 3 cards restored; the others changed since"). If an action finds a card gone (an import, another window), nothing is written, the list is read again, and a notice says the list was out of date. The Undo closure keeps the state and weak references, not the page, so it works after leaving the page.

**Other pages.** The page remembers when it wrote something and, on leaving, asks the window to read the deck pages and the deck list again (`RappelWindow::data_changed`, which also reads an open All Cards page after an import). The deck page's card menu gets Suspend and Unsuspend (with an Undo toast), and its rows say "Suspended".

## Consequences

- The tag name of Add Tag… is typed in a plain entry with the store's rule checked inline; it does not offer the existing tags as the card editor does. A suggestion list is a small later addition if it is missed.
- Escape in selection mode relies on libadwaita's `pop-on-escape` not seeing the key first. If it does, Escape goes back and drops the selection; the checklist of the PR asks to check it.
- Suspend and Delete on the review page are the next PR (SPEC §4.5).
