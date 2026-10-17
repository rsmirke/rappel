//! A study session of one deck: the front, then the back, then Fail or
//! Pass; a summary at the end. Layout in `data/review_page.blp` (ADR 0014).
//!
//! The order of cards is the core's [`Session`]. Each answer is recorded
//! at once with `record_review`, so leaving early loses nothing.
//!
//! Keys, in the bubble phase so a focused button keeps its own Space and
//! Enter: Space or Enter shows the answer; then 1 is Fail, 2 or Space is
//! Pass; E edits the card at any time; S suspends it and Delete deletes it
//! (after a question), both at once out of the session with an Undo toast
//! (ADR 0037); Ctrl+Z (and the header's Undo button)
//! takes back the last answer, repeatedly down to the session's first, also
//! from the summary (ADR 0030). The focus rests on the card between answers.
//!
//! Editing opens the deck page's `CardEditor` on the card on screen. A
//! save re-renders the card in place (the state of the reveal, the timers
//! and the session are untouched); closing without saving changes nothing.
//! The page ignores its keys while the editor is open (`editing`), whatever
//! the dialog does with key events.

use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use rappel_core::content::{self, Document};
use rappel_core::domain::{CardState, Deck, Grade};
use rappel_core::scheduling::{Session, Summary, capped};

use crate::dialogs::card_editor::{CardEditor, Saved};
use crate::dialogs::card_info;
use crate::render::{back_shown, clear, fill, shown_part};
use crate::state::AppState;
use crate::ui::callback::Callback;
use crate::ui::dialog;
use crate::ui::text::{duration_text, group_digits};
use crate::ui::tile::{Tile, set_tile};
use crate::ui::toast::{info_toast, log_failure, show_error, show_toast_from, undo_toast};
use crate::ui::undo::{self, UndoFailure};
use crate::ui::{init_once, nav};
use crate::window::RappelWindow;

type OnSaved = dyn Fn(Saved);

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};
    use std::rc::Rc;
    use std::time::Instant;

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;
    use rappel_core::content::Document;
    use rappel_core::domain::Deck;
    use rappel_core::scheduling::Session;

    use super::OnSaved;
    use crate::state::AppState;
    use crate::ui::callback::Callback;
    use crate::ui::toast::ErrorSlot;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/review_page.ui")]
    pub struct ReviewPage {
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub edit_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub card_menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub card_area: TemplateChild<gtk::Box>,
        #[template_child]
        pub front_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub back_part: TemplateChild<gtk::Box>,
        #[template_child]
        pub back_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub cut_note: TemplateChild<gtk::Label>,
        #[template_child]
        pub bottom_bar: TemplateChild<adw::Clamp>,
        #[template_child]
        pub reveal_hint: TemplateChild<gtk::Label>,
        #[template_child]
        pub fail_hint: TemplateChild<gtk::Label>,
        #[template_child]
        pub pass_hint: TemplateChild<gtk::Label>,
        #[template_child]
        pub buttons: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub short_breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub summary_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub summary_figures: TemplateChild<gtk::Box>,
        #[template_child]
        pub cards_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub cards_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub passed_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub passed_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub time_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub time_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub done_button: TemplateChild<gtk::Button>,
        pub state: OnceCell<AppState>,
        pub deck: OnceCell<Deck>,
        pub session: RefCell<Option<Session>>,
        /// The card on screen.
        pub current: RefCell<Option<String>>,
        /// Its back, parsed, until it is shown.
        pub back: RefCell<Document>,
        /// Whether only the start of the front, or of the back, is shown
        /// (`render::shown_part`).
        pub front_cut: Cell<bool>,
        pub back_cut: Cell<bool>,
        /// When the current card's front was shown.
        pub shown_at: Cell<Option<Instant>>,
        pub revealed: Cell<bool>,
        /// When the back was shown.
        pub revealed_at: Cell<Option<Instant>>,
        /// The last error toast, replaced rather than stacked when saving
        /// keeps failing.
        pub errors: ErrorSlot,
        /// Whether the card editor is open.
        pub editing: Rc<Cell<bool>>,
        /// Whether the question before a delete is open.
        pub asking: Cell<bool>,
        /// Told about every save made from this page's editor.
        pub on_saved: OnceCell<Callback<OnSaved>>,
        /// Fades the back in at the reveal (`super::reveal_step`).
        pub reveal_animation: OnceCell<adw::TimedAnimation>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ReviewPage {
        const NAME: &'static str = "RappelReviewPage";
        type Type = super::ReviewPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.install_action("review.reveal", None, |page, _, _| page.reveal());
            klass.install_action("review.fail", None, |page, _, _| {
                page.grade(rappel_core::domain::Grade::Fail);
            });
            klass.install_action("review.pass", None, |page, _, _| {
                page.grade(rappel_core::domain::Grade::Pass);
            });
            klass.install_action("review.done", None, |page, _, _| page.done());
            klass.install_action("review.edit", None, |page, _, _| page.edit());
            klass.install_action("review.card-info", None, |page, _, _| page.card_info());
            klass.install_action("review.suspend", None, |page, _, _| {
                page.remove_current(super::Removal::Suspend);
            });
            klass.install_action("review.delete", None, |page, _, _| page.confirm_delete());
            klass.install_action("review.undo", None, |page, _, _| page.undo());
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ReviewPage {
        fn constructed(&self) {
            self.parent_constructed();
            let page = self.obj();
            page.setup_keys();
            page.setup_reveal_animation();
            page.setup_short_summary();
            // A grab before the push does nothing, and the navigation view
            // then focuses the first widget: the Back button, which would
            // take Space and Enter. So park the focus once on screen.
            page.connect_shown(super::ReviewPage::park_focus);
        }
    }
    impl WidgetImpl for ReviewPage {}
    impl NavigationPageImpl for ReviewPage {}
}

glib::wrapper! {
    pub struct ReviewPage(ObjectSubclass<imp::ReviewPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// How long after the reveal the grade keys are ignored, against key
/// auto-repeat. Clicks are not delayed.
const KEY_SETTLE: Duration = Duration::from_millis(300);

/// How a card leaves the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removal {
    Suspend,
    Delete,
}

impl Removal {
    /// The toast that offers Undo.
    fn text(self) -> &'static str {
        match self {
            Self::Suspend => "Card suspended",
            Self::Delete => "Card deleted",
        }
    }
}

/// The Undo of a suspend or a delete: the card is studied again. On `page`
/// if it is still shown (the card becomes the next one), and through
/// `window` the other pages read it (the deck may have been left).
fn undo_removal(
    state: &AppState,
    id: &str,
    removal: Removal,
    page: &glib::WeakRef<ReviewPage>,
    window: Option<&glib::WeakRef<RappelWindow>>,
) {
    let window = window.and_then(glib::WeakRef::upgrade);
    let result = match removal {
        Removal::Suspend => state.restore_suspended(&[id.to_owned()], false).map(|_| ()),
        Removal::Delete => state.undelete_card(id).map(|_| ()),
    };
    match result {
        Ok(()) => {
            // Only a card that is live and not suspended is studied again
            // (it may have been changed again since).
            let studyable = state.card(id).is_ok_and(|card| !card.suspended);
            if studyable && let Some(page) = page.upgrade().filter(|p| p.root().is_some()) {
                page.put_back(id);
            }
            if let Some(window) = window {
                window.data_changed();
            }
        }
        Err(err) => {
            let message = format!("The card could not be restored: {err}");
            match window {
                Some(window) => show_error(&window, "a review operation", &message),
                None => log_failure("a review operation", &message),
            }
        }
    }
}

/// Which buttons and actions the bottom bar offers.
#[derive(Debug, Clone, Copy)]
enum Controls {
    /// Show Answer.
    Reveal,
    /// Fail and Pass.
    Grade,
    /// Nothing: the summary is shown.
    Hidden,
}

/// What a key does on this page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// Space: show the answer, or Pass once it is shown.
    Space,
    /// Enter: show the answer.
    Enter,
    Fail,
    Pass,
    /// E: edit the card.
    Edit,
    /// I: show the card's information.
    Info,
    /// S: suspend the card.
    Suspend,
    /// Delete: delete the card, after a question.
    Delete,
    /// Ctrl+Z: take back the last answer.
    Undo,
}

