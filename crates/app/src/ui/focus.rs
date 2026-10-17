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

/// Keeps `label` (selectable) from showing all of its text selected when
/// the window gives it the focus as its page is first shown. On the startup
/// problem pages it is the only focusable widget, and `GtkWindow`, shown
/// without a focus widget, moves the focus to the first focusable one right
/// after mapping (`gtk_window_show`, gtk/gtkwindow.c). A selectable label
/// then selects all of its text, when the `gtk-label-select-on-focus`
/// setting is on (the default): `gtk_label_grab_focus` (gtk/gtklabel.c, GTK
/// 4.20) chains up to the parent's `grab_focus`, which moves the focus and
/// notifies `has-focus`, and only then calls `gtk_label_select_region (self,
/// 0, -1)`. So the selection is dropped in an idle, after that.
///
/// Armed from the label's first map until the main loop is idle again:
/// only a focus that comes with the page being shown counts. On the recovery
/// page that first focus goes to a Restore button, and a later Tab to the
/// label selects it as GTK does.
pub fn unselect_on_first_focus(label: &gtk::Label) {
    let armed = Rc::new(Cell::new(false));
    let seen = Cell::new(false);
    label.connect_map(glib::clone!(
        #[strong]
        armed,
        move |_| arm_on_first_map(&seen, &armed)
    ));
    label.connect_has_focus_notify(move |label| {
        if label.has_focus() {
            focused(label, &armed);
        }
    });
}

/// On the first map only: arms `armed` until the main loop is idle again.
fn arm_on_first_map(seen: &Cell<bool>, armed: &Rc<Cell<bool>>) {
    if seen.replace(true) {
        return;
    }
    armed.set(true);
    glib::idle_add_local_once(glib::clone!(
        #[strong]
        armed,
        move || armed.set(false)
    ));
}

/// `label` got the focus: while armed (once), its selection is dropped in
/// an idle, after `gtk_label_grab_focus` has made it. Says whether it will
/// be.
fn focused(label: &gtk::Label, armed: &Cell<bool>) -> bool {
    let drop = armed.replace(false);
    if drop {
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            label,
            move || label.select_region(0, 0)
        ));
    }
    drop
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the main loop until it has nothing left to do: the idles have
    /// run.
    fn settle() {
        let context = glib::MainContext::default();
        while context.iteration(false) {}
    }

    /// A selectable label with all of its text selected, as
    /// `gtk_label_grab_focus` leaves it. (The focus itself is not used: in a
    /// test under Xvfb the window is never active, so `has-focus` never
    /// changes; the handlers are driven directly instead.)
    fn selected_label() -> gtk::Label {
        let label = gtk::Label::new(Some("Data folder: /home/someone/.local/share/rappel"));
        label.set_selectable(true);
        label.select_region(0, -1);
        assert!(label.selection_bounds().is_some());
        label
    }

    /// Whether the selection survives the main loop is not asserted where
    /// it should: a selectable label with a selection owns the PRIMARY
    /// selection and loses its selection when another label takes it, which
    /// tests running alongside may do. What the code decides is asserted
    /// instead (`focused` says whether it drops the selection).
    #[test]
    fn only_the_first_focus_while_the_page_is_shown_drops_the_selection() {
        crate::testing::with_gtk(|| {
            let label = selected_label();

            // Armed at the first map, then focused: dropped, after GTK's own
            // selection (in an idle, not at once).
            let armed = Rc::new(Cell::new(false));
            let seen = Cell::new(false);
            arm_on_first_map(&seen, &armed);
            assert!(armed.get());
            assert!(focused(&label, &armed));
            assert!(label.selection_bounds().is_some(), "not at once");
            settle();
            assert_eq!(label.selection_bounds(), None);

            // A second focus keeps GTK's selection.
            assert!(!focused(&label, &armed));

            // The page was shown and its first focus went elsewhere (the
            // recovery page's Restore buttons): the arming ended when the
            // main loop was idle, and a later focus keeps the selection.
            let armed = Rc::new(Cell::new(false));
            let seen = Cell::new(false);
            arm_on_first_map(&seen, &armed);
            settle();
            assert!(!armed.get());
            assert!(!focused(&label, &armed));
            // Mapped again (the page shown again): not armed again.
            arm_on_first_map(&seen, &armed);
            assert!(!armed.get());
        });
    }

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
