//! One row of a list of cards (ADR 0042, 0044): the front as the title,
//! the back dimmed under it with the tags as pills at the end of that line,
//! an icon when the card is suspended, and a menu with Edit, Card
//! Information, Suspend or Unsuspend, Reset Progress and Delete. The deck
//! page and All Cards use the same row, so both lists look and cost the
//! same; on All Cards ([`CardRow::for_browser`]) a third line says the deck
//! and the status, and a check button shows while cards are being chosen.
//! Layout in `data/card_row.blp`; what a screen reader says in `text.rs`
//! (and `pages::browser_text` for All Cards).
//!
//! Rows are recycled by the `GtkListView`: [`CardRow::bind`] and
//! [`CardRow::bind_browse`] point a row at another card, reusing its pill
//! labels. The `compact` property (bound to the page's, a narrow window)
//! shows fewer pills; `selecting` (All Cards) shows the check button
//! instead of the menu. As in `DeckRow`, the menu items activate the row's
//! own `row.edit` / `row.info` / `row.suspend` / `row.unsuspend` /
//! `row.reset` / `row.delete` actions, which forward to the page's
//! `edit-card` / `card-info` / `suspend-card` / `unsuspend-card` /
//! `reset-card` / `delete-card` actions (in the `deck.` or `browser.`
//! group) with the bound card's id.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::tag_pills::text::{COMPACT_ROW_PILLS, ROW_PILLS};
use super::{BrowseItem, CardObject};
use crate::pages::browser_text;
use crate::ui::rows;

pub mod text;

