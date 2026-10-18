//! `ChartCard`: a [`BarChart`] with its title, the top of its scale, the
//! first and last day, the caption of the selected day, an optional legend
//! and a note, or a sentence instead when there is nothing to draw. Layout
//! in `data/chart_card.blp`; what it shows is a [`ChartModel`] made by the
//! statistics page's pure code.

use crate::i18n::gettext;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::bar_chart::{BarChart, ChartBar, StyleWatch, paint_swatch};

/// Said under the chart while no day is selected.
fn hint() -> String {
    gettext("Select a day to see its values")
}

/// Everything one chart shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartModel {
    pub bars: Vec<ChartBar>,
    pub ceiling: f64,
    /// The top of the value axis, "Up to 50".
    pub scale: String,
    /// The accessible description: the data in words.
    pub summary: String,
    /// The labels under the first and the last day.
    pub first_day: String,
    pub last_day: String,
    /// Said instead of the chart when there is nothing to show.
    pub empty: Option<String>,
    /// An extra line under the chart, if any.
    pub note: Option<String>,
}

mod imp {
    use std::cell::RefCell;

    use adw::subclass::prelude::*;
    use gtk::glib;
    use gtk::glib::prelude::StaticTypeExt;

    use super::{BarChart, StyleWatch};

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/chart_card.ui")]
    pub struct ChartCard {
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub scale_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub empty_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub chart_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub chart: TemplateChild<BarChart>,
        #[template_child]
        pub first_day_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub last_day_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub caption_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub legend_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub passed_swatch: TemplateChild<gtk::DrawingArea>,
        #[template_child]
        pub failed_swatch: TemplateChild<gtk::DrawingArea>,
        #[template_child]
        pub note_label: TemplateChild<gtk::Label>,
        pub style_watch: RefCell<Vec<StyleWatch>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChartCard {
        const NAME: &'static str = "RappelChartCard";
        type Type = super::ChartCard;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            // The template names this type.
            BarChart::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ChartCard {}
    impl WidgetImpl for ChartCard {}
    impl BoxImpl for ChartCard {}
}

glib::wrapper! {
    pub struct ChartCard(ObjectSubclass<imp::ChartCard>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for ChartCard {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ChartCard {
    /// Shows `model` under `title`; the legend names the two parts of a bar
    /// when `legend` is set.
    pub fn show(&self, title: &str, model: &ChartModel, legend: bool) {
        let imp = self.imp();
        imp.title_label.set_label(title);
        imp.chart.set_data(model.bars.clone(), model.ceiling);
        imp.chart.describe(title, &model.summary);
        let hint = hint();
        imp.chart.connect_selected(glib::clone!(
            #[weak(rename_to = caption)]
            imp.caption_label,
            #[strong]
            hint,
            move |selected| caption.set_label(selected.unwrap_or(&hint))
        ));
        imp.caption_label.set_label(&hint);
        imp.scale_label.set_label(&model.scale);
        imp.first_day_label.set_label(&model.first_day);
        imp.last_day_label.set_label(&model.last_day);
        imp.note_label
            .set_label(model.note.as_deref().unwrap_or(""));
        imp.note_label.set_visible(model.note.is_some());

        let empty = model.empty.as_deref();
        imp.empty_label.set_label(empty.unwrap_or(""));
        imp.empty_label.set_visible(empty.is_some());
        imp.chart_box.set_visible(empty.is_none());
        imp.scale_label.set_visible(empty.is_none());
        imp.legend_box.set_visible(legend && empty.is_none());

        // The chart repeats these in its description; the screen reader
        // need not read them as well.
        for label in [&*imp.first_day_label, &*imp.last_day_label] {
            label.update_state(&[gtk::accessible::State::Hidden(true)]);
        }
        for (swatch, hatched) in [(&*imp.passed_swatch, false), (&*imp.failed_swatch, true)] {
            swatch.set_draw_func(move |area, cr, width, height| {
                paint_swatch(area, cr, width, height, hatched);
            });
            swatch.update_state(&[gtk::accessible::State::Hidden(true)]);
        }
        imp.style_watch.replace(vec![
            StyleWatch::new(&*imp.passed_swatch),
            StyleWatch::new(&*imp.failed_swatch),
        ]);
    }

    /// The chart, to focus it or to drive it in tests.
    pub fn chart(&self) -> &BarChart {
        &self.imp().chart
    }

    /// Whether the chart is shown, as opposed to the empty sentence.
    pub fn shows_chart(&self) -> bool {
        self.imp().chart_box.is_visible()
    }
}
