//! One deck in the deck list: name, what it has to study (or when it next
//! will, `text`) and a menu with Rename and Delete. Layout in
//! `data/deck_row.blp` (ADR 0041).
//!
//! The menu items activate the row's own `row.rename` / `row.delete`
//! actions, which forward to the page's `decks.rename` / `decks.delete`
//! with this row's deck id (see `ui::rows`).

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use rappel_core::domain::DeckSummary;

use crate::ui::rows;

pub mod text;

mod imp {
    use std::cell::RefCell;

    use adw::subclass::prelude::*;
    use gtk::glib;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/deck_row.ui")]
    pub struct DeckRow {
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub due_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub separator_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub other_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
        pub deck_id: RefCell<String>,
        /// The accessible label last set, "Scratch, 12 due, 5 new".
        pub accessible_label: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DeckRow {
        const NAME: &'static str = "RappelDeckRow";
        type Type = super::DeckRow;
        type ParentType = gtk::ListBoxRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("row.rename", None, |row, _, _| {
                row.forward("decks.rename");
            });
            klass.install_action("row.delete", None, |row, _, _| {
                row.forward("decks.delete");
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DeckRow {}
    impl WidgetImpl for DeckRow {}
    impl ListBoxRowImpl for DeckRow {}
}

glib::wrapper! {
    pub struct DeckRow(ObjectSubclass<imp::DeckRow>)
        @extends gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl DeckRow {
    /// A row for `summary`, its line worded for `now` when the next study
    /// day begins at `next_day` (UTC milliseconds).
    pub fn new(summary: &DeckSummary, now: i64, next_day: i64) -> Self {
        let row: Self = glib::Object::new();
        row.imp().deck_id.replace(summary.deck.id.clone());
        row.update(summary, now, next_day);
        row
    }

    /// Shows `summary` (the same deck, maybe renamed or with other counts)
    /// in place, so a refresh keeps the keyboard focus on the row.
    pub fn update(&self, summary: &DeckSummary, now: i64, next_day: i64) {
        let imp = self.imp();
        let name = &summary.deck.name;
        let line = text::row_text(summary, now, next_day);
        imp.name_label.set_label(name);
        set_part(&imp.due_label, line.due.as_deref());
        set_part(&imp.other_label, line.other.as_deref());
        imp.separator_label.set_visible(line.has_separator());
        // The row is read as one: "Scratch, 12 due, 5 new" (the "·" and the
        // units in words).
        // Set only when it changed: the list refreshes every minute or so,
        // and a screen reader may announce a changed label of the focused
        // row again.
        let label = line.accessible_label(name);
        if *imp.accessible_label.borrow() != label {
            self.update_property(&[gtk::accessible::Property::Label(&label)]);
            rows::label_menu_button(&imp.menu_button, name);
            imp.accessible_label.replace(label);
        }
    }

    /// The id of the deck this row shows.
    pub fn deck_id(&self) -> String {
        self.imp().deck_id.borrow().clone()
    }

    fn forward(&self, action: &str) {
        rows::forward_id(self, action, &self.deck_id());
    }
}

/// Shows `text` in `label`, or hides the label.
fn set_part(label: &gtk::Label, text: Option<&str>) {
    label.set_label(text.unwrap_or_default());
    label.set_visible(text.is_some());
}
