//! Every text and small decision of the All Cards page that needs no GTK:
//! row captions and labels, counts, toast texts, which empty state to show
//! and what it says, and what Escape, Space and typing in the list do. Pure, so it is unit-tested (SPEC §4.6, ADR 0036).

use rappel_core::storage::{BrowseQuery, CardStatus, StateFilter};

use crate::ui::text::{count, group_digits, one_line};
use crate::ui::time::relative_due;

/// A card's status as a short phrase: "New", "Learning", "Due now",
/// "Overdue by 2 days", "Due in 3 days", "Suspended".
pub fn status_text(status: CardStatus, now: i64) -> String {
    match status {
        CardStatus::New => "New".to_owned(),
        CardStatus::Learning(_) => "Learning".to_owned(),
        CardStatus::Suspended => "Suspended".to_owned(),
        CardStatus::Due(due) => capitalised(&relative_due(due, now)),
        CardStatus::Scheduled(due) => format!("Due {}", relative_due(due, now)),
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The third line of an All Cards row: "Maths · Due in 3 days". The tags
/// are pills on the line above (`widgets::card_row`).
pub fn caption(deck: &str, status: &str) -> String {
    format!("{} · {status}", one_line(deck))
}

/// What a screen reader says for an All Cards row: the card as the deck
/// page says it (`card_row::text::row_label`, without "suspended", which
/// the status says), then the deck and the status, and, while choosing
/// cards, whether the row is chosen: "2 + 2, back: 4, deck: Maths, Due in
/// 3 days, selected".
pub fn row_label(card: &str, deck: &str, status: &str, selected: Option<bool>) -> String {
    let base = format!("{card}, deck: {}, {status}", one_line(deck));
    match selected {
        Some(true) => format!("{base}, selected"),
        Some(false) => format!("{base}, not selected"),
        None => base,
    }
}

/// "1,234 cards", "1 card", "No cards".
pub fn count_text(n: usize) -> String {
    if n == 0 {
        "No cards".to_owned()
    } else {
        count(n, "card", "cards")
    }
}

/// The header's title in selection mode (HIG, "Selection mode": the
/// heading says how many items are chosen): "3 Selected", or "Select
/// Cards" before the first.
pub fn selection_title(n: usize) -> String {
    if n == 0 {
        "Select Cards".to_owned()
    } else {
        format!("{} Selected", group_digits(n))
    }
}

/// Which way the Suspend button of the action bar goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendChoice {
    Suspend,
    Unsuspend,
}

/// Unsuspend only when every chosen card is suspended; otherwise Suspend,
/// which leaves the suspended ones as they are.
pub fn suspend_choice(suspended: impl IntoIterator<Item = bool>) -> SuspendChoice {
    let mut any = false;
    for flag in suspended {
        if !flag {
            return SuspendChoice::Suspend;
        }
        any = true;
    }
    if any {
        SuspendChoice::Unsuspend
    } else {
        SuspendChoice::Suspend
    }
}

/// What the list area shows when it has no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Empty {
    /// There are rows.
    #[default]
    No,
    /// The collection has no card at all.
    NoCards,
    /// A search or filter matches nothing.
    NoResults,
}

/// The status page of an [`Empty`] state: icon, title and its one
/// sentence.
pub fn empty_page(empty: Empty) -> Option<(&'static str, &'static str, &'static str)> {
    match empty {
        Empty::No => None,
        Empty::NoCards => Some((
            "document-new-symbolic",
            "No Cards Yet",
            "Cards you add to your decks will show up here",
        )),
        Empty::NoResults => Some((
            "edit-find-symbolic",
            "No Results Found",
            "Try a different search or other filters",
        )),
    }
}

/// The button of the "No Cards Yet" state: back to the deck list, where
/// decks are made and cards added.
pub const GO_TO_DECKS: &str = "_Go to Decks";

