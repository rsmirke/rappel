//! `CardObject`: one card as a list model item, for the deck page's
//! `GtkListView` (ADR 0013, 0042). It holds the card, its tags (each name on
//! one line, for the pills) and the one-line previews of its front and back
//! (plain text, never markup). The previews are made the first time a row
//! shows the card and then kept, so opening a deck of 5,000 cards parses
//! nothing until the rows scroll into view, and binding a recycled row
//! again is cheap.

use std::cell::{Cell, OnceCell, Ref, RefCell};

use gtk::glib;
use gtk::subclass::prelude::*;
use rappel_core::content::plain_summary;
use rappel_core::domain::{Card, TaggedCard};

use super::card_row::text::row_label;
use crate::ui::text::one_line;

mod imp {
    use super::{Card, Cell, OnceCell, RefCell};
    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Debug)]
    pub struct CardObject {
        pub card: RefCell<Card>,
        pub tags: RefCell<Vec<String>>,
        pub front: OnceCell<String>,
        pub back: OnceCell<String>,
        pub is_new: Cell<bool>,
    }

    /// `GObject` needs a default; [`super::CardObject::new`], the only
    /// constructor, replaces this empty card at once.
    impl Default for CardObject {
        fn default() -> Self {
            Self {
                card: RefCell::new(Card {
                    id: String::new(),
                    deck_id: String::new(),
                    front_md: String::new(),
                    back_md: String::new(),
                    created_at: 0,
                    updated_at: 0,
                    suspended: false,
                }),
                tags: RefCell::default(),
                front: OnceCell::new(),
                back: OnceCell::new(),
                is_new: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CardObject {
        const NAME: &'static str = "RappelCardObject";
        type Type = super::CardObject;
    }

    impl ObjectImpl for CardObject {}
}

glib::wrapper! {
    pub struct CardObject(ObjectSubclass<imp::CardObject>);
}

impl CardObject {
    pub fn new(tagged: TaggedCard) -> Self {
        let TaggedCard { card, tags } = tagged;
        let obj: Self = glib::Object::new();
        obj.imp()
            .tags
            .replace(tags.iter().map(|tag| one_line(&tag.name)).collect());
        obj.imp().card.replace(card);
        obj
    }

    /// The card. An edit replaces the whole object in the model.
    pub fn card(&self) -> Ref<'_, Card> {
        self.imp().card.borrow()
    }

    /// Whether the card is new: no surviving review since it was created or
    /// last reset. Decides whether its row offers Reset Progress. A card
    /// starts as not new; the page sets it when it puts the card in the
    /// list, and replaces the object when it changes, so the row rebinds.
    pub fn is_new(&self) -> bool {
        self.imp().is_new.get()
    }

    /// Marks the card new or not, before the object is put in the list.
    #[must_use]
    pub fn with_new(self, is_new: bool) -> Self {
        self.imp().is_new.set(is_new);
        self
    }

    /// A copy of this item, new or not: a new object, so the list rebinds
    /// its row. The previews already made are kept.
    pub fn copy_with_new(&self, is_new: bool) -> Self {
        let imp = self.imp();
        let obj: Self = glib::Object::new();
        let copy = obj.imp();
        copy.card.replace(imp.card.borrow().clone());
        copy.tags.replace(imp.tags.borrow().clone());
        for (from, to) in [(&imp.front, &copy.front), (&imp.back, &copy.back)] {
            if let Some(text) = from.get() {
                to.get_or_init(|| text.clone());
            }
        }
        copy.is_new.set(is_new);
        obj
    }

    /// The front on one line, without Markdown syntax (see
    /// [`plain_summary`]).
    pub fn front_preview(&self) -> &str {
        self.imp()
            .front
            .get_or_init(|| plain_summary(&self.imp().card.borrow().front_md))
    }

    /// The back on one line, without Markdown syntax; empty for an empty
    /// back.
    pub fn back_preview(&self) -> &str {
        self.imp()
            .back
            .get_or_init(|| plain_summary(&self.imp().card.borrow().back_md))
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

    /// What a screen reader says for the row (see [`row_label`]).
    pub fn row_label(&self) -> String {
        row_label(
            self.front_preview(),
            self.back_preview(),
            &self.tags(),
            self.card().suspended,
        )
    }
}
