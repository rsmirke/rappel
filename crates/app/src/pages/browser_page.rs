//! The All Cards page: search, filters and every card in one virtualised
//! list, with actions on several cards at once, each undone from a toast
//! (SPEC §4.6, ADR 0035, ADR 0036). Layout in `data/browser_page.blp`.
//!
//! The list model is a `gio::ListStore` of [`BrowseItem`]. Every search,
//! filter change and write reads the rows again with `Store::browse` and
//! replaces the model in one `splice`; the `GtkListView` only builds rows
//! for what is on screen, so 5,000 cards stay smooth. The chosen cards are
//! the page's own [`Selection`] and not a `GtkSelectionModel`'s: outside
//! selection mode a click opens the editor, and a list view with a
//! selection model selects on a click instead.

use crate::i18n::gettext;
use crate::ui::text::with_error;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use rappel_core::domain::{Card, Deck, Tag};
use rappel_core::storage::BrowseQuery;

use crate::dialogs::card_editor::{CardEditor, Saved};
use crate::dialogs::{browser_dialogs, card_info};
use crate::state::AppState;
use crate::ui::focus::index_after_removal;
use crate::ui::text::one_line;
use crate::ui::toast::{info_toast, show_error, show_toast_from, undo_toast};
use crate::ui::{breakpoint, dropdown, init_once, nav};
use crate::widgets::tag_entry::field::MAX_TAGS_PER_CARD;
use crate::widgets::{BrowseItem, CardRow, card_row};
use crate::window::RappelWindow;

pub mod selection;
pub mod text;

