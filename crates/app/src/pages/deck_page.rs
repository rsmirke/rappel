//! One deck's page: its cards in a `GtkListView`, or an empty state, with
//! add, edit, delete and reset progress (each undone from a toast). Layout in
//! `data/deck_page.blp` (ADR 0013).
//!
//! The subtitle and the Study button follow what there is to study. A
//! one-shot timer re-checks it when the next card becomes due or the next
//! study day begins, so they change while the page is open.
//!
//! The list model is a `gio::ListStore` of [`CardObject`] in creation
//! order. Changes update it item by item instead of rebuilding it, so a
//! deck of thousands of cards keeps its scroll position and stays fast.

use std::cmp::Ordering;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use rappel_core::domain::{Card, Deck, TaggedCard};
use rappel_core::scheduling::StudyInput;

use crate::dialogs::card_editor::{CardEditor, Saved};
use crate::dialogs::card_info;
use crate::pages::{ReviewPage, StatsPage};
use crate::state::AppState;
use crate::ui::focus::{self, ShownFocus, index_after_removal};
use crate::ui::text::due_new_line;
use crate::ui::toast::{info_toast, log_failure, show_error, show_toast_from, undo_toast};
use crate::ui::undo::{self, UndoFailure};
use crate::ui::{init_once, nav};
use crate::widgets::{CardObject, CardRow};
use crate::window::RappelWindow;

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};
    use std::rc::Rc;

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::{gio, glib};
    use rappel_core::domain::Deck;

    use crate::state::AppState;
    use crate::ui::debounce::Debounce;
    use crate::ui::rows;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::DeckPage)]
    #[template(resource = "/io/github/rsmirke/Rappel/deck_page.ui")]
    pub struct DeckPage {
        #[template_child]
        pub breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub study_bar: TemplateChild<gtk::ActionBar>,
        #[template_child]
        pub card_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub empty_add_button: TemplateChild<gtk::Button>,
        pub state: OnceCell<AppState>,
        pub deck: OnceCell<Deck>,
        /// The cards, oldest first.
        pub cards: OnceCell<gio::ListStore>,
        /// An editor is open; a second one (Ctrl+N, a click) is not opened.
        pub editing: Rc<Cell<bool>>,
        /// The card to focus when the editor closes: the one last saved.
        pub focus_after_edit: RefCell<Option<String>>,
        /// The pending re-check of what there is to study.
        pub study_timer: Debounce,
        /// A narrow window (the breakpoint applies): the rows show fewer
        /// tag pills.
        #[property(get, set)]
        pub compact: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DeckPage {
        const NAME: &'static str = "RappelDeckPage";
        type Type = super::DeckPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("deck.new-card", None, |page, _, _| {
                page.new_card();
            });
            klass.install_action("deck.study", None, |page, _, _| {
                page.study();
            });
            klass.install_action("deck.statistics", None, |page, _, _| {
                page.show_statistics();
            });
            // Activated by the card rows' menus with the card id.
            rows::install_id_action(klass, "deck.edit-card", super::DeckPage::edit);
            rows::install_id_action(klass, "deck.delete-card", super::DeckPage::delete);
            rows::install_id_action(klass, "deck.suspend-card", |page, id| {
                page.suspend(id, true);
            });
            rows::install_id_action(klass, "deck.unsuspend-card", |page, id| {
                page.suspend(id, false);
            });
            rows::install_id_action(klass, "deck.reset-card", super::DeckPage::reset);
            rows::install_id_action(klass, "deck.card-info", super::DeckPage::card_info);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for DeckPage {
        fn constructed(&self) {
            self.parent_constructed();
            let page = self.obj();
            // Back from studying: what is left to study changed.
            page.connect_shown(|page| {
                page.update_study();
                page.focus_when_shown();
            });
            page.connect_compact_notify(super::DeckPage::update_study_bar);
            // AdwBreakpoint emits `apply` and `unapply` as its condition
            // starts and stops matching, the first time too when the page
            // is first sized (AdwBreakpoint docs; the browser page does
            // the same, ADR 0036).
            self.breakpoint.connect_apply(glib::clone!(
                #[weak]
                page,
                move |_| page.set_compact(true)
            ));
            self.breakpoint.connect_unapply(glib::clone!(
                #[weak]
                page,
                move |_| page.set_compact(false)
            ));
        }
    }
    impl WidgetImpl for DeckPage {}
    impl NavigationPageImpl for DeckPage {}
}

glib::wrapper! {
    pub struct DeckPage(ObjectSubclass<imp::DeckPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DeckPage {
    /// The page for `deck`, with its cards loaded.
    ///
    /// # Errors
    /// If the cards cannot be read.
    pub fn new(state: AppState, deck: Deck) -> rappel_core::Result<Self> {
        let page: Self = glib::Object::new();
        let imp = page.imp();
        let cards = state.list_tagged_cards(&deck.id)?;
        // Deck names are untrusted; NavigationPage:title is plain text.
        page.set_title(&deck.name);
        imp.window_title.set_title(&deck.name);

        let store = gio::ListStore::new::<CardObject>();
        let items: Vec<CardObject> = cards.into_iter().map(CardObject::new).collect();
        store.extend_from_slice(&items);
        store.connect_items_changed(glib::clone!(
            #[weak]
            page,
            move |_, _, _, _| {
                page.update_empty_state();
                page.update_study();
            }
        ));
        imp.card_list
            .set_model(Some(&gtk::NoSelection::new(Some(store.clone()))));
        imp.card_list.set_factory(Some(&row_factory(&page)));
        imp.card_list.connect_activate(glib::clone!(
            #[weak]
            page,
            move |_, position| {
                if let Some(item) = page.item(position) {
                    page.open_editor(Some(&item.card()));
                }
            }
        ));

        init_once(&imp.state, state);
        init_once(&imp.deck, deck);
        init_once(&imp.cards, store);
        page.update_empty_state();
        page.update_study();
        Ok(page)
    }

    /// The id of this page's deck.
    pub fn deck_id(&self) -> Option<String> {
        self.imp().deck.get().map(|d| d.id.clone())
    }

    /// Recomputes what there is to study: the subtitle and whether Study
    /// is enabled.
    pub fn update_study(&self) {
        let imp = self.imp();
        let (Some(state), Some(deck)) = (imp.state.get(), imp.deck.get()) else {
            return;
        };
        let now = rappel_core::time::now_ms();
        match state.study_input(&deck.id, now) {
            Ok(input) => {
                imp.window_title.set_subtitle(&study_subtitle(&input));
                self.action_set_enabled("deck.study", !input.is_empty());
                match (
                    state.next_study_change(&deck.id, now),
                    state.next_day_start(now),
                ) {
                    (Ok(next), Ok(day)) => self.arm_study_timer(timer_delay_ms(now, next, day)),
                    // The subtitle is right now; it only will not update by
                    // itself until the next change on this page.
                    (Err(err), _) | (_, Err(err)) => {
                        tracing::warn!("cannot read the next due time: {err}");
                    }
                }
            }
            Err(err) => {
                imp.window_title.set_subtitle("");
                self.action_set_enabled("deck.study", false);
                self.error_toast(&format!("What to study could not be read: {err}"));
            }
        }
    }

    /// Re-checks what there is to study after `delay_ms`, replacing any
    /// pending re-check. Holds the page weakly: a closed page ends it.
    fn arm_study_timer(&self, delay_ms: u64) {
        let page = self.downgrade();
        self.imp()
            .study_timer
            .restart(Duration::from_millis(delay_ms), move || {
                if let Some(page) = page.upgrade() {
                    page.update_study();
                }
            });
    }

    /// Pushes a review page for this deck.
    fn study(&self) {
        let imp = self.imp();
        let (Some(state), Some(deck)) = (imp.state.get(), imp.deck.get()) else {
            return;
        };
        let Some(view) = nav::navigation_view(self) else {
            return;
        };
        // Cards saved from the session's editor update this list too.
        let on_saved = glib::clone!(
            #[weak(rename_to = deck_page)]
            self,
            move |saved| {
                deck_page.show_saved(saved);
            }
        );
        let pushed = nav::push_unless_on_top(&view, || {
            let page = ReviewPage::new(state.clone(), deck.clone(), on_saved)?;
            self.action_set_enabled("deck.study", false);
            Ok::<_, rappel_core::Error>(page)
        });
        if let Err(err) = pushed {
            self.error_toast(&format!("The session could not start: {err}"));
        }
    }

    /// Pushes this deck's statistics.
    fn show_statistics(&self) {
        let imp = self.imp();
        let (Some(state), Some(deck)) = (imp.state.get(), imp.deck.get()) else {
            return;
        };
        let Some(view) = nav::navigation_view(self) else {
            return;
        };
        if let Err(err) = nav::push_unless_on_top(&view, || StatsPage::new(state, Some(deck))) {
            self.error_toast(&format!("The statistics could not be read: {err}"));
        }
    }

    /// Shows the information of card `id`.
    fn card_info(&self, id: &str) {
        if let Some(state) = self.imp().state.get() {
            // AdwDialog gives the focus back to the widget that had it (the
            // card's row, or its menu button), so there is nothing to do
            // when it closes.
            card_info::open(self, state, id, || {});
        }
    }

    /// Opens the editor for a new card in this deck.
    pub fn new_card(&self) {
        self.open_editor(None);
    }

    fn edit(&self, id: &str) {
        if let Some(item) = self.position(id).and_then(|p| self.item(p)) {
            self.open_editor(Some(&item.card()));
        }
    }

    fn open_editor(&self, card: Option<&Card>) {
        let imp = self.imp();
        let (Some(state), Some(deck)) = (imp.state.get(), imp.deck.get()) else {
            return;
        };
        CardEditor::open(
            self,
            &imp.editing,
            state.clone(),
            &deck.id,
            card,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |saved| page.saved(saved)
            ),
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move || {
                    if let Some(id) = page.imp().focus_after_edit.take()
                        && let Some(position) = page.position(&id)
                    {
                        page.focus(position);
                    }
                }
            ),
        );
    }

    /// Puts a card saved by this page's editor into the list and marks it
    /// to take the focus once the editor closes.
    fn saved(&self, saved: Saved) {
        if let Some(id) = self.show_saved(saved) {
            self.imp().focus_after_edit.replace(Some(id));
        }
    }

    /// Puts a saved card into the list, wherever the save came from (the
    /// review page's editor too). Returns its id.
    fn show_saved(&self, saved: Saved) -> Option<String> {
        let store = self.imp().cards.get()?;
        let tagged = match saved {
            Saved::Added(tagged) => {
                insert_in_order(store, &CardObject::new(tagged.clone()));
                tagged
            }
            Saved::Edited(tagged) => {
                match self.position(&tagged.card.id) {
                    Some(position) => {
                        store.splice(position, 1, &[CardObject::new(tagged.clone())]);
                    }
                    None => {
                        insert_in_order(store, &CardObject::new(tagged.clone()));
                    }
                }
                tagged
            }
        };
        Some(tagged.card.id)
    }

    /// Deletes at once; the toast's Undo brings the card back.
    fn delete(&self, id: &str) {
        let imp = self.imp();
        let (Some(state), Some(store)) = (imp.state.get(), imp.cards.get()) else {
            return;
        };
        let Some(position) = self.position(id) else {
            return;
        };
        if let Err(err) = state.delete_card(id) {
            self.error_toast(&format!("The card could not be deleted: {err}"));
            return;
        }
        store.remove(position);
        self.focus_or_empty_state(index_after_removal(position, store.n_items()));

        // Undo must work after going back to the deck list too, when this
        // page may be gone: the closure keeps the state, not the page.
        let id = id.to_owned();
        let state = state.clone();
        let page = self.downgrade();
        let window = self
            .root()
            .and_downcast::<RappelWindow>()
            .map(|w| w.downgrade());
        // The front is not in the title: it can be long or many lines.
        let toast = undo_toast("Card deleted", move || {
            undo_card_delete(&state, &id, &page, window.as_ref());
        });
        show_toast_from(self, toast);
    }

    /// Makes the card new again at once, keeping its history; the toast's
    /// Undo cancels the reset (ADR 0028, 0030).
    fn reset(&self, id: &str) {
        let Some(state) = self.imp().state.get() else {
            return;
        };
        let entry = match state.reset_card(id, rappel_core::time::now_ms()) {
            Ok(entry) => entry,
            Err(err) => {
                self.error_toast(&format!("The progress could not be reset: {err}"));
                return;
            }
        };
        let window = self.root().and_downcast::<RappelWindow>();
        counts_changed(self, window.as_ref());

        // As for a delete: Undo must work from another page, so the closure
        // keeps the state and weak references, not the page.
        let (card_id, entry_id) = (id.to_owned(), entry.id);
        let state = state.clone();
        let page = self.downgrade();
        let window = window.map(|w| w.downgrade());
        let toast = undo_toast("Progress reset", move || {
            undo_reset(&state, &card_id, &entry_id, &page, window.as_ref());
        });
        show_toast_from(self, toast);
    }

    /// Suspends or unsuspends the card at once; the toast's Undo puts the
    /// flag back (ADR 0035). A suspended card leaves what there is to study.
    fn suspend(&self, id: &str, suspended: bool) {
        let Some(state) = self.imp().state.get() else {
            return;
        };
        let changed = match state.set_suspended(&[id.to_owned()], suspended) {
            Ok(changed) => changed,
            Err(err) => {
                self.error_toast(&format!("The card could not be changed: {err}"));
                return;
            }
        };
        if changed.is_empty() {
            return;
        }
        self.refresh_card(id);
        let window = self.root().and_downcast::<RappelWindow>();
        counts_changed(self, window.as_ref());

        // As for a delete: Undo must work from another page.
        let (id, state) = (id.to_owned(), state.clone());
        let page = self.downgrade();
        let window = window.map(|w| w.downgrade());
        let title = if suspended {
            "Card suspended"
        } else {
            "Card unsuspended"
        };
        let toast = undo_toast(title, move || {
            undo_suspend(&state, &id, suspended, &page, window.as_ref());
        });
        show_toast_from(self, toast);
    }

    /// Reads card `id` again into its row, after its flag changed.
    fn refresh_card(&self, id: &str) {
        let imp = self.imp();
        let (Some(state), Some(store)) = (imp.state.get(), imp.cards.get()) else {
            return;
        };
        let Some(position) = self.position(id) else {
            return;
        };
        match (state.card(id), state.card_tags(id)) {
            (Ok(card), Ok(tags)) => {
                store.splice(position, 1, &[CardObject::new(TaggedCard { card, tags })]);
            }
            (Err(err), _) | (_, Err(err)) => {
                self.error_toast(&format!("The card could not be read: {err}"));
            }
        }
    }

    /// Puts an undeleted card back in its place and focuses it, if it is
    /// a card of this deck. Harmless when it is already there.
    pub fn restored(&self, card: &Card) {
        let imp = self.imp();
        let (Some(store), Some(deck)) = (imp.cards.get(), imp.deck.get()) else {
            return;
        };
        if card.deck_id != deck.id {
            return;
        }
        let position = if let Some(position) = self.position(&card.id) {
            position
        } else {
            // A deleted card keeps its tags (ADR 0029).
            let tags = imp
                .state
                .get()
                .and_then(|state| match state.card_tags(&card.id) {
                    Ok(tags) => Some(tags),
                    Err(err) => {
                        tracing::warn!("a restored card's tags could not be read: {err}");
                        None
                    }
                })
                .unwrap_or_default();
            let tagged = TaggedCard {
                card: card.clone(),
                tags,
            };
            insert_in_order(store, &CardObject::new(tagged))
        };
        self.focus(position);
    }

    /// Reads the deck's cards again, e.g. after an import added some. The
    /// subtitle and Study follow through `items-changed`.
    pub fn reload(&self) {
        let imp = self.imp();
        let (Some(state), Some(deck), Some(store)) =
            (imp.state.get(), imp.deck.get(), imp.cards.get())
        else {
            return;
        };
        match state.list_tagged_cards(&deck.id) {
            Ok(cards) => {
                let items: Vec<CardObject> = cards.into_iter().map(CardObject::new).collect();
                store.splice(0, store.n_items(), &items);
            }
            Err(err) => self.error_toast(&format!("The cards could not be read: {err}")),
        }
    }

    /// Focuses the first card (or Add Card) unless the focus is already
    /// on this page (see [`focus::when_shown`]). On a push the focus is
    /// set here, in `shown`, never before: a grab before or during the push
    /// is lost (`ui::nav`). At startup the page is pushed without animation
    /// before the window is shown, and `shown` is emitted during that push:
    /// with animations off `switch_page` skips the transition, whose `done`
    /// handler emits it (`adw_animation_skip`, `transition_done_cb`,
    /// libadwaita 1.8 and 1.9). The window then keeps this focus widget
    /// (`gtk_window_show` moves the focus only when it has none,
    /// gtk/gtkwindow.c, GTK 4.22).
    fn focus_when_shown(&self) {
        let inside = self
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus.is_ancestor(self));
        let rows = self.imp().cards.get().map_or(0, gio::ListStore::n_items);
        match focus::when_shown(inside, rows) {
            ShownFocus::Keep => {}
            ShownFocus::FirstRow => {
                // `scroll_to` with FOCUS only moves the list's focus item
                // while the focus is outside it; the grab then focuses that
                // item's row, making it if needed (`gtk_list_base_scroll_to`,
                // `gtk_list_base_grab_focus`, gtk/gtklistbase.c, GTK 4.22).
                let list = &self.imp().card_list;
                list.scroll_to(0, gtk::ListScrollFlags::FOCUS, None);
                list.grab_focus();
            }
            ShownFocus::EmptyState => self.focus_or_empty_state(None),
        }
    }

    /// Focuses the card at `position`, or with `None` the empty state's
    /// Add Card button.
    fn focus_or_empty_state(&self, position: Option<u32>) {
        match position {
            Some(position) => self.focus(position),
            None => {
                self.imp().empty_add_button.grab_focus();
            }
        }
    }

    /// Scrolls to the card at `position` and gives it the keyboard focus.
    fn focus(&self, position: u32) {
        self.imp()
            .card_list
            .scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
    }

    fn update_empty_state(&self) {
        let imp = self.imp();
        let empty = imp.cards.get().is_none_or(|s| s.n_items() == 0);
        imp.content_stack
            .set_visible_child_name(if empty { "empty" } else { "list" });
        self.update_study_bar();
    }

    /// Reveals the bottom Study bar only on a narrow window over a deck
    /// with cards (see [`study_bar_shown`]).
    fn update_study_bar(&self) {
        let imp = self.imp();
        let empty = imp.cards.get().is_none_or(|s| s.n_items() == 0);
        imp.study_bar
            .set_revealed(study_bar_shown(self.compact(), empty));
    }

    fn item(&self, position: u32) -> Option<CardObject> {
        self.imp().cards.get()?.item(position).and_downcast()
    }

    /// Where card `id` is in the list (see [`position_in`]).
    fn position(&self, id: &str) -> Option<u32> {
        position_in(self.imp().cards.get()?, id)
    }

    fn error_toast(&self, message: &str) {
        show_error(self, "a card operation", message);
    }
}

