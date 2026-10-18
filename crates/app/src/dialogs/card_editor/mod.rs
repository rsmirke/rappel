//! The card editor: front and back as Markdown in `GtkSourceView`, with a
//! live preview. Layout in `data/card_editor.blp` (ADR 0013, ADR 0022, ADR
//! 0043).
//!
//! - Ctrl+Enter saves and closes; Ctrl+Shift+Enter saves and starts a new
//!   card in the same deck, with the focus in the front field. Both are
//!   caught in the capture phase, before the text views see the Enter.
//! - Ctrl+B / Ctrl+I wrap the selection of the focused side in bold /
//!   italic markers ([`format::wrap`]), as one undoable step.
//! - The preview renders both sides as the review page does, once typing
//!   pauses ([`PREVIEW_DELAY`]), and only while it is shown: beside the text
//!   when the dialog is wide, instead of it when the Edit / Preview toggle
//!   of a narrow dialog says so ([`layout::panes`]).
//! - The highlighting follows the light or dark style.
//! - A blank front disables saving, and so does a side over the store's
//!   length limit, explained under that side (the store's own rules).
//! - Closing with unsaved changes asks first, whether by Escape, Cancel or
//!   closing the window (`RappelWindow` calls [`CardEditor::confirm_discard`]).
//! - A store error keeps the dialog open with the text as typed.
//! - The Tags field under Back is a [`TagEntry`](crate::widgets::TagEntry): pills, and the name being
//!   typed. The card and its tags are saved in one write. An invalid name
//!   shows an error under the field and disables saving. Ctrl+Shift+Enter
//!   keeps the tags for the next card (ADR 0031).

mod format;
mod layout;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use rappel_core::content;
use rappel_core::domain::{Card, CardSide, MAX_CARD_SIDE_BYTES, TaggedCard, card_side_fits};
use sourceview5::prelude::*;

use crate::render::{back_shown, clear, fill, shown_part};
use crate::state::AppState;
use crate::ui::callback::Callback;
use crate::ui::text::group_digits;
use crate::ui::toast::{info_toast, show_error};
use crate::ui::{breakpoint, dialog, init_once};
use crate::widgets::tag_entry::field::{self as tags, TagsCheck};

use self::layout::Mode;

/// How long typing must pause before the preview is rendered again. Short
/// enough to feel live, long enough that a 1 MiB card is not parsed and
/// rendered on every key.
const PREVIEW_DELAY: Duration = Duration::from_millis(250);

/// The save keys, and whether each starts a new card after saving. The
/// Keyboard Shortcuts dialog lists them (a test checks it).
pub(crate) const SAVE_KEYS: [(&str, bool); 4] = [
    ("<Control>Return", false),
    ("<Control>KP_Enter", false),
    ("<Control><Shift>Return", true),
    ("<Control><Shift>KP_Enter", true),
];

/// The formatting keys and the marker each wraps the selection in.
pub(crate) const FORMAT_KEYS: [(&str, &str); 2] =
    [("<Control>b", format::BOLD), ("<Control>i", format::ITALIC)];

/// What a save did, for the deck page to update its list.
#[derive(Debug, Clone)]
pub enum Saved {
    Added(TaggedCard),
    Edited(TaggedCard),
}