/// The button of the "No Results Found" state, naming what it clears:
/// "Clear Search", "Clear Filters" or "Clear Search and Filters".
pub fn clear_label(searched: bool, filtered: bool) -> &'static str {
    match (searched, filtered) {
        (true, false) => "_Clear Search",
        (false, true) => "_Clear Filters",
        _ => "_Clear Search and Filters",
    }
}

/// Whether `query` has words, and whether it has a filter (deck, tag or
/// state), for [`clear_label`].
pub fn searched_and_filtered(query: &BrowseQuery) -> (bool, bool) {
    (
        !query.text.trim().is_empty(),
        query.deck_id.is_some() || query.tag_id.is_some() || query.state.is_some(),
    )
}

/// `shown` rows came back; `filtered` says whether a search text or a filter
/// was set, `collection_empty` whether there is no live card at all
/// (whatever the filters). An empty collection is "No Cards Yet" even with
/// a filter set (opened from an empty deck), so the page does not say "No
/// Results Found" first and then change its mind.
pub fn empty_state(shown: usize, filtered: bool, collection_empty: bool) -> Empty {
    if shown > 0 {
        Empty::No
    } else if collection_empty || !filtered {
        Empty::NoCards
    } else {
        Empty::NoResults
    }
}

/// Whether the filter bar shows: not while there is no card at all, when
/// there is nothing to narrow.
pub fn filters_shown(empty: Empty) -> bool {
    empty != Empty::NoCards
}

/// A key of the card list's own (besides Escape, [`ESCAPE_KEY`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKey {
    /// Ctrl+A: choose every card shown.
    SelectAll,
    /// Space, as [`space_action`] decides.
    Space,
}

/// The card list's keys: a key, whether it is taken only with Ctrl (`true`)
/// or only without it (`false`), and what it is. Other modifiers do not
/// matter. The Keyboard Shortcuts dialog lists them (a test checks it).
pub const LIST_KEYS: [(gtk::gdk::Key, bool, ListKey); 3] = [
    (gtk::gdk::Key::a, true, ListKey::SelectAll),
    (gtk::gdk::Key::A, true, ListKey::SelectAll),
    (gtk::gdk::Key::space, false, ListKey::Space),
];

/// The key that moves from the search field to the list, whatever the
/// modifiers.
pub const TO_LIST_KEY: gtk::gdk::Key = gtk::gdk::Key::Down;

/// The key [`escape_action`] is about, whatever the modifiers.
pub const ESCAPE_KEY: gtk::gdk::Key = gtk::gdk::Key::Escape;

/// Which of [`LIST_KEYS`] `key` with `modifiers` is, if any.
pub fn list_key(key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> Option<ListKey> {
    let ctrl = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
    LIST_KEYS
        .iter()
        .find(|&&(bound, needs_ctrl, _)| bound == key && needs_ctrl == ctrl)
        .map(|&(_, _, which)| which)
}

/// What Space does on the card list (capture phase, before the list's own
/// keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceAction {
    /// Choose or drop the focused card.
    Toggle,
    /// Leave it to the focused widget (a row's menu button opens its menu;
    /// a row outside selection mode does nothing). Space never starts a
    /// search: the page's key capture leaves it (`types_into_search`), as
    /// GTK's own does (gtksearchentry.c, `capture_widget_key_handled`).
    Pass,
}

/// Space chooses the focused card in selection mode when the row itself
/// (not its menu button) has the focus.
pub fn space_action(selecting: bool, row_focused: bool) -> SpaceAction {
    if selecting && row_focused {
        SpaceAction::Toggle
    } else {
        SpaceAction::Pass
    }
}

