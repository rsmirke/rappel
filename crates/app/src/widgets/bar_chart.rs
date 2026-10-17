//! `BarChart`: one bar per day, drawn with Cairo in a `GtkDrawingArea`
//! (SPEC §4.7). Theme colours only: the bars are the accent colour
//! (`AdwStyleManager`) and everything else the widget's CSS `color` at
//! lower opacity, so light, dark and accent changes need no colours of
//! ours. A second series, stacked above the first, is hatched and set off
//! by a gap, so the two parts differ by shape and not only by colour.
//!
//! The geometry is the pure [`layout`] module. The numbers and day names
//! around the chart are ordinary labels of the page that owns it, so they
//! follow the theme and the text size.
//!
//! Keyboard and screen readers: the chart is focusable. Left / Right /
//! Home / End (or a click or tap) select a day; the selected day is
//! outlined and its caption is announced and handed to
//! [`BarChart::connect_selected`], where the page shows it. The chart's
//! accessible role is Img; its label and description (a summary of the
//! data in words) are set with [`BarChart::describe`].
//!
//! Redrawing: `snapshot` draws again when the size changes, and the widget
//! queues a draw when `AdwStyleManager` reports a new accent colour or
//! dark mode. A change of the CSS `color` queues one through GTK's default
//! `css_changed` handling (see `render/math_view.rs`).

pub mod layout;

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use layout::Bar;

use crate::ui::callback::Callback;

/// Called with the caption of the selected day, or `None` when cleared.
type OnSelected = dyn Fn(Option<&str>);

glib::wrapper! {
    pub struct BarChart(ObjectSubclass<imp::BarChart>)
        @extends gtk::DrawingArea, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// One day of a chart: its bar, and the sentence said when it is selected.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartBar {
    pub bar: Bar,
    pub caption: String,
}

/// The height of the drawing in pixels; the width is whatever there is.
const HEIGHT: i32 = 160;

impl Default for BarChart {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl BarChart {
    /// Shows `days` on a value axis from 0 to `ceiling` (see
    /// [`layout::nice_ceiling`]). The selection is cleared.
    pub fn set_data(&self, days: Vec<ChartBar>, ceiling: f64) {
        let imp = self.imp();
        imp.days.replace(days);
        imp.ceiling.set(ceiling);
        self.select(None);
        self.queue_draw();
    }

    /// The chart's accessible label (its title) and description (what the
    /// data says, in words).
    pub fn describe(&self, label: &str, description: &str) {
        self.update_property(&[
            gtk::accessible::Property::Label(label),
            gtk::accessible::Property::Description(description),
        ]);
    }

    /// Calls `f` with the caption of the day now selected, or `None` when
    /// the selection is cleared.
    pub fn connect_selected(&self, f: impl Fn(Option<&str>) + 'static) {
        self.imp()
            .on_selected
            .replace(Some(Callback::new(std::rc::Rc::new(f))));
    }

    /// Selects day `day` (or none), announces its caption to screen readers
    /// and tells the page.
    pub fn select(&self, day: Option<usize>) {
        let imp = self.imp();
        let day = day.filter(|&d| d < imp.days.borrow().len());
        imp.selected.set(day);
        let caption = day.and_then(|d| imp.days.borrow().get(d).map(|c| c.caption.clone()));
        if let Some(caption) = &caption {
            self.announce(caption, gtk::AccessibleAnnouncementPriority::Medium);
        }
        let callback = imp.on_selected.borrow().as_ref().map(Callback::get);
        if let Some(callback) = callback {
            callback(caption.as_deref());
        }
        self.queue_draw();
    }

    /// The selected day, if any.
    pub fn selected(&self) -> Option<usize> {
        self.imp().selected.get()
    }

    /// Moves the selection one `step` (keys and tests).
    pub fn step(&self, step: layout::Step) {
        let days = self.imp().days.borrow().len();
        self.select(layout::step(step, self.selected(), days));
    }
}

/// The keys of a chart and the step each makes, without Ctrl, Alt or
/// Shift. The Keyboard Shortcuts dialog lists them (a test checks it).
pub(crate) const KEYS: [(gtk::gdk::Key, layout::Step); 8] = {
    use gtk::gdk::Key;
    use layout::Step;
    [
        (Key::Left, Step::Earlier),
        (Key::KP_Left, Step::Earlier),
        (Key::Right, Step::Later),
        (Key::KP_Right, Step::Later),
        (Key::Home, Step::First),
        (Key::KP_Home, Step::First),
        (Key::End, Step::Last),
        (Key::KP_End, Step::Last),
    ]
};

/// What a key does to the selection.
pub fn key_step(key: gtk::gdk::Key) -> Option<layout::Step> {
    KEYS.iter()
        .find(|(bound, _)| *bound == key)
        .map(|&(_, step)| step)
}

fn source(cr: &gtk::cairo::Context, color: &gtk::gdk::RGBA, opacity: f64) {
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()) * opacity,
    );
}

