# 31. The tags field in the card editor

- Status: Accepted
- Date: 2026-10-17
- Amends: SPEC §4.4 (the field is comma-separated, not chips)
- Amended by: ADR 0043 (the field is now pills with an entry after them; the suggestions and their keys are unchanged)

## Context

ADR 0029 gave tags a store API and left the UI for later: a field in the card editor, suggestions while typing, tags on the deck page's rows. Four questions needed an answer: what the field is, how to suggest (`GtkEntryCompletion` is deprecated since GTK 4.10), how a card and its tags are saved together, and what counts as the same tag when searching.

## Decision

**A comma-separated `GtkEntry`.** SPEC §4.4 allowed chips or a list. A list needs no custom widget, behaves with a keyboard and a screen reader like any entry, and fits the editor's 360 px width. `parse_tag_field` (core) splits on commas and checks each name with `check_tag_name`. Names equal by `name_key` are merged, keeping the first spelling. An empty piece after the *last* comma is ignored, because "maths, " is what is left after typing a comma or accepting a suggestion; any other empty piece ("a,,b") is an error that names its position.

**Suggestions in a popover, not `GtkEntryCompletion`.** `TagSuggestions` is a `GtkPopover` parented to the entry, with a `GtkListBox`. It is not `autohide` (GTK documents that setting as the one that makes a popover modal, grabbing the keyboard focus and closing on Esc), and its rows are not focusable, so **the keyboard focus never leaves the entry**. The entry's own key controller, in the capture phase, handles, only while the list shows (`tags::key_action`, pure and tested): **Down / Up** highlight (stopping at the ends; Up from nothing goes to the last), **Tab** takes the highlighted suggestion, or the first one if none is highlighted, **Enter** takes a highlighted suggestion and otherwise is not taken, so the typed text stays as it is, and **Escape** closes the list only (the dialog needs a second Escape). Nothing is highlighted when the list opens or changes. Enter with nothing highlighted also closes the list, so a later Tab cannot swap the name just confirmed. The list opens only when the text changes by typing, never on a bare cursor move (arrows, a click) and never while the editor fills the field itself (an accepted suggestion, a loaded card): a Tab meant to leave the field must not rewrite a name the user did not touch. The tag typed in full is listed first, so Tab cannot replace it with a look-alike ("Économie" / "Economie" are two tags). So a new tag that a suggestion starts with ("alg" while "algebra" exists) is kept by Enter or a comma, and an existing one is taken with one key, Tab. Keys with Ctrl, Alt or Shift are never taken, so Ctrl+Enter still saves, and Shift+Tab still moves the focus back. Clicking or tapping a row accepts it too. The entry sets the accessible autocomplete property to `list`, announces how many suggestions appeared, and announces "algebra, suggestion 1 of 3" as the highlight moves. Chips (`AdwEntryRow`) were rejected: they need a custom chip widget that screen readers handle poorly. A row of suggestion buttons under the field was rejected: it costs height the two text views need at 360 px.

**Matching ignores case and accents: `search_key`.** NFD, drop combining marks, lower case. "eco" finds "Économie". Letters that do not decompose (ß, ø, ł) stay as they are. Best matches first: names that start with the text, then names with a word that does, then the rest; at most 8; tags already in the field are left out. `search_key` is only for finding. Whether two names are one tag is still `name_key` (ADR 0029), which keeps accents: "Économie" and "Economie" are two tags, and the field will show both while typing.

**One write for the card and its tags.** `add_card_with_tags` and `edit_card_with_tags` do both in one `IMMEDIATE` transaction, checking every name first; a bad name, a missing deck or a deleted card writes nothing. The card ends up with exactly the names given: tags it had that are not named are unlinked, never deleted (ADR 0029). A change to the tags alone does not write the card row, so `updated_at` stays (ADR 0029). `tag_names: None` leaves the tags alone; the editor passes it when it could not read the card's tags (the field is then disabled and says why), so a save can never wipe them.

**Ctrl+Shift+Enter keeps the tags.** The next new card starts with the tags just saved (as the store has them), and they count as that card's saved state, so an untouched next card closes without asking to discard. Comparing for "unchanged" uses `name_key`s in a fixed order: spacing, case and order are not changes.

**The deck page reads cards with their tags** in one call (`list_tagged_cards`: two queries in one read transaction, 30 ms for 5,000 cards with two tags each, within the bound in `perf.rs`). A row shows its tags after the front, dimmed, on one line with an ellipsis; they are plain text (names are untrusted). A screen reader reads "front, tags: maths, algebra".

## Consequences

- While suggestions show, Tab takes one instead of moving the focus on. To leave the field with a list showing, press Escape first (or keep typing until none match). After a suggestion is taken the name being typed is empty, no list shows, and the next Tab moves on as usual.
- The screen reader announcement says so when the list opens ("3 suggestions: Tab takes the first, the arrow keys choose, Escape keeps what you typed").
- The popover needs room under the entry; GTK flips it above when there is none. How it looks with an on-screen keyboard is for the maintainer to check.
- Removing a tag from the last card that has it leaves it suggested from then on, as ADR 0029 intends. There is no way to delete a tag yet.
