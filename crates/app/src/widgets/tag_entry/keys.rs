//! What the keys of a tag entry do, where the keyboard focus goes, and what
//! a screen reader is told, decided without GTK (ADR 0031, ADR 0043).

/// A key that moves the highlight in the suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
}

/// The highlighted row after `movement`, among `len` rows. It stops at the
/// first and the last. `None` when there are no rows.
pub fn highlight_after(movement: Move, current: Option<usize>, len: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match (movement, current) {
        (Move::Down, None) => 0,
        (Move::Down, Some(i)) => (i + 1).min(last),
        (Move::Up, None) => last,
        (Move::Up, Some(i)) => i.saturating_sub(1),
    })
}

/// A key the text of a tag entry treats specially.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKey {
    Down,
    Up,
    Enter,
    Tab,
    Escape,
    Backspace,
}

/// What the entry is showing when a key is pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntryState {
    /// Suggestions are listed under the field.
    pub showing: bool,
    /// The suggestion highlighted with the arrow keys.
    pub highlighted: Option<usize>,
    /// The text holds nothing but spaces.
    pub blank: bool,
    /// The cursor is at the start, with nothing selected.
    pub at_start: bool,
    /// How many pills there are.
    pub pills: usize,
}

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Move the highlight.
    Highlight(Move),
    /// Make suggestion `n` a pill in place of the name being typed.
    Accept(usize),
    /// Close the list and keep the text.
    Close,
    /// Close the list, if any, and make the typed name a pill.
    Commit,
    /// Move the focus to the last pill's remove button.
    FocusLastPill,
    /// Not ours: the text (or the dialog) handles the key.
    Pass,
}

/// The keys of a tag entry (ADR 0031, ADR 0043). While suggestions show,
/// nothing is highlighted until Down or Up is pressed; **Tab** takes the
/// highlighted suggestion, or the first one if none is highlighted;
/// **Enter** takes a highlighted suggestion, and otherwise makes the typed
/// name a pill; **Escape** closes the list. Without a list, **Enter** makes
/// the typed name a pill, and **Backspace** in an empty entry moves to the
/// last pill (a second Backspace removes it, [`after_removal`]), so a tag
/// is never removed by a key that would otherwise do nothing. Every other
/// key is the text's own (Tab moves the focus on, Escape closes the
/// dialog).
pub fn key_action(key: EntryKey, state: EntryState) -> KeyAction {
    if state.showing {
        return match key {
            EntryKey::Down => KeyAction::Highlight(Move::Down),
            EntryKey::Up => KeyAction::Highlight(Move::Up),
            EntryKey::Enter => state
                .highlighted
                .map_or(KeyAction::Commit, KeyAction::Accept),
            EntryKey::Tab => KeyAction::Accept(state.highlighted.unwrap_or(0)),
            EntryKey::Escape => KeyAction::Close,
            EntryKey::Backspace => KeyAction::Pass,
        };
    }
    match key {
        EntryKey::Enter if !state.blank => KeyAction::Commit,
        EntryKey::Backspace if state.at_start && state.blank && state.pills > 0 => {
            KeyAction::FocusLastPill
        }
        _ => KeyAction::Pass,
    }
}

/// Where the focus goes after the pill at `index` is removed, leaving
/// `remaining` pills: `Some(i)` is pill `i`'s remove button, `None` the
/// entry. Backspace walks back, to the pill before (or the first one, if
/// the first was removed); a click, Enter, Space or Delete goes to the pill
/// that took its place, else the last one, as in a list
/// (`ui::focus::index_after_removal`). With no pills left, the entry.
pub fn after_removal(index: usize, remaining: usize, backspace: bool) -> Option<usize> {
    if backspace {
        (remaining > 0).then(|| index.saturating_sub(1).min(remaining - 1))
    } else {
        crate::ui::focus::index_after_removal(index, remaining)
    }
}

