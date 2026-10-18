# 33. The statistics page and the card information dialog

- Status: Accepted
- Date: 2026-10-17
- Amends: SPEC §4.7 (the pages are done; the chart days can be stepped through with the keyboard)
- Amended by: ADR 0046 (Statistics' empty state)
- Refers to: ADR 0032 (the figures), #39 (card information)

## Context

ADR 0032 left the UI two jobs: build the day grids from the settings' day start hour and draw what `Stats` returns, and show a card's `CardInfo`. The decisions were where to put them, how to draw charts that follow the theme and can be read without sight or colour, and how much of a card's history to list.

## Decision

**Entry points.**
- The global statistics are a **Statistics** item in the deck list's primary menu. One deck's statistics are in a new **Deck Menu** (⋮) in the deck page's header. Adwaita's icon set has no chart icon, and an icon-only button with a guessed icon would be less clear than a menu item with a name.
- Card Information is in each card row's ⋮ menu and, on the review page, in a **Card Menu** (⋮) in the header and on the **I** key. Opening it changes nothing in the session: it grades nothing and leaves the timer alone, as the editor does (the time of an answer is capped, ADR 0014). Closing it puts the focus back on the card area.

**The days.** `clock::day_bounds_ms` gives the starts of consecutive study days in a time zone: `hour`:00 of each calendar date, stepped with `GLib`'s `add_days`, so daylight saving needs no code of ours. `AppState::stats` builds 31 bounds for the last 30 days (today last) and 15 for the next 14 (today first), as ADR 0032 says, and hands back the day starts for the labels.

**Charts.** One `GtkDrawingArea` subclass, `BarChart`, drawn with Cairo.
- *Colours:* the accent colour for the bars (`AdwStyleManager::accent_color_rgba`) and the widget's CSS `color` at lower opacity for everything else. It redraws when the accent colour or the dark mode changes. No colour is written in our code.
- *Not by colour alone:* passes are solid, fails are hatched and set off by a gap. A legend repeats both samples next to the words "Passed" and "Failed".
- *Axis text is not drawn.* The top of the scale, the first and the last day are `GtkLabel`s around the chart, so they follow the theme and the text size, and no Pango-in-Cairo code is needed.
- *Resizing:* the chart fills the width it is given and recomputes its bars (`widgets/bar_chart/layout.rs`, pure). At 360 px a bar of 30 is about 8 px wide.
- *Accessibility:* each chart has the role Img, its title as label and a description in words ("Last 30 days: 412 reviews, 380 passed, busiest day 14 October with 38."). The chart is also focusable: Left, Right, Home and End (or a click or tap) select a day, whose values appear in a caption under the chart and are announced with `gtk_accessible_announce` (GTK 4.14). That gives a keyboard and screen reader user the figure of any day, not only the summary.
- The same widget draws the three charts: reviews per day, time studied per day (SPEC §4.7 lists it) and the due forecast. `ChartCard` is the chart with its title, scale, day labels, caption, legend and note, or a sentence when there is nothing to draw ("No reviews in the last 30 days"). A page with no reviews, no scheduled cards and nothing due is an `AdwStatusPage`.

**The page** is an `AdwBreakpointBin`: three headline tiles (expected retention, pass rate, time studied) side by side, stacked under 500 sp, above the three charts in a clamp. A missing figure shows "—", and its accessible label says why ("no reviews").

**The dialog** lists properties as `AdwActionRow`s with the `property` style class, and the history as a boxed list, newest first. A review's row says "Passed" or "Failed" with a check or a cross icon; a reset is a row of its own, "Progress Reset", with a refresh icon and "counts start again from here". A card without a schedule (never studied, or reset) shows "New card" instead of the memory figures. At most the newest 500 entries are listed, with a note counting the rest: a card with thousands of reviews would otherwise make the dialog slow to open, for rows nobody scrolls to.

**Loading** is synchronous, when the page or dialog is built. ADR 0032 measured the debug build; in a release build, at 5,000 cards and 100,000 reviews, all decks take 58 ms, one deck 41 ms and a card with 2,000 reviews 2 ms. A usual collection takes a fraction of that, so no worker thread is used. Reopening the page reads again; a page left open does not update.

**Pure code.** Every text, number and key decision is a function with unit tests: `pages/stats_page/text.rs` (headline, summaries, captions, empty states), `dialogs/card_info/text.rs` (rows, history, the cap), `widgets/bar_chart/layout.rs` (bars, ceiling, click position, key steps; property-tested) and `ui/text.rs` / `ui/time.rs` (percent, durations, intervals, "in 3 days").

## Consequences

- No core change, no migration and no new dependency.
- The charts' look (hatching, spacing, the focus ring on a focusable drawing area) cannot be checked by tests; the pull request has a checklist.
- A chart is not interactive with a pointer beyond selecting a day (no tooltip on hover). The selected day's caption is the same text for everyone.