/// The page's keys (`setup_keys`). The Keyboard Shortcuts dialog and the
/// Card Menu's hints list them; tests check both.
const KEYS: [(&str, Key); 16] = [
    ("space", Key::Space),
    ("Return", Key::Enter),
    ("KP_Enter", Key::Enter),
    ("1", Key::Fail),
    ("KP_1", Key::Fail),
    ("2", Key::Pass),
    ("KP_2", Key::Pass),
    ("e", Key::Edit),
    ("<Shift>e", Key::Edit),
    ("i", Key::Info),
    ("<Shift>i", Key::Info),
    ("s", Key::Suspend),
    ("<Shift>s", Key::Suspend),
    ("Delete", Key::Delete),
    ("KP_Delete", Key::Delete),
    ("<Control>z", Key::Undo),
];

/// The triggers of [`KEYS`], for the Keyboard Shortcuts dialog's test.
#[cfg(test)]
pub(crate) fn key_triggers() -> impl Iterator<Item = &'static str> {
    KEYS.iter().map(|(trigger, _)| *trigger)
}

/// What a key makes the page do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Reveal,
    Grade(Grade),
    Edit,
    Info,
    Suspend,
    Delete,
    Undo,
}

/// What `key` does when the answer is (not) `revealed`. `settled` says
/// whether [`KEY_SETTLE`] has passed since the reveal. Holding Space a
/// moment too long repeats it: a reveal must not become a Pass by itself,
/// even though a grade can be undone. E and Ctrl+Z are never delayed: they
/// grade nothing; neither does I. S and Delete take the card out of the
/// session, so they are not delayed by `key_action` but guarded elsewhere:
/// Delete asks first (the question stops the repeat), and S needs the card to
/// have been on screen a moment ([`suspend_allowed`]), or holding it would
/// suspend one card after another.
fn key_action(key: Key, revealed: bool, settled: bool) -> Option<Act> {
    match (key, revealed) {
        (Key::Edit, _) => Some(Act::Edit),
        (Key::Info, _) => Some(Act::Info),
        (Key::Suspend, _) => Some(Act::Suspend),
        (Key::Delete, _) => Some(Act::Delete),
        (Key::Undo, _) => Some(Act::Undo),
        (Key::Space | Key::Enter, false) => Some(Act::Reveal),
        (Key::Space | Key::Pass | Key::Fail, true) if !settled => None,
        (Key::Space | Key::Pass, true) => Some(Act::Grade(Grade::Pass)),
        (Key::Fail, true) => Some(Act::Grade(Grade::Fail)),
        // 1 and 2 before the answer, Enter after it: nothing.
        (Key::Fail | Key::Pass, false) | (Key::Enter, true) => None,
    }
}

/// Whether S may suspend a card that has been on screen for `shown_for`.
/// After the first S the next card is on screen, and key repeat would
/// suspend it too: a card must be shown for [`KEY_SETTLE`] first. Clicking
/// the menu item is not delayed.
fn suspend_allowed(shown_for: Option<Duration>) -> bool {
    shown_for.is_some_and(|t| t >= KEY_SETTLE)
}

impl ReviewPage {
    /// A session over what `deck` has to study now, showing its first card.
    /// `on_saved` is called after each save made from the session's editor.
    ///
    /// # Errors
    /// If what to study cannot be read.
    pub fn new(
        state: AppState,
        deck: Deck,
        on_saved: impl Fn(Saved) + 'static,
    ) -> rappel_core::Result<Self> {
        let now = rappel_core::time::now_ms();
        let input = state.study_input(&deck.id, now)?;
        let page: Self = glib::Object::new();
        let imp = page.imp();
        // Deck names are untrusted; AdwWindowTitle shows plain text.
        imp.window_title.set_title(&deck.name);
        imp.session.replace(Some(Session::new(input)));
        init_once(&imp.state, state);
        init_once(&imp.deck, deck);
        init_once(&imp.on_saved, Callback::new(Rc::new(on_saved)));
        page.show_next();
        Ok(page)
    }

