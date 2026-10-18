//! Keeping the focused widget on screen when the window gets shorter: on a
//! phone the on-screen keyboard shrinks the window (Phosh and other mobile
//! compositors resize the toplevel; ADR 0058), and the field being typed
//! in must not end up under the keyboard.
//!
//! GTK scrolls a scrolled window to a widget only when the focus *moves*
//! there: a `GtkScrolledWindow` wraps a child that is not scrollable in a
//! `GtkViewport` (`gtk_scrolled_window_set_child` docs), whose
//! `scroll-to-focus` (TRUE by default since GTK 4.6.2, `gtk/gtkviewport.c`
//! property docs) scrolls "when the focus changes". When the keyboard
//! appears the focus stays where it is, so nothing scrolls. [`install`]
//! watches the window's surface (`GdkSurface::layout`, emitted "when the
//! size of the surface is changed") and, on the next frames, brings the
//! focus back into view in every scrolled window around it, innermost
//! first: a text view keeps its cursor in view
//! (`gtk_text_view_scroll_mark_onscreen`, "the minimum distance such that
//! the mark is contained within the visible area"), and each scrolled
//! window around shows what it contains of the focus by the least scroll
//! ([`scroll_delta`], pure).

use adw::prelude::*;
use gtk::glib;

/// Room kept around the focused widget, in pixels, so its focus ring shows.
pub const MARGIN: f64 = 6.0;

/// How far to scroll (added to the adjustment's value) so that a widget
/// spanning `top`..`bottom`, in the coordinates of a view `page` pixels
/// tall, is in view, by the least scroll: none if it already is; its start
/// if it is taller than the view and does not already fill it.
pub fn scroll_delta(top: f64, bottom: f64, page: f64) -> f64 {
    if bottom - top <= page {
        if top < 0.0 {
            top
        } else if bottom > page {
            bottom - page
        } else {
            0.0
        }
    } else if top <= 0.0 && bottom >= page {
        0.0
    } else if top > 0.0 {
        top
    } else {
        bottom - page
    }
}

/// Brings the focus back into view each time `window`'s surface changes
/// size (see the module comment). Call once, before the window is
/// realized.
pub fn install(window: &impl IsA<gtk::Window>) {
    window
        .upcast_ref::<gtk::Window>()
        .connect_realize(|window| {
            let Some(surface) = window.surface() else {
                return;
            };
            let window = window.downgrade();
            surface.connect_layout(move |_, _, _| {
                if let Some(window) = window.upgrade() {
                    after_layout(window.upcast_ref());
                }
            });
        });
}