/// The next stop of Tab (`forward`) or Shift+Tab inside a tag entry, whose
/// stops are the text (0) and then each pill's remove button (1, 2, …):
/// `len` stops in all. `current` is the stop with the focus, `None` when it
/// comes from outside. `None` means the focus leaves the entry. The text
/// comes first though it is drawn after the pills: adding a tag is what the
/// field is mostly for.
pub fn tab_stop(current: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    let last = len.checked_sub(1)?;
    match (current, forward) {
        (None, true) => Some(0),
        (None, false) => Some(last),
        (Some(i), true) => (i < last).then_some(i + 1),
        (Some(i), false) => i.checked_sub(1),
    }
}

/// Whether the typed name becomes a pill once the focus has left the
/// field: only if the window is still `active` (not just sent to the
/// background) and the focus is not `inside` the field after all (the
/// text, a pill, one of the text's popovers).
pub fn should_split(active: bool, inside: bool) -> bool {
    active && !inside
}

/// What a screen reader is told when the highlight lands on `name`.
pub fn highlight_announcement(name: &str, index: usize, len: usize) -> String {
    format!("{name}, suggestion {} of {len}", index + 1)
}

/// What a screen reader is told when suggestions appear.
pub fn suggestions_announcement(len: usize) -> String {
    format!(
        "{}: Tab takes the first, the arrow keys choose, Escape keeps what you typed",
        crate::ui::text::count(len, "suggestion", "suggestions")
    )
}

/// What a screen reader is told when pills are added: "Tag maths added",
/// "Tags a, b added".
pub fn added_announcement(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [name] => Some(format!("Tag {name} added")),
        _ => Some(format!("Tags {} added", names.join(", "))),
    }
}

/// What a screen reader is told when a pill is removed.
pub fn removed_announcement(name: &str) -> String {
    format!("Tag {name} removed")
}

