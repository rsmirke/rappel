//! `MathParagraph`: a run of text with formulas in it (ADR 0021, "Inline B").
//!
//! Pango lays the text out with a box for each formula (`AttrShape`), whose
//! logical rectangle starts `baseline` above the line, so the formula's
//! baseline lands on the text's baseline and tall formulas push their line
//! apart, as in a browser. Each formula is a [`MathView`] in a horizontal
//! scroller without a scrollbar (ADR 0024), put where Pango put its box. The
//! box is as wide as the formula but never wider than the line, so the text
//! still wraps at the card width, and a formula that is wider than the line
//! scrolls by itself (ADR 0021, point 3).
//!
//! Text goes through [`markup::Markup`], so it is escaped exactly as in a
//! label. The widget is not focusable.

use gtk::glib;
use gtk::subclass::prelude::*;

use super::markup::{self, Formula, Markup};
use super::math_view::{self, MathView};

glib::wrapper! {
    pub struct MathParagraph(ObjectSubclass<imp::MathParagraph>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MathParagraph {
    /// A paragraph of `markup` (which has at least one formula). `wrap`
    /// wraps the text at the widget's width; a table cell does not.
    /// `role` is the accessible role, which can only be set at construction.
    pub fn new(markup: &Markup, wrap: bool, role: gtk::AccessibleRole) -> Self {
        let paragraph: Self = glib::Object::builder()
            .property("accessible-role", role)
            .build();
        paragraph.imp().fill(&paragraph, markup, wrap);
        paragraph
    }
}

/// The text a screen reader gets: the label's text with each placeholder
/// replaced by its formula's LaTeX source (the formulas are also exposed one
/// by one, as Math objects), without the breaks added for layout
/// (`breaks`), which braille displays could show. A zero-width space the
/// card had itself goes too; it is not read anyway.
pub fn accessible_text(text: &str, formulas: &[Formula]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for formula in formulas {
        if let Some(before) = text.get(from..formula.index) {
            out.push_str(before);
            out.push_str(&formula.source);
            from = formula.index + markup::PLACEHOLDER.len_utf8();
        }
    }
    out.push_str(text.get(from..).unwrap_or_default());
    super::breaks::without_breaks(&out)
}

/// The placeholder's length in UTF-8 (3), for Pango's `u32` byte indices.
const PLACEHOLDER_BYTES: u32 = 3;

/// How many layouts a paragraph keeps: GTK alternates between the unlimited
/// width (the horizontal measure) and the real one. The oldest goes first,
/// with `Vec::remove(0)`: at this size that moves one element (issue #42).
const LAYOUT_CACHE_ENTRIES: usize = 2;

/// What a Pango layout depends on besides the text. If it is the same, the
/// layout from the last call is reused: GTK measures and allocates a widget
/// several times and draws it every frame. `css_changed` cannot be
/// overridden from gtk-rs, but a changed font makes GTK measure again, and
/// this comparison then sees it.
#[derive(Debug, Clone, PartialEq)]
struct Fingerprint {
    width: i32,
    font: String,
    dpi: i32,
    direction: gtk::TextDirection,
}

mod imp {
    use std::cell::{Cell, RefCell};
    use std::ops::Range;

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{glib, graphene, gsk, pango};

    use super::super::math::{
        box_width, ceil_px, f32_px, from_pango_units, left_edge, to_pango_units,
    };
    use super::{
        Fingerprint, Formula, Markup, MathView, PLACEHOLDER_BYTES, accessible_text, markup,
        math_view,
    };

    #[derive(Debug, Default)]
    pub struct MathParagraph {
        text: RefCell<String>,
        attrs: RefCell<Option<pango::AttrList>>,
        formulas: RefCell<Vec<Formula>>,
        links: RefCell<Vec<(Range<usize>, String)>>,
        pub(super) views: RefCell<Vec<(MathView, gtk::ScrolledWindow)>>,
        wrap: Cell<bool>,
        /// The last [`super::LAYOUT_CACHE_ENTRIES`] layouts.
        cache: RefCell<Vec<(Fingerprint, pango::Layout)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MathParagraph {
        const NAME: &'static str = "RappelMathParagraph";
        type Type = super::MathParagraph;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MathParagraph {
        fn dispose(&self) {
            // A container unparents its children itself (GtkWidget docs,
            // "Implementing a container").
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MathParagraph {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            if self.wrap.get() {
                gtk::SizeRequestMode::HeightForWidth
            } else {
                gtk::SizeRequestMode::ConstantSize
            }
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                let (_, logical) = self.layout(-1).pixel_extents();
                let natural = logical.width();
                let minimum = if self.wrap.get() {
                    // Text wraps anywhere (WordChar) and formulas shrink to
                    // the line: a few ems is enough to be readable.
                    natural.min(ceil_px(math_view::em_px(&*self.obj())).saturating_mul(2))
                } else {
                    natural
                };
                (minimum, natural, -1, -1)
            } else {
                let layout = self.layout(if self.wrap.get() { for_size } else { -1 });
                let (_, logical) = layout.pixel_extents();
                let baseline = layout.baseline() / pango::SCALE;
                (logical.height(), logical.height(), baseline, baseline)
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let layout = self.layout(if self.wrap.get() { width } else { -1 });
            for (formula, (view, scroller)) in
                self.formulas.borrow().iter().zip(&*self.views.borrow())
            {
                let pos = layout.index_to_pos(i32::try_from(formula.index).unwrap_or(i32::MAX));
                // Right-to-left text gives a negative width.
                let left = left_edge(pos.x(), pos.width());
                let (min_width, _, _, _) = scroller.measure(gtk::Orientation::Horizontal, -1);
                let (min_height, _, _, _) = scroller.measure(gtk::Orientation::Vertical, -1);
                let box_width = ceil_px(from_pango_units(pos.width().abs()));
                let target = gsk::Transform::new().translate(&graphene::Point::new(
                    f32_px(from_pango_units(left)),
                    f32_px(from_pango_units(pos.y())),
                ));
                scroller.allocate(
                    box_width.max(min_width),
                    view.size().height.max(min_height),
                    -1,
                    Some(target),
                );
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let layout = self.layout(if self.wrap.get() { obj.width() } else { -1 });
            snapshot.append_layout(&layout, &obj.color());
            for (_, scroller) in &*self.views.borrow() {
                obj.snapshot_child(scroller, snapshot);
            }
        }
    }

    impl MathParagraph {
        pub fn fill(&self, obj: &super::MathParagraph, markup: &Markup, wrap: bool) {
            // The markup is built by `inline_markup`, which escapes it; a
            // failure here would be a bug there, and the text shows as is.
            let (attrs, text, formulas) = match pango::parse_markup(&markup.markup, '\0') {
                Ok((attrs, text, _)) => (attrs, text.to_string(), markup.formulas.clone()),
                Err(error) => {
                    // GMarkup errors quote the text: debug level only. The
                    // placeholder indices are into the parsed text, so
                    // without it no formula can be placed.
                    tracing::debug!(%error, "invalid markup for a paragraph with math");
                    (pango::AttrList::new(), markup.markup.clone(), Vec::new())
                }
            };
            obj.update_property(&[gtk::accessible::Property::Label(&accessible_text(
                &text, &formulas,
            ))]);
            self.text.replace(text);
            self.attrs.replace(Some(attrs));
            self.links.replace(markup.links.clone());
            self.wrap.set(wrap);
            for formula in &formulas {
                let view = MathView::new(&formula.source, formula.display);
                let scroller = crate::render::inline_math_scroller(&view);
                scroller.set_parent(obj);
                self.views.borrow_mut().push((view, scroller));
            }
            self.formulas.replace(formulas);
            if !markup.links.is_empty() {
                Self::show_link_urls(obj);
            }
        }

        /// The Pango layout for `width` pixels (-1: no limit), made again
        /// only when something it depends on changed.
        pub(super) fn layout(&self, width: i32) -> pango::Layout {
            let obj = self.obj();
            let settings = obj.settings();
            let fingerprint = Fingerprint {
                width,
                font: obj
                    .pango_context()
                    .font_description()
                    .map(|d| d.to_str().to_string())
                    .unwrap_or_default(),
                dpi: settings.gtk_xft_dpi(),
                direction: obj.direction(),
            };
            if let Some((_, layout)) = self
                .cache
                .borrow()
                .iter()
                .find(|(seen, _)| *seen == fingerprint)
            {
                return layout.clone();
            }
            let layout = obj.create_pango_layout(Some(&self.text.borrow()));
            layout.set_wrap(pango::WrapMode::WordChar);
            // `create_pango_layout` made the layout from the widget's
            // context, so the font is the CSS one.
            let attrs = self
                .attrs
                .borrow()
                .as_ref()
                .and_then(pango::AttrList::copy)
                .unwrap_or_default();
            for (formula, (view, scroller)) in
                self.formulas.borrow().iter().zip(&*self.views.borrow())
            {
                // The box is what the scroller will be allocated: the
                // formula's size. The scroller has no scrollbar, so its
                // minimum is 0 (`inline_math_scroller`); taking the larger
                // of the two keeps box and allocation equal even so, and the
                // scroller never covers the text after it.
                let size = view.size();
                let baseline = math_view::baseline_px(view);
                let (min_width, _, _, _) = scroller.measure(gtk::Orientation::Horizontal, -1);
                let (min_height, _, _, _) = scroller.measure(gtk::Orientation::Vertical, -1);
                let box_width = box_width(f64::from(size.width.max(min_width)), width);
                let height = size.height.max(min_height);
                let rect = pango::Rectangle::new(
                    0,
                    to_pango_units(-baseline),
                    to_pango_units(box_width),
                    to_pango_units(f64::from(height)),
                );
                let mut shape = pango::AttrShape::new(&rect, &rect);
                let start = u32::try_from(formula.index).unwrap_or(u32::MAX);
                shape.set_start_index(start);
                shape.set_end_index(start.saturating_add(PLACEHOLDER_BYTES));
                attrs.insert(shape);
            }
            layout.set_attributes(Some(&attrs));
            if self.wrap.get() && width > 0 {
                layout.set_width(width.saturating_mul(pango::SCALE));
            }
            let mut cache = self.cache.borrow_mut();
            if cache.len() >= super::LAYOUT_CACHE_ENTRIES {
                cache.remove(0);
            }
            cache.push((fingerprint, layout.clone()));
            layout
        }

        /// Shows the URL of the link under the pointer as the tooltip, as
        /// the labels do (`render::show_link_urls`).
        fn show_link_urls(obj: &super::MathParagraph) {
            obj.set_has_tooltip(true);
            obj.connect_query_tooltip(|obj, x, y, keyboard, tooltip| {
                if keyboard {
                    return false;
                }
                let imp = obj.imp();
                let width = if imp.wrap.get() { obj.width() } else { -1 };
                let (inside, index, _) = imp.layout(width).xy_to_index(
                    x.saturating_mul(pango::SCALE),
                    y.saturating_mul(pango::SCALE),
                );
                let links = imp.links.borrow();
                let url = usize::try_from(index)
                    .ok()
                    .filter(|_| inside)
                    .and_then(|index| markup::link_at(&links, index));
                match url {
                    Some(url) => {
                        tooltip.set_text(Some(url));
                        true
                    }
                    None => false,
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formula(index: usize, source: &str) -> Formula {
        Formula {
            index,
            source: source.to_owned(),
            display: false,
        }
    }

    #[test]
    fn the_placeholder_is_three_bytes() {
        assert_eq!(PLACEHOLDER_BYTES as usize, markup::PLACEHOLDER.len_utf8());
    }

    #[test]
    fn a_screen_reader_gets_the_latex_source() {
        let text = "a \u{FFFC} b \u{FFFC}";
        let formulas = [formula(2, r"\frac{1}{2}"), formula(8, "x^2")];
        assert_eq!(accessible_text(text, &formulas), r"a \frac{1}{2} b x^2");
        // Text around, nothing to replace, and a wrong index (not a panic).
        assert_eq!(accessible_text("é", &[]), "é");
        assert_eq!(accessible_text("é", &[formula(1, "x")]), "é");
        assert_eq!(accessible_text("ab", &[formula(40, "x")]), "ab");
    }

    /// The width Pango reserves for each formula, and the formula's own
    /// width, in pixels, for "a $source$ b" at the current settings.
    fn reserved_and_drawn(source: &str) -> (f64, f64) {
        let markup = Markup {
            markup: format!("a {} b", markup::PLACEHOLDER),
            links: Vec::new(),
            formulas: vec![formula(2, source)],
        };
        let paragraph = MathParagraph::new(&markup, true, gtk::AccessibleRole::Paragraph);
        let layout = paragraph.imp().layout(-1);
        let reserved = super::super::math::from_pango_units(layout.index_to_pos(2).width());
        let views = paragraph.imp().views.borrow();
        let (view, _) = views.first().expect("the paragraph has one formula");
        (reserved, f64::from(view.size().width))
    }

    /// "Scale" is the text scaling factor (`gtk-xft-dpi`), which sets the
    /// font and so the formula size. A `HiDPI` scale factor cannot enter:
    /// formula sizes, Pango units and allocations are all logical pixels.
    /// Needs a display; CI runs the tests under Xvfb (ADR 0024). The theme
    /// is GTK's default, not libadwaita's: the scroller's minimum without a
    /// scrollbar does not depend on it, but a theme-dependent size would
    /// not be covered here.
    #[test]
    fn the_box_for_an_inline_formula_is_as_wide_as_the_formula() {
        const SOURCES: [&str; 4] = [
            "x",
            r"\alpha",
            r"\frac{a}{b}+c^2",
            r"\sum_{i=1}^{n} i^2 = \frac{n(n+1)(2n+1)}{6}",
        ];
        /// Puts the text scale back even if the body panics, so later
        /// tests on the same GTK thread see the default.
        struct RestoreDpi(gtk::Settings, i32);
        impl Drop for RestoreDpi {
            fn drop(&mut self) {
                self.0.set_gtk_xft_dpi(self.1);
            }
        }
        // (reserved, drawn) for each source at 1×, then at 2×.
        let widths = crate::testing::with_gtk(|| {
            let settings = gtk::Settings::default().expect("GTK is initialised");
            let _restore = RestoreDpi(settings.clone(), settings.gtk_xft_dpi());
            let mut widths = Vec::new();
            for scale in [1, 2] {
                settings.set_gtk_xft_dpi(96 * 1024 * scale);
                widths.extend(SOURCES.map(reserved_and_drawn));
            }
            widths
        });
        let (one, two) = widths.split_at(SOURCES.len());
        for ((source, &(reserved_1, drawn_1)), &(reserved_2, drawn_2)) in
            SOURCES.iter().zip(one).zip(two)
        {
            for (scale, reserved, drawn) in [(1, reserved_1, drawn_1), (2, reserved_2, drawn_2)] {
                assert!(
                    (reserved - drawn).abs() <= 1.0,
                    "{source} at {scale}×: Pango reserves {reserved} px for a formula {drawn} px wide"
                );
            }
            // The text scale really changed the formula's size.
            assert!(
                (1.8..=2.2).contains(&(drawn_2 / drawn_1)),
                "{source}: {drawn_1} px at 1×, {drawn_2} px at 2×"
            );
        }
    }
}