type OnSaved = dyn Fn(Saved);

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use super::OnSaved;
    use crate::state::AppState;
    use crate::ui::callback::Callback;
    use crate::ui::debounce::Debounce;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/card_editor.ui")]
    pub struct CardEditor {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub front_view: TemplateChild<sourceview5::View>,
        #[template_child]
        pub back_view: TemplateChild<sourceview5::View>,
        #[template_child]
        pub front_buffer: TemplateChild<sourceview5::Buffer>,
        #[template_child]
        pub back_buffer: TemplateChild<sourceview5::Buffer>,
        #[template_child]
        pub front_error: TemplateChild<gtk::Label>,
        #[template_child]
        pub back_error: TemplateChild<gtk::Label>,
        #[template_child]
        pub tag_entry: TemplateChild<crate::widgets::TagEntry>,
        #[template_child]
        pub breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub edit_pane: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub pane_separator: TemplateChild<gtk::Separator>,
        #[template_child]
        pub preview_pane: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub mode_toggles: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub mode_bar: TemplateChild<gtk::ActionBar>,
        #[template_child]
        pub preview_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub preview_front: TemplateChild<gtk::Box>,
        #[template_child]
        pub preview_back_separator: TemplateChild<gtk::Separator>,
        #[template_child]
        pub preview_back: TemplateChild<gtk::Box>,
        /// The preview refresh waiting for typing to pause.
        pub preview: Debounce,
        /// Our `notify::dark` handler on the style manager, a singleton
        /// that outlives the editor: disconnected on dispose.
        pub dark_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub state: OnceCell<AppState>,
        pub deck_id: OnceCell<String>,
        /// The card being edited; `None` for a new card.
        pub card_id: RefCell<Option<String>>,
        /// Front and back as last loaded or saved, to tell unsaved changes.
        pub saved_text: RefCell<(String, String)>,
        /// The tags as last loaded or saved, as [`super::tags::tag_keys`].
        pub saved_tag_keys: RefCell<Vec<String>>,
        /// Whether the card's tags could be read. If not, the field is
        /// disabled and a save leaves the tags as they are.
        pub tags_loaded: Cell<bool>,
        /// The breakpoint applies: one pane at a time.
        pub narrow: Cell<bool>,
        pub on_saved: OnceCell<Callback<OnSaved>>,
        /// The "Discard Changes?" question is showing; it is not asked twice.
        pub asking_discard: Cell<bool>,
        /// Discard was chosen; the editor is closing without saving.
        pub discarded: Cell<bool>,
    }

    /// Written by hand to leave out the text fields: card content is never
    /// logged above debug level (SPEC §2), and `{:?}` would print it.
    impl std::fmt::Debug for CardEditor {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CardEditor")
                .field("deck_id", &self.deck_id)
                .field("card_id", &self.card_id)
                .finish_non_exhaustive()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CardEditor {
        const NAME: &'static str = "RappelCardEditor";
        type Type = super::CardEditor;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            // The template names GtkSource types: they must be registered
            // before it is parsed.
            sourceview5::View::ensure_type();
            sourceview5::Buffer::ensure_type();
            crate::widgets::TagEntry::ensure_type();
            klass.bind_template();
            klass.install_action("editor.save", None, |editor, _, _| {
                editor.save(false);
            });
            klass.install_action("editor.save-and-new", None, |editor, _, _| {
                editor.save(true);
            });
            klass.install_action("editor.cancel", None, |editor, _, _| {
                editor.close();
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CardEditor {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            self.preview.cancel();
            if let Some(handler) = self.dark_handler.take() {
                adw::StyleManager::default().disconnect(handler);
            }
        }
    }
    impl WidgetImpl for CardEditor {}
    impl AdwDialogImpl for CardEditor {}
}

glib::wrapper! {
    pub struct CardEditor(ObjectSubclass<imp::CardEditor>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl CardEditor {
    /// An editor for a new card in `deck_id` (`card` is `None`) or for
    /// `card`. `on_saved` is called after each successful save.
    fn new(
        state: AppState,
        deck_id: &str,
        card: Option<&Card>,
        on_saved: impl Fn(Saved) + 'static,
    ) -> Self {
        let editor: Self = glib::Object::new();
        let imp = editor.imp();
        init_once(&imp.state, state);
        init_once(&imp.deck_id, deck_id.to_owned());
        init_once(&imp.on_saved, Callback::new(Rc::new(on_saved)));
        editor.reload_all_tags();
        if let Some(card) = card {
            let tags = editor.read_card_tags(&card.id);
            editor.load(
                Some(&card.id),
                &card.front_md,
                &card.back_md,
                tags.as_deref(),
            );
        } else {
            editor.load(None, "", "", Some(&[]));
        }
        editor
    }

    /// Opens an editor (see [`CardEditor::new`]) over `parent`, unless
    /// `editing` says one is open already. `editing` is set while it is
    /// open; once it closes, `editing` is cleared and `on_closed` is called,
    /// for the caller to set the focus.
    pub fn open(
        parent: &impl IsA<gtk::Widget>,
        editing: &Rc<Cell<bool>>,
        state: AppState,
        deck_id: &str,
        card: Option<&Card>,
        on_saved: impl Fn(Saved) + 'static,
        on_closed: impl Fn() + 'static,
    ) {
        if editing.replace(true) {
            return;
        }
        let editor = Self::new(state, deck_id, card, on_saved);
        let open = Rc::clone(editing);
        editor.connect_closed(move |_| {
            open.set(false);
            on_closed();
        });
        editor.show(parent);
    }

    /// Shows the editor over `parent`.
    fn show(&self, parent: &impl IsA<gtk::Widget>) {
        self.present(Some(parent));
        self.set_focus(Some(&*self.imp().front_view));
    }

    fn setup(&self) {
        self.setup_shortcuts();
        self.setup_views();
        self.setup_tags();
        self.setup_layout();
        self.connect_close_attempt(Self::ask_discard);
    }

    /// Ctrl+Enter, Ctrl+Shift+Enter, Ctrl+B and Ctrl+I.
    fn setup_shortcuts(&self) {
        // Capture phase: the text views would otherwise take the Enter.
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
        for (trigger, and_new) in SAVE_KEYS {
            // Always handled, even with a blank front, so the key never
            // falls through to the text view as a new line.
            let action = gtk::CallbackAction::new(move |widget, _| {
                if let Some(editor) = widget.downcast_ref::<Self>() {
                    editor.save(and_new);
                }
                glib::Propagation::Stop
            });
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(action),
            ));
        }
        for (trigger, marker) in FORMAT_KEYS {
            let action = gtk::CallbackAction::new(move |widget, _| {
                let editor = widget.downcast_ref::<Self>();
                match editor.and_then(Self::focused_buffer) {
                    Some(buffer) => {
                        wrap_selection(&buffer, marker);
                        glib::Propagation::Stop
                    }
                    // Not in a text field: the key is not ours.
                    None => glib::Propagation::Proceed,
                }
            });
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(action),
            ));
        }
        self.add_controller(shortcuts);
    }

    /// Highlighting and the colour scheme of both sides, and what a change
    /// of their text updates.
    fn setup_views(&self) {
        let imp = self.imp();
        let language = sourceview5::LanguageManager::default().language("markdown");
        if language.is_none() {
            tracing::warn!("GtkSourceView has no markdown language; no highlighting");
        }
        for buffer in [&*imp.front_buffer, &*imp.back_buffer] {
            buffer.set_language(language.as_ref());
            buffer.set_highlight_syntax(true);
            buffer.set_highlight_matching_brackets(false);
            buffer.connect_changed(glib::clone!(
                #[weak(rename_to = editor)]
                self,
                move |_| {
                    editor.update_actions();
                    editor.schedule_preview();
                }
            ));
        }

        let style = adw::StyleManager::default();
        self.apply_scheme(style.is_dark());
        let handler = style.connect_dark_notify(glib::clone!(
            #[weak(rename_to = editor)]
            self,
            move |style| editor.apply_scheme(style.is_dark())
        ));
        imp.dark_handler.replace(Some(handler));
    }

    /// Changes of the tags field count as edits.
    fn setup_tags(&self) {
        self.imp().tag_entry.set_on_changed(glib::clone!(
            #[weak(rename_to = editor)]
            self,
            move || editor.update_actions()
        ));
    }

    /// Reads the tags in use again, for the suggestions: a tag no live card
    /// has is not offered (ADR 0052). A failure only means no suggestions.
    fn reload_all_tags(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        let all = state.used_tags().unwrap_or_else(|err| {
            tracing::warn!("the tags could not be read for suggestions: {err}");
            Vec::new()
        });
        imp.tag_entry.set_all_tags(all);
    }

    /// The tags of card `card_id`, or `None` if they cannot be read: the
    /// field is then disabled and says why, and a save leaves the card's
    /// tags alone.
    fn read_card_tags(&self, card_id: &str) -> Option<Vec<String>> {
        let imp = self.imp();
        let read = match imp.state.get() {
            Some(state) => state.card_tags(card_id).map_err(|err| err.to_string()),
            // Cannot happen: `new` sets the state before reading. Treated
            // like a failed read, so a save never wipes the tags.
            None => Err("the editor has no data".to_owned()),
        };
        match read {
            Ok(card_tags) => Some(card_tags.into_iter().map(|tag| tag.name).collect()),
            Err(err) => {
                imp.tag_entry
                    .set_unavailable("The tags could not be read, so they are left as they are.");
                let message = format!("The card's tags could not be read: {err}");
                show_error(&*imp.toast_overlay, "reading a card's tags", &message);
                None
            }
        }
    }

    /// The field's names, or what is wrong with them. A disabled field
    /// (tags not loaded) has nothing to check.
    fn tags_check(&self) -> TagsCheck {
        let imp = self.imp();
        if imp.tags_loaded.get() {
            imp.tag_entry.check()
        } else {
            TagsCheck {
                names: Vec::new(),
                error: None,
            }
        }
    }

    /// The breakpoint and the Edit / Preview toggle decide which panes
    /// show. The breakpoint's `apply` and `unapply` run after its setters
    /// (`AdwBreakpoint::apply` docs).
    fn setup_layout(&self) {
        let imp = self.imp();
        breakpoint::follow(
            &imp.breakpoint,
            glib::clone!(
                #[weak(rename_to = editor)]
                self,
                move |narrow| editor.set_narrow(narrow)
            ),
        );
        imp.mode_toggles.connect_active_name_notify(glib::clone!(
            #[weak(rename_to = editor)]
            self,
            move |_| editor.mode_changed()
        ));
        self.apply_layout();
    }

    fn mode(&self) -> Mode {
        Mode::from_name(self.imp().mode_toggles.active_name().as_deref())
    }

    fn set_narrow(&self, narrow: bool) {
        self.imp().narrow.set(narrow);
        self.apply_layout_keeping_focus();
    }

    /// Shows the panes `layout::panes` picks, and renders the preview if it
    /// is one of them (or frees it if not).
    fn apply_layout(&self) {
        let imp = self.imp();
        let panes = layout::panes(imp.narrow.get(), self.mode());
        imp.edit_pane.set_visible(panes.edit);
        imp.preview_pane.set_visible(panes.preview);
        imp.pane_separator.set_visible(panes.separator);
        self.refresh_preview();
    }

    /// The toggle changed: show the other pane.
    fn mode_changed(&self) {
        self.apply_layout_keeping_focus();
    }

    /// Applies the layout, then moves the focus if it would be left where
    /// keys go nowhere (`layout::focus_after`). Where the focus is, is read
    /// before the panes change. On a breakpoint change the setters have
    /// already shown or hidden the toggle's bar (`AdwBreakpoint::apply`
    /// runs after them), so a focus on the hidden toggle reads as on the
    /// toggle with no bar, or as lost if GTK has moved it already.
    fn apply_layout_keeping_focus(&self) {
        let imp = self.imp();
        let place = self.focus_place();
        self.apply_layout();
        let panes = layout::panes(imp.narrow.get(), self.mode());
        match layout::focus_after(place, panes, imp.mode_bar.is_visible()) {
            Some(layout::Target::Front) => {
                imp.front_view.grab_focus();
            }
            Some(layout::Target::Toggle) => {
                imp.mode_toggles.child_focus(gtk::DirectionType::TabForward);
            }
            None => {}
        }
    }

    fn focus_place(&self) -> layout::Place {
        let imp = self.imp();
        match self.focus() {
            None => layout::Place::Lost,
            Some(focus) if !focus.is_mapped() || !focus.is_focusable() => layout::Place::Lost,
            Some(focus) if focus.is_ancestor(&*imp.edit_pane) => layout::Place::Edit,
            Some(focus) if focus.is_ancestor(&*imp.mode_bar) => layout::Place::Toggle,
            Some(_) => layout::Place::Other,
        }
    }

    /// Whether the preview is on screen.
    fn preview_shown(&self) -> bool {
        layout::panes(self.imp().narrow.get(), self.mode()).preview
    }

    /// The buffer of the side that has the keyboard focus, if any.
    fn focused_buffer(&self) -> Option<sourceview5::Buffer> {
        let imp = self.imp();
        let focus = self.focus()?;
        [
            (&*imp.front_view, &*imp.front_buffer),
            (&*imp.back_view, &*imp.back_buffer),
        ]
        .into_iter()
        .find(|(view, _)| focus == *view.upcast_ref::<gtk::Widget>())
        .map(|(_, buffer)| buffer.clone())
    }

    /// Highlights with the Adwaita scheme matching the style.
    fn apply_scheme(&self, dark: bool) {
        let id = scheme_id(dark);
        let scheme = sourceview5::StyleSchemeManager::default().scheme(id);
        if scheme.is_none() {
            tracing::warn!("GtkSourceView has no style scheme {id}");
        }
        let imp = self.imp();
        for buffer in [&*imp.front_buffer, &*imp.back_buffer] {
            buffer.set_style_scheme(scheme.as_ref());
        }
    }

    /// Renders the preview again once typing pauses.
    fn schedule_preview(&self) {
        if !self.preview_shown() {
            return;
        }
        self.imp().preview.restart(
            PREVIEW_DELAY,
            glib::clone!(
                #[weak(rename_to = editor)]
                self,
                move || editor.refresh_preview()
            ),
        );
    }

    /// Renders the preview now, or empties it while it is hidden (so a
    /// long card's widgets are not kept for nothing).
    fn refresh_preview(&self) {
        let imp = self.imp();
        imp.preview.cancel();
        if !self.preview_shown() {
            clear(&imp.preview_front);
            clear(&imp.preview_back);
            return;
        }
        let started = Instant::now();
        let (front_md, back_md) = self.texts();
        let (front_part, front_cut) = shown_part(&front_md);
        let (back_part, back_cut) = shown_part(&back_md);
        fill(&imp.preview_front, &content::parse(front_part));
        let back = content::parse(back_part);
        let shown = back_shown(&back);
        if shown {
            fill(&imp.preview_back, &back);
        } else {
            clear(&imp.preview_back);
        }
        imp.preview_back_separator.set_visible(shown);
        imp.preview_back.set_visible(shown);
        imp.preview_group.set_description(
            (front_cut || back_cut).then_some("This card is long: only its start is previewed."),
        );
        tracing::debug!(
            bytes = front_md.len() + back_md.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "card editor preview rendered"
        );
    }

    /// Shows `front` and `back` as the saved state of card `card_id`, with
    /// `tags`, which count as saved too; `None` when they could not be read
    /// (the field is disabled already, `read_card_tags`).
    fn load(&self, card_id: Option<&str>, front: &str, back: &str, tags: Option<&[String]>) {
        let imp = self.imp();
        self.set_title(if card_id.is_some() {
            "Edit Card"
        } else {
            "New Card"
        });
        imp.card_id.replace(card_id.map(str::to_owned));
        imp.saved_text.replace((front.to_owned(), back.to_owned()));
        // Irreversible: Ctrl+Z must not take a loaded card's text away, or
        // bring the previous card back after Ctrl+Shift+Enter.
        for (buffer, text) in [(&*imp.front_buffer, front), (&*imp.back_buffer, back)] {
            buffer.begin_irreversible_action();
            buffer.set_text(text);
            buffer.end_irreversible_action();
        }
        imp.tags_loaded.set(tags.is_some());
        imp.saved_tag_keys
            .replace(tags::tag_keys(tags.unwrap_or_default()));
        if let Some(tags) = tags {
            imp.tag_entry.set_tags(tags);
        }
        self.update_actions();
        self.refresh_preview();
    }

    fn texts(&self) -> (String, String) {
        let text = |buffer: &sourceview5::Buffer| {
            let (start, end) = buffer.bounds();
            buffer.text(&start, &end, false).to_string()
        };
        let imp = self.imp();
        (text(&imp.front_buffer), text(&imp.back_buffer))
    }

    fn update_actions(&self) {
        let imp = self.imp();
        let texts = self.texts();
        let check = check_card(&texts.0, &texts.1);
        let tags = self.tags_check();
        let can_save = check.can_save && tags.error.is_none();
        self.action_set_enabled("editor.save", can_save);
        self.action_set_enabled("editor.save-and-new", can_save);
        show_field_error(&*imp.front_view, &imp.front_error, check.front_error);
        show_field_error(&*imp.back_view, &imp.back_error, check.back_error);
        // The tags field shows its own error.
        // With unsaved changes, closing emits `close-attempt` instead. A
        // tags field that holds an error is a change.
        let tags_same = !imp.tags_loaded.get()
            || (tags.error.is_none()
                && tags::tag_keys(&tags.names) == *imp.saved_tag_keys.borrow());
        self.set_can_close(texts == *imp.saved_text.borrow() && tags_same);
    }

    fn save(&self, and_new: bool) {
        let imp = self.imp();
        let (front, back) = self.texts();
        let tags = self.tags_check();
        if !check_card(&front, &back).can_save || tags.error.is_some() {
            return;
        }
        let (Some(state), Some(deck_id)) = (imp.state.get(), imp.deck_id.get()) else {
            return;
        };
        let card_id = imp.card_id.borrow().clone();
        // `None` leaves the tags alone: the field could not be loaded.
        let names = imp.tags_loaded.get().then_some(tags.names.as_slice());
        let tags_same =
            names.is_none_or(|names| tags::tag_keys(names) == *imp.saved_tag_keys.borrow());
        // An unchanged card is not written (that would bump `updated_at`).
        if card_id.is_some()
            && tags_same
            && (front.clone(), back.clone()) == *imp.saved_text.borrow()
        {
            self.after_save(and_new, &tags.names);
            return;
        }
        let result = match &card_id {
            Some(id) => state
                .edit_card_with_tags(id, &front, &back, names)
                .map(Saved::Edited),
            None => state
                .add_card_with_tags(deck_id, &front, &back, &tags.names)
                .map(Saved::Added),
        };
        let saved = match result {
            Ok(saved) => saved,
            Err(err) => {
                // The editor's own overlay: the dialog covers the page's.
                let message = format!("The card could not be saved: {err}");
                show_error(&*imp.toast_overlay, "a card save", &message);
                return;
            }
        };
        let (toast_text, kept) = match &saved {
            Saved::Added(tagged) => ("Card added", tagged.tags.clone()),
            Saved::Edited(tagged) => ("Card saved", tagged.tags.clone()),
        };
        if let Some(on_saved) = imp.on_saved.get().map(Callback::get) {
            on_saved(saved);
        }
        // New tags can be suggested from now on.
        self.reload_all_tags();
        let kept: Vec<String> = kept.into_iter().map(|tag| tag.name).collect();
        self.after_save(and_new, &kept);
        if and_new {
            imp.toast_overlay.add_toast(info_toast(toast_text));
        }
    }

    /// After a save (or nothing to save): an empty editor for the next card
    /// that starts with the same tags (`kept`), or close.
    fn after_save(&self, and_new: bool, kept: &[String]) {
        if and_new {
            let imp = self.imp();
            // The next card starts in the text, also on a narrow dialog
            // showing the preview.
            imp.mode_toggles.set_active_name(Some(Mode::EDIT));
            imp.front_view.grab_focus();
            self.load(None, "", "", Some(kept));
            imp.front_view.grab_focus();
        } else {
            self.force_close();
        }
    }

    fn ask_discard(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = editor)]
            self,
            async move {
                if editor.confirm_discard().await {
                    editor.discard();
                }
            }
        ));
    }

    /// Whether the editor holds text that is not saved and not discarded.
    pub fn has_unsaved_changes(&self) -> bool {
        !self.imp().discarded.get() && !self.can_close()
    }

    /// Closes without saving, after the user chose Discard.
    pub fn discard(&self) {
        self.imp().discarded.set(true);
        self.force_close();
    }

    /// Asks "Discard Changes?" and returns whether the user chose Discard.
    /// Returns `false` at once if the question is already showing, so the
    /// caller that asked first decides.
    pub async fn confirm_discard(&self) -> bool {
        let asking = &self.imp().asking_discard;
        if asking.replace(true) {
            return false;
        }
        let discard = match dialog::DISCARD_CHANGES.load() {
            Ok(dialog) => dialog.choose_future(Some(self)).await == "discard",
            Err(_) => false,
        };
        asking.set(false);
        discard
    }
}