use text::{Empty, EscapeAction, ListKey, SpaceAction, SuspendChoice};

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};
    use std::rc::Rc;

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::{gio, glib};
    use rappel_core::domain::{Deck, Tag};

    use super::selection::Selection;
    use crate::state::AppState;
    use crate::ui::rows;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::BrowserPage)]
    #[template(resource = "/io/github/rsmirke/Rappel/browser_page.ui")]
    pub struct BrowserPage {
        #[template_child]
        pub empty_short: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub title_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub selection_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub select_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub select_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub deck_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub tag_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub state_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub filter_bar: TemplateChild<adw::Clamp>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub empty_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub tags_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub count_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub card_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub action_bar: TemplateChild<gtk::ActionBar>,
        #[template_child]
        pub move_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub suspend_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub delete_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub suspend_content: TemplateChild<adw::ButtonContent>,
        /// A narrow window; the rows follow it.
        #[property(get, set)]
        pub compact: Cell<bool>,
        /// Cards are being chosen; the rows follow it.
        #[property(get, set)]
        pub selecting: Cell<bool>,
        pub state: OnceCell<AppState>,
        pub items: OnceCell<gio::ListStore>,
        pub selection: RefCell<Selection>,
        /// The decks and tags the filter drop-downs list, after "All".
        pub decks: RefCell<Vec<Deck>>,
        pub tags: RefCell<Vec<Tag>>,
        /// An editor is open; a second one is not opened.
        pub editing: Rc<Cell<bool>>,
        /// The card to focus when the editor closes: the one last saved.
        pub focus_after_edit: RefCell<Option<String>>,
        /// What the list area shows (the rows, or which status page).
        pub empty: Cell<super::Empty>,
        /// The filters are being rebuilt: their changes are not searches.
        pub quiet: Cell<bool>,
        /// Something was written here: the other pages read it on leaving.
        pub dirty: Cell<bool>,
        /// The window, noted when the page is shown: a page that was popped
        /// may no longer have a root when `hidden` is emitted.
        pub window: glib::WeakRef<crate::window::RappelWindow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BrowserPage {
        const NAME: &'static str = "RappelBrowserPage";
        type Type = super::BrowserPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            // Activated by the card rows' menus with the card id.
            rows::install_id_action(klass, "browser.edit-card", super::BrowserPage::edit);
            rows::install_id_action(klass, "browser.card-info", super::BrowserPage::card_info);
            rows::install_id_action(klass, "browser.suspend-card", |page, id| {
                page.run_on(id, super::Bulk::Suspend(true));
            });
            rows::install_id_action(klass, "browser.unsuspend-card", |page, id| {
                page.run_on(id, super::Bulk::Suspend(false));
            });
            rows::install_id_action(klass, "browser.reset-card", |page, id| {
                page.run_on(id, super::Bulk::Reset);
            });
            rows::install_id_action(klass, "browser.delete-card", |page, id| {
                page.run_on(id, super::Bulk::Delete);
            });
            // A long press on a touch screen.
            rows::install_id_action(klass, "browser.select-card", super::BrowserPage::press);
            // The action bar.
            klass.install_action("browser.move", None, |page, _, _| page.move_selected());
            klass.install_action("browser.add-tags", None, |page, _, _| {
                page.add_tags();
            });
            klass.install_action("browser.remove-tag", None, |page, _, _| page.remove_tag());
            klass.install_action("browser.suspend-selected", None, |page, _, _| {
                page.suspend_selected();
            });
            klass.install_action("browser.reset-selected", None, |page, _, _| {
                page.run_selected(super::Bulk::Reset);
            });
            klass.install_action("browser.delete-selected", None, |page, _, _| {
                page.run_selected(super::Bulk::Delete);
            });
            // The header in and out of selection mode.
            klass.install_action("browser.choose", None, |page, _, _| page.start_choosing());
            klass.install_action("browser.cancel", None, |page, _, _| page.stop_choosing());
            klass.install_action("browser.select-all", None, |page, _, _| page.select_all());
            // The status page's button: back to the decks, or clear.
            klass.install_action("browser.empty-action", None, |page, _, _| {
                page.empty_action();
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for BrowserPage {}
    impl WidgetImpl for BrowserPage {
        fn root(&self) {
            // Roots the children first (`gtk_widget_real_root`), so every
            // `AdwButtonContent` has named its button by then.
            self.parent_root();
            for button in [
                &*self.select_all_button,
                &*self.move_button,
                &*self.suspend_button,
                &*self.delete_button,
                &*self.reset_button,
            ] {
                super::name_by_own_label(button);
            }
        }
    }
    impl NavigationPageImpl for BrowserPage {}
}

glib::wrapper! {
    pub struct BrowserPage(ObjectSubclass<imp::BrowserPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// What a bulk action does to the cards it is given.
#[derive(Debug, Clone)]
enum Bulk {
    Suspend(bool),
    Move { deck_id: String },
    AddTags(Vec<String>),
    RemoveTag(Tag),
    Reset,
    Delete,
}

/// Takes a bulk action back; returns how many cards it restored.
type UndoFn = Box<dyn Fn(&AppState) -> rappel_core::Result<usize>>;

/// A bulk action that was done: what to tell, how many cards it changed
/// and how to take it back. The undo returns how many cards it restored.
struct Done {
    /// The Undo toast's title: short, so it fits beside Undo at 360 px
    /// (ADR 0058).
    text: String,
    /// More to say, in a toast of its own after it (which gets Details
    /// when long): the cards Add Tags left out.
    note: Option<String>,
    total: usize,
    undo: UndoFn,
}

/// Does `action` on `ids`. `Err` with the notice to show if no card
/// changed: every card was already as asked, or (Add Tags) had no room.
fn perform(
    state: &AppState,
    ids: &[String],
    action: Bulk,
    now: i64,
) -> rappel_core::Result<Result<Done, String>> {
    let done = match action {
        Bulk::Suspend(suspended) => {
            let changed = state.set_suspended(ids, suspended)?;
            Done {
                note: None,
                text: text::suspended_text(changed.len(), suspended),
                total: changed.len(),
                undo: Box::new(move |s| s.restore_suspended(&changed, !suspended)),
            }
        }
        Bulk::Move { deck_id } => {
            let moved = state.move_cards(ids, &deck_id)?;
            Done {
                note: None,
                text: text::moved_text(moved.len()),
                total: moved.len(),
                undo: Box::new(move |s| s.unmove_cards(&moved)),
            }
        }
        Bulk::AddTags(names) => {
            let limit = MAX_TAGS_PER_CARD;
            let added = state.add_tags_to_cards(ids, &names, limit)?;
            if added.cards.is_empty() {
                return Ok(Err(if added.full.is_empty() {
                    text::unchanged_text()
                } else {
                    text::no_room_text(limit)
                }));
            }
            let links = added.links;
            Done {
                text: text::tags_added_text(added.cards.len()),
                note: text::tags_left_out_text(added.full.len(), limit),
                total: added.cards.len(),
                undo: Box::new(move |s| s.release_tags(&links)),
            }
        }
        Bulk::RemoveTag(tag) => {
            let changed = state.remove_tag_from_cards(ids, &tag.id)?;
            Done {
                note: None,
                text: text::tag_removed_text(changed.len()),
                total: changed.len(),
                undo: Box::new(move |s| s.restore_tag_on_cards(&changed, &tag.id)),
            }
        }
        Bulk::Reset => {
            let events = state.reset_cards(ids, now)?;
            if events.is_empty() {
                return Ok(Err(text::no_progress_text(ids.len())));
            }
            Done {
                note: None,
                text: text::reset_text(events.len()),
                total: events.len(),
                undo: Box::new(move |s| {
                    let skipped = s.undo_entries(&events, rappel_core::time::now_ms())?;
                    Ok(events.len().saturating_sub(skipped.len()))
                }),
            }
        }
        Bulk::Delete => {
            state.delete_cards(ids)?;
            let ids = ids.to_vec();
            Done {
                note: None,
                text: text::deleted_text(ids.len()),
                total: ids.len(),
                undo: Box::new(move |s| s.undelete_cards(&ids)),
            }
        }
    };
    Ok(if done.total > 0 {
        Ok(done)
    } else {
        Err(text::unchanged_text())
    })
}

impl BrowserPage {
    /// The page, listing every card; `deck_id` starts it filtered to a deck.
    ///
    /// # Errors
    /// If the decks or tags cannot be read.
    pub fn new(state: AppState, deck_id: Option<&str>) -> rappel_core::Result<Self> {
        let page: Self = glib::Object::new();
        let imp = page.imp();
        let store = gio::ListStore::new::<BrowseItem>();
        imp.card_list
            .set_model(Some(&gtk::NoSelection::new(Some(store.clone()))));
        imp.card_list.set_factory(Some(&page.row_factory()));
        // The empty state on a short window (ADR 0058); its icon changes
        // with the state, so only `compact`.
        crate::ui::breakpoint::compact_when(&imp.empty_short, &*imp.empty_page);
        for filter in [&*imp.deck_filter, &*imp.tag_filter, &*imp.state_filter] {
            dropdown::shrinkable(filter);
        }
        init_once(&imp.state, state);
        init_once(&imp.items, store);
        page.load_filters(deck_id)?;
        page.connect_signals();
        page.load_rows()?;
        Ok(page)
    }

    /// Puts the keyboard in the search field (Ctrl+F). Not while cards are
    /// being chosen: the header shows the selection then, and Cancel or
    /// Escape comes first (the choice is not dropped by a shortcut).
    pub fn focus_search(&self) {
        if !self.selecting() {
            self.imp().search_entry.grab_focus();
        }
    }

    /// Reads the rows again, after an import.
    pub fn reload_all(&self) {
        if let Err(err) = self.load_filters(None) {
            // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
            self.error_toast(&with_error(
                &gettext("The decks could not be read: {error}"),
                &err,
            ));
        }
        self.requery();
    }

    fn connect_signals(&self) {
        let imp = self.imp();
        breakpoint::follow(
            &imp.breakpoint,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |compact| {
                    page.set_compact(compact);
                    page.update_selection_ui();
                }
            ),
        );
        imp.search_entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.search()
        ));
        // `stop-search` (Escape) has no default action (GtkSearchEntry
        // docs): clear the text, or, if there is none, leave the field.
        imp.search_entry.connect_stop_search(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |entry| {
                if entry.text().is_empty() {
                    page.leave_search();
                } else {
                    entry.set_text("");
                }
            }
        ));
        // Down in the field goes to the list.
        let down = gtk::EventControllerKey::new();
        down.connect_key_pressed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == text::TO_LIST_KEY {
                    page.imp().card_list.grab_focus();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        imp.search_entry.add_controller(down);
        for dropdown in [&*imp.deck_filter, &*imp.tag_filter, &*imp.state_filter] {
            dropdown.connect_selected_notify(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_| {
                    if !page.imp().quiet.get() {
                        page.search();
                    }
                }
            ));
        }
        imp.card_list.connect_activate(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_, position| page.activate(position)
        ));
        self.add_type_to_search();
        self.connect_hidden(|page| {
            // The deck pages and the deck list read what was written here
            // (the page may be about to go, so it is the window's to ask).
            if page.imp().dirty.replace(false)
                && let Some(window) = page.imp().window.upgrade()
            {
                window.data_changed();
            }
        });
        self.connect_shown(|page| {
            page.imp()
                .window
                .set(page.root().and_downcast::<RappelWindow>().as_ref());
            page.focus_search();
        });
        self.add_list_shortcuts();
        self.add_escape();
    }

    /// Typing in the list or on the status page searches: a key that types
    /// a character goes into the search field, which takes the focus
    /// without selecting its text, so typing, Backspace and the arrows
    /// carry on there (`text::types_into_search`). Bubble phase on the
    /// content, so the list's own keys come first, the filters and the
    /// header keep theirs, and a key typed while a popover is open (a row's
    /// menu, a drop-down's popup: the event comes from the popover's own
    /// surface) stays there. Not GTK's `gtk_search_entry_set_key_capture_widget`:
    /// it forwards keys from popovers under the capture widget too and
    /// leaves the focus behind (gtksearchentry.c,
    /// `capture_widget_key_handled`).
    fn add_type_to_search(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |keys, key, _, modifiers| {
                let imp = page.imp();
                let in_popover = keys.current_event().and_then(|event| event.surface())
                    != imp.content_stack.native().and_then(|n| n.surface());
                let search_empty = imp.search_entry.text().is_empty();
                if !text::types_into_search(
                    key,
                    modifiers,
                    in_popover,
                    page.selecting(),
                    search_empty,
                ) {
                    return glib::Propagation::Proceed;
                }
                let Some(field) = imp.search_entry.delegate().and_downcast::<gtk::Text>() else {
                    return glib::Propagation::Proceed;
                };
                field.grab_focus_without_selecting();
                field.set_position(-1);
                if keys.forward(&field) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        self.imp().content_stack.add_controller(keys);
    }

    /// Ctrl+A and Space on the list. Capture phase on the list, so they come
    /// before the list view's own bindings (`list.select-all` and the item's
    /// `listitem.select` do nothing on a list without a selection model).
    /// The search field is not under the list, so it keeps its own Ctrl+A.
    fn add_list_shortcuts(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| match text::list_key(key, modifiers) {
                Some(ListKey::SelectAll) => {
                    page.select_all();
                    glib::Propagation::Stop
                }
                Some(ListKey::Space) => {
                    let selecting = page.imp().selection.borrow().active();
                    match text::space_action(selecting, page.row_has_focus()) {
                        SpaceAction::Toggle => {
                            if let Some(id) = page.focused_card() {
                                page.toggle(&id);
                            }
                            glib::Propagation::Stop
                        }
                        SpaceAction::Pass => glib::Propagation::Proceed,
                    }
                }
                None => glib::Propagation::Proceed,
            }
        ));
        self.imp().card_list.add_controller(keys);
    }

    /// Escape while the focus is in the list. With nothing to leave or
    /// clear it is not handled, so the navigation view goes back.
    fn add_escape(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == text::ESCAPE_KEY {
                    page.escape()
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        self.add_controller(keys);
    }

    /// What Escape does in the list (see [`text::escape_action`]).
    fn escape(&self) -> glib::Propagation {
        // Read first: the actions below borrow the selection again, and a
        // `Ref` kept in the `match` scrutinee would still be alive in them.
        let selecting = self.imp().selection.borrow().active();
        let filtered = text::is_filtered(&self.query());
        match text::escape_action(selecting, filtered) {
            EscapeAction::LeaveSelection => {
                self.choose_mode(false);
                glib::Propagation::Stop
            }
            EscapeAction::ClearSearch => {
                self.clear_filters();
                glib::Propagation::Stop
            }
            EscapeAction::Nothing => glib::Propagation::Proceed,
        }
    }

    /// Escape in an empty search field leaves it: for the list, or the
    /// button of the empty state.
    fn leave_search(&self) {
        let imp = self.imp();
        match imp.content_stack.visible_child_name().as_deref() {
            Some("list") => {
                imp.card_list.grab_focus();
            }
            Some("empty") => {
                imp.empty_button.grab_focus();
            }
            _ => {}
        }
    }

    /// A row per visible card, following the page's `compact` and
    /// `selecting` properties.
    fn row_factory(&self) -> gtk::SignalListItemFactory {
        card_row::factory(
            self,
            CardRow::for_browser,
            &["compact", "selecting"],
            CardRow::bind_browse,
        )
    }

    // --- reading ---------------------------------------------------------

    /// Fills the deck and tag drop-downs, keeping what was chosen (or
    /// choosing `deck_id`). The tag filter lists the tags in use, and the
    /// chosen one even if no card has it any more (`text::filter_tags`,
    /// ADR 0052).
    fn load_filters(&self, deck_id: Option<&str>) -> rappel_core::Result<()> {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return Ok(());
        };
        let decks = state.decks()?;
        let kept_deck = text::filter_choice(imp.deck_filter.selected(), imp.decks.borrow().len())
            .map(|i| imp.decks.borrow()[i].id.clone());
        let kept = text::filter_choice(imp.tag_filter.selected(), imp.tags.borrow().len())
            .map(|i| imp.tags.borrow()[i].clone());
        let kept_tag = kept.as_ref().map(|t| t.id.clone());
        let tags = text::filter_tags(state.used_tags()?, kept);
        let want_deck = deck_id.map(str::to_owned).or(kept_deck);

        imp.quiet.set(true);
        dropdown::set_labels(
            &imp.deck_filter,
            &labels(&text::all_decks(), decks.iter().map(|d| &d.name)),
        );
        dropdown::set_labels(
            &imp.tag_filter,
            &labels(&text::all_tags(), tags.iter().map(|t| &t.name)),
        );
        let position =
            |found: Option<usize>| found.map_or(0, |i| u32::try_from(i + 1).unwrap_or(0));
        imp.deck_filter.set_selected(position(
            want_deck
                .as_ref()
                .and_then(|id| decks.iter().position(|d| &d.id == id)),
        ));
        imp.tag_filter.set_selected(position(
            kept_tag
                .as_ref()
                .and_then(|id| tags.iter().position(|t| &t.id == id)),
        ));
        imp.quiet.set(false);
        imp.decks.replace(decks);
        imp.tags.replace(tags);
        Ok(())
    }

    /// What the search field and the filters ask for.
    fn query(&self) -> BrowseQuery {
        let imp = self.imp();
        BrowseQuery {
            text: imp.search_entry.text().to_string(),
            deck_id: text::filter_choice(imp.deck_filter.selected(), imp.decks.borrow().len())
                .map(|i| imp.decks.borrow()[i].id.clone()),
            tag_id: text::filter_choice(imp.tag_filter.selected(), imp.tags.borrow().len())
                .map(|i| imp.tags.borrow()[i].id.clone()),
            state: text::state_filter(imp.state_filter.selected()),
        }
    }

    /// The user changed the words or a filter: read the rows again and tell
    /// a screen reader how many there are.
    fn search(&self) {
        self.requery();
        let imp = self.imp();
        let n = imp.items.get().map_or(0, gio::ListStore::n_items);
        // From the page: the count label is hidden on a narrow window.
        self.announce(
            &text::count_text(n as usize),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    }

    /// Reads the rows again and puts them in the list. The cards chosen
    /// stay chosen if they are still listed.
    pub fn requery(&self) {
        if let Err(err) = self.load_rows() {
            // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
            self.error_toast(&with_error(
                &gettext("The cards could not be read: {error}"),
                &err,
            ));
        }
    }

    /// [`Self::requery`] that returns the error: a page that cannot read
    /// its cards must not open and say "No Cards Yet".
    fn load_rows(&self) -> rappel_core::Result<()> {
        let imp = self.imp();
        let (Some(state), Some(store)) = (imp.state.get(), imp.items.get()) else {
            return Ok(());
        };
        let query = self.query();
        let rows = state.browse(&query, rappel_core::time::now_ms())?;
        let items: Vec<BrowseItem> = {
            let mut selection = imp.selection.borrow_mut();
            selection.retain(rows.iter().map(|r| r.card.id.as_str()));
            rows.into_iter()
                .map(|row| {
                    let item = BrowseItem::new(row);
                    item.set_selected(selection.contains(&item.id()));
                    item
                })
                .collect()
        };
        store.splice(0, store.n_items(), &items);
        let filtered = text::is_filtered(&query);
        // Whether there is any card at all, whatever the filters: read only
        // when a filtered search found nothing (unfiltered, no rows means
        // no cards).
        let collection_empty = if items.is_empty() && filtered {
            state
                .list_decks(rappel_core::time::now_ms())?
                .iter()
                .all(|deck| deck.card_count == 0)
        } else {
            items.is_empty()
        };
        let empty = text::empty_state(items.len(), filtered, collection_empty);
        imp.empty.set(empty);
        imp.filter_bar.set_visible(text::filters_shown(empty));
        // Nothing to choose from.
        self.action_set_enabled("browser.choose", !items.is_empty());
        match text::empty_page(empty) {
            None => imp.content_stack.set_visible_child_name("list"),
            Some((icon, title, description)) => {
                imp.empty_page.set_icon_name(Some(icon));
                imp.empty_page.set_title(&title);
                imp.empty_page.set_description(Some(&description));
                imp.empty_button.set_label(&if empty == Empty::NoCards {
                    text::go_to_decks()
                } else {
                    let (searched, filtered) = text::searched_and_filtered(&query);
                    text::clear_label(searched, filtered)
                });
                imp.content_stack.set_visible_child_name("empty");
            }
        }
        imp.count_label.set_label(&text::count_text(items.len()));
        self.update_selection_ui();
        Ok(())
    }

    fn clear_filters(&self) {
        let imp = self.imp();
        imp.quiet.set(true);
        imp.deck_filter.set_selected(0);
        imp.tag_filter.set_selected(0);
        imp.state_filter.set_selected(0);
        imp.quiet.set(false);
        // Clearing the text searches again; if it was empty nothing fires.
        imp.search_entry.set_text("");
        self.search();
        self.focus_search();
    }

    fn item(&self, position: u32) -> Option<BrowseItem> {
        self.imp().items.get()?.item(position).and_downcast()
    }

    /// Where card `id` is in the list. A linear scan, only on a user action.
    fn position(&self, id: &str) -> Option<u32> {
        let store = self.imp().items.get()?;
        (0..store.n_items()).find(|&p| self.item(p).is_some_and(|i| i.id() == id))
    }

    fn ids(&self) -> Vec<String> {
        let n = self.imp().items.get().map_or(0, gio::ListStore::n_items);
        (0..n)
            .filter_map(|p| self.item(p))
            .map(|i| i.id())
            .collect()
    }

    /// Whether a row itself has the keyboard focus (not a button in it).
    fn row_has_focus(&self) -> bool {
        let list = &self.imp().card_list;
        let focused = list.root().and_then(|root| root.focus());
        focused.is_some() && focused == list.focus_child()
    }

    /// The card whose row has the keyboard focus.
    fn focused_card(&self) -> Option<String> {
        let row = self
            .imp()
            .card_list
            .focus_child()?
            .first_child()
            .and_downcast::<CardRow>()?;
        let id = row.card_id();
        (!id.is_empty()).then_some(id)
    }

    // --- choosing --------------------------------------------------------

    /// Enters or leaves selection mode.
    fn choose_mode(&self, on: bool) {
        let imp = self.imp();
        if on {
            imp.selection.borrow_mut().enter();
        } else {
            imp.selection.borrow_mut().leave();
            if let Some(store) = imp.items.get() {
                for p in 0..store.n_items() {
                    if let Some(item) = self.item(p) {
                        item.set_selected(false);
                    }
                }
            }
        }
        self.update_selection_ui();
    }

    fn select_all(&self) {
        let imp = self.imp();
        let ids = self.ids();
        if ids.is_empty() {
            return;
        }
        imp.selection
            .borrow_mut()
            .select_all(ids.iter().map(String::as_str));
        if let Some(store) = imp.items.get() {
            for p in 0..store.n_items() {
                if let Some(item) = self.item(p) {
                    item.set_selected(true);
                }
            }
        }
        self.update_selection_ui();
    }

    fn toggle(&self, id: &str) {
        let Some(item) = self.position(id).and_then(|p| self.item(p)) else {
            return;
        };
        let chosen = self.imp().selection.borrow_mut().toggle(id);
        item.set_selected(chosen);
        self.update_selection_ui();
    }

    /// The Select button: start choosing. The button hides, so the
    /// keyboard goes to the list (or stays where it was if it is empty).
    fn start_choosing(&self) {
        self.choose_mode(true);
        let imp = self.imp();
        if imp.content_stack.visible_child_name().as_deref() == Some("list") {
            imp.card_list.grab_focus();
        }
    }

    /// Cancel: stop choosing (the keyboard goes to the Select button that
    /// takes its place, `update_selection_ui`).
    fn stop_choosing(&self) {
        self.choose_mode(false);
    }

    /// The status page's button: on "No Cards Yet" back to the deck list
    /// (where decks are made and cards added), on "No Results Found" clear
    /// the search and filters.
    fn empty_action(&self) {
        match self.imp().empty.get() {
            Empty::NoCards => nav::pop(self),
            Empty::NoResults => self.clear_filters(),
            Empty::No => {}
        }
    }

    /// A long press on a row: start choosing with this card.
    fn press(&self, id: &str) {
        if !self.imp().selection.borrow().active() {
            self.choose_mode(true);
            self.toggle(id);
        }
    }

    /// A row was activated: choose it, or open its editor.
    fn activate(&self, position: u32) {
        let Some(item) = self.item(position) else {
            return;
        };
        if self.imp().selection.borrow().active() {
            self.toggle(&item.id());
        } else {
            self.open_editor(&item.row().card);
        }
    }

    fn chosen_ids(&self) -> Vec<String> {
        let ids = self.ids();
        self.imp()
            .selection
            .borrow()
            .ids_in(ids.iter().map(String::as_str))
    }

    /// Brings the mode toggle, the action bar and the rows in line with the
    /// selection.
    fn update_selection_ui(&self) {
        let imp = self.imp();
        let (active, n) = {
            let selection = imp.selection.borrow();
            (selection.active(), selection.len())
        };
        if self.selecting() != active {
            // A header button about to hide must not keep the keyboard: it
            // goes to the Select button that takes its place.
            let header_focus = imp.select_all_button.has_focus() || imp.cancel_button.has_focus();
            self.set_selecting(active);
            // The header of selection mode (HIG): how many are chosen, Select
            // All and Cancel; Back hides and Escape does not go back
            // (`AdwNavigationPage:can-pop` docs), so Escape leaves the mode.
            imp.title_stack
                .set_visible_child_name(if active { "selection" } else { "search" });
            imp.select_button.set_visible(!active);
            imp.select_all_button.set_visible(active);
            imp.cancel_button.set_visible(active);
            self.set_can_pop(!active);
            imp.action_bar.set_revealed(active);
            if header_focus && !active {
                imp.select_button.grab_focus();
            }
        }
        imp.selection_title.set_title(&text::selection_title(n));
        for action in [
            "browser.move",
            "browser.add-tags",
            "browser.remove-tag",
            "browser.suspend-selected",
            "browser.delete-selected",
        ] {
            self.action_set_enabled(action, n > 0);
        }
        let chosen = self.chosen_items();
        self.action_set_enabled(
            "browser.reset-selected",
            text::reset_offered(chosen.iter().map(BrowseItem::is_new)),
        );
        // Tags is a menu button, which does not follow its menu's actions.
        imp.tags_button.set_sensitive(n > 0);
        let choice = text::suspend_choice(chosen.iter().map(BrowseItem::suspended));
        let (label, accessible) = text::suspend_labels(choice);
        imp.suspend_button.set_tooltip_text(Some(&label));
        imp.suspend_button
            .update_property(&[gtk::accessible::Property::Label(&accessible)]);
        imp.suspend_content.set_icon_name(match choice {
            SuspendChoice::Suspend => "media-playback-pause-symbolic",
            SuspendChoice::Unsuspend => "media-playback-start-symbolic",
        });
        imp.suspend_content
            .set_label(if self.compact() { "" } else { &label });
    }

    fn suspend_choice(&self) -> SuspendChoice {
        text::suspend_choice(self.chosen_items().iter().map(BrowseItem::suspended))
    }

    /// The listed items that are chosen, in list order.
    fn chosen_items(&self) -> Vec<BrowseItem> {
        let imp = self.imp();
        let Some(store) = imp.items.get() else {
            return Vec::new();
        };
        let selection = imp.selection.borrow();
        (0..store.n_items())
            .filter_map(|p| self.item(p))
            .filter(|i| selection.contains(&i.id()))
            .collect()
    }

    // --- one card --------------------------------------------------------

    fn edit(&self, id: &str) {
        if let Some(item) = self.position(id).and_then(|p| self.item(p)) {
            self.open_editor(&item.row().card);
        }
    }

    fn open_editor(&self, card: &Card) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        CardEditor::open(
            self,
            &imp.editing,
            state.clone(),
            &card.deck_id,
            Some(card),
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |saved| page.saved(&saved)
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

    /// A save from the editor: the saved row is read again in place (so the
    /// list keeps its place), a card added with Ctrl+Shift+Enter reads the
    /// list again.
    fn saved(&self, saved: &Saved) {
        let imp = self.imp();
        imp.dirty.set(true);
        match saved {
            Saved::Edited(tagged) => {
                let id = &tagged.card.id;
                imp.focus_after_edit.replace(Some(id.clone()));
                let (Some(state), Some(store)) = (imp.state.get(), imp.items.get()) else {
                    return;
                };
                let (Some(position), Ok(row)) = (
                    self.position(id),
                    state.browse_row(id, rappel_core::time::now_ms()),
                ) else {
                    self.requery();
                    return;
                };
                match row {
                    Some(row) => {
                        let item = BrowseItem::new(row);
                        item.set_selected(imp.selection.borrow().contains(id));
                        store.splice(position, 1, &[item]);
                    }
                    None => self.requery(),
                }
                // A new tag may be in the filter's list.
                if let Err(err) = self.load_filters(None) {
                    // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
                    self.error_toast(&with_error(
                        &gettext("The tags could not be read: {error}"),
                        &err,
                    ));
                }
            }
            Saved::Added(_) => {
                if let Err(err) = self.load_filters(None) {
                    // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
                    self.error_toast(&with_error(
                        &gettext("The tags could not be read: {error}"),
                        &err,
                    ));
                }
                self.requery();
            }
        }
    }

    fn card_info(&self, id: &str) {
        if let Some(state) = self.imp().state.get() {
            // AdwDialog gives the focus back to the widget that had it.
            card_info::open(self, state, id, || {});
        }
    }

    fn focus(&self, position: u32) {
        self.imp()
            .card_list
            .scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
    }

    // --- actions on cards ------------------------------------------------

    fn run_on(&self, id: &str, action: Bulk) {
        self.run(&[id.to_owned()], action);
    }

    fn run_selected(&self, action: Bulk) {
        self.run(&self.chosen_ids(), action);
    }

    fn suspend_selected(&self) {
        let suspend = self.suspend_choice() == SuspendChoice::Suspend;
        self.run_selected(Bulk::Suspend(suspend));
    }

    fn move_selected(&self) {
        let decks: Vec<Deck> = self.imp().decks.borrow().clone();
        let ids = self.chosen_ids();
        if ids.is_empty() {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let names: Vec<String> = decks.iter().map(|d| d.name.clone()).collect();
                let body = text::move_body(ids.len());
                let Some(i) = browser_dialogs::choose(
                    &page,
                    &gettext("Move Cards"),
                    &body,
                    &gettext("_Move"),
                    &names,
                )
                .await
                else {
                    return;
                };
                if let Some(deck) = decks.get(i) {
                    page.run(
                        &ids,
                        Bulk::Move {
                            deck_id: deck.id.clone(),
                        },
                    );
                }
            }
        ));
    }

    /// Asks for tags in the card editor's tag entry, with the tags in use
    /// as suggestions (ADR 0051, 0052), and puts them on the chosen cards.
    /// Returns the dialog (tests answer it).
    fn add_tags(&self) -> Option<adw::AlertDialog> {
        let ids = self.chosen_ids();
        if ids.is_empty() {
            return None;
        }
        let state = self.imp().state.get()?;
        let all = state.used_tags().unwrap_or_else(|err| {
            tracing::warn!("the tags could not be read for suggestions: {err}");
            Vec::new()
        });
        let body = text::add_tags_body(ids.len());
        browser_dialogs::add_tags(
            self,
            &body,
            all,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |names| page.run(&ids, Bulk::AddTags(names))
            ),
        )
    }

    fn remove_tag(&self) {
        let ids = self.chosen_ids();
        if ids.is_empty() {
            return;
        }
        // Only the tags the chosen cards have.
        let mut tags: Vec<Tag> = Vec::new();
        for item in (0..self.imp().items.get().map_or(0, gio::ListStore::n_items))
            .filter_map(|p| self.item(p))
            .filter(|i| ids.contains(&i.id()))
        {
            for tag in &item.row().tags {
                if !tags.iter().any(|t| t.id == tag.id) {
                    tags.push(tag.clone());
                }
            }
        }
        if tags.is_empty() {
            show_toast_from(self, info_toast(&text::no_tags_text()));
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let names: Vec<String> = tags.iter().map(|t| t.name.clone()).collect();
                let body = text::remove_tag_body(ids.len());
                let Some(i) = browser_dialogs::choose(
                    &page,
                    &gettext("Remove Tag"),
                    &body,
                    &gettext("_Remove"),
                    &names,
                )
                .await
                else {
                    return;
                };
                if let Some(tag) = tags.get(i) {
                    page.run(&ids, Bulk::RemoveTag(tag.clone()));
                }
            }
        ));
    }

    /// Does `action` on `ids` at once and offers Undo in a toast.
    fn run(&self, ids: &[String], action: Bulk) {
        let Some(state) = self.imp().state.get().cloned() else {
            return;
        };
        let first = ids.first().and_then(|id| self.position(id));
        let first_id = ids.first().cloned();
        match perform(&state, ids, action, rappel_core::time::now_ms()) {
            Ok(Ok(done)) => {
                self.choose_mode(false);
                self.changed();
                self.refocus(first, first_id.as_deref());
                self.offer_undo(state, done);
            }
            Ok(Err(notice)) => {
                // Nothing was written, but the rows may be stale (a card
                // reset elsewhere still offering Reset Progress).
                self.requery();
                show_toast_from(self, info_toast(&notice));
            }
            Err(rappel_core::Error::CardNotFound(_) | rappel_core::Error::DeckNotFound(_)) => {
                // Another window or an import removed a card or a deck: the
                // list shown is old. Nothing was written (the write is all or
                // nothing). `changed` reads the filters and the rows again.
                self.changed();
                show_toast_from(self, info_toast(&text::stale_text()));
            }
            // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
            Err(err) => self.error_toast(&with_error(
                &gettext("The cards could not be changed: {error}"),
                &err,
            )),
        }
    }

    /// After an action the focus goes to the card it started with, or, if
    /// that left the list, to the row now at its place, else the last one,
    /// else the empty state's button.
    ///
    /// Always set, because the action bar was hiding the focused button.
    fn refocus(&self, first: Option<u32>, first_id: Option<&str>) {
        let (Some(position), Some(id)) = (first, first_id) else {
            return;
        };
        if let Some(still) = self.position(id) {
            self.focus(still);
            return;
        }
        let n = self.imp().items.get().map_or(0, gio::ListStore::n_items);
        match index_after_removal(position, n) {
            Some(next) => self.focus(next),
            None => self.leave_search(),
        }
    }

    /// Something was written: read the rows again, and the other pages when
    /// this one goes.
    fn changed(&self) {
        self.imp().dirty.set(true);
        // The tags in use may have changed (a tag added, taken off, or on
        // deleted cards only), and the tag filter lists those.
        if let Err(err) = self.load_filters(None) {
            // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
            self.error_toast(&with_error(
                &gettext("The decks and tags could not be read: {error}"),
                &err,
            ));
        }
        self.requery();
    }

    /// The toast with Undo. It must work after leaving the page, so the
    /// closure keeps the state and weak references, not the page.
    fn offer_undo(&self, state: AppState, done: Done) {
        let page = self.downgrade();
        let window = self
            .root()
            .and_downcast::<RappelWindow>()
            .map(|w| w.downgrade());
        let title = done.text.clone();
        let note = done.note.clone();
        let toast = undo_toast(&title, move || {
            undo(&state, &done, &page, window.as_ref());
        });
        show_toast_from(self, toast);
        // Queued after the Undo toast, which has the high priority.
        if let Some(note) = note {
            show_toast_from(self, info_toast(&note));
        }
    }

    fn error_toast(&self, message: &str) {
        show_error(self, "a card operation", message);
    }
}