    fn setup_keys(&self) {
        let shortcuts = gtk::ShortcutController::new();
        // Bubble: a focused button handles its own Space and Enter first.
        shortcuts.set_propagation_phase(gtk::PropagationPhase::Bubble);
        for (trigger, key) in KEYS {
            let action = gtk::CallbackAction::new(move |widget, _| {
                widget
                    .downcast_ref::<Self>()
                    .map_or(glib::Propagation::Proceed, |page| page.key(key))
            });
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(action),
            ));
        }
        self.add_controller(shortcuts);
    }

    /// Puts the keyboard focus on the card, or on Back to Deck at the end.
    fn park_focus(&self) {
        let imp = self.imp();
        if imp.current.borrow().is_some() {
            imp.card_area.grab_focus();
        } else {
            imp.done_button.grab_focus();
        }
    }

    fn key(&self, key: Key) -> glib::Propagation {
        let imp = self.imp();
        if imp.editing.get() || imp.asking.get() {
            return glib::Propagation::Proceed;
        }
        // Also on the summary, where no card is on screen.
        if key == Key::Undo {
            self.undo();
            return glib::Propagation::Stop;
        }
        if imp.current.borrow().is_none() {
            return glib::Propagation::Proceed;
        }
        let settled = imp
            .revealed_at
            .get()
            .is_some_and(|t| t.elapsed() >= KEY_SETTLE);
        match key_action(key, imp.revealed.get(), settled) {
            Some(Act::Reveal) => self.reveal(),
            Some(Act::Grade(grade)) => self.grade(grade),
            Some(Act::Edit) => self.edit(),
            Some(Act::Info) => self.card_info(),
            Some(Act::Suspend) => {
                if suspend_allowed(imp.shown_at.get().map(|t| t.elapsed())) {
                    self.remove_current(Removal::Suspend);
                }
            }
            Some(Act::Delete) => self.confirm_delete(),
            Some(Act::Undo) => self.undo(),
            None => {}
        }
        glib::Propagation::Stop
    }

    /// Shows the next card, or the summary when the session is over.
    fn show_next(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        // A loop and not a recursion: a database that stays unreadable skips
        // every card of the queue, which may be thousands.
        let (id, card) = loop {
            let now = rappel_core::time::now_ms();
            let next = imp
                .session
                .borrow()
                .as_ref()
                .and_then(|s| s.next(now).map(str::to_owned));
            let Some(id) = next else {
                self.show_summary();
                return;
            };
            let card = match state.card(&id) {
                Ok(card) => card,
                Err(err) => {
                    // Skip it and go on; it is not counted as answered.
                    self.error_toast(&format!("A card could not be read and was skipped: {err}"));
                    self.skip_in_session(&id);
                    continue;
                }
            };
            // Suspended since the session began (another window, or an Undo
            // of an answer that brought it back): not studied, not counted.
            if card.suspended {
                self.skip_in_session(&id);
                continue;
            }
            break (id, card);
        };
        imp.revealed.set(false);
        // A reveal still fading in ends now, out of sight.
        if let Some(animation) = imp.reveal_animation.get() {
            animation.skip();
        }
        self.show_front(&card.front_md);
        clear(&imp.back_box);
        imp.back_part.set_visible(false);
        imp.card_area
            .reset_property(gtk::AccessibleProperty::Description);
        self.keep_back(&card.back_md);
        imp.revealed_at.set(None);
        imp.current.replace(Some(id));
        imp.shown_at.set(Some(Instant::now()));
        let left = imp.session.borrow().as_ref().map_or(0, Session::remaining);
        imp.window_title.set_subtitle(&left_text(left));
        self.set_controls(Controls::Reveal);
        imp.stack.set_visible_child_name("card");
        imp.card_area.grab_focus();
    }

    /// Makes the animation that brings the answer in: a short fade with a
    /// small slide up ([`reveal_step`]). libadwaita skips it to its end when
    /// animations are off (the system's reduced-motion setting turns off
    /// `gtk-enable-animations`) or the widget is not mapped (`AdwAnimation`
    /// docs), so the back is then at once fully shown.
    fn setup_reveal_animation(&self) {
        let imp = self.imp();
        let back = imp.back_part.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |progress| {
            if let Some(back) = back.upgrade() {
                let (opacity, margin) = reveal_step(progress);
                back.set_opacity(opacity);
                back.set_margin_top(margin);
            }
        });
        // On the card area, which is mapped whenever a card is on screen
        // (the back itself is mapped only once it is made visible).
        let animation = adw::TimedAnimation::new(&*imp.card_area, 0.0, 1.0, REVEAL_MS, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        init_once(&imp.reveal_animation, animation);
    }

    /// Makes the summary `compact` (libadwaita's style class for a status
    /// page in a small space: less spacing) and drops its icon while the
    /// short-window breakpoint applies, so its figures and Back to Deck fit
    /// at 360 × 294. `apply` and `unapply` come when the breakpoint starts
    /// and stops matching (`AdwBreakpoint` docs).
    fn setup_short_summary(&self) {
        let imp = self.imp();
        let summary = imp.summary_page.downgrade();
        imp.short_breakpoint.connect_apply(move |_| {
            if let Some(summary) = summary.upgrade() {
                summary.add_css_class("compact");
                summary.set_icon_name(None);
            }
        });
        let summary = imp.summary_page.downgrade();
        imp.short_breakpoint.connect_unapply(move |_| {
            if let Some(summary) = summary.upgrade() {
                summary.remove_css_class("compact");
                summary.set_icon_name(Some(SUMMARY_ICON));
            }
        });
    }

    /// Fades the back in, from its first step: `play` restarts the
    /// animation, but the first frame is drawn before its first tick, so
    /// the back starts out hidden here.
    fn animate_reveal(&self) {
        let imp = self.imp();
        let (opacity, margin) = reveal_step(0.0);
        imp.back_part.set_opacity(opacity);
        imp.back_part.set_margin_top(margin);
        if let Some(animation) = imp.reveal_animation.get() {
            animation.play();
        }
    }

    fn skip_in_session(&self, id: &str) {
        if let Some(session) = self.imp().session.borrow_mut().as_mut() {
            session.skip(id);
        }
    }

    fn reveal(&self) {
        let imp = self.imp();
        if imp.current.borrow().is_none() || imp.revealed.replace(true) {
            return;
        }
        imp.revealed_at.set(Some(Instant::now()));
        let back = imp.back.take();
        // Only the reveal animates; a redraw after an edit does not.
        if self.show_back(&back) {
            self.animate_reveal();
        }
        self.set_controls(Controls::Grade);
        imp.card_area.grab_focus();
    }

    /// Shows the front `front_md`, or its start if it is too long to lay
    /// out (`render::shown_part`). Screen readers read it when the card
    /// area has the focus.
    fn show_front(&self, front_md: &str) {
        let imp = self.imp();
        let (part, cut) = shown_part(front_md);
        let front = content::parse(part);
        fill(&imp.front_box, &front);
        imp.card_area
            .update_property(&[gtk::accessible::Property::Label(&with_cut_note(
                content::plain_text(&front),
                cut,
            ))]);
        imp.front_cut.set(cut);
        self.update_cut_note();
    }

    /// Parses the back `back_md` (or its start, as for the front) for
    /// [`Self::reveal`].
    fn keep_back(&self, back_md: &str) {
        let imp = self.imp();
        let (part, cut) = shown_part(back_md);
        imp.back.replace(content::parse(part));
        imp.back_cut.set(cut);
    }

    /// Shows the note under the card while a side on screen is cut.
    fn update_cut_note(&self) {
        let imp = self.imp();
        imp.cut_note.set_visible(cut_note_shown(
            imp.front_cut.get(),
            imp.back_cut.get(),
            imp.revealed.get(),
        ));
    }

    /// Shows `back` under the front, or nothing when it is empty: then
    /// only the grade buttons appear. Says whether it is shown.
    fn show_back(&self, back: &Document) -> bool {
        let imp = self.imp();
        let shown = back_shown(back);
        if shown {
            fill(&imp.back_box, back);
            imp.card_area
                .update_property(&[gtk::accessible::Property::Description(&with_cut_note(
                    content::plain_text(back),
                    imp.back_cut.get(),
                ))]);
        } else {
            clear(&imp.back_box);
            imp.card_area
                .reset_property(gtk::AccessibleProperty::Description);
            // A cut back whose start has nothing to show (only blank
            // lines) is not on screen, so the note is not about it.
            imp.back_cut.set(false);
        }
        imp.back_part.set_visible(shown);
        self.update_cut_note();
        shown
    }

    /// Opens the card editor on the card on screen.
    fn edit(&self) {
        let imp = self.imp();
        let (Some(state), Some(deck), Some(id)) = (
            imp.state.get(),
            imp.deck.get(),
            imp.current.borrow().clone(),
        ) else {
            return;
        };
        if imp.editing.get() {
            return;
        }
        // Read again, so the editor starts from what is stored.
        let card = match state.card(&id) {
            Ok(card) => card,
            Err(err) => {
                self.error_toast(&format!("The card could not be read: {err}"));
                return;
            }
        };
        CardEditor::open(
            self,
            &imp.editing,
            state.clone(),
            &deck.id,
            Some(&card),
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |saved| page.saved(saved)
            ),
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                // Set here, not left to whatever focus the dialog restores:
                // the card area must have it for the keys to work.
                move || page.park_focus()
            ),
        );
    }

    /// Shows the information of the card on screen. The session carries on
    /// as it was: nothing is graded, and the timer keeps running (the time
    /// of an answer is capped, ADR 0014).
    fn card_info(&self) {
        let imp = self.imp();
        let (Some(state), Some(id)) = (imp.state.get(), imp.current.borrow().clone()) else {
            return;
        };
        if imp.editing.get() {
            return;
        }
        card_info::open(
            self,
            state,
            &id,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                // The card area needs the focus for the keys to work.
                move || page.park_focus()
            ),
        );
    }

    /// Takes the card on screen out of the session by suspending or deleting
    /// it, shows the next card, and offers Undo in a toast (ADR 0037). The
    /// card is not counted as answered.
    fn remove_current(&self, removal: Removal) {
        let imp = self.imp();
        if imp.editing.get() || imp.asking.get() {
            return;
        }
        let Some(id) = imp.current.borrow().clone() else {
            return;
        };
        self.remove(&id, removal);
    }

    fn remove(&self, id: &str, removal: Removal) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        let written = match removal {
            Removal::Suspend => state
                .set_suspended(&[id.to_owned()], true)
                .map(|changed| !changed.is_empty()),
            Removal::Delete => state.delete_card(id).map(|()| true),
        };
        match written {
            Ok(true) => {}
            Ok(false) => {
                // Suspended elsewhere already: there is nothing to undo here.
                self.leave_session(id);
                show_toast_from(self, info_toast("The card was already suspended"));
                return;
            }
            Err(rappel_core::Error::CardNotFound(_)) => {
                // Deleted elsewhere meanwhile: it is gone from the session too.
                self.leave_session(id);
                show_toast_from(self, info_toast("The card was already gone"));
                return;
            }
            Err(err) => {
                self.error_toast(&format!("The card could not be changed: {err}"));
                return;
            }
        }
        self.leave_session(id);

        // Undo must work after leaving the page, so the closure keeps the
        // state and weak references, not the page.
        let window = self.root().and_downcast::<RappelWindow>();
        if let Some(window) = &window {
            window.data_changed();
        }
        let (id, state) = (id.to_owned(), state.clone());
        let page = self.downgrade();
        let window = window.map(|w| w.downgrade());
        let toast = undo_toast(removal.text(), move || {
            undo_removal(&state, &id, removal, &page, window.as_ref());
        });
        show_toast_from(self, toast);
    }

    /// Drops card `id` from the session and, if it is on screen, shows the
    /// next one.
    fn leave_session(&self, id: &str) {
        let imp = self.imp();
        if let Some(session) = imp.session.borrow_mut().as_mut() {
            session.skip(id);
        }
        if imp.current.borrow().as_deref() == Some(id) {
            imp.current.replace(None);
            self.show_next();
        }
    }

    /// Asks before deleting the card on screen. Cancel is the default.
    fn confirm_delete(&self) {
        let imp = self.imp();
        if imp.editing.get() || imp.asking.get() {
            return;
        }
        let Some(id) = imp.current.borrow().clone() else {
            return;
        };
        let Some(question) = dialog::alert("delete_card_dialog.ui", "delete_card_dialog") else {
            return;
        };
        imp.asking.set(true);
        // AdwDialog gives the focus back to the widget that had it when it
        // has closed, which would undo a grab made on the response (the
        // editor and Card Information set theirs on `closed` for the same
        // reason): the card area needs the focus for the keys to work.
        question.connect_closed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.park_focus()
        ));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let answer = question.choose_future(Some(&page)).await;
                page.imp().asking.set(false);
                // Only the card the question was about.
                if answer == "delete" && page.imp().current.borrow().as_deref() == Some(&id) {
                    page.remove(&id, Removal::Delete);
                }
            }
        ));
    }

    /// Brings card `id` back into the session after the Undo of a suspend or
    /// a delete: it comes next (a card an undo brought back, or a step card
    /// that is due, still goes first), and the summary gives way to it.
    fn put_back(&self, id: &str) {
        let imp = self.imp();
        if let Some(session) = imp.session.borrow_mut().as_mut() {
            session.put_back(id);
        }
        if imp.current.borrow().is_none() {
            self.show_next();
        } else {
            let left = imp.session.borrow().as_ref().map_or(0, Session::remaining);
            imp.window_title.set_subtitle(&left_text(left));
        }
    }

    /// A save made from the editor: shows the card on screen again if it
    /// is the one saved, then tells the owner of this page.
    fn saved(&self, saved: Saved) {
        let imp = self.imp();
        if let Saved::Edited(tagged) = &saved
            && imp.current.borrow().as_deref() == Some(tagged.card.id.as_str())
        {
            self.redraw(&tagged.card.front_md, &tagged.card.back_md);
        }
        if let Some(on_saved) = imp.on_saved.get().map(Callback::get) {
            on_saved(saved);
        }
    }

    /// Renders the card on screen from new texts. Only what is shown
    /// changes: the reveal, the timers and the session stay as they are.
    fn redraw(&self, front_md: &str, back_md: &str) {
        let imp = self.imp();
        self.show_front(front_md);
        self.keep_back(back_md);
        if imp.revealed.get() {
            let back = imp.back.take();
            // A redraw never animates: the answer was already on screen,
            // so it changes in place (only `reveal` plays the animation).
            let _shown = self.show_back(&back);
        }
    }

    /// Shows `controls` in the bottom bar and enables only their actions.
    fn set_controls(&self, controls: Controls) {
        let (name, reveal, grade) = match controls {
            Controls::Reveal => (Some("reveal"), true, false),
            Controls::Grade => (Some("grade"), false, true),
            Controls::Hidden => (None, false, false),
        };
        // Hidden with the summary, where there is no card to edit.
        let editable = !matches!(controls, Controls::Hidden);
        self.action_set_enabled("review.edit", editable);
        self.action_set_enabled("review.card-info", editable);
        self.action_set_enabled("review.suspend", editable);
        self.action_set_enabled("review.delete", editable);
        self.imp().edit_button.set_visible(editable);
        self.imp().card_menu_button.set_visible(editable);
        // The summary has its own button; the bar goes, and the summary
        // gets its room.
        self.imp().bottom_bar.set_visible(name.is_some());
        if let Some(name) = name {
            self.imp().buttons.set_visible_child_name(name);
        }
        self.action_set_enabled("review.reveal", reveal);
        self.action_set_enabled("review.fail", grade);
        self.action_set_enabled("review.pass", grade);
        self.update_undo();
    }

    /// Enables Undo while the session has an answer to take back.
    fn update_undo(&self) {
        let can_undo = self
            .imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(|s| s.last_answer().is_some());
        self.action_set_enabled("review.undo", can_undo);
    }

    /// Takes back the last answer of the session: the log first, then the
    /// session, then the card is shown again as new (front side, timer
    /// restarted), whether a card or the summary was on screen.
    fn undo(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        let last = imp
            .session
            .borrow()
            .as_ref()
            .and_then(|s| s.last_answer().map(|(c, e)| (c.to_owned(), e.to_owned())));
        let Some((card_id, entry_id)) = last else {
            return;
        };
        match state.undo(&card_id, &entry_id, rappel_core::time::now_ms()) {
            Ok(_) => {
                if let Some(session) = imp.session.borrow_mut().as_mut() {
                    session.undo_last();
                }
                imp.current.replace(None);
                self.show_next();
            }
            Err(err) => match undo::failure(&err) {
                UndoFailure::Stale => {
                    // Older answers are undone only after this one, so none
                    // of them can be now.
                    if let Some(session) = imp.session.borrow_mut().as_mut() {
                        session.forget_answers();
                    }
                    self.update_undo();
                    // The button may have had the focus and is now off:
                    // put it where the page's keys work.
                    self.park_focus();
                    show_toast_from(self, info_toast("This answer can no longer be undone"));
                }
                UndoFailure::Failed => {
                    self.error_toast(&format!("The answer could not be undone: {err}"));
                }
            },
        }
    }

    /// Records the answer for the card on screen and shows the next one.
    /// If it cannot be recorded, the card stays so the answer can be given
    /// again.
    fn grade(&self, grade: Grade) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        if !imp.revealed.get() {
            return;
        }
        let Some(id) = imp.current.borrow().clone() else {
            return;
        };
        let duration = capped(imp.shown_at.get().map_or(Duration::ZERO, |t| t.elapsed()));
        let now = rappel_core::time::now_ms();
        let event = match state.record_review(&id, grade, duration, now) {
            Ok(event) => event,
            Err(err) => {
                self.error_toast(&format!("The answer could not be saved: {err}"));
                return;
            }
        };
        let card_state = state.card_state(&id).unwrap_or_else(|err| {
            // The answer is saved; without its new state the card just
            // does not come back in this session.
            tracing::warn!("cannot read a card's state after an answer: {err}");
            CardState::New
        });
        if let Some(session) = imp.session.borrow_mut().as_mut() {
            session.graded(&id, &event.id, grade, duration, &card_state);
        }
        imp.current.replace(None);
        self.show_next();
    }

    fn show_summary(&self) {
        let imp = self.imp();
        imp.current.replace(None);
        imp.shown_at.set(None);
        let summary = imp
            .session
            .borrow()
            .as_ref()
            .map(Session::summary)
            .unwrap_or_default();
        if let Some([cards, passed, time]) = summary_tiles(&summary) {
            set_tile(&imp.cards_value, &imp.cards_caption, &cards);
            set_tile(&imp.passed_value, &imp.passed_caption, &passed);
            set_tile(&imp.time_value, &imp.time_caption, &time);
            imp.summary_figures.set_visible(true);
            imp.summary_page.set_description(None);
        } else {
            imp.summary_figures.set_visible(false);
            imp.summary_page.set_description(Some(NOTHING_REVIEWED));
        }
        imp.window_title.set_subtitle("");
        self.set_controls(Controls::Hidden);
        imp.stack.set_visible_child_name("summary");
        imp.done_button.grab_focus();
    }

    fn done(&self) {
        nav::pop(self);
    }

    /// Shows `message`, dismissing the previous error toast of this page
    /// first, so a toast that stays until dismissed does not hold it back.
    fn error_toast(&self, message: &str) {
        self.imp().errors.show(self, "a review operation", message);
    }
}