fn rectangle(cr: &gtk::cairo::Context, r: layout::Rect) {
    cr.rectangle(r.x, r.y, r.width, r.height);
}

/// Diagonal lines inside `rect` and an outline around it.
fn hatch(
    cr: &gtk::cairo::Context,
    rect: layout::Rect,
    foreground: &gtk::gdk::RGBA,
) -> Result<(), gtk::cairo::Error> {
    const SPACING: f64 = 4.0;
    cr.save()?;
    rectangle(cr, rect);
    cr.clip();
    source(cr, foreground, 0.75);
    cr.set_line_width(1.0);
    let mut offset = -rect.height;
    while offset < rect.width {
        cr.move_to(rect.x + offset, rect.y + rect.height);
        cr.line_to(rect.x + offset + rect.height, rect.y);
        offset += SPACING;
    }
    cr.stroke()?;
    cr.restore()?;
    // The outline keeps a one-pixel-high part visible.
    source(cr, foreground, 0.75);
    cr.set_line_width(1.0);
    cr.rectangle(
        rect.x + 0.5,
        rect.y + 0.5,
        (rect.width - 1.0).max(0.0),
        (rect.height - 1.0).max(0.0),
    );
    cr.stroke()?;
    Ok(())
}

/// The legend's sample of a bar part: the accent colour, or the hatching.
/// `area` is the swatch's `GtkDrawingArea`; the colours are read now.
pub fn paint_swatch(
    area: &impl IsA<gtk::Widget>,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    hatched: bool,
) {
    let rect = layout::Rect {
        x: 0.0,
        y: 0.0,
        width: f64::from(width),
        height: f64::from(height),
    };
    let painted = if hatched {
        hatch(cr, rect, &area.color())
    } else {
        source(cr, &adw::StyleManager::default().accent_color_rgba(), 1.0);
        rectangle(cr, rect);
        cr.fill()
    };
    if let Err(error) = painted {
        tracing::debug!(%error, "could not draw a legend swatch");
    }
}

/// Redraws a widget when the accent colour or the dark mode changes, for
/// as long as it lives (the handlers are disconnected on drop).
#[derive(Default)]
pub struct StyleWatch(Vec<glib::SignalHandlerId>);

impl std::fmt::Debug for StyleWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StyleWatch")
    }
}

impl StyleWatch {
    pub fn new(widget: &impl IsA<gtk::Widget>) -> Self {
        let manager = adw::StyleManager::default();
        let weak = widget.upcast_ref::<gtk::Widget>().downgrade();
        let redraw = move |_: &adw::StyleManager| {
            if let Some(widget) = weak.upgrade() {
                widget.queue_draw();
            }
        };
        Self(vec![
            manager.connect_accent_color_rgba_notify(redraw.clone()),
            manager.connect_dark_notify(redraw),
        ])
    }
}

impl Drop for StyleWatch {
    fn drop(&mut self) {
        let manager = adw::StyleManager::default();
        for id in self.0.drain(..) {
            manager.disconnect(id);
        }
    }
}

mod imp {
    use adw::prelude::*;
    use gtk::cairo;
    use gtk::gdk;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    use super::layout::{self, Bar};
    use super::{ChartBar, HEIGHT, OnSelected, StyleWatch, hatch, key_step, rectangle, source};
    use crate::ui::callback::Callback;

