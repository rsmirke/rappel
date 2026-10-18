//! Keeping the field being typed in on screen when the window gets
//! shorter: on a phone the on-screen keyboard shrinks the window (Phosh and
//! other mobile compositors resize the toplevel; ADR 0058), and the field
//! must not end up under the keyboard.
//!
//! GTK scrolls a scrolled window to a widget only when the focus *moves*
//! there: a `GtkScrolledWindow` wraps a child that is not scrollable in a
//! `GtkViewport` (`gtk_scrolled_window_set_child` docs), whose
//! `scroll-to-focus` (TRUE by default since GTK 4.6.2, `gtk/gtkviewport.c`
//! property docs) scrolls "when the focus changes". When the keyboard
//! appears the focus stays where it is, so nothing scrolls.
//!
//! [`install`] watches the window's surface. `GdkSurface::layout` is
//! "emitted when the size of surface is changed, or when relayout should be
//! performed" (`gdk/gdksurface.c`), so on every relayout too: only a
//! **shorter** surface than the last one counts ([`keyboard_may_have_opened`],
//! pure), and only while the focus is a field that takes typing (an
//! editable, or a text view: only typing raises the keyboard). Scrolling a
//! list or a card by hand never snaps back to the focus. Then, on the next
//! frames, the focus is brought back into view in every scrolled window
//! around it, innermost first: a text view keeps its cursor in view
//! (`gtk_text_view_scroll_mark_onscreen`, "the minimum distance such that
//! the mark is contained within the visible area"), and each scrolled
//! window around shows the cursor (or the focused field) by the least
//! scroll ([`scroll_delta`], pure), where the inner scrolled window will
//! have put it ([`clamp_span`], pure). The walk stops at the window's own
//! surface: a field in a popover is not in the window's scrolled windows.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{glib, graphene};

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

/// The span `top`..`bottom` moved by the least into `low`..`high` (where a
/// scrolled window that scrolls by the least puts it), or the whole of
/// `low`..`high` when it is taller.
pub fn clamp_span(top: f64, bottom: f64, low: f64, high: f64) -> (f64, f64) {
    let height = bottom - top;
    if height >= high - low {
        (low, high)
    } else if top < low {
        (low, low + height)
    } else if bottom > high {
        (high - height, high)
    } else {
        (top, bottom)
    }
}

/// Whether a surface now `height` pixels tall, after `last`, may have made
/// room for an on-screen keyboard: only when it got shorter. Relayouts at
/// the same height, and growing, never move anything.
pub fn keyboard_may_have_opened(last: i32, height: i32) -> bool {
    height < last
}

/// Whether `widget` takes typing, so that focusing it raises the
/// on-screen keyboard: an editable (`GtkText`, an entry's text, a spin
/// button) or a text view (the card editor's sides).
fn takes_typing(widget: &gtk::Widget) -> bool {
    widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>()
}

