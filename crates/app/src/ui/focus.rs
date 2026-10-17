//! Where the keyboard focus goes in a list after a change (pure, so it is
//! unit-tested without GTK), and a selectable label that takes a page's
//! first focus without selecting its text.

use std::cell::Cell;
use std::ops::Sub;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// The row to focus after the row at `removed` was taken out, leaving
/// `remaining` rows: the row now at that position, or the last row if it
/// was the last. `None` when the list is empty: focus the empty state's
/// button instead.
pub fn index_after_removal<T>(removed: T, remaining: T) -> Option<T>
where
    T: Copy + Ord + Sub<Output = T> + From<u8>,
{
    (remaining > T::from(0)).then(|| removed.min(remaining - T::from(1)))
}

/// Keeps `label` (selectable) from showing all of its text selected when it
/// gets the focus the first time. On a page where it is the only focusable
/// widget (the startup problem pages), the window gives it the focus when
/// it is shown, and a selectable label selects all of its text when it
/// gets the focus if the `gtk-label-select-on-focus` setting is on (the
/// default; `GtkSettings` docs), so the page would open with its path
/// highlighted. The selection is dropped in an idle, because the label
/// selects its text only after the focus has moved to it. Later focus, from
/// Tab, keeps GTK's behaviour.
pub fn unselect_on_first_focus(label: &gtk::Label) {
    let first = Rc::new(Cell::new(true));
    label.connect_has_focus_notify(move |label| {
        if label.has_focus() && first.replace(false) {
            glib::idle_add_local_once(glib::clone!(
                #[weak]
                label,
                move || label.select_region(0, 0)
            ));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_row_takes_the_place_of_the_removed_one() {
        assert_eq!(index_after_removal(0_usize, 3), Some(0));
        assert_eq!(index_after_removal(1_u32, 3), Some(1));
    }

    #[test]
    fn removing_the_last_row_focuses_the_new_last_row() {
        assert_eq!(index_after_removal(3_usize, 3), Some(2));
        assert_eq!(index_after_removal(1_u32, 1), Some(0));
    }

    #[test]
    fn an_empty_list_has_no_row_to_focus() {
        assert_eq!(index_after_removal(0_usize, 0), None);
        assert_eq!(index_after_removal(5_u32, 0), None);
    }
}