/// Where card `id` is in `store`. A linear scan, only on a user action:
/// about a millisecond for 5,000 cards in a debug build (issue #42; see
/// `finding_the_last_of_5000_cards_is_fast`).
fn position_in(store: &gio::ListStore, id: &str) -> Option<u32> {
    (0..store.n_items()).find(|&position| {
        store
            .item(position)
            .and_downcast::<CardObject>()
            .is_some_and(|c| c.card().id == id)
    })
}

/// Updates the counts after a reset or its undo: the deck's subtitle and
/// Study button, and the deck list. Through `window` when there is one, since
/// the deck may have been reopened in a new page.
fn counts_changed(page: &DeckPage, window: Option<&RappelWindow>) {
    match window {
        Some(window) => window.study_changed(),
        None => page.update_study(),
    }
}

/// The Undo of a reset: cancels reset entry `entry_id` of card `card_id`.
/// If the card was studied since, the entry is no longer its latest and
/// nothing changes but a short message.
fn undo_reset(
    state: &AppState,
    card_id: &str,
    entry_id: &str,
    page: &glib::WeakRef<DeckPage>,
    window: Option<&glib::WeakRef<RappelWindow>>,
) {
    let window = window.and_then(glib::WeakRef::upgrade);
    match state.undo(card_id, entry_id, rappel_core::time::now_ms()) {
        Ok(_) => {
            if let Some(page) = page.upgrade() {
                counts_changed(&page, window.as_ref());
            } else if let Some(window) = &window {
                window.study_changed();
            }
        }
        Err(err) => {
            let (what, message) = match undo::failure(&err) {
                UndoFailure::Stale => {
                    // Not an error: the card moved on, and nothing was written.
                    if let Some(window) = &window {
                        show_toast_from(window, info_toast("The reset can no longer be undone"));
                    }
                    return;
                }
                UndoFailure::Failed => (
                    "a card operation",
                    format!("The reset could not be undone: {err}"),
                ),
            };
            if let Some(window) = window {
                show_error(&window, what, &message);
            } else {
                log_failure(what, &message);
            }
        }
    }
}