    #[derive(Debug, Default)]
    pub struct BarChart {
        pub days: RefCell<Vec<ChartBar>>,
        pub ceiling: Cell<f64>,
        pub selected: Cell<Option<usize>>,
        pub on_selected: RefCell<Option<Callback<OnSelected>>>,
        style_watch: RefCell<StyleWatch>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BarChart {
        const NAME: &'static str = "RappelBarChart";
        type Type = super::BarChart;
        type ParentType = gtk::DrawingArea;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for BarChart {
        fn constructed(&self) {
            self.parent_constructed();
            let chart = self.obj();
            chart.set_content_height(HEIGHT);
            chart.set_hexpand(true);
            chart.set_focusable(true);
            chart.add_css_class("bar-chart");

            chart.set_draw_func(glib::clone!(
                #[weak]
                chart,
                move |_, cr, width, height| {
                    if let Err(error) = chart.imp().paint(cr, width, height) {
                        tracing::debug!(%error, "could not draw a chart");
                    }
                }
            ));

            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(glib::clone!(
                #[weak]
                chart,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, key, _, state| {
                    // Leave Ctrl/Alt/Shift shortcuts to the window.
                    let plain = !state.intersects(
                        gdk::ModifierType::CONTROL_MASK
                            | gdk::ModifierType::ALT_MASK
                            | gdk::ModifierType::SHIFT_MASK,
                    );
                    match key_step(key).filter(|_| plain) {
                        Some(step) => {
                            chart.step(step);
                            glib::Propagation::Stop
                        }
                        None => glib::Propagation::Proceed,
                    }
                }
            ));
            chart.add_controller(keys);

            let click = gtk::GestureClick::new();
            click.connect_pressed(glib::clone!(
                #[weak]
                chart,
                move |_, _, x, _| {
                    let days = chart.imp().days.borrow().len();
                    let day = layout::day_at(x, f64::from(chart.width()), days);
                    chart.grab_focus();
                    chart.select(day);
                }
            ));
            chart.add_controller(click);

            self.style_watch.replace(StyleWatch::new(&*chart));
        }
    }

    impl WidgetImpl for BarChart {}
    impl DrawingAreaImpl for BarChart {}

    impl BarChart {
        pub(super) fn paint(
            &self,
            cr: &cairo::Context,
            width: i32,
            height: i32,
        ) -> Result<(), cairo::Error> {
            let (w, h) = (f64::from(width), f64::from(height));
            let chart = self.obj();
            let foreground = chart.color();
            let accent = adw::StyleManager::default().accent_color_rgba();
            let days = self.days.borrow();
            let data: Vec<Bar> = days.iter().map(|d| d.bar).collect();
            let rects = layout::bars(&data, w, h, self.ceiling.get());

            // The scale: a dashed line at the top value, a solid base line.
            cr.set_line_width(1.0);
            source(cr, &foreground, 0.25);
            cr.set_dash(&[3.0, 3.0], 0.0);
            cr.move_to(0.0, 0.5);
            cr.line_to(w, 0.5);
            cr.stroke()?;
            cr.set_dash(&[], 0.0);
            source(cr, &foreground, 0.4);
            cr.move_to(0.0, h - 0.5);
            cr.line_to(w, h - 0.5);
            cr.stroke()?;

            if let Some(slot) = self
                .selected
                .get()
                .and_then(|d| rects.get(d))
                .map(|r| r.slot)
            {
                source(cr, &foreground, 0.12);
                rectangle(cr, slot);
                cr.fill()?;
            }

            for r in &rects {
                if let Some(solid) = r.solid {
                    source(cr, &accent, 1.0);
                    rectangle(cr, solid);
                    cr.fill()?;
                }
                if let Some(hatched) = r.hatched {
                    hatch(cr, hatched, &foreground)?;
                }
            }

            if let Some(slot) = self
                .selected
                .get()
                .and_then(|d| rects.get(d))
                .map(|r| r.slot)
            {
                source(cr, &foreground, 0.7);
                cr.set_line_width(1.5);
                cr.rectangle(
                    slot.x + 0.75,
                    slot.y + 0.75,
                    (slot.width - 1.5).max(0.0),
                    (slot.height - 1.5).max(0.0),
                );
                cr.stroke()?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gtk::cairo::{Context, Format, ImageSurface};

    use super::*;

    fn days(values: &[(f64, f64)]) -> Vec<ChartBar> {
        values
            .iter()
            .enumerate()
            .map(|(i, &(solid, hatched))| ChartBar {
                bar: Bar { solid, hatched },
                caption: format!("day {i}"),
            })
            .collect()
    }

    /// Paints `chart` at `width` × 160 and returns the pixels' alpha bytes.
    fn painted(chart: &BarChart, width: i32) -> Vec<u8> {
        let mut surface = ImageSurface::create(Format::ARgb32, width, HEIGHT).unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            chart.imp().paint(&cr, width, HEIGHT).unwrap();
        }
        surface.flush();
        let stride = usize::try_from(surface.stride()).unwrap();
        let data = surface.data().unwrap();
        let row_bytes = usize::try_from(width).unwrap() * 4;
        (0..usize::try_from(HEIGHT).unwrap())
            .flat_map(|y| (0..row_bytes).step_by(4).map(move |x| (y, x)))
            .map(|(y, x)| data[y * stride + x + 3])
            .collect()
    }

    #[test]
    fn keys_move_the_selection() {
        use gtk::gdk::Key;
        assert_eq!(key_step(Key::Left), Some(layout::Step::Earlier));
        assert_eq!(key_step(Key::Right), Some(layout::Step::Later));
        assert_eq!(key_step(Key::Home), Some(layout::Step::First));
        assert_eq!(key_step(Key::End), Some(layout::Step::Last));
        assert_eq!(key_step(Key::Up), None);
        assert_eq!(key_step(Key::space), None);
    }

    #[test]
    fn stepping_selects_a_day_and_tells_the_page() {
        crate::testing::with_gtk(|| {
            adw::init().unwrap();
            let chart = BarChart::default();
            let heard: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
            let sink = Rc::clone(&heard);
            chart.connect_selected(move |c| sink.borrow_mut().push(c.map(str::to_owned)));
            chart.set_data(days(&[(1.0, 0.0), (2.0, 1.0), (3.0, 0.0)]), 5.0);
            assert_eq!(chart.selected(), None);
            chart.step(layout::Step::Earlier);
            assert_eq!(chart.selected(), Some(2));
            chart.step(layout::Step::Earlier);
            chart.step(layout::Step::Earlier);
            chart.step(layout::Step::Earlier);
            assert_eq!(chart.selected(), Some(0));
            chart.step(layout::Step::Last);
            assert_eq!(chart.selected(), Some(2));
            // New data clears the selection and says so.
            chart.set_data(days(&[(1.0, 0.0)]), 1.0);
            assert_eq!(chart.selected(), None);
            let heard = heard.borrow();
            assert_eq!(heard.first(), Some(&None));
            assert_eq!(heard.get(1), Some(&Some("day 2".to_owned())));
            assert_eq!(heard.last(), Some(&None));
        });
    }

    #[test]
    fn a_day_that_does_not_exist_cannot_be_selected() {
        crate::testing::with_gtk(|| {
            adw::init().unwrap();
            let chart = BarChart::default();
            chart.set_data(days(&[(1.0, 0.0)]), 1.0);
            chart.select(Some(5));
            assert_eq!(chart.selected(), None);
            chart.step(layout::Step::Later);
            assert_eq!(chart.selected(), Some(0));
        });
    }

    #[test]
    fn the_chart_draws_at_a_phone_width_and_a_wide_one() {
        crate::testing::with_gtk(|| {
            adw::init().unwrap();
            let chart = BarChart::default();
            let thirty: Vec<(f64, f64)> = (0..30)
                .map(|i| (f64::from(i) * 3.0, f64::from(i % 4)))
                .collect();
            let cases = [
                (days(&[]), 1.0),
                (days(&[(0.0, 0.0)]), 1.0),
                (days(&[(1.0e9, 1.0e9)]), 1.0),
                (days(&thirty), 100.0),
            ];
            for (data, ceiling) in cases {
                chart.set_data(data, ceiling);
                chart.select(Some(0));
                for width in [296, 1200] {
                    // The scale lines are always drawn.
                    assert!(painted(&chart, width).iter().any(|&a| a > 0));
                }
            }
        });
    }

    #[test]
    fn bars_are_drawn_on_top_of_an_empty_chart() {
        crate::testing::with_gtk(|| {
            adw::init().unwrap();
            let chart = BarChart::default();
            chart.set_data(days(&[]), 1.0);
            let empty = painted(&chart, 296);
            chart.set_data(days(&[(10.0, 5.0); 30]), 20.0);
            let full = painted(&chart, 296);
            let filled = |pixels: &[u8]| pixels.iter().filter(|&&a| a > 0).count();
            assert!(filled(&full) > filled(&empty) + 1000);
        });
    }
}
