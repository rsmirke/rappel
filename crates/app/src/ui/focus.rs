//! Where the keyboard focus goes in a list after a change (pure, so it is
//! unit-tested without GTK), and a selectable label that takes a page's
//! first focus without selecting its text.

use std::cell::Cell;
use std::ops::Sub;

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
/// 4.20) chains up to the parent's `grab_focus` and then calls
/// `gtk_label_select_region (self, 0, -1)`. All of that happens inside
/// `present()`, before the main loop runs again.
///
/// So an idle queued at the label's first map drops the selection if the
/// label is then the window's focus widget. `is_focus` is the focus within
/// the window; `has-focus` would wait for the compositor to activate the
/// window, which comes later (`gtk_widget_has_focus` docs). Only that first
/// focus counts: on the recovery page it goes to a Restore button, and a
/// later Tab to the label selects it as GTK does.
pub fn unselect_on_first_focus(label: &gtk::Label) {
    after_first_map(label, |_| {});
}

/// [`unselect_on_first_focus`], calling `done` from the idle with whether
/// the label had the focus (and so was unselected), for the tests.
fn after_first_map(label: &gtk::Label, done: impl Fn(bool) + 'static) {
    let seen = Cell::new(false);
    let done = std::rc::Rc::new(done);
    label.connect_map(move |label| {
        if seen.replace(true) {
            return;
        }
        let done = done.clone();
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            label,
            move || {
                let focused = label.is_focus();
                if focused {
                    label.select_region(0, 0);
                }
                done(focused);
            }
        ));
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A presented window with a selectable label and, if `button_first`,
    /// a button before it, and what the first-map idle reported.
    fn shown(button_first: bool) -> (gtk::Window, gtk::Label, std::rc::Rc<Cell<Option<bool>>>) {
        let label = gtk::Label::new(Some("Data folder: /home/someone/.local/share/rappel"));
        label.set_selectable(true);
        let report = std::rc::Rc::new(Cell::new(None));
        after_first_map(
            &label,
            glib::clone!(
                #[strong]
                report,
                move |focused| report.set(Some(focused))
            ),
        );
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        if button_first {
            content.append(&gtk::Button::with_label("Restore"));
        }
        content.append(&label);
        let window = gtk::Window::new();
        window.set_child(Some(&content));
        window.present();
        assert!(crate::testing::settle_until(
            || report.get().is_some(),
            Duration::from_secs(5)
        ));
        (window, label, report)
    }

    /// The selection itself cannot be watched across the main loop under
    /// Xvfb: a label's selection claims the PRIMARY selection, which a test
    /// cannot get without an input event's timestamp, so GTK clears it at
    /// the next iteration anyway. The test checks what the code decides
    /// instead: the idle runs after the window's first focus, and finds the
    /// label focused only when it is the first focusable widget.
    #[test]
    fn the_first_focus_given_by_the_window_is_the_one_unselected() {
        crate::testing::with_gtk(|| {
            let (window, label, report) = shown(false);
            assert_eq!(
                report.get(),
                Some(true),
                "the window focused the label as it was shown"
            );
            assert!(label.is_focus());
            // Selected by GTK at once, so the idle had something to drop.
            GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            assert!(label.grab_focus());
            assert!(
                label.selection_bounds().is_some(),
                "a later focus selects, as GTK does"
            );
            // Mapped again: not checked again.
            report.set(None);
            label.unmap();
            label.map();
            crate::testing::settle_until(|| false, Duration::from_millis(20));
            assert_eq!(report.get(), None);
            window.destroy();

            // The first focus goes elsewhere, as on the recovery page with
            // backups: nothing is unselected.
            let (window, label, report) = shown(true);
            assert_eq!(report.get(), Some(false));
            assert!(!label.is_focus());
            window.destroy();
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