/// The Undo of a suspend (`suspended` is what the action did): puts the flag
/// of card `id` back, on `page` if it is still there, and updates the counts
/// through `window`.
fn undo_suspend(
    state: &AppState,
    id: &str,
    suspended: bool,
    page: &glib::WeakRef<DeckPage>,
    window: Option<&glib::WeakRef<RappelWindow>>,
) {
    let window = window.and_then(glib::WeakRef::upgrade);
    match state.restore_suspended(&[id.to_owned()], !suspended) {
        Ok(_) => {
            if let Some(page) = page.upgrade() {
                page.refresh_card(id);
                counts_changed(&page, window.as_ref());
            } else if let Some(window) = &window {
                window.study_changed();
            }
        }
        Err(err) => {
            let message = format!("The change could not be undone: {err}");
            if let Some(window) = window {
                show_error(&window, "a card operation", &message);
            } else {
                log_failure("a card operation", &message);
            }
        }
    }
}

/// The Undo of a card delete: brings card `id` back, on `page` if it is
/// still there and through `window` (the deck may have been reopened in a
/// new page, or left). Errors go to the window's toasts.
fn undo_card_delete(
    state: &AppState,
    id: &str,
    page: &glib::WeakRef<DeckPage>,
    window: Option<&glib::WeakRef<RappelWindow>>,
) {
    let window = window.and_then(glib::WeakRef::upgrade);
    match state.undelete_card(id) {
        Ok(card) => {
            if let Some(page) = page.upgrade() {
                page.restored(&card);
            }
            if let Some(window) = window {
                // The deck may have been reopened in a new page.
                window.card_restored(&card);
            }
        }
        Err(err) => {
            let message = format!("The card could not be restored: {err}");
            if let Some(window) = window {
                show_error(&window, "a card operation", &message);
            } else {
                log_failure("a card operation", &message);
            }
        }
    }
}