/// The Undo of a bulk action. What it could not restore is said in a note.
fn undo(
    state: &AppState,
    done: &Done,
    page: &glib::WeakRef<BrowserPage>,
    window: Option<&glib::WeakRef<RappelWindow>>,
) {
    let window = window.and_then(glib::WeakRef::upgrade);
    match (done.undo)(state) {
        Ok(restored) => {
            // On the page if it is still shown, else through the window
            // (the deck pages and the list read it).
            match page.upgrade().filter(|p| p.root().is_some()) {
                Some(page) => {
                    page.changed();
                    if let Some(note) = text::partial_undo_text(restored, done.total) {
                        show_toast_from(&page, info_toast(&note));
                    }
                }
                None => {
                    if let Some(window) = &window {
                        window.data_changed();
                        if let Some(note) = text::partial_undo_text(restored, done.total) {
                            show_toast_from(window, info_toast(&note));
                        }
                    }
                }
            }
        }
        Err(err) => {
            // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
            let message = with_error(&gettext("The change could not be undone: {error}"), &err);
            match window {
                Some(window) => show_error(&window, "a card operation", &message),
                None => crate::ui::toast::log_failure("a card operation", &message),
            }
        }
    }
}

/// "All …" first, then the names, each on one line.
fn labels<'a>(all: &str, names: impl Iterator<Item = &'a String>) -> Vec<String> {
    std::iter::once(all.to_owned())
        .chain(names.map(|n| one_line(n)))
        .collect()
}

