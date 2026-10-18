# 42. The deck page's card rows, and tag pills

- Status: Accepted
- Date: 2026-10-17
- Refers to: ADR 0013 (deck page list), ADR 0029 (tags), ADR 0035 (suspension), ADR 0036 (All Cards), ADR 0041 (deck rows), issue #42 (speed at 5,000 cards), `docs/briefs/phase-1-finish.md` ("Card list", "Tags")

## Context

The deck page showed each card as one line: the front's summary, the tags as dimmed comma-separated text on the right, a "Suspended" caption, and a ⋮ menu. The phase 1 brief calls it the weakest screen and asks for two-line rows (the front as the title, the back or the due information as a dimmed subtitle), tags as small neutral pills, suspension shown by an icon, a comfortable row height, and good speed at 5,000 cards. The pills are to be shared with the editor and the All Cards page, whose overhauls come later.

## Decision

### The row

`CardRow` (`data/card_row.blp`) is laid out like an action row through the CSS node names libadwaita styles in any row, as the deck rows are (ADR 0041): the row is `box.header`, its text column `box.title`, the front a `.title` label and the back a `label.subtitle` (dimmed and smaller by libadwaita's `row label.subtitle`). The list view drops `rich-list`, whose padding would add to the header's margins. As on the deck rows, the text column has 12 px above and below instead of 6, and libadwaita's own 3 px between the two lines; the back line is at least as tall as a pill, so rows with and without tags line up (about 69 px).

- **Title**: the front's plain summary (`content::plain_summary`), one line, "…" at the end. Normal weight: in a list of hundreds of cards, bold titles would be heavy, and the deck name is the page title.
- **Subtitle: the back's excerpt**, not the due information. A learner scanning a deck is checking content (is this card right, is it a duplicate, which card was that); the back answers that, and seeing the question with its answer is what a flashcard list is for. When a card is due is shown by the page subtitle for the deck as a whole and per card in All Cards and Card Information. A per-card due time would also go stale while the page is open, which would mean re-binding rows on a timer. A card with an empty back and no tags is one line.
- **Tags**: on the subtitle line, after the back, as pills (below). The back gives way first: the pills keep their width and the back ends in "…". The front keeps the whole first line.
- **Suspended**: `media-playback-pause-symbolic` (in the Adwaita icon theme; pause is what suspension does to a card), dimmed, before the menu, with "Suspended" as its tooltip and accessible label. The row itself is not dimmed: its second line is already at libadwaita's dim opacity, and dimming the row again would put the back below a readable contrast.
- **Accessible label** (`widgets::card_row::text::row_label`, unit-tested): "front, back: …, tags: a, b, c, d, suspended", every tag included, also those folded into "+N". The menu button keeps "Actions for …".
- The ⋮ menu, its items, the row actions and every key are unchanged.

### Tag pills

`TagPills` (`widgets/tag_pills.rs`, `data/tag_pills.blp`) shows tag names as pills in one of two forms:

- **wrapping** (the default, for the editor later): every tag in an `AdwWrapBox` (libadwaita 1.7, within the 1.8 we build against), 6 px apart in both directions;
- **single line** (list rows): at most `max-shown` pills, then one "+N" pill (`.numeric`) whose tooltip names the hidden tags. Card rows show three, or one when the page is narrow: the deck page's existing 400 sp breakpoint sets its `compact` property, which each row follows, as on the All Cards page.

A pill is a plain `GtkLabel` (`use-markup` off; tag names are untrusted), `.caption`, cut with "…" past 16 characters (the full name is then its tooltip, also plain text). Its style, `.tag-pill` in `data/style.css`: 18 px high, 6 px side padding, fully rounded, filled with the text colour at 10 % (`alpha(currentColor, 0.1)`). No accent and no literal colour, so it follows light, dark and any accent. Under high contrast (`@media (prefers-contrast: more)`, which libadwaita sets, ADW_DEBUG_HIGH_CONTRAST=1 to try it) a pill also gets a 1 px outline in `var(--border-color)`, which libadwaita raises to 50 % there. Pills are not focusable; in a row, Tab goes from the row to its menu as before.

The texts and counts (`pill_text`, `split`, `overflow_text`, `overflow_tooltip`, `spoken`, `is_cut`) are pure functions in `widgets/tag_pills/text.rs`, unit-tested.

### Speed at 5,000 cards

Rows are recycled by the `GtkListView`, so the costs that matter are opening the deck (the model plus the first screen of rows) and a bind while scrolling:

- `CardObject` makes the front and back previews when a row first asks (as `BrowseItem` does), not when the deck opens.
- `TagPills::set_tags` keeps its pill labels and only relabels them; it does nothing when the tags are those already shown. No widget is made or destroyed on a bind after the first few.
- A row's ⋮ menu is given to its button on first use (`ui::rows::menu_on_demand`, `gtk_menu_button_set_create_popup_func`). Building the six-item popover was more than half the cost of making a row, and most rows' menus are never opened. GTK 4.24 calls that function on every open, and setting the model again would build a new popover each time, so the model is set only once (a test opens a menu twice and gets the same popover).

Measured on one machine, debug build, the 5,000-card scratch deck (`fill_scratch_db <dir> 5000 --due 200 --tags --suspended 50`), five runs each under Xvfb with the Cairo renderer, from activating the deck row to the first frame after the push (the `deck page opened` debug log, below):

| | phase-1 (#73) | this ADR |
|---|---|---|
| building the page (`DeckPage::new`) | 83 ms | 48 ms |
| to the first frame | 255 ms | 182 ms |
| making one row (100 rows, warm) | 0.53 ms | 0.11 ms |
| binding one row to all 5,000 cards in turn | 92 ms (previews made at open) | 115 ms with the previews made then, 30 ms once made |

`binding_a_row_to_thousands_of_cards_makes_no_widgets` checks the work rather than the clock, so it holds on a loaded machine (#67): after a first bind, binding a row to 4,000 of 5,000 cards makes no pill label and no popover, and only those 4,000 cards get previews. It also bounds the thread's CPU time (from `/proc/thread-self/stat`, `testing::thread_cpu_time`) at 2 s, against about 100 ms idle and 380 ms with three busy threads per CPU; a wall-clock bound failed under such load. `DecksPage::open` logs `built_ms` and `first_frame_ms` at debug level (`RUST_LOG=rappel=debug`), from the frame clock's first `after-paint` after the push; no card content is logged.

### Deck page subtitle and empty state

The subtitle uses `ui::text::due_new_line` ("1,234 due · 20 new"), built from `due_count` / `new_count`, which the deck rows use too, so both say the counts the same way with grouped digits. The empty state already followed the brief (icon, "No Cards Yet", one sentence, a `.pill .suggested-action` **Add Card**) and is unchanged, except that on a narrow window the bottom Study bar (ADR 0040) now hides while the deck is empty (`study_bar_shown`, unit-tested), so the status page and its Add Card stand alone. On a wide window the header's Study stays, disabled, so the header keeps its shape when the first card is added.

## Consequences

- The All Cards rows (`browser_row`) are still the old layout. Their overhaul should reuse `TagPills` (single line, `compact` bound as here), the `box.header.card-row` structure and its CSS, and `menu_on_demand`, so both lists look and cost the same.
- The editor's tag field can use `TagPills` in its wrapping form.
- Card rows follow libadwaita's row styles through their node names, like the deck rows: a libadwaita change to `row > box.header` restyles both.
- A cut pill name and the tags behind "+N" can be read in full only in a tooltip, which keyboard and touch users cannot open; screen readers get every tag from the row's label. Phase 2's no-hover-only rule (SPEC §4.8) needs another way to see them, e.g. the full list in Card Information or the editor. This is a follow-up.
- A pill's maximum width is counted in characters (`max-width-chars`), so a name of wide letters may be cut a little before 16 characters without a tooltip.
