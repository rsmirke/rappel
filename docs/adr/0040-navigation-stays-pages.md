# 40. Navigation stays pages, not a split view

- Status: Accepted
- Date: 2026-10-17
- Refers to: ADR 0011 (startup screens), ADR 0013 (deck page), ADR 0014 (study sessions), ADR 0036 (All Cards), ADR 0039 (the remembered deck), `docs/briefs/phase-1-finish.md` ("Shell")

## Context

The phase 1 brief asks to evaluate an `AdwNavigationSplitView` for the shell: the deck list in a sidebar on wide windows, collapsing to today's `AdwNavigationView` pages when narrow, and to build it only if it looks clean.

A prototype was built and kept as commit 2030aa1 ("chore: prototype decks in an adaptive split view") on the local branch `spike/split-view`. Its widget tree, inside the window's start-up stack:

```
AdwApplicationWindow
├── AdwBreakpoint (max-width: 600sp → split_view.collapsed = true)
└── AdwToastOverlay
    └── stack page "main"
        └── AdwNavigationSplitView split_view
            ├── sidebar: DecksPage (AdwNavigationPage; header: New Deck | "Rappel" | Search All Cards, Main Menu)
            └── content: AdwNavigationPage
                └── AdwNavigationView navigation_view
                    ├── AdwNavigationPage "placeholder" (AdwStatusPage "No Deck Selected")
                    └── pushed: DeckPage, then ReviewPage / StatsPage / BrowserPage
```

Opening a deck popped the content view to the placeholder, pushed the deck page and set `show-content`. Statistics and All Cards were pushed onto the same content view. It was screenshotted at 1000 × 720 and 360 × 640 next to the current pages.

## Decision

**Keep the pages.** The window stays an `AdwNavigationView` with the deck list as its root, as in ADR 0011 and 0013. Reasons, from the prototype:

1. **The review session is squeezed beside the sidebar.** A study session should be focused on one card. In the split view it sits beside a list of decks the learner can click, which would end the session (or need a guard and a question). Keeping the session full-window needs a third, outer navigation view around the split view, so three nested navigation containers for one window.
2. **Two headers both start with a "+".** The sidebar's New Deck and the content's New Card sit side by side, both as `list-add-symbolic`. Which one adds what is no longer clear at a glance.
3. **The deck list's rows do not suit a sidebar.** They are a boxed list with a menu per row and two lines of counts. In a sidebar of about 250 px the names were cut ("Markdown (one card p…"); a proper sidebar needs `.navigation-sidebar` rows with selection, a full redesign of the deck list.
4. **Back gets ambiguous.** The deck page's Back went to the empty placeholder on wide windows and, collapsed, through the nested view back to the sidebar. Escape and Back from the review or All Cards would behave differently by width, and the focus rules in CLAUDE.md (focus in `shown`, the deck row focused when coming back) would need a second path for each width.
5. **Few decks, long stays.** A learner opens one deck and stays there (cards, study). Switching decks often, which a sidebar is good at, is not the common path. The deck list is one Back away, and the window reopens the last deck at start (ADR 0039).
6. **Simplicity.** `ui::nav`, `window_state::deck_to_reopen`, `win.new` and `win.search` all assume one navigation view. The split view would change each, for no gain in the main tasks.

**Shell polish that comes with this choice.**
- The headers checked at 360 px wide (360 × 640 and 360 × 294) keep at most three controls per side, window buttons not counted: the deck list (New Deck | Search All Cards, Main Menu), the deck page (Back, New Card | Deck Menu, Study moved down as below), the review page (Back, Undo | Edit Card, Card Menu), Statistics (Back) and All Cards (Back, search field | Select Cards). The card editor dialog's header (Cancel | Preview, Save) fits once the window no longer grows. The other dialogs were not checked here (dialog slice). The deck page header (Back, New Card, title, Study, Deck Menu, plus the window buttons) needed more than 360 px, so the window grew wider than a phone screen. Its layout is now an `AdwBreakpointBin` with a breakpoint at 400 sp: there Study moves from the header to a `GtkActionBar` at the bottom, revealed only on narrow windows, with the same action (`deck.study`). The bar's button is a pill, like the review page's answer buttons, with no extra margins (the window can be 294 px tall). Only the visible Study button has the Alt+S mnemonic. The bin's size request is the window minimum.
- The window's start-up stack is an `AdwViewStack` with `enable-transitions`, so its crossfade is libadwaita's transition, as the brief's design rules ask. Nothing changes for the user: `GtkStack`'s transition also honoured `gtk-enable-animations`.
- The primary menu, header tooltips and accessible labels, and the page titles were already as the brief asks; they are unchanged.

## Consequences

- No new widget tree or navigation path: the review session, All Cards, Statistics, the window-state restore and the focus rules work as before.
- If Rappel later gets many decks or nested decks, a sidebar can be reconsidered; the prototype branch shows the shape and the problems above.
- The deck page now has two Study buttons, one shown at a time; both use `deck.study`, so they are enabled and disabled together.