/// The deck page's subtitle: "1,234 due · 20 new", or "Nothing to study"
/// (the same counts as the deck list's rows, `ui::text::due_new_line`).
fn study_subtitle(input: &StudyInput) -> String {
    due_new_line(input.reviews.len() + input.steps.len(), input.new.len())
        .unwrap_or_else(|| "Nothing to study".to_owned())
}

/// Whether the bottom Study bar shows: on a narrow window (`compact`, where
/// Study leaves the header), unless the deck is empty. An empty deck shows
/// its status page with Add Card alone; a disabled Study pill under it
/// would only be noise. On a wide window the header's Study stays, disabled,
/// so the header does not change shape when the first card is added.
fn study_bar_shown(compact: bool, empty: bool) -> bool {
    compact && !empty
}

/// The longest single wait before re-checking what there is to study. A
/// later change is reached in steps, so a changed clock or time zone is
/// noticed within this long.
const MAX_STUDY_WAIT_MS: u64 = 3_600_000;

/// How long to wait from `now` until the earlier of `next_change` (the next
/// card that becomes due, if any) and `next_day` (the next study day, which
/// renews the new cards), capped at [`MAX_STUDY_WAIT_MS`]. All times are
/// UTC milliseconds; a time already past gives 0.
fn timer_delay_ms(now: i64, next_change: Option<i64>, next_day: i64) -> u64 {
    let at = next_change.map_or(next_day, |change| change.min(next_day));
    u64::try_from(at.saturating_sub(now))
        .unwrap_or(0)
        .min(MAX_STUDY_WAIT_MS)
}