/// Whether a key pressed in the list or the status page goes into the
/// search field (`search_empty`: the field has no text yet):
/// - a key that types a character, without Ctrl or Alt;
/// - a dead key or Compose (`Multi_key`): the field takes the focus and
///   its input method composes the key that follows (Compose s s → ß);
/// - Backspace, when there is text to delete (GTK's own key capture
///   forwards it too).
///
/// Never while a popover (a row's menu, a drop-down's popup) is open, nor
/// while cards are being chosen (the field is hidden then). Space, Enter,
/// Tab, Escape and the movement keys stay with the focused widget, as in
/// GTK's own key capture (gtksearchentry.c, `gtk_search_entry_is_keynav`
/// and `capture_widget_key_handled`).
pub fn types_into_search(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
    in_popover: bool,
    selecting: bool,
    search_empty: bool,
) -> bool {
    use gtk::gdk::{Key, ModifierType};
    use gtk::glib::translate::IntoGlib;
    if in_popover
        || selecting
        || modifiers.intersects(ModifierType::CONTROL_MASK | ModifierType::ALT_MASK)
        || matches!(
            key,
            Key::space | Key::KP_Space | Key::Return | Key::KP_Enter | Key::ISO_Enter | Key::Menu
        )
    {
        return false;
    }
    if key == Key::BackSpace {
        return !search_empty;
    }
    // The dead keys are one range of keysyms, dead_grave (0xfe50) to
    // dead_longsolidusoverlay (0xfe93) (gdk/gdkkeysyms.h).
    let dead = (Key::dead_grave.into_glib()..=Key::dead_longsolidusoverlay.into_glib())
        .contains(&key.into_glib());
    if dead || key == Key::Multi_key {
        return true;
    }
    key.to_unicode().is_some_and(|c| !c.is_control())
}

/// What Escape does while the focus is in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeAction {
    LeaveSelection,
    ClearSearch,
    Nothing,
}

/// Escape first leaves the selection mode (the mode you are in), then
/// clears the search and filters. In the search entry, the entry's own
/// `stop-search` clears the text first.
pub fn escape_action(selecting: bool, filtered: bool) -> EscapeAction {
    if selecting {
        EscapeAction::LeaveSelection
    } else if filtered {
        EscapeAction::ClearSearch
    } else {
        EscapeAction::Nothing
    }
}

/// Which entry of a filter drop-down (entry 0 is "All …") names an item:
/// the index into the `len` items, or `None` for "All …" or no selection.
pub fn filter_choice(selected: u32, len: usize) -> Option<usize> {
    let index = usize::try_from(selected).ok()?.checked_sub(1)?;
    (index < len).then_some(index)
}

/// The state drop-down's entries: Any State, New, Learning, Due, Suspended.
pub fn state_filter(selected: u32) -> Option<StateFilter> {
    match selected {
        1 => Some(StateFilter::New),
        2 => Some(StateFilter::Learning),
        3 => Some(StateFilter::Due),
        4 => Some(StateFilter::Suspended),
        _ => None,
    }
}

/// Whether `query` asks for anything (words or a filter).
pub fn is_filtered(query: &BrowseQuery) -> bool {
    let (searched, filtered) = searched_and_filtered(query);
    searched || filtered
}

/// The note after an action found that a card of the list was gone.
pub fn stale_text() -> String {
    "The list was out of date; nothing was changed".to_owned()
}

/// The first entry of the deck and tag drop-downs.
pub const ALL_DECKS: &str = "All Decks";
pub const ALL_TAGS: &str = "All Tags";

/// The Suspend button's word (also its tooltip, like the other action
/// buttons') and accessible label for `choice`.
pub fn suspend_labels(choice: SuspendChoice) -> (&'static str, &'static str) {
    match choice {
        SuspendChoice::Suspend => ("Suspend", "Suspend selected cards"),
        SuspendChoice::Unsuspend => ("Unsuspend", "Unsuspend selected cards"),
    }
}

/// The body of the Move dialog.
pub fn move_body(n: usize) -> String {
    format!("Choose the deck to move {} to.", count_text(n))
}

/// The body of the Remove Tag dialog.
pub fn remove_tag_body(n: usize) -> String {
    format!("Choose the tag to take off {}.", count_text(n))
}

/// The notice when the chosen cards have no tag to take off.
pub fn no_tags_text() -> String {
    "The chosen cards have no tags".to_owned()
}

/// "3 cards" / "1 card".
fn cards(n: usize) -> String {
    count(n, "card", "cards")
}