/// The accessible label of a pill's remove button. Its tooltip is the
/// shorter "Remove Tag", next to the name it is drawn beside.
pub fn remove_label(name: &str) -> String {
    format!("Remove tag {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn showing(highlighted: Option<usize>) -> EntryState {
        EntryState {
            showing: true,
            highlighted,
            ..EntryState::default()
        }
    }

    fn typed(text: &str, pills: usize) -> EntryState {
        EntryState {
            blank: text.trim().is_empty(),
            at_start: text.is_empty(),
            pills,
            ..EntryState::default()
        }
    }

    #[test]
    fn the_highlight_stops_at_both_ends() {
        use Move::{Down, Up};
        assert_eq!(highlight_after(Down, None, 0), None);
        assert_eq!(highlight_after(Up, Some(0), 0), None);
        assert_eq!(highlight_after(Down, None, 3), Some(0));
        assert_eq!(highlight_after(Down, Some(0), 3), Some(1));
        assert_eq!(highlight_after(Down, Some(2), 3), Some(2));
        assert_eq!(highlight_after(Up, Some(2), 3), Some(1));
        assert_eq!(highlight_after(Up, Some(0), 3), Some(0));
        assert_eq!(highlight_after(Up, None, 3), Some(2));
    }

    #[test]
    fn tab_takes_the_first_or_the_highlighted_suggestion() {
        use EntryKey::Tab;
        assert_eq!(key_action(Tab, showing(None)), KeyAction::Accept(0));
        assert_eq!(key_action(Tab, showing(Some(2))), KeyAction::Accept(2));
        // No list: Tab moves the focus on.
        assert_eq!(key_action(Tab, typed("alg", 0)), KeyAction::Pass);
    }

    #[test]
    fn enter_takes_a_highlighted_suggestion_or_makes_the_typed_name_a_pill() {
        use EntryKey::Enter;
        assert_eq!(key_action(Enter, showing(None)), KeyAction::Commit);
        assert_eq!(key_action(Enter, showing(Some(1))), KeyAction::Accept(1));
        assert_eq!(key_action(Enter, typed("alg", 2)), KeyAction::Commit);
        // Nothing typed: not ours.
        assert_eq!(key_action(Enter, typed(" ", 2)), KeyAction::Pass);
    }

    #[test]
    fn the_arrows_highlight_and_escape_closes() {
        use EntryKey::{Backspace, Down, Escape, Up};
        assert_eq!(
            key_action(Down, showing(None)),
            KeyAction::Highlight(Move::Down)
        );
        assert_eq!(
            key_action(Up, showing(Some(0))),
            KeyAction::Highlight(Move::Up)
        );
        assert_eq!(key_action(Escape, showing(Some(0))), KeyAction::Close);
        // Backspace edits the name typed.
        assert_eq!(key_action(Backspace, showing(None)), KeyAction::Pass);
        for key in [Down, Up, Escape] {
            assert_eq!(key_action(key, typed("a", 1)), KeyAction::Pass);
        }
    }

    #[test]
    fn backspace_in_an_empty_entry_goes_to_the_last_pill() {
        use EntryKey::Backspace;
        assert_eq!(
            key_action(Backspace, typed("", 2)),
            KeyAction::FocusLastPill
        );
        assert_eq!(key_action(Backspace, typed("", 0)), KeyAction::Pass);
        assert_eq!(key_action(Backspace, typed("a", 2)), KeyAction::Pass);
        // Spaces before the cursor are the text's to delete.
        let spaces = EntryState {
            blank: true,
            at_start: false,
            pills: 2,
            ..EntryState::default()
        };
        assert_eq!(key_action(Backspace, spaces), KeyAction::Pass);
    }

    #[test]
    fn backspace_walks_back_through_the_pills() {
        // Removing pill 2 of 3 (index 2, 2 left): the one before it.
        assert_eq!(after_removal(2, 2, true), Some(1));
        assert_eq!(after_removal(1, 2, true), Some(0));
        // The first: the new first.
        assert_eq!(after_removal(0, 2, true), Some(0));
        assert_eq!(after_removal(0, 0, true), None);
    }

    #[test]
    fn a_click_or_delete_goes_to_the_pill_that_took_its_place() {
        assert_eq!(after_removal(0, 2, false), Some(0));
        assert_eq!(after_removal(1, 2, false), Some(1));
        assert_eq!(after_removal(2, 2, false), Some(1));
        assert_eq!(after_removal(0, 0, false), None);
    }

    #[test]
    fn tab_goes_from_the_text_to_the_pills_and_out() {
        // The text and two pills.
        assert_eq!(tab_stop(None, 3, true), Some(0));
        assert_eq!(tab_stop(Some(0), 3, true), Some(1));
        assert_eq!(tab_stop(Some(2), 3, true), None);
        assert_eq!(tab_stop(None, 3, false), Some(2));
        assert_eq!(tab_stop(Some(1), 3, false), Some(0));
        assert_eq!(tab_stop(Some(0), 3, false), None);
        assert_eq!(tab_stop(None, 0, true), None);
    }

    #[test]
    fn the_name_becomes_a_pill_only_when_the_focus_really_left() {
        assert!(should_split(true, false));
        assert!(!should_split(true, true));
        assert!(!should_split(false, false));
        assert!(!should_split(false, true));
    }

    #[test]
    fn announcements_say_what_happened() {
        assert_eq!(
            highlight_announcement("algebra", 0, 3),
            "algebra, suggestion 1 of 3"
        );
        assert_eq!(
            suggestions_announcement(1),
            "1 suggestion: Tab takes the first, the arrow keys choose, Escape keeps what you typed"
        );
        assert_eq!(
            suggestions_announcement(3),
            "3 suggestions: Tab takes the first, the arrow keys choose, Escape keeps what you typed"
        );
        assert_eq!(added_announcement(&[]), None);
        assert_eq!(
            added_announcement(&["maths".to_owned()]).as_deref(),
            Some("Tag maths added")
        );
        assert_eq!(
            added_announcement(&["a".to_owned(), "b".to_owned()]).as_deref(),
            Some("Tags a, b added")
        );
        assert_eq!(removed_announcement("maths"), "Tag maths removed");
        assert_eq!(remove_label("maths"), "Remove tag maths");
    }
}