/// Inserts `item` where its creation order puts it and returns that
/// position. Card ids increase with creation (ADR 0008), the same order as
/// `list_cards`. `insert_sorted` searches by halves, so it relies on that:
/// an import that inserts cards with older ids (SPEC §4.1) must keep `seq`
/// order equal to id order, or this must compare by `seq` (ADR 0013).
fn insert_in_order(store: &gio::ListStore, item: &CardObject) -> u32 {
    store.insert_sorted(item, |a, b| {
        match (
            a.downcast_ref::<CardObject>(),
            b.downcast_ref::<CardObject>(),
        ) {
            (Some(a), Some(b)) => a.card().id.cmp(&b.card().id),
            _ => Ordering::Equal,
        }
    })
}

/// Builds a [`CardRow`] per visible row, following the page's `compact`,
/// and points it at its card.
fn row_factory(page: &DeckPage) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let page = page.downgrade();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = CardRow::default();
        if let Some(page) = page.upgrade() {
            page.bind_property("compact", &row, "compact")
                .sync_create()
                .build();
        }
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let (Some(card), Some(row)) = (
            item.item().and_downcast::<CardObject>(),
            item.child().and_downcast::<CardRow>(),
        ) {
            row.bind(&card);
            // Screen readers read the front, the back and the tags, not
            // "row" (`card_row::text::row_label`).
            item.set_accessible_label(&card.row_label());
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
    use rappel_core::domain::Grade;
    use rappel_core::scheduling::Due;

    use super::*;

    fn due(id: &str) -> Due {
        Due {
            card_id: id.to_owned(),
            due_at: 0,
        }
    }

    #[test]
    fn the_timer_waits_for_the_next_due_card_or_the_next_day() {
        let now = 1_000_000;
        // A card due in 10 minutes comes before tomorrow.
        assert_eq!(
            timer_delay_ms(now, Some(now + 600_000), now + 50_000_000),
            600_000
        );
        // Nothing scheduled: wait for the next day, at most an hour at a time.
        assert_eq!(timer_delay_ms(now, None, now + 1_800_000), 1_800_000);
        assert_eq!(
            timer_delay_ms(now, None, now + 50_000_000),
            MAX_STUDY_WAIT_MS
        );
        // The next day comes before the card.
        assert_eq!(
            timer_delay_ms(now, Some(now + 900_000), now + 300_000),
            300_000
        );
        // A time already past fires at once.
        assert_eq!(timer_delay_ms(now, Some(now - 5), now + 300_000), 0);
    }

    #[test]
    fn finding_the_last_of_5000_cards_is_fast() {
        let (found, scans) = crate::testing::within(
            Duration::from_secs(5),
            "5,000 cards made and scanned",
            || {
                let store = gio::ListStore::new::<CardObject>();
                let items: Vec<CardObject> = (0..5_000)
                    .map(|i| {
                        CardObject::new(TaggedCard {
                            card: Card {
                                id: format!("card-{i:05}"),
                                deck_id: "deck".to_owned(),
                                front_md: "Front".to_owned(),
                                back_md: "Back".to_owned(),
                                created_at: i,
                                updated_at: i,
                                suspended: false,
                            },
                            tags: Vec::new(),
                        })
                    })
                    .collect();
                store.extend_from_slice(&items);
                let start = std::time::Instant::now();
                let found = (
                    position_in(&store, "card-04999"),
                    position_in(&store, "missing"),
                );
                (found, start.elapsed())
            },
        );
        crate::testing::print_timing("position_in, two scans of 5,000 cards", scans);
        assert_eq!(found, (Some(4_999), None));
        // About 2 ms in a debug build; the bound leaves 10 times that.
        assert!(scans < Duration::from_millis(20), "{scans:?}");
    }

    #[test]
    fn the_study_bar_shows_only_on_a_narrow_window_with_cards() {
        assert!(study_bar_shown(true, false));
        assert!(!study_bar_shown(true, true));
        assert!(!study_bar_shown(false, false));
        assert!(!study_bar_shown(false, true));
    }

    #[test]
    fn subtitle_names_what_is_left() {
        let mut input = StudyInput::default();
        assert_eq!(study_subtitle(&input), "Nothing to study");
        input.new = vec!["n".to_owned(); 20];
        assert_eq!(study_subtitle(&input), "20 new");
        input.reviews = vec![due("r1"), due("r2")];
        input.steps = vec![due("s")];
        assert_eq!(study_subtitle(&input), "3 due · 20 new");
        input.new.clear();
        assert_eq!(study_subtitle(&input), "3 due");
        // Digits are grouped, as in the deck list.
        input.reviews = vec![due("r"); 1_234];
        assert_eq!(study_subtitle(&input), "1,235 due");
    }

    /// A page over a deck of `n` cards that are all due.
    fn page_of(n: usize) -> (tempfile::TempDir, AppState, DeckPage, Vec<String>) {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::open(tmp.path().join("data")).unwrap();
        let deck = state.create_deck("Deck").unwrap();
        let now = rappel_core::time::now_ms();
        let day = 24 * 60 * 60 * 1000;
        let ids = (0..n)
            .map(|i| {
                let id = state
                    .add_card_with_tags(&deck.id, &format!("front {i}"), "back", &[])
                    .unwrap()
                    .card
                    .id;
                for at in [now - 60 * day, now - 60 * day + 11 * 60 * 1000] {
                    state
                        .record_review(&id, Grade::Pass, Duration::ZERO, at)
                        .unwrap();
                }
                id
            })
            .collect();
        let page = DeckPage::new(state.clone(), deck).unwrap();
        (tmp, state, page, ids)
    }

    #[test]
    fn a_reset_changes_the_counts_and_its_undo_changes_them_back() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, page, ids) = page_of(2);
            let subtitle = || page.imp().window_title.subtitle().to_string();
            assert_eq!(subtitle(), "2 due");
            page.reset(&ids[0]);
            assert_eq!(subtitle(), "1 due · 1 new");
            // The reset is the card's latest entry: the toast's Undo works.
            let entry = state
                .review_events(&ids[0])
                .unwrap()
                .last()
                .unwrap()
                .id
                .clone();
            undo_reset(&state, &ids[0], &entry, &page.downgrade(), None);
            assert_eq!(subtitle(), "2 due");
        });
    }

    #[test]
    fn a_suspended_card_leaves_the_counts_and_its_undo_brings_it_back() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, page, ids) = page_of(2);
            let subtitle = || page.imp().window_title.subtitle().to_string();
            assert_eq!(subtitle(), "2 due");
            page.suspend(&ids[0], true);
            assert_eq!(subtitle(), "1 due");
            assert!(page.item(0).unwrap().card().suspended);
            assert!(page.item(0).unwrap().row_label().ends_with(", suspended"));
            undo_suspend(&state, &ids[0], true, &page.downgrade(), None);
            assert_eq!(subtitle(), "2 due");
            assert!(!page.item(0).unwrap().card().suspended);
        });
    }

    #[test]
    fn the_study_bar_follows_the_width_and_the_cards() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, page, ids) = page_of(1);
            let bar = || page.imp().study_bar.is_revealed();
            assert!(!bar());
            page.set_compact(true);
            assert!(bar());
            // The last card goes: the empty state stands alone.
            page.delete(&ids[0]);
            assert!(!bar());
            undo_card_delete(&state, &ids[0], &page.downgrade(), None);
            assert!(bar());
            page.set_compact(false);
            assert!(!bar());
        });
    }

    #[test]
    fn an_undo_after_the_card_was_studied_again_changes_nothing() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, page, ids) = page_of(1);
            page.reset(&ids[0]);
            let reset = state
                .review_events(&ids[0])
                .unwrap()
                .last()
                .unwrap()
                .id
                .clone();
            let now = rappel_core::time::now_ms();
            state
                .record_review(&ids[0], Grade::Pass, Duration::ZERO, now)
                .unwrap();
            let log = state.review_events(&ids[0]).unwrap();
            let card_state = state.card_state(&ids[0]).unwrap();
            undo_reset(&state, &ids[0], &reset, &page.downgrade(), None);
            assert_eq!(state.review_events(&ids[0]).unwrap(), log);
            assert_eq!(state.card_state(&ids[0]).unwrap(), card_state);
        });
    }
}
