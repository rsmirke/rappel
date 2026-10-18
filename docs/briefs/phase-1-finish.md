# Phase 1 finish — brief for batch mode

Goal: Rappel looks and feels like an exquisite GNOME core app — calm, spacious,
very fast, at home on Ubuntu/GNOME. Models to study: GNOME Text Editor, Calendar,
Contacts, Settings, Characters; Circle apps Apostrophe (Markdown editing),
Secrets (list/detail), Fragments, Dialect, Wike. Follow the GNOME HIG strictly.
No hard-coded colours; theme variables and style classes only. Check current
libadwaita 1.9 docs for every widget/class used (don't rely on memory).

## Slices, in order
1. UI overhaul (several PRs: shell+navigation, deck list, card list+tags,
   editor, browser, review, dialogs). Subtle helpful UI is welcome; no
   big new features.
2. Code clean-up and refactor #2 (SPEC §4.10): harmonise everything above,
   plus open issues: shared "push unless on top" helper, undo test
   strengthening, Blueprint changes always recompile (+ test constructing
   every template widget).
3. Hardening round 2: attack tests on everything since the last round;
   fix the flaky timing tests (robust under machine load).
4. Remaining Phase 1 issues: bulk "Add Tag…" reuses the editor tag entry
   with suggestions; Orca checks from #47 as far as testable.
5. Beta preparation: version 0.2.0-beta.1 in Cargo.toml and CHANGELOG
   (do NOT tag), then open a draft PR phase-1 → main with the final report.

## Design rules
- Typography: libadwaita style classes only (title-1…4, heading, body,
  caption, numeric for all counts, dimmed for secondary text). Card text
  on the review page a bit larger than today; front slightly larger than back.
- Spacing in multiples of 6 px; content in AdwClamp (lists ~600 px, review
  column ~700 px). Header bars: at most ~3 controls per side.
- Writing: Header Capitalisation for titles/buttons, sentence case for
  descriptions, "…" for actions needing more input. Plain, friendly words.
- Primary menu order: Preferences, Keyboard Shortcuts, About Rappel.
- Empty states: AdwStatusPage with icon, title, one sentence, one .pill
  .suggested-action button.
- No spinner for work under ~300 ms. Animations only via libadwaita
  (they respect the system's reduced-motion setting).
- Adaptive: everything usable at 360×294; AdwBreakpoint for layout changes.

## Per screen
- Shell: evaluate an AdwNavigationSplitView with decks in a sidebar on wide
  windows (collapsing to today's navigation when narrow). Build it only if
  it looks clean; otherwise keep pages. Screenshot both options and record
  the choice with reasons.
- Deck list: boxed list; deck name prominent; subtitle "12 due · 5 new"
  (numeric); due count in accent colour when non-zero; "Next review in 3 h"
  when nothing is due. Empty state with "Create Deck".
- Card list (currently the weakest screen): two-line rows — front as title
  (rendered plain summary), back excerpt or due info as dimmed subtitle;
  tags as small pills; suspended shown with an icon, not just text;
  comfortable row height; good at 5,000 cards.
- Tags: small rounded pills everywhere (AdwWrapBox where they can wrap),
  neutral style, readable in light/dark/high contrast.
- Editor: sections laid out like a preferences page — Front, Back, Tags —
  with clear labels and spacing; tags as pills with the entry for adding;
  preview side-by-side when wide, toggle when narrow; save hint
  ("Ctrl+Enter to save") shown subtly.
- Browser: search bar at the top in the GNOME style; filters as compact
  controls (dropdowns or toggle groups) that collapse gracefully at 360 px;
  rows consistent with the card list; selection mode with a bottom action
  bar; a proper empty and no-results state.
- Review: larger text; Show Answer and Fail/Pass as large, easy-to-tap
  buttons at the bottom; subtle key hints (Space, 1, 2) under them;
  "N left" in the subtitle; a short, tasteful reveal transition.
- Dialogs (preferences, card info, about, shortcuts): standard Adw dialogs,
  consistent spacing and capitalisation.

## Final report (docs/briefs/phase-1-report.md, in the draft PR)
What changed per slice; every "Decision" with its reason; the open
QUESTIONS; screenshots of each screen (light, dark, 360 px); and a single
numbered checklist for the maintainer to try on the phase-1 branch.