mod imp {
    use std::cell::{Cell, RefCell};

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use crate::widgets::{BrowseItem, TagPills};

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::CardRow)]
    #[template(resource = "/io/github/rsmirke/Rappel/card_row.ui")]
    pub struct CardRow {
        #[template_child]
        pub check: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub front_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub detail_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub back_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub tag_pills: TemplateChild<TagPills>,
        #[template_child]
        pub caption_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub suspended_icon: TemplateChild<gtk::Image>,
        /// A narrow row: fewer pills, so the back keeps some room.
        #[property(get, set)]
        pub compact: Cell<bool>,
        /// Cards are being chosen (All Cards): the check button shows, the
        /// menu does not.
        #[property(get, set)]
        pub selecting: Cell<bool>,
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub card_menu: TemplateChild<gtk::gio::MenuModel>,
        /// A row of All Cards: its actions are `browser.*`, not `deck.*`.
        pub browsing: Cell<bool>,
        /// The bound card, empty while unbound.
        pub card_id: RefCell<String>,
        /// All Cards: the bound item, whose `selected` the row follows, and
        /// the list item whose accessible label says it.
        pub item: RefCell<Option<BrowseItem>>,
        pub list_item: glib::WeakRef<gtk::ListItem>,
        pub selected_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// How many times an item's `selected` reached the row, so the
        /// tests can tell that an old item's handler is gone.
        #[cfg(test)]
        pub selected_seen: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CardRow {
        const NAME: &'static str = "RappelCardRow";
        type Type = super::CardRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            TagPills::ensure_type();
            klass.bind_template();
            klass.install_action("row.edit", None, |row, _, _| {
                row.forward("edit-card");
            });
            klass.install_action("row.info", None, |row, _, _| {
                row.forward("card-info");
            });
            klass.install_action("row.suspend", None, |row, _, _| {
                row.forward("suspend-card");
            });
            klass.install_action("row.unsuspend", None, |row, _, _| {
                row.forward("unsuspend-card");
            });
            klass.install_action("row.reset", None, |row, _, _| {
                row.forward("reset-card");
            });
            klass.install_action("row.delete", None, |row, _, _| {
                row.forward("delete-card");
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for CardRow {
        fn constructed(&self) {
            self.parent_constructed();
            crate::ui::rows::menu_on_demand(&self.menu_button, &self.card_menu);
            let row = self.obj();
            row.connect_compact_notify(super::CardRow::update_pill_count);
            row.update_pill_count();
            row.connect_selecting_notify(super::CardRow::update_selection);
        }
    }
    impl WidgetImpl for CardRow {}
    impl BoxImpl for CardRow {}
}

glib::wrapper! {
    pub struct CardRow(ObjectSubclass<imp::CardRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for CardRow {
    /// A row of the deck page.
    fn default() -> Self {
        glib::Object::new()
    }
}

impl CardRow {
    /// A row of All Cards: its menu acts through `browser.*`, it shows the
    /// deck and status line, and a long press on a touch screen starts
    /// choosing cards with it (a mouse has the Select button and Ctrl+A).
    pub fn for_browser() -> Self {
        let row = Self::default();
        let imp = row.imp();
        imp.browsing.set(true);
        imp.caption_label.set_visible(true);
        // Claiming the sequence keeps the release from activating the row
        // (GtkGesture docs: a claimed sequence cancels the other gestures).
        let press = gtk::GestureLongPress::new();
        press.set_touch_only(true);
        press.connect_pressed(glib::clone!(
            #[weak]
            row,
            move |gesture, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                row.forward("select-card");
            }
        ));
        row.add_controller(press);
        row
    }

    /// Shows `item` in this row. Called on every bind of a recycled row,
    /// so it only sets texts and reuses the pills (`TagPills::set_tags`).
    pub fn bind(&self, item: &CardObject) {
        let card = item.card();
        self.show(
            item.front_preview(),
            item.back_preview(),
            &item.tags(),
            card.suspended,
            &card.id,
        );
    }

    /// Shows an All Cards `item` in this row: the card, its deck and status,
    /// and whether it is chosen. `list_item`'s accessible label is what a
    /// screen reader says for the row.
    pub fn bind_browse(&self, item: &BrowseItem, list_item: &gtk::ListItem) {
        self.unbind();
        let imp = self.imp();
        {
            let row = item.row();
            self.show(
                item.front_preview(),
                item.back_preview(),
                &item.tags(),
                row.card.suspended,
                &row.card.id,
            );
            let status = browser_text::status_text(row.status, rappel_core::time::now_ms());
            imp.caption_label
                .set_label(&browser_text::caption(&row.deck_name, &status));
        }
        imp.list_item.set(Some(list_item));
        imp.item.replace(Some(item.clone()));
        let handler = item.connect_selected_notify(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_| {
                #[cfg(test)]
                row.imp()
                    .selected_seen
                    .set(row.imp().selected_seen.get() + 1);
                row.update_selection();
            }
        ));
        imp.selected_handler.replace(Some(handler));
        self.update_selection();
    }

    fn show(&self, front: &str, back: &str, tags: &[String], suspended: bool, id: &str) {
        let imp = self.imp();
        imp.front_label.set_label(front);
        imp.back_label.set_label(back);
        imp.tag_pills.set_tags(tags);
        imp.tag_pills.set_visible(!tags.is_empty());
        // Without a back or tags, the line is left out.
        imp.detail_box
            .set_visible(!back.is_empty() || !tags.is_empty());
        rows::label_menu_button(&imp.menu_button, text::spoken_front(front));
        imp.suspended_icon.set_visible(suspended);
        // The row offers the one of the two that changes something.
        self.action_set_enabled("row.suspend", !suspended);
        self.action_set_enabled("row.unsuspend", suspended);
        imp.card_id.replace(id.to_owned());
    }

    /// The check button, the menu and the accessible label of an All Cards
    /// row, after `selecting` or the item's `selected` changed.
    fn update_selection(&self) {
        let imp = self.imp();
        let selecting = self.selecting();
        imp.check.set_visible(selecting);
        imp.menu_button.set_visible(!selecting);
        let item = imp.item.borrow();
        let Some(item) = item.as_ref() else {
            return;
        };
        imp.check.set_active(item.selected());
        if let Some(list_item) = imp.list_item.upgrade() {
            let row = item.row();
            let status = browser_text::status_text(row.status, rappel_core::time::now_ms());
            list_item.set_accessible_label(&browser_text::row_label(
                &text::row_label(
                    item.front_preview(),
                    item.back_preview(),
                    &item.tags(),
                    false,
                ),
                &row.deck_name,
                &status,
                selecting.then(|| item.selected()),
            ));
        }
    }

    fn update_pill_count(&self) {
        let max = if self.compact() {
            COMPACT_ROW_PILLS
        } else {
            ROW_PILLS
        };
        self.imp().tag_pills.set_max_shown(max);
    }

    /// Forgets the card, before the row is recycled.
    pub fn unbind(&self) {
        let imp = self.imp();
        imp.card_id.borrow_mut().clear();
        if let (Some(item), Some(handler)) = (imp.item.take(), imp.selected_handler.take()) {
            item.disconnect(handler);
        }
        imp.list_item.set(None);
    }

    /// The bound card's id, empty while unbound.
    pub fn card_id(&self) -> String {
        self.imp().card_id.borrow().clone()
    }

    /// Activates the page's action `name` with the bound card's id.
    fn forward(&self, name: &str) {
        let id = self.card_id();
        if id.is_empty() {
            return;
        }
        let group = if self.imp().browsing.get() {
            "browser"
        } else {
            "deck"
        };
        rows::forward_id(self, &format!("{group}.{name}"), &id);
    }
}

/// A list factory of card rows, for the deck page's list and All Cards':
/// `make` makes each row, which then follows `page`'s properties named in
/// `follows` (`compact`, `selecting`); `bind` shows the list item's item, a
/// `T`, in its row; and a row is unbound before it is recycled.
pub fn factory<T: IsA<glib::Object>>(
    page: &impl IsA<glib::Object>,
    make: fn() -> CardRow,
    follows: &'static [&'static str],
    bind: fn(&CardRow, &T, &gtk::ListItem),
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let page = page.upcast_ref::<glib::Object>().downgrade();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = make();
        if let Some(page) = page.upgrade() {
            for &property in follows {
                page.bind_property(property, &row, property)
                    .sync_create()
                    .build();
            }
        }
        item.set_child(Some(&row));
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let (Some(shown), Some(row)) = (
            item.item().and_downcast::<T>(),
            item.child().and_downcast::<CardRow>(),
        ) {
            bind(&row, &shown, item);
        }
    });
    factory.connect_unbind(|_, item| {
        if let Some(row) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(|item| item.child().and_downcast::<CardRow>())
        {
            row.unbind();
        }
    });
    factory
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rappel_core::domain::{Card, Tag, TaggedCard};
    use rappel_core::storage::{BrowseRow, CardStatus};

    use super::*;

    /// Card `i` of a deck: a Markdown front, a back and a few tags.
    fn card(i: usize) -> CardObject {
        let tags = (0..i % 6)
            .map(|t| Tag {
                id: format!("tag-{t}"),
                name: format!("tag number {t}"),
            })
            .collect();
        CardObject::new(TaggedCard {
            card: Card {
                id: format!("card-{i:05}"),
                deck_id: "deck".to_owned(),
                front_md: format!("**Card {i}**: what is $\\frac{{{i}}}{{2}}$?"),
                back_md: format!("Answer {i}, with `code` and a [link](x)"),
                created_at: 0,
                updated_at: 0,
                suspended: i.is_multiple_of(7),
            },
            tags,
        })
    }

    /// Card `i` as All Cards lists it.
    fn browse_item(i: usize) -> BrowseItem {
        let item = card(i);
        BrowseItem::new(BrowseRow {
            card: item.card().clone(),
            deck_name: "Maths".to_owned(),
            tags: (0..i % 6)
                .map(|t| Tag {
                    id: format!("tag-{t}"),
                    name: format!("tag number {t}"),
                })
                .collect(),
            status: if i.is_multiple_of(7) {
                CardStatus::Suspended
            } else {
                CardStatus::New
            },
        })
    }

    #[test]
    fn a_row_shows_the_card_and_reads_it_out() {
        crate::testing::with_widgets(|| {
            let row = CardRow::default();
            let item = card(4);
            row.bind(&item);
            let imp = row.imp();
            assert_eq!(imp.front_label.label(), "Card 4: what is \\frac{4}{2}?");
            assert_eq!(imp.back_label.label(), "Answer 4, with code and a link");
            assert_eq!(imp.tag_pills.tags().len(), 4);
            assert!(!imp.suspended_icon.is_visible());
            // The menu is built when it first opens, not with the row.
            assert!(imp.menu_button.popover().is_none());
            assert_eq!(
                item.row_label(),
                "Card 4: what is \\frac{4}{2}?, back: Answer 4, with code and a link, \
                 tags: tag number 0, tag number 1, tag number 2, tag number 3"
            );
            // A recycled row follows its new card.
            row.bind(&card(42));
            assert!(imp.suspended_icon.is_visible());
            assert!(!imp.tag_pills.is_visible());
        });
    }

    #[test]
    fn an_all_cards_row_says_the_deck_and_follows_the_selection() {
        crate::testing::with_widgets(|| {
            let row = CardRow::for_browser();
            let list_item = glib::Object::new::<gtk::ListItem>();
            let item = browse_item(4);
            row.bind_browse(&item, &list_item);
            let imp = row.imp();
            assert_eq!(imp.caption_label.label(), "Maths · New");
            assert!(imp.caption_label.is_visible());
            assert!(!imp.check.is_visible() && imp.menu_button.is_visible());
            assert_eq!(imp.tag_pills.tags().len(), 4);
            assert_eq!(
                list_item.accessible_label(),
                ("Card 4: what is \\frac{4}{2}?, back: Answer 4, with code and a link, \
                     tags: tag number 0, tag number 1, tag number 2, tag number 3, \
                     deck: Maths, New")
            );
            // Choosing cards: the check button shows the item's state.
            row.set_selecting(true);
            assert!(imp.check.is_visible() && !imp.menu_button.is_visible());
            assert!(!imp.check.is_active());
            item.set_selected(true);
            assert!(imp.check.is_active());
            assert!(
                list_item
                    .accessible_label()
                    .ends_with("deck: Maths, New, selected")
            );
            // Recycled, it no longer follows the old item: the old item's
            // handler is gone, the new one's is there.
            let other = browse_item(7);
            row.bind_browse(&other, &list_item);
            assert!(!imp.check.is_active());
            let seen = imp.selected_seen.get();
            item.set_selected(false);
            assert_eq!(
                imp.selected_seen.get(),
                seen,
                "the old item reached the row"
            );
            other.set_selected(true);
            assert_eq!(imp.selected_seen.get(), seen + 1);
            assert!(imp.check.is_active());
            assert_eq!(imp.caption_label.label(), "Maths · Suspended");
            assert!(imp.suspended_icon.is_visible());
            // A deck page row has no third line.
            assert!(!CardRow::default().imp().caption_label.is_visible());
        });
    }

    /// The menu of a deck page row reaches `deck.*`, of an All Cards row
    /// `browser.*`, with the bound card's id.
    #[test]
    fn a_row_forwards_to_its_own_pages_actions() {
        crate::testing::with_widgets(|| {
            use std::cell::RefCell;
            use std::rc::Rc;

            let got: Rc<RefCell<Vec<String>>> = Rc::default();
            let group = |prefix: &'static str| {
                let actions = gtk::gio::SimpleActionGroup::new();
                let action =
                    gtk::gio::SimpleAction::new("edit-card", Some(glib::VariantTy::STRING));
                let got = got.clone();
                action.connect_activate(move |_, id| {
                    let id = id.and_then(glib::Variant::str).unwrap_or_default();
                    got.borrow_mut().push(format!("{prefix}.edit-card {id}"));
                });
                actions.add_action(&action);
                actions
            };
            let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
            outer.insert_action_group("deck", Some(&group("deck")));
            let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
            inner.insert_action_group("browser", Some(&group("browser")));
            outer.append(&inner);

            let deck_row = CardRow::default();
            inner.append(&deck_row);
            deck_row.bind(&card(3));
            deck_row.activate_action("row.edit", None).unwrap();

            let browser_row = CardRow::for_browser();
            inner.append(&browser_row);
            let list_item = glib::Object::new::<gtk::ListItem>();
            browser_row.bind_browse(&browse_item(4), &list_item);
            browser_row.activate_action("row.edit", None).unwrap();

            assert_eq!(
                *got.borrow(),
                ["deck.edit-card card-00003", "browser.edit-card card-00004"]
            );
        });
    }

    #[test]
    fn a_narrow_row_shows_fewer_pills() {
        crate::testing::with_widgets(|| {
            let row = CardRow::default();
            let pills = &row.imp().tag_pills;
            assert_eq!(pills.max_shown(), ROW_PILLS);
            row.set_compact(true);
            assert_eq!(pills.max_shown(), COMPACT_ROW_PILLS);
            row.set_compact(false);
            assert_eq!(pills.max_shown(), ROW_PILLS);
        });
    }

    #[test]
    fn the_menu_is_built_once_on_first_open() {
        crate::testing::with_widgets(|| {
            let window = gtk::Window::new();
            let row = CardRow::default();
            window.set_child(Some(&row));
            window.present();
            let button = &row.imp().menu_button;
            assert!(button.popover().is_none());
            button.popup();
            let first = button.popover().expect("opening the menu builds it");
            button.popdown();
            button.popup();
            assert_eq!(button.popover(), Some(first));
            button.popdown();
            window.destroy();
        });
    }

    /// Scrolling binds recycled rows over and over. What keeps it fast is
    /// checked as work, not time, so the test holds on a loaded machine
    /// (#67): after a first bind that makes the row's pills, binding it to
    /// 4,000 of 5,000 cards makes no widget (the same pill labels, no menu popover),
    /// and only the cards bound get their previews made. The CPU time of
    /// the thread (not the wall clock, which grows while the thread waits
    /// for a CPU) is printed with `RAPPEL_TIMINGS=1` and bounded only
    /// against a large regression: about 100 ms in a debug build (most of
    /// it making each card's two previews), 380 ms with three busy threads
    /// per CPU (shared caches slow the thread even while it runs).
    #[test]
    fn binding_a_row_to_thousands_of_cards_makes_no_widgets() {
        crate::testing::with_widgets(|| {
            let items: Vec<CardObject> = (0..5_000).map(card).collect();
            let row = CardRow::default();
            let pills = &row.imp().tag_pills;
            // Card 5 has the most tags: every pill and the "+N" pill exist.
            row.bind(&items[5]);
            let made = pills.pill_widgets();
            let (cpu, ()) = crate::testing::cpu_time_of(|| {
                for item in &items[..4_000] {
                    row.bind(item);
                    drop(item.row_label());
                    row.unbind();
                }
            });
            assert_eq!(pills.pill_widgets(), made);
            assert!(row.imp().menu_button.popover().is_none());
            // Previews are made only for the cards a row showed.
            assert!(items[..4_000].iter().all(CardObject::has_previews));
            assert!(!items[4_000..].iter().any(CardObject::has_previews));
            crate::testing::print_timing("CardRow::bind, 4,000 cards (CPU)", cpu);
            // Over five times the time measured under load; checked on this
            // thread, which did the work (ADR 0049).
            crate::testing::assert_cpu_within(cpu, Duration::from_secs(2), "CardRow::bind");
        });
    }

    /// As above, for the rows of All Cards (ADR 0044): binding one row to
    /// 4,000 of 5,000 cards makes no pill label and no popover, makes the
    /// previews of those cards only, and leaves no handler on the items it
    /// was bound to before.
    #[test]
    fn binding_an_all_cards_row_to_thousands_of_cards_makes_no_widgets() {
        crate::testing::with_widgets(|| {
            let items: Vec<BrowseItem> = (0..5_000).map(browse_item).collect();
            let list_item = glib::Object::new::<gtk::ListItem>();
            let row = CardRow::for_browser();
            row.set_selecting(true);
            let pills = &row.imp().tag_pills;
            row.bind_browse(&items[5], &list_item);
            let made = pills.pill_widgets();
            let (cpu, ()) = crate::testing::cpu_time_of(|| {
                for item in &items[..4_000] {
                    row.bind_browse(item, &list_item);
                    row.unbind();
                }
            });
            assert_eq!(pills.pill_widgets(), made);
            assert!(row.imp().menu_button.popover().is_none());
            assert!(items[..4_000].iter().all(BrowseItem::has_previews));
            assert!(!items[4_000..].iter().any(BrowseItem::has_previews));
            // Unbound, the row no longer follows any of them: no handler
            // is left on the items it was bound to.
            let seen = row.imp().selected_seen.get();
            for item in &items[..4_000] {
                item.set_selected(true);
            }
            assert_eq!(row.imp().selected_seen.get(), seen);
            crate::testing::print_timing("CardRow::bind_browse, 4,000 cards (CPU)", cpu);
            // The same bound as the deck page's rows, far above the time
            // measured.
            crate::testing::assert_cpu_within(cpu, Duration::from_secs(2), "CardRow::bind_browse");
        });
    }
}
