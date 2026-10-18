//! `BrowseItem`: one row of the All Cards page as a list model item
//! (ADR 0036). It holds the [`BrowseRow`] and whether the card is chosen
//! (the `selected` property, which a row widget follows), and the tag
//! names for the pills. The front and
//! back previews are made the first time a row asks, so reading 5,000 rows
//! parses nothing until they scroll into view.

use std::cell::{Cell, OnceCell, Ref, RefCell};

use gtk::glib;
use gtk::subclass::prelude::*;
use rappel_core::content::plain_summary;
use rappel_core::domain::Card;
use rappel_core::storage::{BrowseRow, CardStatus};

use crate::ui::text::one_line;

mod imp {
    use super::{BrowseRow, Card, CardStatus, Cell, OnceCell, RefCell};
    use gtk::glib;
    use gtk::glib::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Debug, glib::Properties)]
    #[properties(wrapper_type = super::BrowseItem)]
    pub struct BrowseItem {
        pub row: RefCell<BrowseRow>,
        pub front: OnceCell<String>,
        pub back: OnceCell<String>,
        /// The tag names, each on one line, for the pills.
        pub tags: RefCell<Vec<String>>,
        /// Chosen in selection mode.
        #[property(get, set)]
        pub selected: Cell<bool>,
    }

    /// `GObject` needs a default; [`super::BrowseItem::new`], the only
    /// constructor, replaces this empty row at once.
    impl Default for BrowseItem {
        fn default() -> Self {
            Self {
                row: RefCell::new(BrowseRow {
                    card: Card {
                        id: String::new(),
                        deck_id: String::new(),
                        front_md: String::new(),
                        back_md: String::new(),
                        created_at: 0,
                        updated_at: 0,
                        suspended: false,
                    },
                    deck_name: String::new(),
                    tags: Vec::new(),
                    status: CardStatus::New,
                    is_new: true,
                }),
                front: OnceCell::new(),
                back: OnceCell::new(),
                tags: RefCell::default(),
                selected: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BrowseItem {
        const NAME: &'static str = "RappelBrowseItem";
        type Type = super::BrowseItem;
    }

    #[glib::derived_properties]
    impl ObjectImpl for BrowseItem {}
}

glib::wrapper! {
    pub struct BrowseItem(ObjectSubclass<imp::BrowseItem>);
}

impl BrowseItem {
    pub fn new(row: BrowseRow) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp()
            .tags
            .replace(row.tags.iter().map(|tag| one_line(&tag.name)).collect());
        obj.imp().row.replace(row);
        obj
    }

    /// The row as the store gave it. A change replaces the whole object.
    pub fn row(&self) -> Ref<'_, BrowseRow> {
        self.imp().row.borrow()
    }

    pub fn id(&self) -> String {
        self.imp().row.borrow().card.id.clone()
    }

    pub fn suspended(&self) -> bool {
        self.imp().row.borrow().card.suspended
    }

    /// No surviving review since it was created or last reset (see
    /// [`BrowseRow::is_new`]).
    pub fn is_new(&self) -> bool {
        self.imp().row.borrow().is_new
    }

    /// The front on one line, without Markdown syntax.
    pub fn front_preview(&self) -> &str {
        self.imp()
            .front
            .get_or_init(|| plain_summary(&self.imp().row.borrow().card.front_md))
    }

    /// The back on one line, without Markdown syntax.
    pub fn back_preview(&self) -> &str {
        self.imp()
            .back
            .get_or_init(|| plain_summary(&self.imp().row.borrow().card.back_md))
    }

    /// The tag names, each on one line (`ui::text::one_line`).
    pub fn tags(&self) -> Ref<'_, Vec<String>> {
        self.imp().tags.borrow()
    }

    /// Whether either preview has been made (they are made on first use).
    #[cfg(test)]
    pub fn has_previews(&self) -> bool {
        self.imp().front.get().is_some() || self.imp().back.get().is_some()
    }
}
