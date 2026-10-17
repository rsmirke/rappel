//! The statistics page: three headline numbers and three charts, for every
//! deck or for one (SPEC §4.7, ADR 0032, 0033). Layout in
//! `data/stats_page.blp`; every text and number the page shows is decided in
//! the pure [`text`] module.
//!
//! The data is read once, when the page is built; reopening the page reads
//! it again.

mod text;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use rappel_core::domain::Deck;

use self::text::{DayNames, FORECAST_TITLE, REVIEWS_TITLE, TIME_TITLE};
use crate::state::{AppState, StatsData};
use crate::ui::tile::set_tile;
use crate::ui::time;
use crate::widgets::chart_card::{ChartCard, ChartModel};

mod imp {
    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;
    use gtk::glib::prelude::StaticTypeExt;

    use crate::widgets::chart_card::ChartCard;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/stats_page.ui")]
    pub struct StatsPage {
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub retention_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub retention_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub pass_rate_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub pass_rate_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub time_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub time_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub reviews_chart: TemplateChild<ChartCard>,
        #[template_child]
        pub time_chart: TemplateChild<ChartCard>,
        #[template_child]
        pub forecast_chart: TemplateChild<ChartCard>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StatsPage {
        const NAME: &'static str = "RappelStatsPage";
        type Type = super::StatsPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            // The template names this type.
            ChartCard::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for StatsPage {
        fn constructed(&self) {
            self.parent_constructed();
            // GTK documents `shown` as the place to set the focus after a
            // push (ui/nav.rs): earlier, the view focuses the Back button.
            self.obj()
                .connect_shown(super::StatsPage::focus_first_chart);
        }
    }
    impl WidgetImpl for StatsPage {}
    impl NavigationPageImpl for StatsPage {}
}

glib::wrapper! {
    pub struct StatsPage(ObjectSubclass<imp::StatsPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StatsPage {
    /// The page of `deck`, or of every deck for `None`, as of now.
    ///
    /// # Errors
    /// If the statistics cannot be read.
    pub fn new(state: &AppState, deck: Option<&Deck>) -> rappel_core::Result<Self> {
        let data = state.stats(deck.map(|d| d.id.as_str()), rappel_core::time::now_ms())?;
        let page: Self = glib::Object::new();
        // Deck names are untrusted; AdwWindowTitle shows plain text.
        page.imp()
            .window_title
            .set_subtitle(deck.map_or("All Decks", |d| d.name.as_str()));
        page.fill(&data);
        Ok(page)
    }

    fn fill(&self, data: &StatsData) {
        let imp = self.imp();
        if text::nothing_to_show(data) {
            imp.content_stack.set_visible_child_name("empty");
            return;
        }
        imp.content_stack.set_visible_child_name("stats");
        let [retention, pass_rate, studied] = text::headline(data);
        set_tile(&imp.retention_value, &imp.retention_caption, &retention);
        set_tile(&imp.pass_rate_value, &imp.pass_rate_caption, &pass_rate);
        set_tile(&imp.time_value, &imp.time_caption, &studied);

        let names = DayNames {
            short: &time::day_short,
            with_weekday: &time::day_with_weekday,
            long: &time::day_long,
        };
        let charts: [(&ChartCard, &str, ChartModel, bool); 3] = [
            (
                &imp.reviews_chart,
                REVIEWS_TITLE,
                text::reviews_chart(data, &names),
                true,
            ),
            (
                &imp.time_chart,
                TIME_TITLE,
                text::time_chart(data, &names),
                false,
            ),
            (
                &imp.forecast_chart,
                FORECAST_TITLE,
                text::forecast_chart(data, &names),
                false,
            ),
        ];
        for (card, title, model, legend) in &charts {
            card.show(title, model, *legend);
        }
    }

    /// Puts the keyboard focus on the first chart that is drawn.
    fn focus_first_chart(&self) {
        let imp = self.imp();
        if let Some(card) = [&*imp.reviews_chart, &*imp.time_chart, &*imp.forecast_chart]
            .into_iter()
            .find(|card| card.shows_chart())
        {
            card.chart().grab_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rappel_core::domain::Grade;

    use super::*;

    fn state() -> (tempfile::TempDir, AppState) {
        adw::init().unwrap();
        gtk::gio::resources_register_include!("rappel.gresource").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::open(tmp.path().join("data")).unwrap();
        (tmp, state)
    }

    /// A deck of two cards, each passed twice ten days ago and
    /// failed once yesterday.
    fn studied(state: &AppState) -> Deck {
        let deck = state.create_deck("Deck").unwrap();
        let now = rappel_core::time::now_ms();
        let day = 86_400_000;
        for i in 0..2 {
            let id = state
                .add_card_with_tags(&deck.id, &format!("front {i}"), "back", &[])
                .unwrap()
                .card
                .id;
            for (grade, at) in [
                (Grade::Pass, now - 10 * day),
                (Grade::Pass, now - 10 * day + 660_000),
                (Grade::Fail, now - day),
            ] {
                state
                    .record_review(&id, grade, Duration::from_secs(5), at)
                    .unwrap();
            }
        }
        deck
    }

    fn shown(page: &StatsPage) -> Option<String> {
        page.imp()
            .content_stack
            .visible_child_name()
            .map(|n| n.to_string())
    }

    #[test]
    fn nothing_studied_shows_the_empty_state() {
        crate::testing::with_gtk(|| {
            let (_tmp, state) = state();
            let page = StatsPage::new(&state, None).unwrap();
            assert_eq!(shown(&page).as_deref(), Some("empty"));
            // A deck of new cards has nothing to show either.
            let deck = state.create_deck("Deck").unwrap();
            state
                .add_card_with_tags(&deck.id, "front", "back", &[])
                .unwrap();
            let page = StatsPage::new(&state, Some(&deck)).unwrap();
            assert_eq!(shown(&page).as_deref(), Some("empty"));
        });
    }

    #[test]
    fn studied_cards_fill_the_headline_and_the_charts() {
        crate::testing::with_gtk(|| {
            let (_tmp, state) = state();
            let deck = studied(&state);
            let page = StatsPage::new(&state, Some(&deck)).unwrap();
            let imp = page.imp();
            assert_eq!(shown(&page).as_deref(), Some("stats"));
            assert_eq!(imp.window_title.subtitle(), "Deck");
            assert_ne!(imp.retention_value.label(), "—");
            // Six answers in the 30 days, two failed.
            assert_eq!(imp.pass_rate_value.label(), "67%");
            assert_eq!(imp.time_value.label(), "30 s");
            assert!(imp.reviews_chart.shows_chart());
            assert!(imp.time_chart.shows_chart());
            assert!(imp.forecast_chart.shows_chart());
            let all = StatsPage::new(&state, None).unwrap();
            assert_eq!(all.imp().window_title.subtitle(), "All Decks");
            assert_eq!(all.imp().pass_rate_value.label(), "67%");
        });
    }

    #[test]
    fn a_chart_without_data_says_so_instead() {
        crate::testing::with_gtk(|| {
            let (_tmp, state) = state();
            let deck = state.create_deck("Deck").unwrap();
            let id = state
                .add_card_with_tags(&deck.id, "front", "back", &[])
                .unwrap()
                .card
                .id;
            // Reviewed once, long ago and failed: no review in the window.
            let long_ago = rappel_core::time::now_ms() - 90 * 86_400_000;
            state
                .record_review(&id, Grade::Fail, Duration::from_secs(3), long_ago)
                .unwrap();
            let page = StatsPage::new(&state, None).unwrap();
            let imp = page.imp();
            assert_eq!(shown(&page).as_deref(), Some("stats"));
            assert!(!imp.reviews_chart.shows_chart());
            assert!(!imp.time_chart.shows_chart());
            assert_eq!(imp.pass_rate_value.label(), "—");
            assert_eq!(imp.time_value.label(), "—");
        });
    }

    #[test]
    fn a_deleted_deck_has_no_page() {
        crate::testing::with_gtk(|| {
            let (_tmp, state) = state();
            let deck = studied(&state);
            state.delete_deck(&deck.id).unwrap();
            assert!(matches!(
                StatsPage::new(&state, Some(&deck)),
                Err(rappel_core::Error::DeckNotFound(_))
            ));
        });
    }
}
