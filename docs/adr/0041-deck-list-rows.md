# 41. What the deck list's rows say, and keeping it current

- Status: Accepted
- Date: 2026-10-17
- Refers to: ADR 0012 (deck list), ADR 0014 (study sessions), ADR 0027 (daily limits), ADR 0035 (suspension), ADR 0040 (pages), issue #42 (speed at 5,000 cards), `docs/briefs/phase-1-finish.md` ("Deck list")

## Context

The deck list showed each deck as an `AdwActionRow`: the name, and "N new · N due" as the subtitle, always, even "0 new · 0 due". The phase 1 brief asks for the deck name prominent, "12 due · 5 new" with the due count in the accent colour, "Next review in 3 h" when nothing is due, and an empty state with "Create Deck".

Three problems:

1. An `AdwActionRow` subtitle is one label. Colouring only the due count would need Pango markup, which cannot name a theme colour (only literal colours, which the project forbids), and `use-markup` would apply to the deck name too, which is untrusted.
2. "Nothing now" covers several situations that need different words: a deck with no cards, a deck whose cards are all suspended, a deck waiting for its next review, and a deck held back by the daily limits (ADR 0027) until the next study day. `DeckSummary` had only the two counts.
3. "Next review in 25 min" goes stale while the list is on screen.

## Decision

### The row

`DeckRow` is a `GtkListBoxRow` with its own layout (`data/deck_row.blp`), not an `AdwActionRow`. Its boxes carry the classes that libadwaita styles in any row (`row > box.header > box.title`, `.title`, `row label.subtitle`), so it looks like an action row. The name has libadwaita's `heading` class (bold), so it is clearly the row's primary element; it does not also carry `title`, whose row rule (`font-weight: inherit`) is more specific and would undo it. The line under the name is three plain-text labels: the due count (`.accent`, kept at full opacity by `style.css`), a "·", and the rest (dimmed by the `subtitle` rule). All counts have `.numeric`. No label parses markup. The title box has 12 px above and below instead of 6, so the two-line rows are comfortable to point at and tap (about 67 px high). A long name wraps onto a second line, then ends in "…"; the row's accessible label has the whole name.

The row's accessible label is the name and the line in words: "Scratch, 12 due, 5 new", "Maths, next review in 3 hours". The menu button keeps "Actions for …".

### The texts

Decided by the pure `widgets::deck_row::text::row_text` (unit-tested; the table is at the top of that file):

| The deck | Shown |
|---|---|
| due and new cards | **12 due** · 5 new |
| only due / only new | **12 due** / 5 new |
| no cards | No cards yet |
| every card suspended | All cards suspended |
| next card within the hour | Next review in 25 min |
| … later this study day, or within 12 h | Next review in 3 h |
| … during the next study day | Next review tomorrow |
| … later (study days; months from 60 days) | Next review in 4 days / in 3 months |
| the daily limits hold cards back and nothing comes before the next study day | Done for today |

Zero counts are left out ("5 new", not "0 due · 5 new"), as on the deck page's subtitle. "Tomorrow" and the days are counted in study days (the day start hour of the settings), so the list never says "tomorrow" and then "in 1 day". A review late in the evening after the next day start but within 12 hours is still "in 9 h".

### The data

`DeckSummary` gains `card_count`, `suspended_count`, `held_back_count` (new cards past today's allowance plus due reviews past it) and `next_change_at` (exactly `Store::next_study_change`, so the deck list and the deck page's timer agree). `list_decks` reads them in its one read transaction, and no longer builds each deck's `StudyInput`: `deck_counts_in` (in `storage/review.rs`) takes today's two counts in one pass over the counted reviews (`today_counts_in`, shared with `study_input_in`) and everything else in one aggregate pass over the deck's live cards (indexed: `cards_deck_live`, then the schedule's primary key), with the same conditions as `study_input_in` and `next_study_change`. The counts are then cut to the daily allowances. The test that the deck list's counts equal the lengths of `study_input` at many times keeps the two equal. No migration. At 5,000 cards (2,000 due) in a debug build, measured side by side on one machine: `list_decks` 28 ms, against 42 ms before this ADR (a first version with a separate count query and next-change query per deck took 52 to 56 ms and broke the 150 ms bound under load).

Known imprecision, kept for agreement with the deck page: `next_study_change` does not know that the review limit is used up. A deck whose review allowance is spent, with nothing overdue held back and a review due later today, says "Next review in 2 h"; at that time it becomes "Done for today". It needs a deck that reached 200 reviews in a day. With `max_new_per_day` set to 0, a deck of only new cards says "Done for today" every day, which is true: the setting means no new cards.

### Keeping it current

The list is read again **in place** (same decks in the same order: each row updated, the focus untouched; otherwise rebuilt with the focused deck refocused):

- when the page is shown (coming back from any page, not only a deck);
- when the window becomes active, while the list is on top: GLib timeouts use the monotonic clock, which does not advance during a suspend, so the timer alone would be late after a laptop wakes;
- by a `ui::debounce` timer at the next change (any deck's `next_change_at` or the next study day), but no later than its text stays right: a minute while a row counts minutes, ten minutes while one counts hours, an hour otherwise (as the deck page, so a changed clock or time zone is noticed). It is never under a second. Under another page the timer does nothing; `shown` reads the list on the way back. (`refresh_delay_ms`, unit-tested.)

Each read is one `list_decks`: at 5,000 cards (2,000 due), 28 ms in a debug build on a loaded machine (where phase-1's took 42 ms), at most once a minute. A failed read shows one toast in an `ErrorSlot`, replaced by the next failure and dismissed by the next success.

### The empty state

Unchanged except the button, now "Create Deck" (`.pill .suggested-action`), as the brief says; the header's button stays "New Deck" (an icon with a tooltip, as GNOME apps name it).

## Consequences

- The deck list never shows "0 new · 0 due"; every deck says what it has or when it will.
- The row's look follows libadwaita's row styles through their CSS node names, not the `AdwActionRow` widget. If libadwaita restyles `row > box.header`, the deck rows follow; if it changes those node names, they need a look.
- `AppState::list_decks` takes `now`, so the row texts use the same instant as the counts.