/// Lets `button`'s own accessible label (Blueprint, or `suspend_labels`)
/// name it, not its `AdwButtonContent`'s text. That content makes its
/// label the button's mnemonic label when it is rooted
/// (`adw_button_content_root`, source read for libadwaita 1.9 and main),
/// which sets the button's labelled-by relation
/// (`gtk_widget_add_mnemonic_label`, source read for GTK 4.24), and
/// a labelled-by relation names a widget before its label property does
/// (`gtk_at_context_get_text_accumulate`, steps 2B and 2C of the ARIA
/// name computation, gtk/gtkatcontext.c, GTK 4.24). The button was then
/// named by its visible word ("Move…") and, on a phone, where the
/// breakpoint empties the words, not at all (observed over AT-SPI with
/// libadwaita 1.10.0 and GTK 4.24.1). Only the accessible relation is
/// reset: the mnemonic label stays, so a mnemonic such as Select All's
/// "_All" still activates the button.
fn name_by_own_label(button: &gtk::Button) {
    button.reset_relation(gtk::AccessibleRelation::LabelledBy);
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use rappel_core::domain::Grade;
    use rappel_core::storage::{CardStatus, StateFilter};

    use super::*;

    use crate::ui::time::DAY_MS;

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    /// A state with decks "Maths" (algebra cards) and "Art" (one card).
    /// Returns the ids of the Maths cards.
    fn state_with_cards() -> (tempfile::TempDir, AppState, Vec<String>, String) {
        let (tmp, state) = crate::testing::temporary_state();
        let maths = state.create_deck("Maths").unwrap();
        let art = state.create_deck("Art").unwrap();
        let ids = ["alpha equation", "beta matrix", "gamma vector"]
            .iter()
            .map(|front| {
                state
                    .add_card_with_tags(&maths.id, front, "back", &tags(&["algebra"]))
                    .unwrap()
                    .card
                    .id
            })
            .collect();
        state
            .add_card_with_tags(&art.id, "alpha painting", "back", &[])
            .unwrap();
        (tmp, state, ids, maths.id)
    }

    fn page_over(state: &AppState, deck: Option<&str>) -> BrowserPage {
        BrowserPage::new(state.clone(), deck).unwrap()
    }

    fn shown(page: &BrowserPage) -> Vec<String> {
        page.ids()
    }

    #[test]
    fn every_action_is_taken_back_by_its_undo() {
        let (_tmp, state, ids, maths) = state_with_cards();
        let art = state
            .decks()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "Art")
            .unwrap();
        let now = rappel_core::time::now_ms();
        let query = BrowseQuery::default();
        let rows = |state: &AppState| state.browse(&query, now).unwrap();
        let before = rows(&state);

        // Suspend and unsuspend.
        let done = perform(&state, &ids, Bulk::Suspend(true), now)
            .unwrap()
            .unwrap();
        assert_eq!((done.text.as_str(), done.total), ("3 cards suspended", 3));
        assert_eq!(rows(&state).iter().filter(|r| r.card.suspended).count(), 3);
        assert_eq!((done.undo)(&state).unwrap(), 3);
        assert_eq!(rows(&state), before);
        // Already as asked: nothing to tell.
        assert_eq!(
            perform(&state, &ids, Bulk::Suspend(false), now)
                .unwrap()
                .err()
                .as_deref(),
            Some("Nothing changed; the cards were already like that")
        );

        // Move.
        let bulk = Bulk::Move {
            deck_id: art.id.clone(),
        };
        let done = perform(&state, &ids, bulk, now).unwrap().unwrap();
        assert_eq!(done.text, "3 cards moved");
        assert_eq!(state.card(&ids[0]).unwrap().deck_id, art.id);
        assert_eq!((done.undo)(&state).unwrap(), 3);
        assert_eq!(state.card(&ids[0]).unwrap().deck_id, maths);

        // Add a tag, take a tag off.
        let done = perform(&state, &ids, Bulk::AddTags(tags(&["hard"])), now)
            .unwrap()
            .unwrap();
        assert_eq!(done.text, "3 cards tagged");
        assert_eq!(state.card_tags(&ids[1]).unwrap().len(), 2);
        assert_eq!((done.undo)(&state).unwrap(), 3);
        assert_eq!(rows(&state), before);
        // Several at once; one the cards have already is not named, and one
        // Undo takes off the others.
        let bulk = Bulk::AddTags(tags(&["Algebra", "hard", "exam"]));
        let done = perform(&state, &ids, bulk, now).unwrap().unwrap();
        assert_eq!(done.text, "3 cards tagged");
        assert_eq!(state.card_tags(&ids[1]).unwrap().len(), 3);
        assert_eq!((done.undo)(&state).unwrap(), 3);
        assert_eq!(rows(&state), before);
        let algebra = state
            .tags()
            .unwrap()
            .into_iter()
            .find(|t| t.name == "algebra")
            .unwrap();
        let done = perform(&state, &ids, Bulk::RemoveTag(algebra), now)
            .unwrap()
            .unwrap();
        assert_eq!(done.text, "3 cards untagged");
        assert_eq!(state.card_tags(&ids[1]).unwrap().len(), 0);
        assert_eq!((done.undo)(&state).unwrap(), 3);
        assert_eq!(rows(&state), before);

        // Delete.
        let done = perform(&state, &ids, Bulk::Delete, now).unwrap().unwrap();
        assert_eq!(done.text, "3 cards deleted");
        assert_eq!(rows(&state).len(), 1);
        assert_eq!((done.undo)(&state).unwrap(), 3);
        assert_eq!(rows(&state), before);
    }

    #[test]
    fn a_reset_is_undone_unless_the_card_was_studied_since() {
        let (_tmp, state, ids, _) = state_with_cards();
        let now = rappel_core::time::now_ms();
        for id in &ids[..2] {
            for at in [now - 60 * DAY_MS, now - 60 * DAY_MS + 11 * 60 * 1000] {
                state
                    .record_review(id, Grade::Pass, Duration::ZERO, at)
                    .unwrap();
            }
        }
        let done = perform(&state, &ids[..2], Bulk::Reset, now)
            .unwrap()
            .unwrap();
        assert_eq!(done.text, "2 cards reset");
        state
            .record_review(&ids[0], Grade::Pass, Duration::ZERO, now)
            .unwrap();
        // One card was studied since: only the other comes back.
        assert_eq!((done.undo)(&state).unwrap(), 1);
        assert_eq!(
            text::partial_undo_text(1, done.total).as_deref(),
            Some("1 of 2 cards restored; the others changed since")
        );
    }

    #[test]
    fn add_tags_leaves_out_cards_at_the_limit_of_the_tags_field() {
        let (_tmp, state, ids, _) = state_with_cards();
        // The first card has the field's 100 tags: "algebra" and 99 more.
        let many: Vec<String> = (1..MAX_TAGS_PER_CARD).map(|i| format!("t{i}")).collect();
        state
            .add_tags_to_cards(&ids[..1], &many, MAX_TAGS_PER_CARD)
            .unwrap();
        assert_eq!(state.card_tags(&ids[0]).unwrap().len(), MAX_TAGS_PER_CARD);
        let now = rappel_core::time::now_ms();
        let result = perform(&state, &ids[..1], Bulk::AddTags(tags(&["x"])), now);
        assert_eq!(
            result.unwrap().err().as_deref(),
            Some("No tags added; a card can have at most 100 tags")
        );
        // A tag it has already is no tag past the limit.
        let result = perform(&state, &ids[..1], Bulk::AddTags(tags(&["t1"])), now);
        assert_eq!(
            result.unwrap().err().as_deref(),
            Some("Nothing changed; the cards were already like that")
        );
        let done = perform(&state, &ids, Bulk::AddTags(tags(&["x"])), now)
            .unwrap()
            .unwrap();
        assert_eq!(done.text, "2 cards tagged");
        assert_eq!(
            done.note.as_deref(),
            Some("1 card left out. A card can have at most 100 tags")
        );
        assert_eq!(state.card_tags(&ids[0]).unwrap().len(), MAX_TAGS_PER_CARD);
        assert_eq!((done.undo)(&state).unwrap(), 2);
    }

    #[test]
    fn a_stale_list_changes_nothing() {
        let (_tmp, state, ids, _) = state_with_cards();
        state.delete_card(&ids[2]).unwrap();
        let result = perform(&state, &ids, Bulk::Suspend(true), 0);
        assert!(matches!(result, Err(rappel_core::Error::CardNotFound(_))));
        assert!(!state.card(&ids[0]).unwrap().suspended);
    }

    #[test]
    fn typing_and_the_filters_narrow_the_list() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, ids, maths) = state_with_cards();
            let page = page_over(&state, None);
            let imp = page.imp();
            assert_eq!(shown(&page).len(), 4);
            assert_eq!(
                imp.content_stack.visible_child_name().as_deref(),
                Some("list")
            );

            // Case, accents and prefixes are ignored.
            imp.search_entry.set_text("ALPH");
            page.search();
            assert_eq!(shown(&page).len(), 2);
            // The deck filter: entry 0 is "All Decks", then the decks by name.
            let position = imp
                .decks
                .borrow()
                .iter()
                .position(|d| d.id == maths)
                .unwrap();
            imp.deck_filter
                .set_selected(u32::try_from(position + 1).unwrap());
            assert_eq!(shown(&page), [ids[0].clone()]);
            imp.search_entry.set_text("nothing like it");
            page.search();
            assert_eq!(
                imp.content_stack.visible_child_name().as_deref(),
                Some("empty")
            );
            assert_eq!(imp.empty_page.title(), "No Results Found");
            assert_eq!(
                imp.empty_button.label().as_deref(),
                Some("_Clear Search and Filters")
            );
            // Its button clears both.
            page.empty_action();
            assert_eq!(shown(&page).len(), 4);
            imp.search_entry.set_text("nothing like it");
            page.search();
            assert_eq!(imp.empty_button.label().as_deref(), Some("_Clear Search"));

            page.clear_filters();
            assert_eq!(shown(&page).len(), 4);
            assert_eq!(imp.search_entry.text(), "");
            assert_eq!(imp.deck_filter.selected(), 0);
        });
    }

    #[test]
    fn opened_from_a_deck_it_starts_filtered_to_it() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, _, maths) = state_with_cards();
            let page = page_over(&state, Some(&maths));
            assert_eq!(shown(&page).len(), 3);
            assert!(text::is_filtered(&page.query()));
        });
    }

    #[test]
    fn an_empty_collection_says_so() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let page = page_over(&state, None);
            let imp = page.imp();
            assert_eq!(
                imp.content_stack.visible_child_name().as_deref(),
                Some("empty")
            );
            assert_eq!(imp.empty_page.title(), "No Cards Yet");
            assert_eq!(imp.empty_button.label().as_deref(), Some("_Go to Decks"));
            assert!(!imp.filter_bar.is_visible(), "nothing to filter");
            // Nothing to choose: no selection mode, Ctrl+A does nothing.
            let _ = page.activate_action("browser.choose", None);
            assert!(!page.selecting(), "a disabled Select does nothing");
            page.select_all();
            assert!(!page.selecting());
        });
    }

    #[test]
    fn opened_from_an_empty_deck_it_says_no_cards_or_no_results() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let empty = state.create_deck("Empty").unwrap();
            // No card anywhere: "No Cards Yet" at once, even filtered.
            let page = page_over(&state, Some(&empty.id));
            assert_eq!(page.imp().empty_page.title(), "No Cards Yet");
            assert!(!page.imp().filter_bar.is_visible());
            // Cards elsewhere: the deck filter finds none of them.
            let other = state.create_deck("Other").unwrap();
            state
                .add_card_with_tags(&other.id, "front", "back", &[])
                .unwrap();
            let page = page_over(&state, Some(&empty.id));
            assert_eq!(page.imp().empty_page.title(), "No Results Found");
            assert!(page.imp().filter_bar.is_visible());
            let _ = page.activate_action("browser.choose", None);
            assert!(!page.selecting(), "a disabled Select does nothing");
        });
    }

    #[test]
    fn chosen_cards_are_suspended_and_selection_mode_ends() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, ids, _) = state_with_cards();
            let page = page_over(&state, None);
            let imp = page.imp();
            page.choose_mode(true);
            assert!(imp.action_bar.is_revealed());
            assert_eq!(imp.selection_title.title(), "Select Cards");
            assert!(imp.cancel_button.is_visible() && !imp.select_button.is_visible());
            assert!(
                !page.can_pop(),
                "Escape leaves selection mode, not the page"
            );
            page.toggle(&ids[0]);
            page.toggle(&ids[1]);
            assert_eq!(imp.selection_title.title(), "2 Selected");
            assert_eq!(page.suspend_choice(), SuspendChoice::Suspend);

            page.suspend_selected();
            assert!(!imp.selection.borrow().active());
            assert!(!imp.action_bar.is_revealed());
            assert!(page.can_pop() && imp.select_button.is_visible());
            let suspended = |state: &AppState| {
                state
                    .browse(
                        &BrowseQuery {
                            state: Some(StateFilter::Suspended),
                            ..BrowseQuery::default()
                        },
                        rappel_core::time::now_ms(),
                    )
                    .unwrap()
                    .len()
            };
            assert_eq!(suspended(&state), 2);

            // Choosing only suspended cards offers Unsuspend.
            page.choose_mode(true);
            page.toggle(&ids[0]);
            assert_eq!(page.suspend_choice(), SuspendChoice::Unsuspend);
            page.toggle(&ids[2]);
            assert_eq!(page.suspend_choice(), SuspendChoice::Suspend);
        });
    }

    #[test]
    fn reset_is_offered_only_for_cards_with_progress() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, ids, _) = state_with_cards();
            let now = rappel_core::time::now_ms();
            state
                .record_review(&ids[1], Grade::Pass, Duration::ZERO, now - DAY_MS)
                .unwrap();
            // Suspended, which its status shows instead of New.
            state.set_suspended(&ids[0..1], true).unwrap();
            let page = page_over(&state, None);
            // Without a root a button stays insensitive whatever its action
            // says (seen with GTK 4.24, see the review page's `can_undo`).
            let _window = gtk::Window::builder().child(&page).build();
            let imp = page.imp();
            page.choose_mode(true);
            page.toggle(&ids[0]);
            page.toggle(&ids[2]);
            assert!(!imp.reset_button.is_sensitive(), "only new cards chosen");
            page.toggle(&ids[1]);
            assert!(imp.reset_button.is_sensitive());
            let item = |id: &str| page.position(id).and_then(|p| page.item(p)).unwrap();
            assert!(item(&ids[0]).is_new() && item(&ids[2]).is_new());
            assert!(!item(&ids[1]).is_new());

            // Reset elsewhere: the stale row's reset writes nothing, says
            // so, and the row catches up.
            state.reset_card(&ids[1], now).unwrap();
            assert!(!item(&ids[1]).is_new());
            let log = state.review_events(&ids[1]).unwrap();
            assert_eq!(
                perform(&state, &ids[1..2], Bulk::Reset, now).unwrap().err(),
                Some(text::no_progress_text(1))
            );
            page.run(&ids[1..2], Bulk::Reset);
            assert_eq!(state.review_events(&ids[1]).unwrap(), log);
            assert!(item(&ids[1]).is_new());
        });
    }

    #[test]
    fn the_header_buttons_enter_and_leave_selection_mode() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, _, _) = state_with_cards();
            let page = page_over(&state, None);
            let imp = page.imp();
            let window = gtk::Window::new();
            window.set_child(Some(&page));
            window.present();
            imp.select_button.emit_clicked();
            assert!(page.selecting());
            assert_eq!(
                imp.title_stack.visible_child_name().as_deref(),
                Some("selection")
            );
            imp.select_all_button.emit_clicked();
            assert_eq!(page.chosen_ids().len(), 4);
            assert_eq!(imp.selection_title.title(), "4 Selected");
            imp.cancel_button.emit_clicked();
            assert!(!page.selecting());
            assert!(page.can_pop(), "Back and Escape go back again after Cancel");
            assert_eq!(page.chosen_ids().len(), 0);
            assert_eq!(
                imp.title_stack.visible_child_name().as_deref(),
                Some("search")
            );
            window.destroy();
        });
    }

    #[test]
    fn add_tags_puts_the_tags_of_its_entry_on_the_chosen_cards() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, ids, maths) = state_with_cards();
            // A tag on another card, to be suggested.
            state
                .add_card_with_tags(&maths, "other", "back", &tags(&["Économie"]))
                .unwrap();
            let page = page_over(&state, None);
            let window = adw::Window::new();
            window.set_content(Some(&page));
            window.present();
            page.choose_mode(true);
            page.toggle(&ids[0]);
            page.toggle(&ids[1]);

            let dialog = page.add_tags().unwrap();
            assert_eq!(
                dialog.body(),
                "Choose the tags to put on 2 cards. Press Enter or type a comma after each one."
            );
            let entry = dialog
                .extra_child()
                .and_downcast::<crate::widgets::TagEntry>()
                .unwrap();
            // The focus goes to the entry's text; Enter there is Add once a
            // tag is there.
            assert_eq!(dialog.focus(), Some(entry.text().upcast()));
            assert!(entry.text().activates_default());
            assert!(!dialog.is_response_enabled("confirm"), "no tag yet");

            // The editor's suggestions: "eco" finds "Économie".
            entry.type_text("eco");
            let found = entry.suggest_for_test();
            assert_eq!(found.len(), 1);
            entry.accept_suggestion(0);
            assert_eq!(entry.pill_names(), ["Économie"]);
            assert!(dialog.is_response_enabled("confirm"));
            // A name too long for a tag turns Add off; a typed name counts.
            entry.type_text(&"x".repeat(300));
            assert!(!dialog.is_response_enabled("confirm"));
            entry.type_text("exam");
            assert!(dialog.is_response_enabled("confirm"));

            dialog.emit_by_name::<()>("response", &[&"confirm"]);
            let names = |id: &str| -> Vec<String> {
                state
                    .card_tags(id)
                    .unwrap()
                    .into_iter()
                    .map(|t| t.name)
                    .collect()
            };
            assert_eq!(names(&ids[0]), ["algebra", "exam", "Économie"]);
            assert_eq!(names(&ids[1]), ["algebra", "exam", "Économie"]);
            assert_eq!(names(&ids[2]), ["algebra"], "not chosen");
            assert!(
                !page.selecting(),
                "selection mode ends, as for every action"
            );

            // Cancel adds nothing.
            page.choose_mode(true);
            page.toggle(&ids[2]);
            let dialog = page.add_tags().unwrap();
            let entry = dialog
                .extra_child()
                .and_downcast::<crate::widgets::TagEntry>()
                .unwrap();
            entry.type_text("later");
            dialog.emit_by_name::<()>("response", &[&"cancel"]);
            assert_eq!(names(&ids[2]), ["algebra"]);
            window.destroy();
        });
    }

    #[test]
    fn choose_all_then_leave_forgets_the_choice() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, _, _) = state_with_cards();
            let page = page_over(&state, None);
            page.select_all();
            assert_eq!(page.chosen_ids().len(), 4);
            // The list read again keeps the choice.
            page.requery();
            assert_eq!(page.chosen_ids().len(), 4);
            page.choose_mode(false);
            assert_eq!(page.chosen_ids().len(), 0);
            assert!(!page.selecting());
        });
    }

    /// The action buttons have no labelled-by relation once rooted, so
    /// their own accessible label names them (`name_by_own_label`), also on
    /// a phone, where the breakpoint empties their words. A button with an
    /// `AdwButtonContent` that is not reset has the relation, so the check
    /// can fail.
    #[test]
    fn the_action_buttons_are_named_by_their_own_labels() {
        use gtk::{AccessibleProperty, AccessibleRelation};
        crate::testing::with_widgets(|| {
            let control = gtk::Button::new();
            control.set_child(Some(&adw::ButtonContent::builder().label("Move…").build()));
            let control_window = gtk::Window::new();
            control_window.set_child(Some(&control));
            assert!(gtk::test_accessible_has_relation(
                &control,
                AccessibleRelation::LabelledBy
            ));
            control_window.destroy();

            let (_tmp, state, _, _) = state_with_cards();
            let page = page_over(&state, None);
            let imp = page.imp();
            let buttons = [
                &*imp.select_all_button,
                &*imp.move_button,
                &*imp.suspend_button,
                &*imp.delete_button,
                &*imp.reset_button,
            ];
            let check = |when: &str| {
                for button in buttons {
                    assert!(
                        !gtk::test_accessible_has_relation(button, AccessibleRelation::LabelledBy),
                        "{when}: {:?} is labelled by its words",
                        button.tooltip_text()
                    );
                    assert!(
                        gtk::test_accessible_has_property(button, AccessibleProperty::Label),
                        "{when}: {:?} has no label",
                        button.tooltip_text()
                    );
                }
            };
            let window = gtk::Window::new();
            window.set_default_size(360, 640);
            window.set_child(Some(&page));
            check("rooted");
            // Shown at 360 px, the breakpoint applies and empties the words.
            window.present();
            assert!(crate::testing::settle_until(
                || page.compact(),
                std::time::Duration::from_secs(5)
            ));
            check("narrow");
            window.destroy();
        });
    }

    #[test]
    fn a_suspended_card_reads_as_suspended() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, ids, _) = state_with_cards();
            state.set_suspended(&ids[..1], true).unwrap();
            let page = page_over(&state, None);
            let item = page.position(&ids[0]).and_then(|p| page.item(p)).unwrap();
            assert_eq!(item.row().status, CardStatus::Suspended);
        });
    }

    #[test]
    fn escape_leaves_selection_mode_then_clears_the_search_without_panicking() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, ids, _) = state_with_cards();
            let page = page_over(&state, None);
            let imp = page.imp();
            // Both arms borrow the selection again: a `Ref` held while they
            // run would panic inside GTK's key handler and abort.
            page.choose_mode(true);
            assert!(!page.can_pop());
            page.toggle(&ids[0]);
            assert_eq!(page.escape(), glib::Propagation::Stop);
            assert!(!imp.selection.borrow().active());
            assert!(page.can_pop(), "Back and Escape go back again after Escape");

            imp.search_entry.set_text("alpha");
            assert_eq!(page.escape(), glib::Propagation::Stop);
            assert_eq!(imp.search_entry.text(), "");
            assert_eq!(page.ids().len(), 4);

            // Nothing to leave or clear: not handled, so the page goes back.
            assert_eq!(page.escape(), glib::Propagation::Proceed);
        });
    }

    #[test]
    fn an_action_on_a_list_that_is_out_of_date_changes_nothing_and_reads_again() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, ids, _) = state_with_cards();
            let page = page_over(&state, None);
            assert_eq!(page.ids().len(), 4);
            // Another window deletes a card the list still shows.
            state.delete_card(&ids[2]).unwrap();
            page.run(&ids, Bulk::Suspend(true));
            assert_eq!(page.ids().len(), 3, "the list was read again");
            assert!(
                !state.card(&ids[0]).unwrap().suspended,
                "nothing was written"
            );
        });
    }

    /// Five thousand cards: the page loads, searches and filters within a
    /// bound, and the list only builds the rows that fit on screen.
    #[test]
    fn five_thousand_cards_stay_fast_and_virtual() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let deck = state.create_deck("Scratch").unwrap();
            for i in 0..5_000 {
                state
                    .add_card_with_tags(
                        &deck.id,
                        &format!("Card {i}"),
                        "back",
                        &tags(&[&format!("tag {}", i % 40)]),
                    )
                    .unwrap();
            }
            // CPU time on this thread, so a loaded machine does not fail
            // the bounds (ADR 0049); the row count below is the work.
            let (load, page) =
                crate::testing::cpu_time_of(|| BrowserPage::new(state.clone(), None).unwrap());
            let imp = page.imp();
            assert_eq!(page.ids().len(), 5_000);

            let (search, ()) = crate::testing::cpu_time_of(|| {
                imp.search_entry.set_text("card 49");
                page.search();
            });
            assert_eq!(page.ids().len(), 111, "Card 49, 490-499, 4900-4999");
            imp.search_entry.set_text("");
            page.search();

            // A huge paste: 1 MiB of words and search syntax. Only the start
            // of the query is searched (ADR 0050, row 15), and the app does
            // not cut the field; GTK keeps at most 65,535 bytes in it
            // (`GTK_ENTRY_BUFFER_MAX_SIZE`, gtkentrybuffer.c). Laid out as
            // the field would be when drawn.
            let pasted = "card \"NEAR\" * OR ( x-y ".repeat(1 << 20 >> 5);
            let (paste, ()) = crate::testing::cpu_time_of(|| {
                imp.search_entry.set_text(&pasted);
                let _ = imp.search_entry.measure(gtk::Orientation::Horizontal, -1);
                page.search();
            });
            let kept = imp.search_entry.text().len();
            assert!((60_000..=usize::from(u16::MAX)).contains(&kept), "{kept}");
            assert_eq!(page.ids().len(), 0, "no card holds every word");
            imp.search_entry.set_text("");
            page.search();

            let (choose, ()) = crate::testing::cpu_time_of(|| page.select_all());
            assert_eq!(page.chosen_ids().len(), 5_000);
            page.choose_mode(false);

            // On screen: only some rows exist.
            let window = gtk::Window::builder()
                .default_width(600)
                .default_height(700)
                .child(&page)
                .build();
            window.present();
            let context = glib::MainContext::default();
            for _ in 0..200 {
                if !context.iteration(false) {
                    break;
                }
            }
            let rows = count_rows(page.upcast_ref());
            window.destroy();

            crate::testing::print_timing("browser page: load 5,000", load);
            crate::testing::print_timing("browser page: search", search);
            crate::testing::print_timing("browser page: 1 MiB paste", paste);
            crate::testing::print_timing("browser page: choose all", choose);
            if std::env::var_os("RAPPEL_TIMINGS").is_some() {
                eprintln!("browser page: rows built: {rows}");
            }
            crate::testing::assert_cpu_within(load, Duration::from_secs(5), "load");
            crate::testing::assert_cpu_within(search, Duration::from_secs(2), "search");
            crate::testing::assert_cpu_within(paste, Duration::from_millis(500), "1 MiB paste");
            crate::testing::assert_cpu_within(choose, Duration::from_secs(2), "choose all");
            // The list view builds the rows in view and a few more to size itself
            // (about 200 here), never one per card.
            assert!(rows > 0 && rows < 500, "{rows} rows are built");
        });
    }

    /// Pasting, then typing at once after it, is one search, run once the
    /// field's `search-delay` (150 ms, `data/browser_page.blp`) has passed:
    /// `GtkSearchEntry` restarts that delay on each change of a non-empty
    /// text and emits `search-changed` when it runs out
    /// (`GtkSearchEntry:search-delay` docs). The page searches only on that
    /// signal and on a filter's change. The text goes in as typing and
    /// pasting put it, through the field's `GtkText`; `set_text` would
    /// first empty the field, a change `GtkSearchEntry` reports at once.
    #[test]
    fn a_paste_and_the_keys_after_it_are_one_search() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let deck = state.create_deck("Deck").unwrap();
            state
                .add_card_with_tags(&deck.id, "front", "back", &[])
                .unwrap();
            let page = BrowserPage::new(state, None).unwrap();
            let entry = page.imp().search_entry.clone();
            let field = entry
                .delegate()
                .and_downcast::<gtk::Text>()
                .expect("a search entry edits through a GtkText");
            let searches = Rc::new(Cell::new(0));
            let seen = Rc::clone(&searches);
            entry.connect_search_changed(move |_| seen.set(seen.get() + 1));
            let mut position = 0;
            field.insert_text(&"word ".repeat(10_000), &mut position);
            for key in ["x", "y", "z"] {
                field.insert_text(key, &mut position);
            }
            assert_eq!(searches.get(), 0, "nothing is searched before the delay");
            assert!(crate::testing::settle_until(
                || searches.get() > 0,
                Duration::from_secs(5)
            ));
            // Nothing else is waiting to run.
            let more =
                crate::testing::settle_until(|| searches.get() > 1, Duration::from_millis(400));
            assert!(!more, "{} searches", searches.get());
            assert_eq!(page.ids().len(), 0);
        });
    }

    fn count_rows(widget: &gtk::Widget) -> usize {
        let mut n = usize::from(widget.is::<CardRow>());
        let mut child = widget.first_child();
        while let Some(c) = child {
            n += count_rows(&c);
            child = c.next_sibling();
        }
        n
    }
}