/// Whether the note "only its start is shown" is under the card: the front
/// is cut, or the back is cut and shown.
fn cut_note_shown(front_cut: bool, back_cut: bool, revealed: bool) -> bool {
    front_cut || (back_cut && revealed)
}

/// What screen readers read for a side: its text, and the note when only
/// its start is shown, since the note itself is not in the focus chain.
fn with_cut_note(text: String, cut: bool) -> String {
    if cut {
        format!("{text}\n\n{CUT_NOTE}")
    } else {
        text
    }
}

/// The note's text for screen readers. Keep it equal to `cut_note` in
/// `review_page.blp`.
const CUT_NOTE: &str = "This card is long: only its start is shown. Edit it to see all of it.";

/// The subtitle while studying: "12 left", "1,234 left". It counts the
/// card on screen and step cards that will come back, so it can go up
/// after a Fail.
fn left_text(left: usize) -> String {
    format!("{} left", group_digits(left))
}

/// How long the answer takes to come in, in milliseconds: short enough
/// never to hold anyone up.
const REVEAL_MS: u32 = 180;

/// How far below its place the answer starts, in pixels.
const REVEAL_SLIDE: f64 = 12.0;

/// The back's opacity and top margin at `progress` (0 to 1, clamped) of the
/// reveal: from hidden and [`REVEAL_SLIDE`] lower to fully shown in place.
fn reveal_step(progress: f64) -> (f64, i32) {
    let progress = if progress.is_nan() {
        1.0
    } else {
        progress.clamp(0.0, 1.0)
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a value between 0 and REVEAL_SLIDE, rounded"
    )]
    let margin = (REVEAL_SLIDE * (1.0 - progress)).round() as i32;
    (progress, margin)
}