/// Brings the focused field back into view each time `window`'s surface
/// gets shorter (see the module comment). Call once, before the window is
/// realized.
pub fn install(window: &impl IsA<gtk::Window>) {
    window
        .upcast_ref::<gtk::Window>()
        .connect_realize(|window| {
            let Some(surface) = window.surface() else {
                return;
            };
            let last = Rc::new(Cell::new(surface.height()));
            let window = window.downgrade();
            surface.connect_layout(move |_, _, height| {
                let shorter = keyboard_may_have_opened(last.replace(height), height);
                if let Some(window) = window.upgrade()
                    && shorter
                    && gtk::prelude::RootExt::focus(&window).is_some_and(|f| takes_typing(&f))
                {
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
    let frames = Cell::new(0);
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

/// The part of `focus` to keep in view, in its own coordinates: a text
/// view's cursor (`gtk_text_view_get_iter_location`, in buffer
/// coordinates, then `gtk_text_view_buffer_to_window_coords` to the
/// widget's), else the whole widget.
fn focus_rect(focus: &gtk::Widget) -> graphene::Rect {
    if let Some(text) = focus.downcast_ref::<gtk::TextView>() {
        let buffer = text.buffer();
        let cursor = text.iter_location(&buffer.iter_at_mark(&buffer.get_insert()));
        let (x, y) =
            text.buffer_to_window_coords(gtk::TextWindowType::Widget, cursor.x(), cursor.y());
        #[expect(
            clippy::cast_precision_loss,
            reason = "pixel coordinates are far below 2^24"
        )]
        return graphene::Rect::new(
            x as f32,
            y as f32,
            cursor.width().max(1) as f32,
            cursor.height().max(1) as f32,
        );
    }
    #[expect(clippy::cast_precision_loss, reason = "pixel sizes are far below 2^24")]
    graphene::Rect::new(0.0, 0.0, focus.width() as f32, focus.height() as f32)
}

/// The vertical span of `rect` (in `from`'s coordinates) in `to`'s.
fn span_in(from: &gtk::Widget, rect: &graphene::Rect, to: &gtk::Widget) -> Option<(f64, f64)> {
    let top = from.compute_point(to, &graphene::Point::new(rect.x(), rect.y()))?;
    let bottom = from.compute_point(
        to,
        &graphene::Point::new(rect.x(), rect.y() + rect.height()),
    )?;
    Some((f64::from(top.y()), f64::from(bottom.y())))
}

/// Scrolls every scrolled window around `window`'s focus so the focus (a
/// text view's cursor) is in view (see the module comment). Does nothing
/// without a focus.
pub fn bring_focus_into_view(window: &gtk::Window) {
    let Some(focus) = gtk::prelude::RootExt::focus(window) else {
        return;
    };
    let rect = focus_rect(&focus);
    // The scrolled window passed last: what it holds of the focus is
    // within its own bounds once it has scrolled by the least.
    let mut inner: Option<gtk::ScrolledWindow> = None;
    let mut parent = focus.parent();
    while let Some(widget) = parent {
        // Another surface (a popover): its widgets are not in the window's
        // scrolled windows, whatever their parent.
        if widget.is::<gtk::Native>() {
            break;
        }
        parent = widget.parent();
        let Ok(scrolled) = widget.downcast::<gtk::ScrolledWindow>() else {
            continue;
        };
        if inner.is_none()
            && let Some(text) = focus.downcast_ref::<gtk::TextView>()
            && scrolled.child().as_ref() == Some(focus.upcast_ref())
        {
            text.scroll_mark_onscreen(&text.buffer().get_insert());
        } else if let Some((top, bottom)) = span_in(&focus, &rect, scrolled.upcast_ref()) {
            let (top, bottom) = match inner
                .as_ref()
                .and_then(|inner| inner.compute_bounds(&scrolled))
            {
                Some(b) => clamp_span(top, bottom, f64::from(b.y()), f64::from(b.y() + b.height())),
                None => (top, bottom),
            };
            let adjustment = scrolled.vadjustment();
            let delta = scroll_delta(top - MARGIN, bottom + MARGIN, adjustment.page_size());
            if delta != 0.0 {
                // `set_value` clamps to lower ..= upper - page_size.
                adjustment.set_value(adjustment.value() + delta);
            }
        }
        inner = Some(scrolled);
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

    #[test]
    fn a_span_is_moved_into_its_scrolled_window_by_the_least() {
        assert_eq!(clamp_span(10.0, 20.0, 0.0, 100.0), (10.0, 20.0));
        assert_eq!(clamp_span(500.0, 520.0, 0.0, 100.0), (80.0, 100.0));
        assert_eq!(clamp_span(-50.0, -30.0, 0.0, 100.0), (0.0, 20.0));
        assert_eq!(clamp_span(-50.0, 300.0, 0.0, 100.0), (0.0, 100.0));
    }

    #[test]
    fn only_a_shorter_surface_may_mean_the_keyboard_opened() {
        assert!(keyboard_may_have_opened(640, 300));
        assert!(!keyboard_may_have_opened(640, 640), "a relayout");
        assert!(!keyboard_may_have_opened(300, 640), "the keyboard closed");
    }

    /// A form of fillers with `field` at the bottom, in a scrolled window
    /// `height` pixels tall, in a window (inside Xvfb's default screen).
    fn form(field: &impl IsA<gtk::Widget>, height: i32) -> (gtk::Window, gtk::ScrolledWindow) {
        let window = gtk::Window::new();
        window.set_default_size(360, height);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        for _ in 0..12 {
            let filler = gtk::Label::new(Some("filler"));
            filler.set_size_request(-1, 40);
            column.append(&filler);
        }
        column.append(field);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&column)
            .vexpand(true)
            .build();
        window.set_child(Some(&scrolled));
        (window, scrolled)
    }

    /// Whether `widget` is wholly in `scrolled`'s view.
    fn in_view(widget: &impl IsA<gtk::Widget>, scrolled: &gtk::ScrolledWindow) -> bool {
        widget.compute_bounds(scrolled).is_some_and(|b| {
            let page = scrolled.vadjustment().page_size();
            f64::from(b.y()) >= 0.0 && f64::from(b.y() + b.height()) <= page
        })
    }

    /// Scrolls `scrolled` to its end, where the field is.
    fn to_end(scrolled: &gtk::ScrolledWindow) {
        let adjustment = scrolled.vadjustment();
        adjustment.set_value(adjustment.upper());
    }

    /// Shows `window` and waits until it is laid out.
    fn show(window: &gtk::Window) {
        window.present();
        crate::testing::settle_until(
            || window.is_mapped() && window.height() > 0,
            Duration::from_secs(5),
        );
        crate::testing::settle_until(|| false, Duration::from_millis(100));
    }

    /// Closes `window` and waits for it to go, before the next test, which
    /// may show a popover.
    fn close(window: &gtk::Window) {
        window.destroy();
        crate::testing::settle_until(|| !window.is_mapped(), Duration::from_secs(5));
    }

    /// The case of the on-screen keyboard: a field at the bottom of a
    /// scrolled form has the focus, the view gets shorter, and the field is
    /// brought back into view.
    #[test]
    fn a_focused_field_is_brought_back_into_view() {
        crate::testing::with_widgets(|| {
            let field = gtk::Entry::new();
            let (window, scrolled) = form(&field, 400);
            scrolled.set_vexpand(false);
            scrolled.set_valign(gtk::Align::Start);
            scrolled.set_min_content_height(400);
            scrolled.set_max_content_height(400);
            scrolled.set_propagate_natural_height(true);
            show(&window);
            field.grab_focus();
            to_end(&scrolled);
            assert!(
                crate::testing::settle_until(|| in_view(&field, &scrolled), Duration::from_secs(5)),
                "in view before"
            );
            // The keyboard comes up: the view is now 250 px tall.
            scrolled.set_min_content_height(250);
            scrolled.set_max_content_height(250);
            crate::testing::settle_until(
                || (scrolled.vadjustment().page_size() - 250.0).abs() < 1.0,
                Duration::from_secs(5),
            );
            assert!(
                !in_view(&field, &scrolled),
                "the shorter view hides the field"
            );
            bring_focus_into_view(&window);
            assert!(
                crate::testing::settle_until(|| in_view(&field, &scrolled), Duration::from_secs(5)),
                "the field is back in view"
            );
            close(&window);
        });
    }

    /// With [`install`]: the window itself gets shorter, as with a phone's
    /// keyboard, and the focused field comes back into view.
    #[test]
    fn a_shorter_window_brings_the_focused_field_back() {
        crate::testing::with_widgets(|| {
            let field = gtk::Entry::new();
            let (window, scrolled) = form(&field, 400);
            install(&window);
            show(&window);
            field.grab_focus();
            to_end(&scrolled);
            assert!(
                crate::testing::settle_until(|| in_view(&field, &scrolled), Duration::from_secs(5)),
                "in view before"
            );
            window.set_default_size(360, 200);
            assert!(
                crate::testing::settle_until(
                    || window.height() <= 200 && in_view(&field, &scrolled),
                    Duration::from_secs(5)
                ),
                "the field is back in view in the shorter window ({} px)",
                window.height()
            );
            close(&window);
        });
    }

    /// The other way: relayouts at the same height (the surface's `layout`
    /// signal fires on every one) never scroll back to the focus, so a
    /// list or a card scrolled by hand stays where it was put.
    #[test]
    fn scrolling_by_hand_never_snaps_back_to_the_focus() {
        crate::testing::with_widgets(|| {
            let field = gtk::Entry::new();
            let (window, scrolled) = form(&field, 300);
            install(&window);
            show(&window);
            field.grab_focus();
            to_end(&scrolled);
            assert!(
                crate::testing::settle_until(|| in_view(&field, &scrolled), Duration::from_secs(5)),
                "in view before"
            );
            // Scrolled away from the focus by hand, then relaid out.
            let adjustment = scrolled.vadjustment();
            adjustment.set_value(0.0);
            let column = scrolled.child().expect("the form");
            for _ in 0..5 {
                column.queue_resize();
                crate::testing::settle_until(|| false, Duration::from_millis(50));
            }
            assert!(
                adjustment.value().abs() < f64::EPSILON,
                "{}",
                adjustment.value()
            );
            assert!(!in_view(&field, &scrolled));
            close(&window);
        });
    }

    /// The text view's own scroller keeps the cursor in view, and the form
    /// around it shows where the cursor is.
    #[test]
    fn a_text_view_keeps_its_cursor_in_view() {
        crate::testing::with_widgets(|| {
            let text = gtk::TextView::new();
            text.buffer().set_text(&"line\n".repeat(40));
            let inner = gtk::ScrolledWindow::builder()
                .child(&text)
                .min_content_height(120)
                .max_content_height(120)
                .build();
            let (window, outer) = form(&inner, 400);
            outer.set_vexpand(false);
            outer.set_valign(gtk::Align::Start);
            outer.set_min_content_height(400);
            outer.set_max_content_height(400);
            outer.set_propagate_natural_height(true);
            show(&window);
            text.grab_focus();
            // The cursor at the end of the text, then both views scrolled
            // away from it, as after a resize.
            let buffer = text.buffer();
            buffer.place_cursor(&buffer.end_iter());
            crate::testing::settle_until(|| text.is_mapped(), Duration::from_secs(5));
            inner.vadjustment().set_value(0.0);
            outer.vadjustment().set_value(0.0);
            outer.set_min_content_height(200);
            outer.set_max_content_height(200);
            crate::testing::settle_until(
                || (outer.vadjustment().page_size() - 200.0).abs() < 1.0,
                Duration::from_secs(5),
            );
            bring_focus_into_view(&window);
            let shown = || {
                // The cursor is at the inner view's end once it scrolled:
                // that end is in the outer view.
                let page = outer.vadjustment().page_size();
                let end_in_view = inner.compute_bounds(&outer).is_some_and(|b| {
                    let end = f64::from(b.y() + b.height());
                    end > 0.0 && end <= page
                });
                let adjustment = inner.vadjustment();
                let at_end =
                    adjustment.value() + adjustment.page_size() >= adjustment.upper() - 1.0;
                end_in_view && at_end
            };
            assert!(
                crate::testing::settle_until(shown, Duration::from_secs(5)),
                "the cursor is in view"
            );
            close(&window);
        });
    }
}