pub fn moved_text(n: usize, deck: &str) -> String {
    format!("{} moved to {}", cards(n), one_line(deck))
}

pub fn tag_added_text(n: usize, tag: &str) -> String {
    format!("Tag “{}” added to {}", one_line(tag), cards(n))
}

pub fn tag_removed_text(n: usize, tag: &str) -> String {
    format!("Tag “{}” removed from {}", one_line(tag), cards(n))
}

pub fn suspended_text(n: usize, suspended: bool) -> String {
    let what = if suspended {
        "suspended"
    } else {
        "unsuspended"
    };
    format!("{} {what}", cards(n))
}

pub fn reset_text(n: usize) -> String {
    format!("Progress reset for {}", cards(n))
}

pub fn deleted_text(n: usize) -> String {
    format!("{} deleted", cards(n))
}

/// What an action did when every card was already as asked.
pub fn unchanged_text() -> String {
    "Nothing changed; the cards were already like that".to_owned()
}

/// The note after an Undo that could not change every card (`done` of
/// `total`), or `None` if it did.
pub fn partial_undo_text(done: usize, total: usize) -> Option<String> {
    (done < total).then(|| {
        format!(
            "{} of {} restored; the others changed since",
            group_digits(done),
            cards(total)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000_000;
    const DAY: i64 = 86_400_000;

    #[test]
    fn the_list_keys() {
        use gtk::gdk::{Key, ModifierType};
        let ctrl = ModifierType::CONTROL_MASK;
        let none = ModifierType::empty();
        assert_eq!(list_key(Key::a, ctrl), Some(ListKey::SelectAll));
        assert_eq!(
            list_key(Key::A, ctrl | ModifierType::SHIFT_MASK),
            Some(ListKey::SelectAll)
        );
        assert_eq!(
            list_key(Key::a, none),
            None,
            "a plain a types into the search"
        );
        assert_eq!(list_key(Key::space, none), Some(ListKey::Space));
        assert_eq!(
            list_key(Key::space, ModifierType::SHIFT_MASK),
            Some(ListKey::Space)
        );
        assert_eq!(list_key(Key::space, ctrl), None);
        assert_eq!(list_key(Key::Return, none), None);
    }

    #[test]
    fn a_status_reads_as_a_phrase() {
        assert_eq!(status_text(CardStatus::New, NOW), "New");
        assert_eq!(status_text(CardStatus::Learning(NOW), NOW), "Learning");
        assert_eq!(status_text(CardStatus::Suspended, NOW), "Suspended");
        assert_eq!(status_text(CardStatus::Due(NOW - 1), NOW), "Due now");
        assert_eq!(
            status_text(CardStatus::Due(NOW - 2 * DAY), NOW),
            "Overdue by 2 days"
        );
        assert_eq!(
            status_text(CardStatus::Scheduled(NOW + 3 * DAY), NOW),
            "Due in 3 days"
        );
    }

    #[test]
    fn a_caption_says_the_deck_and_the_status() {
        assert_eq!(caption("Maths", "New"), "Maths · New");
        assert_eq!(caption("a\n b", "Due now"), "a b · Due now");
    }

    #[test]
    fn a_row_is_read_with_its_deck_status_and_selection() {
        assert_eq!(
            row_label("2 + 2, back: 4", "Maths", "New", None),
            "2 + 2, back: 4, deck: Maths, New"
        );
        assert_eq!(
            row_label("2 + 2", "a\n b", "Suspended", Some(true)),
            "2 + 2, deck: a b, Suspended, selected"
        );
        assert_eq!(
            row_label("2 + 2", "Maths", "New", Some(false)),
            "2 + 2, deck: Maths, New, not selected"
        );
    }

    #[test]
    fn empty_states_have_a_title_a_sentence_and_a_button() {
        assert_eq!(empty_page(Empty::No), None);
        let (icon, title, body) = empty_page(Empty::NoCards).unwrap();
        assert_eq!(title, "No Cards Yet");
        assert_eq!(body, "Cards you add to your decks will show up here");
        assert!(icon.ends_with("-symbolic"));
        let (_, title, body) = empty_page(Empty::NoResults).unwrap();
        assert_eq!(title, "No Results Found");
        assert_eq!(body, "Try a different search or other filters");
        assert_eq!(GO_TO_DECKS, "_Go to Decks");
    }

    #[test]
    fn the_clear_button_names_what_it_clears() {
        let mut q = BrowseQuery {
            text: "x".to_owned(),
            ..BrowseQuery::default()
        };
        assert_eq!(searched_and_filtered(&q), (true, false));
        assert_eq!(clear_label(true, false), "_Clear Search");
        q.state = Some(StateFilter::New);
        assert_eq!(searched_and_filtered(&q), (true, true));
        assert_eq!(clear_label(true, true), "_Clear Search and Filters");
        q.text = " ".to_owned();
        assert_eq!(searched_and_filtered(&q), (false, true));
        assert_eq!(clear_label(false, true), "_Clear Filters");
    }

    #[test]
    fn counts_and_titles() {
        assert_eq!(count_text(0), "No cards");
        assert_eq!(count_text(1), "1 card");
        assert_eq!(count_text(5_000), "5,000 cards");
        assert_eq!(selection_title(0), "Select Cards");
        assert_eq!(selection_title(3), "3 Selected");
        assert_eq!(selection_title(1_200), "1,200 Selected");
    }

    #[test]
    fn unsuspend_is_offered_only_when_every_card_is_suspended() {
        use SuspendChoice::{Suspend, Unsuspend};
        assert_eq!(suspend_choice([true, true]), Unsuspend);
        assert_eq!(suspend_choice([true, false]), Suspend);
        assert_eq!(suspend_choice([false]), Suspend);
        assert_eq!(suspend_choice([]), Suspend);
    }

    #[test]
    fn the_empty_state_depends_on_the_collection_then_on_what_was_asked() {
        assert_eq!(empty_state(0, false, true), Empty::NoCards);
        assert_eq!(empty_state(0, true, false), Empty::NoResults);
        // Opened from an empty deck in an empty collection: the deck
        // filter is set, but there is nothing to find anywhere.
        assert_eq!(empty_state(0, true, true), Empty::NoCards);
        assert_eq!(empty_state(1, true, false), Empty::No);
        assert_eq!(empty_state(1, false, false), Empty::No);
        assert!(!filters_shown(Empty::NoCards));
        assert!(filters_shown(Empty::NoResults));
        assert!(filters_shown(Empty::No));
    }

    #[test]
    fn space_chooses_only_a_focused_row_in_selection_mode() {
        assert_eq!(space_action(true, true), SpaceAction::Toggle);
        assert_eq!(space_action(true, false), SpaceAction::Pass);
        assert_eq!(space_action(false, true), SpaceAction::Pass);
        assert_eq!(space_action(false, false), SpaceAction::Pass);
    }

    #[test]
    fn only_typing_keys_go_into_the_search() {
        use gtk::gdk::{Key, ModifierType};
        let none = ModifierType::empty();
        let types = |key, modifiers, in_popover, selecting| {
            types_into_search(key, modifiers, in_popover, selecting, true)
        };
        assert!(types(Key::a, none, false, false));
        assert!(types(Key::A, ModifierType::SHIFT_MASK, false, false));
        assert!(types(Key::eacute, none, false, false));
        assert!(types(Key::_7, none, false, false));
        // A popover open (a row's menu, the State popup), or choosing.
        assert!(!types(Key::a, none, true, false));
        assert!(!types(Key::a, none, false, true));
        // Shortcuts and keys that move or activate.
        assert!(!types(Key::a, ModifierType::CONTROL_MASK, false, false));
        assert!(!types(Key::a, ModifierType::ALT_MASK, false, false));
        for key in [
            Key::space,
            Key::Return,
            Key::KP_Enter,
            Key::Tab,
            Key::Escape,
            Key::Down,
            Key::Home,
            Key::Page_Down,
            Key::BackSpace,
            Key::Delete,
            Key::F1,
            Key::Menu,
        ] {
            assert!(!types(key, none, false, false), "{key:?}");
        }
        // Dead keys and Compose start a search, so the field's input
        // method composes what follows.
        for key in [
            Key::Multi_key,
            Key::dead_acute,
            Key::dead_grave,
            Key::dead_circumflex,
            Key::dead_longsolidusoverlay,
        ] {
            assert!(types(key, none, false, false), "{key:?}");
            assert!(!types(key, none, true, false), "{key:?} in a popover");
        }
        // Backspace only when there is text to delete.
        assert!(!types_into_search(Key::BackSpace, none, false, false, true));
        assert!(types_into_search(Key::BackSpace, none, false, false, false));
        assert!(!types_into_search(Key::BackSpace, none, false, true, false));
        assert!(!types_into_search(Key::BackSpace, none, true, false, false));
    }

    #[test]
    fn escape_leaves_the_selection_before_it_clears_the_search() {
        assert_eq!(escape_action(true, true), EscapeAction::LeaveSelection);
        assert_eq!(escape_action(false, true), EscapeAction::ClearSearch);
        assert_eq!(escape_action(false, false), EscapeAction::Nothing);
    }

    #[test]
    fn filter_entries_map_to_items_and_states() {
        assert_eq!(filter_choice(0, 3), None);
        assert_eq!(filter_choice(1, 3), Some(0));
        assert_eq!(filter_choice(3, 3), Some(2));
        assert_eq!(filter_choice(4, 3), None);
        // GTK_INVALID_LIST_POSITION
        assert_eq!(filter_choice(u32::MAX, 3), None);
        assert_eq!(state_filter(0), None);
        assert_eq!(state_filter(1), Some(StateFilter::New));
        assert_eq!(state_filter(4), Some(StateFilter::Suspended));
        assert_eq!(state_filter(u32::MAX), None);
    }

    #[test]
    fn a_query_is_filtered_by_words_or_any_filter() {
        let mut q = BrowseQuery::default();
        assert!(!is_filtered(&q));
        q.text = "  ".to_owned();
        assert!(!is_filtered(&q));
        q.text = "x".to_owned();
        assert!(is_filtered(&q));
        q = BrowseQuery {
            state: Some(StateFilter::Due),
            ..BrowseQuery::default()
        };
        assert!(is_filtered(&q));
    }

    #[test]
    fn dialog_and_button_texts() {
        assert_eq!(move_body(3), "Choose the deck to move 3 cards to.");
        assert_eq!(remove_tag_body(1), "Choose the tag to take off 1 card.");
        assert_eq!(
            suspend_labels(SuspendChoice::Unsuspend),
            ("Unsuspend", "Unsuspend selected cards")
        );
    }

    #[test]
    fn toasts_count_and_name() {
        assert_eq!(moved_text(3, "Maths"), "3 cards moved to Maths");
        assert_eq!(moved_text(1, "a\n b"), "1 card moved to a b");
        assert_eq!(tag_added_text(2, "hard"), "Tag “hard” added to 2 cards");
        assert_eq!(
            tag_removed_text(1, "hard"),
            "Tag “hard” removed from 1 card"
        );
        assert_eq!(suspended_text(2, true), "2 cards suspended");
        assert_eq!(suspended_text(1, false), "1 card unsuspended");
        assert_eq!(reset_text(1_000), "Progress reset for 1,000 cards");
        assert_eq!(deleted_text(1), "1 card deleted");
        assert_eq!(
            unchanged_text(),
            "Nothing changed; the cards were already like that"
        );
    }

    #[test]
    fn a_partial_undo_says_how_much_came_back() {
        assert_eq!(partial_undo_text(3, 3), None);
        assert_eq!(
            partial_undo_text(2, 3).as_deref(),
            Some("2 of 3 cards restored; the others changed since")
        );
        assert_eq!(
            partial_undo_text(0, 1).as_deref(),
            Some("0 of 1 card restored; the others changed since")
        );
    }
}