/// The summary's icon, a check mark. Keep it equal to `summary_page` in
/// `review_page.blp`.
const SUMMARY_ICON: &str = "object-select-symbolic";

/// The summary page's text when no card was answered (all of them were
/// suspended or deleted during the session).
const NOTHING_REVIEWED: &str = "No cards were reviewed";

/// The summary's three tiles: cards reviewed, pass rate and time, or
/// `None` when no card was answered.
fn summary_tiles(summary: &Summary) -> Option<[Tile; 3]> {
    let percent = summary.pass_percent().filter(|_| summary.cards > 0)?;
    let cards_caption = if summary.cards == 1 {
        "Card reviewed"
    } else {
        "Cards reviewed"
    };
    let tile = |value: String, caption: &str| Tile {
        accessible: format!("{caption}: {value}"),
        value,
        caption: caption.to_owned(),
    };
    Some([
        tile(group_digits(summary.cards), cards_caption),
        tile(format!("{percent}%"), "Pass rate"),
        tile(duration_text(summary.time), "Time"),
    ])
}

#[cfg(test)]
mod tests {
    use rappel_core::domain::EventKind;

    use super::*;
    use crate::render::MAX_SHOWN_BYTES;

    #[test]
    fn the_cut_note_follows_the_sides_on_screen() {
        assert!(!cut_note_shown(false, false, false));
        assert!(!cut_note_shown(false, false, true));
        assert!(cut_note_shown(true, false, false));
        // A cut back is only noted once it is shown.
        assert!(!cut_note_shown(false, true, false));
        assert!(cut_note_shown(false, true, true));
    }

    #[test]
    fn screen_readers_hear_the_cut_note() {
        assert_eq!(with_cut_note("Front".to_owned(), false), "Front");
        assert_eq!(
            with_cut_note("Front".to_owned(), true),
            format!("Front\n\n{CUT_NOTE}")
        );
    }

    #[test]
    fn the_cut_note_matches_the_blueprint() {
        crate::testing::with_widgets(|| {
            let page: ReviewPage = glib::Object::new();
            assert_eq!(page.imp().cut_note.label(), CUT_NOTE);
        });
    }

    #[test]
    fn keys_reveal_grade_and_edit() {
        use Key::{Edit, Enter, Fail, Pass, Space};
        let grade = |g| Some(Act::Grade(g));
        // Before the answer.
        assert_eq!(key_action(Space, false, false), Some(Act::Reveal));
        assert_eq!(key_action(Enter, false, false), Some(Act::Reveal));
        assert_eq!(key_action(Fail, false, true), None);
        assert_eq!(key_action(Pass, false, true), None);
        // After it, once settled.
        assert_eq!(key_action(Space, true, true), grade(Grade::Pass));
        assert_eq!(key_action(Pass, true, true), grade(Grade::Pass));
        assert_eq!(key_action(Fail, true, true), grade(Grade::Fail));
        assert_eq!(key_action(Enter, true, true), None);
        // Key repeat right after the reveal grades nothing.
        for key in [Space, Pass, Fail] {
            assert_eq!(key_action(key, true, false), None);
        }
        // E edits at any time, settled or not. So does I, to look.
        for (revealed, settled) in [(false, false), (true, false), (true, true)] {
            assert_eq!(key_action(Edit, revealed, settled), Some(Act::Edit));
            assert_eq!(key_action(Key::Info, revealed, settled), Some(Act::Info));
        }
        // S and Delete too: they grade nothing.
        for (revealed, settled) in [(false, false), (true, false), (true, true)] {
            assert_eq!(
                key_action(Key::Suspend, revealed, settled),
                Some(Act::Suspend)
            );
            assert_eq!(
                key_action(Key::Delete, revealed, settled),
                Some(Act::Delete)
            );
        }
        // Ctrl+Z too, and it grades nothing, so key repeat does no harm.
        for (revealed, settled) in [(false, false), (true, false), (true, true)] {
            assert_eq!(key_action(Key::Undo, revealed, settled), Some(Act::Undo));
        }
    }

    /// A page over a deck of `n` cards that are all due, with the app state.
    fn page_of(n: usize) -> (tempfile::TempDir, AppState, ReviewPage, Vec<String>) {
        adw::init().unwrap();
        gtk::gio::resources_register_include!("rappel.gresource").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::open(tmp.path().join("data")).unwrap();
        let deck = state.create_deck("Deck").unwrap();
        let now = rappel_core::time::now_ms();
        let day = 24 * 60 * 60 * 1000;
        let ids: Vec<String> = (0..n)
            .map(|i| {
                let id = state
                    .add_card_with_tags(&deck.id, &format!("front {i}"), "back", &[])
                    .unwrap()
                    .card
                    .id;
                // Two Passes long ago leave the card in review and due now.
                for at in [now - 60 * day, now - 60 * day + 11 * 60 * 1000] {
                    state
                        .record_review(&id, Grade::Pass, Duration::ZERO, at)
                        .unwrap();
                }
                id
            })
            .collect();
        let page = ReviewPage::new(state.clone(), deck, |_| {}).unwrap();
        (tmp, state, page, ids)
    }

    /// Puts `page` in a window, so it has a root as a pushed page does.
    fn in_window(page: &ReviewPage) -> gtk::Window {
        gtk::Window::builder().child(page).build()
    }

    #[test]
    fn the_delete_question_loads_and_defaults_to_cancel() {
        crate::testing::with_gtk(|| {
            adw::init().unwrap();
            gtk::gio::resources_register_include!("rappel.gresource").unwrap();
            let question = dialog::alert("delete_card_dialog.ui", "delete_card_dialog").unwrap();
            assert_eq!(question.default_response().as_deref(), Some("cancel"));
            assert_eq!(question.close_response(), "cancel");
            assert_eq!(
                question.response_appearance("delete"),
                adw::ResponseAppearance::Destructive
            );
        });
    }

    #[test]
    fn s_waits_for_the_card_to_have_been_on_screen() {
        assert!(!suspend_allowed(None));
        assert!(!suspend_allowed(Some(Duration::ZERO)));
        assert!(!suspend_allowed(Some(
            KEY_SETTLE.saturating_sub(Duration::from_millis(1))
        )));
        assert!(suspend_allowed(Some(KEY_SETTLE)));
    }