/// What the editor shows for the texts typed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CardCheck {
    /// Whether Save is enabled.
    can_save: bool,
    /// The error under the front, if any.
    front_error: Option<String>,
    /// The error under the back, if any.
    back_error: Option<String>,
}

/// The store's rules, checked as the user types: a blank front cannot be
/// saved (no error is shown, the empty field says it), nor can a side over
/// the length limit, which is explained under it.
fn check_card(front: &str, back: &str) -> CardCheck {
    let too_long = |text: &str, side| (!card_side_fits(text)).then(|| too_long_text(side));
    let front_error = too_long(front, CardSide::Front);
    let back_error = too_long(back, CardSide::Back);
    CardCheck {
        can_save: !front.trim().is_empty() && front_error.is_none() && back_error.is_none(),
        front_error,
        back_error,
    }
}

fn too_long_text(side: CardSide) -> String {
    format!(
        "The {side} is too long (the limit is {} bytes)",
        group_digits(MAX_CARD_SIDE_BYTES)
    )
}

/// Shows `error` under `view`, announcing it when it appears or changes.
fn show_field_error(view: &impl IsA<gtk::Widget>, label: &gtk::Label, error: Option<String>) {
    match error {
        Some(message) => {
            if !label.is_visible() || label.label() != message {
                view.upcast_ref::<gtk::Widget>()
                    .announce(&message, gtk::AccessibleAnnouncementPriority::Medium);
            }
            label.set_label(&message);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

/// The `GtkSourceView` style scheme for the light or dark style. Both come
/// with `GtkSourceView` itself, built in as resources.
fn scheme_id(dark: bool) -> &'static str {
    if dark { "Adwaita-dark" } else { "Adwaita" }
}

/// Wraps the selection in `buffer` (or the cursor's empty spot) in
/// `marker`, as one user action so one Ctrl+Z takes it back.
fn wrap_selection(buffer: &sourceview5::Buffer, marker: &str) {
    let (mut start, mut end) = buffer.selection_bounds().unwrap_or_else(|| {
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        (cursor, cursor)
    });
    // `slice` with hidden chars, not `text`: it keeps the U+FFFC of any
    // embedded object, so its characters match the iterators' offsets.
    let selected = buffer.slice(&start, &end, true);
    let wrapped = format::wrap(&selected, marker);
    let offset = start.offset();
    let at = |chars: usize| {
        buffer.iter_at_offset(offset.saturating_add(i32::try_from(chars).unwrap_or(i32::MAX)))
    };
    buffer.begin_user_action();
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &wrapped.text);
    // The cursor (insert mark) at the end, so Shift+Arrow extends from there.
    buffer.select_range(&at(wrapped.select.end), &at(wrapped.select.start));
    buffer.end_user_action();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scheme_follows_the_style() {
        assert_eq!(scheme_id(false), "Adwaita");
        assert_eq!(scheme_id(true), "Adwaita-dark");
    }

    #[test]
    fn a_blank_front_cannot_be_saved() {
        for front in ["", " \n\t "] {
            let check = check_card(front, "back");
            assert!(!check.can_save);
            assert_eq!((check.front_error, check.back_error), (None, None));
        }
        assert!(check_card(" x ", "").can_save);
    }

    #[test]
    fn sides_at_the_limit_can_be_saved() {
        let longest = "ü".repeat(MAX_CARD_SIDE_BYTES / 2);
        let check = check_card(&longest, &longest);
        assert!(check.can_save);
        assert_eq!((check.front_error, check.back_error), (None, None));
    }

    #[test]
    fn a_side_over_the_limit_is_explained_under_it() {
        let over = "x".repeat(MAX_CARD_SIDE_BYTES + 1);
        let message = |side| {
            Some(format!(
                "The {side} is too long (the limit is 1,048,576 bytes)"
            ))
        };

        let check = check_card(&over, "back");
        assert!(!check.can_save);
        assert_eq!(check.front_error, message("front"));
        assert_eq!(check.back_error, None);

        let check = check_card("front", &over);
        assert!(!check.can_save);
        assert_eq!(check.front_error, None);
        assert_eq!(check.back_error, message("back"));

        let check = check_card(&over, &over);
        assert_eq!(
            (check.front_error, check.back_error),
            (message("front"), message("back"))
        );
    }

    /// A state with one deck, and an editor on a new card in it or on the
    /// card `existing` (front, tags) already in it.
    fn editor_over(
        existing: Option<(&str, &[&str])>,
    ) -> (
        tempfile::TempDir,
        AppState,
        String,
        CardEditor,
        Rc<std::cell::RefCell<Vec<Saved>>>,
    ) {
        let (tmp, state) = crate::testing::temporary_state();
        let deck = state.create_deck("Deck").unwrap();
        let card = existing.map(|(front, tags)| {
            let names: Vec<String> = tags.iter().map(|t| (*t).to_owned()).collect();
            state
                .add_card_with_tags(&deck.id, front, "", &names)
                .unwrap()
                .card
        });
        let saved = Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = Rc::clone(&saved);
        let editor = CardEditor::new(state.clone(), &deck.id, card.as_ref(), move |s| {
            sink.borrow_mut().push(s);
        });
        (tmp, state, deck.id, editor, saved)
    }

    fn type_into(editor: &CardEditor, front: &str, tags: &str) {
        editor.imp().front_buffer.set_text(front);
        editor.imp().tag_entry.type_text(tags);
    }

    #[test]
    fn a_card_is_saved_with_its_tags_and_the_next_one_keeps_them() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, deck, editor, saved) = editor_over(None);
            // Two names made pills by their commas, and spaces typed after.
            type_into(&editor, "2 + 2", " maths ,Alg\u{E8}bre, ");
            editor.save(true);

            let cards = state.list_tagged_cards(&deck).unwrap();
            assert_eq!(cards.len(), 1);
            let names: Vec<&str> = cards[0].tags.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["Alg\u{E8}bre", "maths"]);
            assert!(matches!(saved.borrow()[0], Saved::Added(_)));
            // The next card starts with the same tags, as saved state: no
            // "Discard Changes?" for an untouched editor.
            let imp = editor.imp();
            assert_eq!(imp.tag_entry.pill_names(), ["Alg\u{E8}bre", "maths"]);
            assert_eq!(imp.tag_entry.typed(), "");
            assert_eq!(imp.front_buffer.char_count(), 0);
            assert!(editor.can_close());
            // A name typed, not yet a pill, is a change; the front still
            // blocks saving.
            imp.tag_entry.type_text("new");
            assert!(!editor.can_close());
            editor.save(false);
            assert_eq!(state.list_tagged_cards(&deck).unwrap().len(), 1);
            // The new tags are suggested from now on: without the pill,
            // "mat" finds "maths".
            imp.tag_entry.set_tags(&[]);
            imp.tag_entry.type_text("mat");
            let found: Vec<String> = imp
                .tag_entry
                .suggest_for_test()
                .into_iter()
                .map(|t| t.name)
                .collect();
            assert_eq!(found, ["maths"]);
        });
    }

    #[test]
    fn a_name_typed_without_a_comma_is_saved_too() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, deck, editor, _saved) = editor_over(None);
            type_into(&editor, "front", "maths, geo");
            editor.save(false);
            let cards = state.list_tagged_cards(&deck).unwrap();
            let names: Vec<&str> = cards[0].tags.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["geo", "maths"]);
        });
    }

    #[test]
    fn an_invalid_tag_shows_an_error_and_blocks_saving() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, deck, editor, _saved) = editor_over(None);
            let imp = editor.imp();
            type_into(&editor, "front", &"x".repeat(300));
            assert_eq!(
                imp.tag_entry.shown_error().as_deref(),
                Some("This tag is too long (the limit is 256 bytes)")
            );
            // Pressing the key anyway writes nothing.
            editor.save(false);
            assert_eq!(state.list_tagged_cards(&deck).unwrap(), []);
            assert!(!editor.can_close());

            type_into(&editor, "front", "ok");
            assert_eq!(imp.tag_entry.shown_error(), None);
            editor.save(false);
            assert_eq!(state.list_tagged_cards(&deck).unwrap().len(), 1);
        });
    }

    #[test]
    fn editing_a_card_loads_its_tags_and_unlinks_the_ones_removed() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, deck, editor, saved) = editor_over(Some(("front", &["a", "b"])));
            let imp = editor.imp();
            assert_eq!(imp.tag_entry.pill_names(), ["a", "b"]);
            assert!(editor.can_close());
            // Case and order are not changes: "a" removed and typed again.
            imp.tag_entry.remove_for_test(0);
            imp.tag_entry.type_text("A");
            assert!(editor.can_close());

            let before = state.list_tagged_cards(&deck).unwrap().remove(0);
            imp.tag_entry.type_text("");
            assert!(!editor.can_close());
            editor.save(false);
            let after = state.list_tagged_cards(&deck).unwrap().remove(0);
            let names: Vec<&str> = after.tags.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["b"]);
            // Tags only: the card itself is untouched, and the tag stays.
            assert_eq!(after.card, before.card);
            assert_eq!(state.tags().unwrap().len(), 2);
            assert!(matches!(saved.borrow()[0], Saved::Edited(_)));
        });
    }

    #[test]
    fn unreadable_tags_disable_the_field_and_say_why_and_a_save_keeps_them() {
        crate::testing::with_widgets(|| {
            let (_tmp, state, deck, editor, _saved) = editor_over(Some(("front", &["a"])));
            let imp = editor.imp();
            let card = state.list_tagged_cards(&deck).unwrap().remove(0);
            // A card that is gone cannot have its tags read.
            state.delete_card(&card.card.id).unwrap();
            let tags = editor.read_card_tags(&card.card.id);
            assert_eq!(tags, None);
            editor.load(Some(&card.card.id), "front", "", None);
            assert!(!imp.tag_entry.is_available());
            assert!(imp.tag_entry.shown_error().is_some());
            // Edits to the sides do not hide the explanation.
            imp.front_buffer.set_text("front 2");
            assert!(imp.tag_entry.shown_error().is_some());
            assert!(!editor.can_close());
        });
    }

    #[test]
    fn a_suggestion_becomes_a_pill_of_the_card() {
        crate::testing::with_widgets(|| {
            let (_tmp, _state, _deck, editor, _saved) =
                editor_over(Some(("front", &["algebra", "maths"])));
            let imp = editor.imp();
            imp.tag_entry.remove_for_test(0);
            assert!(!editor.can_close());
            imp.tag_entry.type_text("alg");
            assert_eq!(imp.tag_entry.suggest_for_test().len(), 1);
            imp.tag_entry.accept_suggestion(0);
            assert_eq!(imp.tag_entry.pill_names(), ["maths", "algebra"]);
            assert!(editor.can_close());
        });
    }

    #[test]
    fn a_narrow_editor_shows_the_text_or_the_preview() {
        crate::testing::with_widgets(|| {
            let (_tmp, _state, _deck, editor, _saved) = editor_over(None);
            let imp = editor.imp();
            assert!(imp.edit_pane.is_visible() && imp.preview_pane.is_visible());
            editor.set_narrow(true);
            assert!(imp.edit_pane.is_visible() && !imp.preview_pane.is_visible());
            imp.mode_toggles.set_active_name(Some(Mode::PREVIEW));
            assert!(!imp.edit_pane.is_visible() && imp.preview_pane.is_visible());
            // Wide again: both, whatever the toggle says.
            editor.set_narrow(false);
            assert!(imp.edit_pane.is_visible() && imp.preview_pane.is_visible());
            // Save and add another starts the next card in the text.
            editor.set_narrow(true);
            imp.front_buffer.set_text("front");
            editor.save(true);
            assert_eq!(editor.mode(), Mode::Edit);
            assert!(imp.edit_pane.is_visible());
        });
    }

    #[test]
    fn switching_panes_keeps_the_focus_where_keys_reach() {
        use std::time::Duration;
        crate::testing::with_widgets(|| {
            let (_tmp, _state, _deck, editor, _saved) = editor_over(None);
            let imp = editor.imp();
            // Wide enough that the breakpoint stays off by itself.
            let window = adw::Window::builder()
                .default_width(1000)
                .default_height(720)
                .build();
            window.present();
            editor.present(Some(&window));
            assert!(
                crate::testing::settle_until(|| imp.front_view.is_mapped(), Duration::from_secs(5)),
                "the editor did not show"
            );
            editor.set_focus(Some(&*imp.front_view));
            let toggle_focused = || {
                editor
                    .focus()
                    .is_some_and(|f| f.is_ancestor(&*imp.mode_toggles))
            };

            // Narrow, as the breakpoint would make it: its setter shows the
            // bar, then `apply` sets narrow.
            imp.mode_bar.set_visible(true);
            editor.set_narrow(true);
            assert!(crate::testing::settle_until(
                || imp.mode_toggles.is_mapped(),
                Duration::from_secs(5)
            ));
            assert_eq!(editor.focus().as_ref(), Some(imp.front_view.upcast_ref()));
            // To the preview with the focus in the text: on the toggle.
            imp.mode_toggles.set_active_name(Some(Mode::PREVIEW));
            assert!(toggle_focused(), "focus: {:?}", editor.focus());
            // Back to the text: the toggle keeps it (the user is there).
            imp.mode_toggles.set_active_name(Some(Mode::EDIT));
            assert!(toggle_focused());
            // Wide again, the bar hidden by the setter: to the front.
            imp.mode_bar.set_visible(false);
            editor.set_narrow(false);
            assert_eq!(editor.focus().as_ref(), Some(imp.front_view.upcast_ref()));
            editor.force_close();
            window.destroy();
        });
    }

    /// The longest time the main loop is blocked in one step over the next
    /// `settle`, which is what a person feels as a frozen window, as CPU
    /// time of this thread, so a loaded machine does not lengthen it (ADR
    /// 0049). Stops after 200 ms with nothing to do; `settle` only stops a
    /// test that would never settle.
    fn longest_stall(settle: std::time::Duration) -> std::time::Duration {
        use std::time::{Duration, Instant};
        let context = glib::MainContext::default();
        let end = Instant::now() + settle;
        let mut idle_since = None;
        let mut longest = Duration::ZERO;
        while Instant::now() < end {
            let (step, worked) = crate::testing::cpu_time_of(|| context.iteration(false));
            if worked {
                longest = longest.max(step);
                idle_since = None;
            } else {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_millis(200) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        longest
    }

    /// #37, ADR 0025 row 14: a side of the largest size the store accepts,
    /// in one paragraph, in the editor's own text field (not the preview,
    /// which shows 32 KiB). Measured like the review page's labels: the
    /// view is not shown (no window opens during tests), and GTK is asked
    /// for its natural width and then its height at a width, as a window
    /// would (GTK validates the lines later, in idle steps, so the stall
    /// below is the figure that matters). Also measured: loading the text, the main loop's longest stall
    /// while the highlighter works through it, and typing one character in
    /// the middle.
    #[test]
    fn a_huge_one_paragraph_side_is_usable_in_the_text_field() {
        use crate::testing::{cpu_time_of, cpu_within};
        use std::time::Duration;
        let sides = [
            ("words", "word ".repeat(MAX_CARD_SIDE_BYTES / 5)),
            ("no spaces", "x".repeat(MAX_CARD_SIDE_BYTES)),
        ];
        let results = crate::testing::with_widgets(move || {
            sides.map(|(what, side)| {
                let buffer = sourceview5::Buffer::new(None);
                buffer.set_language(
                    sourceview5::LanguageManager::default()
                        .language("markdown")
                        .as_ref(),
                );
                buffer.set_highlight_syntax(true);
                let view = sourceview5::View::with_buffer(&buffer);
                view.set_monospace(true);
                view.set_wrap_mode(gtk::WrapMode::WordChar);

                // CPU time of this thread, where GTK does the work, so a
                // loaded machine does not fail the bounds (ADR 0049).
                let (load, ()) = cpu_time_of(|| buffer.set_text(&side));
                let (first, ()) = cpu_time_of(|| {
                    let _ = view.measure(gtk::Orientation::Horizontal, -1);
                    let _ = view.measure(gtk::Orientation::Vertical, 600);
                });
                let stall = longest_stall(Duration::from_secs(20));

                let middle = buffer.iter_at_offset(buffer.char_count() / 2);
                buffer.place_cursor(&middle);
                let (typed, ()) = cpu_time_of(|| {
                    buffer.insert_at_cursor("y");
                    let _ = view.measure(gtk::Orientation::Vertical, 600);
                });
                let stall_after = longest_stall(Duration::from_secs(20));
                // About 0.16 s and 3 ms here (release and debug alike: the
                // time is in GTK); the bounds leave ten times that for slow
                // machines. Checked on this thread, which did the work.
                let verdict = cpu_within(load, Duration::from_secs(1))
                    .and(cpu_within(first, Duration::from_secs(2)))
                    .and(cpu_within(stall, Duration::from_secs(2)))
                    .and(cpu_within(typed, Duration::from_secs(1)))
                    .and(cpu_within(stall_after, Duration::from_secs(2)));
                (what, load, first, stall, typed, stall_after, verdict)
            })
        });
        for (what, load, first, stall, typed, stall_after, verdict) in results {
            let print =
                |step: &str, t| crate::testing::print_timing(&format!("1 MiB {what}: {step}"), t);
            print("load", load);
            print("first layout", first);
            print("longest main-loop step while highlighting", stall);
            print("type one character and lay out", typed);
            print("longest main-loop step after typing", stall_after);
            if let Err(message) = verdict {
                panic!(
                    "{what}: load {load:?}, layout {first:?}, stall {stall:?}, \
                     typing {typed:?}, stall after typing {stall_after:?}: {message}"
                );
            }
        }
    }
}