/// Runs [`bring_focus_into_view`] at the start of the next two frames: the
/// widgets are allocated at the new size in the frame that reported it,
/// and a scrolled window adjusted in the first is laid out again in the
/// second (`gtk_widget_add_tick_callback` runs before each frame's layout).
fn after_layout(window: &gtk::Window) {
    let frames = std::cell::Cell::new(0);
    window.add_tick_callback(move |window, _| {
        bring_focus_into_view(window);
        frames.set(frames.get() + 1);
        if frames.get() < 2 {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}

/// Scrolls every scrolled window around `window`'s focus so the focus is
/// in view (see the module comment). Does nothing without a focus.
pub fn bring_focus_into_view(window: &gtk::Window) {
    let Some(focus) = gtk::prelude::RootExt::focus(window) else {
        return;
    };
    // What the next scrolled window out must show: the focus, then each
    // scrolled window passed (its own content is its business).
    let mut target = focus.clone();
    let mut parent = focus.parent();
    while let Some(widget) = parent {
        parent = widget.parent();
        let Ok(scrolled) = widget.downcast::<gtk::ScrolledWindow>() else {
            continue;
        };
        if target == focus
            && let Some(text) = focus.downcast_ref::<gtk::TextView>()
            && scrolled.child().as_ref() == Some(focus.upcast_ref())
        {
            text.scroll_mark_onscreen(&text.buffer().get_insert());
        } else if let Some(bounds) = target.compute_bounds(&scrolled) {
            let adjustment = scrolled.vadjustment();
            let delta = scroll_delta(
                f64::from(bounds.y()) - MARGIN,
                f64::from(bounds.y() + bounds.height()) + MARGIN,
                adjustment.page_size(),
            );
            if delta != 0.0 {
                // `set_value` clamps to lower ..= upper - page_size.
                adjustment.set_value(adjustment.value() + delta);
            }
        }
        target = scrolled.upcast();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_widget_in_view_needs_no_scroll() {
        assert!(scroll_delta(10.0, 50.0, 100.0).abs() < f64::EPSILON);
        assert!(scroll_delta(0.0, 100.0, 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_widget_below_or_above_scrolls_by_the_least() {
        // Below: its bottom comes to the view's bottom.
        assert!((scroll_delta(120.0, 160.0, 100.0) - 60.0).abs() < f64::EPSILON);
        // Partly below.
        assert!((scroll_delta(80.0, 120.0, 100.0) - 20.0).abs() < f64::EPSILON);
        // Above: its top comes to the view's top.
        assert!((scroll_delta(-30.0, 10.0, 100.0) + 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_widget_taller_than_the_view_shows_its_start_unless_it_fills_it() {
        assert!((scroll_delta(50.0, 300.0, 100.0) - 50.0).abs() < f64::EPSILON);
        assert!(scroll_delta(-50.0, 300.0, 100.0).abs() < f64::EPSILON);
        // Its end is in view but not its start: show as much as can be,
        // from its end.
        assert!((scroll_delta(-250.0, 50.0, 100.0) + 50.0).abs() < f64::EPSILON);
    }

    /// The case of the on-screen keyboard: a field at the bottom of a
    /// scrolled form has the focus, the view gets shorter, and the field is
    /// brought back into view.
    #[test]
    fn a_focused_field_stays_in_view_when_the_view_gets_shorter() {
        crate::testing::with_widgets(|| {
            let window = gtk::Window::new();
            window.set_default_size(360, 400);
            let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
            for _ in 0..12 {
                let filler = gtk::Label::new(Some("filler"));
                filler.set_size_request(-1, 40);
                column.append(&filler);
            }
            let field = gtk::Entry::new();
            column.append(&field);
            let scrolled = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&column)
                .min_content_height(400)
                .max_content_height(400)
                .propagate_natural_height(true)
                .vexpand(false)
                .valign(gtk::Align::Start)
                .build();
            window.set_child(Some(&scrolled));
            install(&window);
            window.present();
            crate::testing::settle_until(|| window.is_mapped(), Duration::from_secs(5));
            field.grab_focus();
            crate::testing::settle_until(|| false, Duration::from_millis(200));
            let in_view = || {
                field.compute_bounds(&scrolled).is_some_and(|b| {
                    let page = scrolled.vadjustment().page_size();
                    f64::from(b.y()) >= 0.0 && f64::from(b.y() + b.height()) <= page
                })
            };
            assert!(in_view(), "in view before");
            // The keyboard comes up: the view is now 250 px tall.
            scrolled.set_min_content_height(250);
            scrolled.set_max_content_height(250);
            crate::testing::settle_until(
                || (scrolled.vadjustment().page_size() - 250.0).abs() < 1.0,
                Duration::from_secs(5),
            );
            assert!(!in_view(), "the shorter view hides the field");
            bring_focus_into_view(&window);
            assert!(
                crate::testing::settle_until(in_view, Duration::from_secs(5)),
                "the field is back in view"
            );
            // Gone before the next test, which may show a popover.
            window.destroy();
            crate::testing::settle_until(|| !window.is_mapped(), Duration::from_secs(5));
        });
    }

    /// The text view's own scroller keeps the cursor in view, and the form
    /// around it shows the text view's scroller.
    #[test]
    fn a_text_view_keeps_its_cursor_and_its_frame_in_view() {
        crate::testing::with_widgets(|| {
            let window = gtk::Window::new();
            window.set_default_size(360, 400);
            let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
            for _ in 0..12 {
                let filler = gtk::Label::new(Some("filler"));
                filler.set_size_request(-1, 40);
                column.append(&filler);
            }
            let text = gtk::TextView::new();
            text.buffer().set_text(&"line\n".repeat(40));
            let inner = gtk::ScrolledWindow::builder()
                .child(&text)
                .min_content_height(120)
                .max_content_height(120)
                .build();
            column.append(&inner);
            let outer = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&column)
                .min_content_height(400)
                .max_content_height(400)
                .propagate_natural_height(true)
                .valign(gtk::Align::Start)
                .build();
            window.set_child(Some(&outer));
            window.present();
            crate::testing::settle_until(|| window.is_mapped(), Duration::from_secs(5));
            text.grab_focus();
            // The cursor at the end of the text, then the view scrolled away
            // from it, as after a resize.
            let buffer = text.buffer();
            buffer.place_cursor(&buffer.end_iter());
            crate::testing::settle_until(|| false, Duration::from_millis(200));
            inner.vadjustment().set_value(0.0);
            outer.set_min_content_height(200);
            outer.set_max_content_height(200);
            crate::testing::settle_until(
                || (outer.vadjustment().page_size() - 200.0).abs() < 1.0,
                Duration::from_secs(5),
            );
            bring_focus_into_view(&window);
            let shown = || {
                let page = outer.vadjustment().page_size();
                let frame_in_view = inner.compute_bounds(&outer).is_some_and(|b| {
                    f64::from(b.y()) >= 0.0 && f64::from(b.y() + b.height()) <= page
                });
                let adjustment = inner.vadjustment();
                let at_end =
                    adjustment.value() + adjustment.page_size() >= adjustment.upper() - 1.0;
                frame_in_view && at_end
            };
            assert!(
                crate::testing::settle_until(shown, Duration::from_secs(5)),
                "the cursor and the text view's frame are in view"
            );
            // Gone before the next test, which may show a popover.
            window.destroy();
            crate::testing::settle_until(|| !window.is_mapped(), Duration::from_secs(5));
        });
    }
}