    #[test]
    fn holding_s_suspends_one_card_not_a_run() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(3);
            let _window = in_window(&page);
            let first = current(&page).unwrap();
            // Just shown: the key does nothing.
            page.key(Key::Suspend);
            assert_eq!(current(&page).as_deref(), Some(first.as_str()));
            assert!(!state.card(&first).unwrap().suspended);
            // Shown a moment: the first S suspends, the repeat finds a new card.
            page.imp()
                .shown_at
                .set(Instant::now().checked_sub(Duration::from_secs(1)));
            page.key(Key::Suspend);
            page.key(Key::Suspend);
            page.key(Key::Suspend);
            let suspended = ids
                .iter()
                .filter(|id| state.card(id).unwrap().suspended)
                .count();
            assert_eq!(suspended, 1);
        });
    }

    #[test]
    fn the_keys_do_nothing_while_the_editor_or_the_question_is_open() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, _) = page_of(2);
            let _window = in_window(&page);
            let first = current(&page).unwrap();
            page.imp()
                .shown_at
                .set(Instant::now().checked_sub(Duration::from_secs(1)));
            page.imp().asking.set(true);
            assert_eq!(page.key(Key::Suspend), glib::Propagation::Proceed);
            assert_eq!(page.key(Key::Delete), glib::Propagation::Proceed);
            page.imp().asking.set(false);
            page.imp().editing.set(true);
            assert_eq!(page.key(Key::Suspend), glib::Propagation::Proceed);
            page.imp().editing.set(false);
            assert!(!state.card(&first).unwrap().suspended);
            assert_eq!(current(&page).as_deref(), Some(first.as_str()));
        });
    }

    #[test]
    fn a_card_already_suspended_elsewhere_leaves_the_session_without_an_undo() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, _) = page_of(2);
            let _window = in_window(&page);
            let first = current(&page).unwrap();
            state
                .set_suspended(std::slice::from_ref(&first), true)
                .unwrap();
            page.remove_current(Removal::Suspend);
            assert_ne!(current(&page).as_deref(), Some(first.as_str()));
        });
    }

    #[test]
    fn an_undo_after_the_page_is_gone_restores_the_card_only_in_the_database() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(1);
            let weak = page.downgrade();
            page.remove(&ids[0], Removal::Delete);
            drop(page);
            assert!(weak.upgrade().is_none());
            undo_removal(&state, &ids[0], Removal::Delete, &weak, None);
            assert!(state.card(&ids[0]).is_ok());
        });
    }

    #[test]
    fn an_undo_that_cannot_restore_the_card_leaves_it_deleted_and_the_session_alone() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(1);
            let _window = in_window(&page);
            let deck = state.card(&ids[0]).unwrap().deck_id;
            page.remove(&ids[0], Removal::Delete);
            state.delete_deck(&deck).unwrap();
            // The deck is deleted: the card cannot come back into it.
            undo_removal(&state, &ids[0], Removal::Delete, &page.downgrade(), None);
            assert!(state.card(&ids[0]).is_err());
            assert_eq!(current(&page), None);
        });
    }

    #[test]
    fn a_removal_is_named_in_its_toast() {
        assert_eq!(Removal::Suspend.text(), "Card suspended");
        assert_eq!(Removal::Delete.text(), "Card deleted");
    }

    #[test]
    fn a_suspended_card_leaves_the_session_and_its_undo_brings_it_back() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, _) = page_of(3);
            let _window = in_window(&page);
            let first = current(&page).unwrap();
            let summary_before = page.imp().session.borrow().as_ref().unwrap().summary();

            page.remove_current(Removal::Suspend);
            assert_ne!(current(&page).as_deref(), Some(first.as_str()));
            assert!(state.card(&first).unwrap().suspended);
            // Not counted as answered, and nothing was written to the log.
            let session = page.imp().session.borrow();
            assert_eq!(session.as_ref().unwrap().summary(), summary_before);
            // The card on screen stays queued until it is answered.
            assert_eq!(session.as_ref().unwrap().remaining(), 2);
            drop(session);
            assert_eq!(state.review_events(&first).unwrap().len(), 2);

            undo_removal(&state, &first, Removal::Suspend, &page.downgrade(), None);
            assert!(!state.card(&first).unwrap().suspended);
            // It is the card after the one on screen.
            let second = current(&page).unwrap();
            pass(&page);
            assert_eq!(current(&page).as_deref(), Some(first.as_str()));
            assert_ne!(second, first);
        });
    }

    #[test]
    fn deleting_the_last_card_shows_the_summary_and_undo_shows_the_card_again() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(1);
            let _window = in_window(&page);
            page.remove(&ids[0], Removal::Delete);
            assert_eq!(current(&page), None);
            assert!(state.card(&ids[0]).is_err(), "the card is deleted");

            undo_removal(&state, &ids[0], Removal::Delete, &page.downgrade(), None);
            assert_eq!(current(&page).as_deref(), Some(ids[0].as_str()));
            assert!(state.card(&ids[0]).is_ok());
        });
    }

    #[test]
    fn a_removal_of_a_card_gone_elsewhere_just_moves_on() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(2);
            let _window = in_window(&page);
            let first = current(&page).unwrap();
            state.delete_card(&first).unwrap();
            page.remove_current(Removal::Suspend);
            assert_ne!(current(&page).as_deref(), Some(first.as_str()));
            assert!(ids.contains(&current(&page).unwrap()));
        });
    }

    #[test]
    fn a_card_suspended_elsewhere_is_skipped_when_its_turn_comes() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(2);
            let first = current(&page).unwrap();
            let other = ids.iter().find(|id| **id != first).unwrap().clone();
            state.set_suspended(&[other], true).unwrap();
            pass(&page);
            assert_eq!(current(&page), None, "the session is over");
        });
    }

    #[test]
    fn ctrl_z_takes_back_an_answer_without_showing_a_card_suspended_since() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, _) = page_of(2);
            let first = current(&page).unwrap();
            pass(&page);
            let second = current(&page).unwrap();
            state
                .set_suspended(std::slice::from_ref(&first), true)
                .unwrap();

            page.undo();
            // The answer is taken back in the log, and the card the undo
            // would show is not studied.
            assert_eq!(undo_entries(&state, &first), 1);
            assert_eq!(current(&page).as_deref(), Some(second.as_str()));
        });
    }

    /// Reveals and passes the card on screen.
    fn pass(page: &ReviewPage) {
        page.reveal();
        page.grade(Grade::Pass);
    }

    fn current(page: &ReviewPage) -> Option<String> {
        page.imp().current.borrow().clone()
    }

    /// The text of the first label under `widget`, depth first.
    fn first_label(widget: &gtk::Widget) -> Option<String> {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            return Some(label.text().to_string());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(text) = first_label(&c) {
                return Some(text);
            }
            child = c.next_sibling();
        }
        None
    }

    fn can_undo(page: &ReviewPage) -> bool {
        page.imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(|s| s.last_answer().is_some())
    }

    fn undo_entries(state: &AppState, card: &str) -> usize {
        let log = state.review_events(card).unwrap();
        log.iter()
            .filter(|e| matches!(e.kind, EventKind::Undo { .. }))
            .count()
    }

    #[test]
    fn undo_goes_back_to_the_first_answer() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(3);
            let before = state.card_state(&ids[0]).unwrap();
            assert!(!can_undo(&page));
            pass(&page);
            pass(&page);
            assert_eq!(current(&page).as_deref(), Some(ids[2].as_str()));
            assert!(can_undo(&page));
            page.undo();
            assert_eq!(current(&page).as_deref(), Some(ids[1].as_str()));
            page.undo();
            // The first card is back: front side, nothing revealed.
            assert_eq!(current(&page).as_deref(), Some(ids[0].as_str()));
            assert!(!page.imp().revealed.get());
            assert_eq!(page.imp().window_title.subtitle(), "3 left");
            assert!(!can_undo(&page));
            assert_eq!(state.card_state(&ids[0]).unwrap(), before);
        });
    }

    #[test]
    fn undo_from_the_summary_brings_the_last_card_back() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(2);
            pass(&page);
            pass(&page);
            let imp = page.imp();
            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("summary"));
            assert!(current(&page).is_none());
            page.undo();
            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("card"));
            assert_eq!(current(&page).as_deref(), Some(ids[1].as_str()));
            let summary = imp.session.borrow().as_ref().unwrap().summary();
            assert_eq!((summary.cards, summary.answers), (1, 1));
            assert_eq!(undo_entries(&state, &ids[1]), 1);
        });
    }

    #[test]
    fn undo_shows_the_card_as_edited_meanwhile() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(2);
            pass(&page);
            state
                .edit_card_with_tags(&ids[0], "edited front", "back", None)
                .unwrap();
            page.undo();
            let front = first_label(page.imp().front_box.upcast_ref());
            assert_eq!(front.as_deref(), Some("edited front"));
        });
    }

    #[test]
    fn rapid_repeated_undo_stops_at_the_first_answer() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(3);
            for _ in 0..3 {
                pass(&page);
            }
            for _ in 0..5 {
                page.undo();
            }
            assert!(!can_undo(&page));
            assert_eq!(current(&page).as_deref(), Some(ids[0].as_str()));
            let undone: usize = ids.iter().map(|id| undo_entries(&state, id)).sum();
            assert_eq!(undone, 3, "one undo per answer, no more");
        });
    }

    #[test]
    fn an_answer_that_is_no_longer_the_latest_is_not_undone() {
        crate::testing::with_gtk(|| {
            let (_tmp, state, page, ids) = page_of(2);
            pass(&page);
            // Something else (an import, say) made a newer entry.
            state
                .reset_card(&ids[0], rappel_core::time::now_ms())
                .unwrap();
            let log = state.review_events(&ids[0]).unwrap().len();
            page.undo();
            assert_eq!(state.review_events(&ids[0]).unwrap().len(), log);
            assert_eq!(current(&page).as_deref(), Some(ids[1].as_str()));
            assert!(!can_undo(&page));
        });
    }

    #[test]
    fn the_summary_shows_cards_rate_and_time() {
        let summary = Summary {
            cards: 1_234,
            answers: 18,
            passes: 15,
            time: Duration::from_secs(250),
        };
        let values = |tiles: [Tile; 3]| tiles.map(|t| (t.value, t.caption, t.accessible));
        assert_eq!(
            values(summary_tiles(&summary).unwrap()),
            [
                (
                    "1,234".to_owned(),
                    "Cards reviewed".to_owned(),
                    "Cards reviewed: 1,234".to_owned()
                ),
                (
                    "83%".to_owned(),
                    "Pass rate".to_owned(),
                    "Pass rate: 83%".to_owned()
                ),
                (
                    "4 min 10 s".to_owned(),
                    "Time".to_owned(),
                    "Time: 4 min 10 s".to_owned()
                ),
            ]
        );
        let one = Summary {
            cards: 1,
            answers: 1,
            passes: 0,
            time: Duration::from_secs(3),
        };
        let [cards, passed, time] = summary_tiles(&one).unwrap();
        assert_eq!(
            (cards.value.as_str(), cards.caption.as_str()),
            ("1", "Card reviewed")
        );
        assert_eq!(passed.value, "0%");
        assert_eq!(time.value, "3 s");
        // Every card suspended or deleted: nothing to sum up.
        assert_eq!(summary_tiles(&Summary::default()), None);
    }

    #[test]
    fn the_summary_icon_matches_the_blueprint() {
        crate::testing::with_widgets(|| {
            let page: ReviewPage = glib::Object::new();
            assert_eq!(
                page.imp().summary_page.icon_name().as_deref(),
                Some(SUMMARY_ICON)
            );
        });
    }

    #[test]
    fn the_subtitle_counts_what_is_left_with_grouped_digits() {
        assert_eq!(left_text(0), "0 left");
        assert_eq!(left_text(3), "3 left");
        assert_eq!(left_text(1_234), "1,234 left");
    }

    #[test]
    fn the_answer_fades_in_and_slides_up_into_place() {
        assert_eq!(reveal_step(0.0), (0.0, 12));
        assert_eq!(reveal_step(0.5), (0.5, 6));
        assert_eq!(reveal_step(1.0), (1.0, 0));
        // Out of range (or not a number) ends shown, in place.
        assert_eq!(reveal_step(-1.0), (0.0, 12));
        assert_eq!(reveal_step(2.0), (1.0, 0));
        assert_eq!(reveal_step(f64::NAN), (1.0, 0));
        // It only ever comes closer to its place.
        let steps: Vec<_> = (0..=100)
            .map(|i| reveal_step(f64::from(i) / 100.0))
            .collect();
        assert!(
            steps
                .windows(2)
                .all(|w| w[0].0 <= w[1].0 && w[0].1 >= w[1].1)
        );
    }

    /// The hints under the buttons name keys the page binds to the same act,
    /// and screen readers skip them.
    #[test]
    fn the_key_hints_match_the_keys() {
        crate::testing::with_widgets(|| {
            let page: ReviewPage = glib::Object::new();
            let imp = page.imp();
            for (hint, trigger, act) in [
                (&*imp.reveal_hint, "space", Act::Reveal),
                (&*imp.fail_hint, "1", Act::Grade(Grade::Fail)),
                (&*imp.pass_hint, "2", Act::Grade(Grade::Pass)),
            ] {
                assert_eq!(hint.accessible_role(), gtk::AccessibleRole::Presentation);
                // The hint shows the key as GTK names it ("space" → "Space").
                let (key, mods) = gtk::accelerator_parse(trigger).unwrap();
                assert_eq!(
                    hint.label(),
                    gtk::accelerator_get_label(key, mods),
                    "{trigger}"
                );
                let (_, bound) = KEYS.iter().find(|(t, _)| *t == trigger).unwrap();
                let revealed = act != Act::Reveal;
                assert_eq!(key_action(*bound, revealed, true), Some(act), "{trigger}");
            }
        });
        // Space also passes, as the Pass button's description says.
        assert_eq!(
            key_action(Key::Space, true, true),
            Some(Act::Grade(Grade::Pass))
        );
    }

    /// GTK has no public getter for an accessible description, so the
    /// buttons' descriptions are checked in the template's source.
    #[test]
    fn the_buttons_describe_their_keys() {
        let blueprint = include_str!("../../data/review_page.blp");
        for description in ["Press Space", "Press 1", "Press 2 or Space"] {
            assert!(
                blueprint.contains(&format!("description: _(\"{description}\");")),
                "{description}"
            );
        }
    }

    #[test]
    fn a_short_window_makes_the_summary_compact_without_its_icon() {
        crate::testing::with_widgets(|| {
            let page: ReviewPage = glib::Object::new();
            let imp = page.imp();
            assert!(!imp.summary_page.has_css_class("compact"));
            imp.short_breakpoint.emit_by_name::<()>("apply", &[]);
            assert!(imp.summary_page.has_css_class("compact"));
            assert_eq!(imp.summary_page.icon_name(), None);
            imp.short_breakpoint.emit_by_name::<()>("unapply", &[]);
            assert!(!imp.summary_page.has_css_class("compact"));
            assert_eq!(imp.summary_page.icon_name().as_deref(), Some(SUMMARY_ICON));
        });
    }

    /// `page` in a presented window, with animations on, once its card is
    /// mapped (an animation on an unmapped widget is skipped).
    fn on_screen(page: &ReviewPage) -> adw::Window {
        gtk::Settings::default()
            .expect("GTK is started")
            .set_gtk_enable_animations(true);
        let window = adw::Window::builder()
            .default_width(1000)
            .default_height(720)
            .content(page)
            .build();
        window.present();
        assert!(
            crate::testing::settle_until(
                || page.imp().card_area.is_mapped(),
                Duration::from_secs(5)
            ),
            "the page did not show"
        );
        window
    }

    fn animation(page: &ReviewPage) -> adw::AnimationState {
        page.imp()
            .reveal_animation
            .get()
            .expect("made in constructed")
            .state()
    }

    /// The reveal really plays on screen, and the next card, or an undo,
    /// ends it with the back hidden. No main-loop iteration runs between
    /// the calls and the checks, so the state cannot move on by itself.
    #[test]
    fn the_reveal_plays_on_screen_and_the_next_card_or_an_undo_ends_it() {
        crate::testing::with_widgets(|| {
            let (_tmp, _state, page, _) = page_of(3);
            let window = on_screen(&page);
            let imp = page.imp();

            page.reveal();
            assert_eq!(animation(&page), adw::AnimationState::Playing);
            assert!(imp.back_part.is_visible());
            assert!(imp.back_part.opacity() < 1.0);
            page.grade(Grade::Pass);
            assert_eq!(animation(&page), adw::AnimationState::Finished);
            assert!(!imp.back_part.is_visible());

            page.reveal();
            assert_eq!(animation(&page), adw::AnimationState::Playing);
            page.undo();
            assert_eq!(animation(&page), adw::AnimationState::Finished);
            assert!(!imp.back_part.is_visible());
            assert!(!imp.revealed.get());

            // Left alone, it finishes with the back fully shown in place.
            page.reveal();
            assert!(crate::testing::settle_until(
                || animation(&page) == adw::AnimationState::Finished,
                Duration::from_secs(5)
            ));
            assert!((imp.back_part.opacity() - 1.0).abs() < f64::EPSILON);
            assert_eq!(imp.back_part.margin_top(), 0);
            window.destroy();
        });
    }

    /// A card whose back is `back`, due now, on a page.
    fn page_with_back(back: &str) -> (tempfile::TempDir, ReviewPage) {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::open(tmp.path().join("data")).unwrap();
        let deck = state.create_deck("Deck").unwrap();
        let now = rappel_core::time::now_ms();
        let day = 24 * 60 * 60 * 1000;
        let id = state
            .add_card_with_tags(&deck.id, "front", back, &[])
            .unwrap()
            .card
            .id;
        for at in [now - 60 * day, now - 60 * day + 11 * 60 * 1000] {
            state
                .record_review(&id, Grade::Pass, Duration::ZERO, at)
                .unwrap();
        }
        let page = ReviewPage::new(state, deck, |_| {}).unwrap();
        (tmp, page)
    }

    /// The reveal animation on the largest backs the page lays out: a 32 KiB
    /// paragraph, 200 inline formulas, a list of 2,000 items. Its frames
    /// change the back's opacity and top margin; the margin re-measures only
    /// the back's box and its ancestors (GTK keeps the labels' and formulas'
    /// measures), so the frames cost what drawing costs (ADR 0045). Timed
    /// in CPU time once the back's first layout is done; about 0 ms, 180 ms
    /// and 10 ms here (debug build, Xvfb), the same as an animation of the
    /// opacity alone. The bound leaves room for slow machines.
    #[test]
    fn a_reveal_of_the_largest_backs_stays_cheap() {
        use std::fmt::Write as _;
        let sides = [
            ("32 KiB paragraph", "word ".repeat(MAX_SHOWN_BYTES / 5)),
            (
                "200 formulas",
                (0..200).fold(String::new(), |mut side, i| {
                    let _ = write!(side, "$x_{{{i}}}^2$ and ");
                    side
                }),
            ),
            (
                "2,000 items",
                (0..2_000).fold(String::new(), |mut side, i| {
                    let _ = writeln!(side, "- item {i}");
                    side
                }),
            ),
        ];
        let times = crate::testing::with_widgets(move || {
            sides.map(|(what, back)| {
                assert!(back.len() <= MAX_SHOWN_BYTES, "{what}");
                let (_tmp, page) = page_with_back(&back);
                let window = on_screen(&page);
                let finished = || animation(&page) == adw::AnimationState::Finished;
                page.reveal();
                assert!(crate::testing::settle_until(
                    finished,
                    Duration::from_secs(20)
                ));
                let cpu = crate::testing::thread_cpu_time();
                page.animate_reveal();
                assert!(crate::testing::settle_until(
                    finished,
                    Duration::from_secs(20)
                ));
                let used = cpu
                    .zip(crate::testing::thread_cpu_time())
                    .map(|(a, b)| b.saturating_sub(a));
                window.destroy();
                (what, used)
            })
        });
        for (what, used) in times {
            if let Some(used) = used {
                crate::testing::print_timing(&format!("reveal animation, {what}"), used);
                assert!(used < Duration::from_secs(2), "{what}: {used:?}");
            }
        }
    }

    #[test]
    fn the_back_is_shown_whole_at_the_reveal_and_hidden_for_the_next_card() {
        crate::testing::with_gtk(|| {
            let (_tmp, _state, page, _) = page_of(2);
            let imp = page.imp();
            assert!(!imp.back_part.is_visible());
            assert_eq!(imp.buttons.visible_child_name().as_deref(), Some("reveal"));
            page.reveal();
            // Not on screen, so libadwaita skips the animation to its end.
            assert!(imp.back_part.is_visible());
            assert!((imp.back_part.opacity() - 1.0).abs() < f64::EPSILON);
            assert_eq!(imp.back_part.margin_top(), 0);
            assert_eq!(imp.buttons.visible_child_name().as_deref(), Some("grade"));
            assert_eq!(
                first_label(imp.back_box.upcast_ref()).as_deref(),
                Some("back")
            );
            page.grade(Grade::Pass);
            assert!(!imp.back_part.is_visible());
            assert_eq!(imp.buttons.visible_child_name().as_deref(), Some("reveal"));
            assert_eq!(imp.window_title.subtitle(), "1 left");
        });
    }

    #[test]
    fn the_summary_replaces_the_bar_and_shows_the_figures() {
        crate::testing::with_gtk(|| {
            let (_tmp, _state, page, _) = page_of(2);
            let imp = page.imp();
            assert!(imp.bottom_bar.is_visible());
            pass(&page);
            pass(&page);
            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("summary"));
            assert!(!imp.bottom_bar.is_visible());
            assert!(imp.summary_figures.is_visible());
            assert_eq!(imp.cards_value.label(), "2");
            assert_eq!(imp.cards_caption.label(), "Cards reviewed");
            assert_eq!(imp.passed_value.label(), "100%");
            assert!(
                imp.summary_page
                    .description()
                    .unwrap_or_default()
                    .is_empty()
            );
            assert_eq!(imp.window_title.subtitle(), "");
            // Undo brings the card and the bar back.
            page.undo();
            assert!(imp.bottom_bar.is_visible());
            assert_eq!(imp.buttons.visible_child_name().as_deref(), Some("reveal"));
        });
    }

    #[test]
    fn a_session_with_nothing_answered_says_so() {
        crate::testing::with_gtk(|| {
            let (_tmp, _state, page, ids) = page_of(1);
            let _window = in_window(&page);
            page.remove(&ids[0], Removal::Suspend);
            let imp = page.imp();
            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("summary"));
            assert!(!imp.summary_figures.is_visible());
            assert_eq!(
                imp.summary_page.description().as_deref(),
                Some(NOTHING_REVIEWED)
            );
        });
    }

    /// The action each key's menu item runs, for the hints' test.
    fn menu_action(key: Key) -> Option<&'static str> {
        match key {
            Key::Info => Some("review.card-info"),
            Key::Suspend => Some("review.suspend"),
            Key::Delete => Some("review.delete"),
            _ => None,
        }
    }

    /// (action, accel) of every item of `menu` and its sections.
    fn menu_items(menu: &gtk::gio::MenuModel, out: &mut Vec<(String, Option<String>)>) {
        let string = |i, name| {
            menu.item_attribute_value(i, name, Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>())
        };
        for i in 0..menu.n_items() {
            if let Some(section) = menu.item_link(i, "section") {
                menu_items(&section, out);
            } else if let Some(action) = string(i, "action") {
                out.push((action, string(i, "accel")));
            }
        }
    }

    #[test]
    fn the_card_menu_shows_the_keys_the_page_binds() {
        crate::testing::with_gtk(|| {
            adw::init().unwrap();
            gtk::gio::resources_register_include!("rappel.gresource").unwrap();
            let page: ReviewPage = glib::Object::new();
            let menu = page
                .imp()
                .card_menu_button
                .menu_model()
                .expect("the card menu button has its menu");
            let mut items = Vec::new();
            menu_items(&menu, &mut items);
            assert_eq!(items.len(), 3);
            for (action, accel) in items {
                let accel = accel.unwrap_or_else(|| panic!("{action} shows no key"));
                let bound = KEYS
                    .iter()
                    .find(|(trigger, _)| *trigger == accel)
                    .unwrap_or_else(|| panic!("{accel} is not one of the page's keys"));
                assert_eq!(menu_action(bound.1), Some(action.as_str()));
                assert!(gtk::accelerator_parse(&accel).is_some());
            }
            // And GTK's menu shows them: each item's GtkModelButton has the
            // key as its `accel` (what it displays).
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            let mut shown = Vec::new();
            let mut todo = vec![popover.upcast::<gtk::Widget>()];
            while let Some(widget) = todo.pop() {
                if widget.type_().name() == "GtkModelButton" {
                    shown.push(widget.property::<Option<String>>("accel"));
                }
                let mut child = widget.first_child();
                while let Some(c) = child {
                    child = c.next_sibling();
                    todo.push(c);
                }
            }
            shown.sort();
            assert_eq!(
                shown,
                [
                    Some("Delete".to_owned()),
                    Some("i".to_owned()),
                    Some("s".to_owned())
                ]
            );
        });
    }
}
