//! `TagEntry`: a field of tags, as removable pills followed by the text for
//! typing the next one (ADR 0043, after ADR 0031). Layout in
//! `data/tag_entry.blp`. Used by the card editor; made to be used wherever
//! tags are typed. What the keys, the text and the focus do is decided by
//! the pure functions in `field.rs` and `keys.rs`.
//!
//! - A comma, or Enter, turns the typed name into a pill (`split_typed`).
//!   The comma is acted on just after the text changes, so typing, pasting
//!   and an on-screen keyboard all work the same.
//! - Backspace in the empty text moves the focus to the last pill's remove
//!   button; Backspace or Delete there removes the pill (`keys::key_action`,
//!   `keys::after_removal`).
//! - Tab goes from the text to the pills' remove buttons, then out of the
//!   field (`keys::tab_stop`), though the text is drawn after the pills.
//! - The name being typed counts as a tag (`field::check`), so a save does
//!   not lose it, and it becomes a pill when the focus leaves the field.
//!
//! `GtkEntryCompletion` is deprecated since GTK 4.10, so the suggestions are
//! a [`TagSuggestions`] popover under the frame. The text keeps the keyboard
//! focus throughout: the popover is not `autohide` (which GTK documents as
//! the setting that makes a popover modal, grabbing the keyboard focus and
//! closing on Esc), and its rows are not focusable. So the text's own key
//! controller moves the highlight, accepts and closes (Tab, Enter, Escape).
//! That controller runs in the capture phase, which GTK delivers before the
//! bubble phase (`GtkPropagationPhase`), so Escape reaches it before the
//! dialog's own Escape handling: with suggestions showing, Escape closes only
//! them, and the dialog is left for the next Escape.

pub mod field;
pub mod keys;

use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use rappel_core::domain::Tag;

use self::field::TagsCheck;
use self::keys::{EntryKey, EntryState, KeyAction, Move};
use super::TagSuggestions;
use crate::ui::callback::Callback;
use crate::ui::text::one_line;

/// Called when the tags or the typed text change.
type OnChanged = dyn Fn();

/// The keys the text of a tag entry looks at, without Ctrl, Alt or Shift;
/// `keys::key_action` decides what each does. The Keyboard Shortcuts
/// dialog lists them (a test checks it).
pub(crate) const KEYS: [(gdk::Key, EntryKey); 10] = [
    (gdk::Key::Down, EntryKey::Down),
    (gdk::Key::KP_Down, EntryKey::Down),
    (gdk::Key::Up, EntryKey::Up),
    (gdk::Key::KP_Up, EntryKey::Up),
    (gdk::Key::Return, EntryKey::Enter),
    (gdk::Key::KP_Enter, EntryKey::Enter),
    (gdk::Key::ISO_Enter, EntryKey::Enter),
    (gdk::Key::Tab, EntryKey::Tab),
    (gdk::Key::Escape, EntryKey::Escape),
    (gdk::Key::BackSpace, EntryKey::Backspace),
];

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;
    use rappel_core::domain::Tag;

    use super::OnChanged;
    use crate::ui::callback::Callback;
    use crate::widgets::{TagPills, TagSuggestions};

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/tag_entry.ui")]
    pub struct TagEntry {
        #[template_child]
        pub frame: TemplateChild<gtk::Frame>,
        #[template_child]
        pub pills: TemplateChild<TagPills>,
        #[template_child]
        pub error: TemplateChild<gtk::Label>,
        /// Where the next name is typed, after the pills.
        pub text: OnceCell<gtk::Text>,
        /// The tags shown as pills, as named (the pills show them on one
        /// line each).
        pub names: RefCell<Vec<String>>,
        /// Every tag, for the suggestions.
        pub all_tags: RefCell<Vec<Tag>>,
        /// The suggestions under the field, parented to the frame.
        pub suggestions: OnceCell<TagSuggestions>,
        /// The tags the suggestions list shows now, and the highlighted one.
        pub suggested: RefCell<Vec<Tag>>,
        pub highlighted: Cell<Option<usize>>,
        /// The widget is putting text in the field itself (loaded tags, a
        /// name made a pill): that is not typing.
        pub filling: Cell<bool>,
        /// The text changed and its cursor has not been seen moving since.
        pub edited: Cell<bool>,
        /// A comma was typed and the split is waiting to run.
        pub split_queued: Cell<bool>,
        /// The tags could not be read: the field is disabled and says so.
        pub unavailable: Cell<bool>,
        pub on_changed: RefCell<Option<Callback<OnChanged>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TagEntry {
        const NAME: &'static str = "RappelTagEntry";
        type Type = super::TagEntry;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            TagPills::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TagEntry {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            // A popover parented to a widget must be unparented before that
            // widget is finalised.
            if let Some(suggestions) = self.suggestions.get() {
                suggestions.unparent();
            }
        }
    }

    impl WidgetImpl for TagEntry {
        /// Tab and Shift+Tab: the text first, then the pills' remove
        /// buttons, then out (`keys::tab_stop`). GTK moves the focus by
        /// calling `focus` down the widgets on the focus path, and a widget
        /// returning false lets its parent go on to the next one
        /// (`gtk_widget_child_focus`). Other directions are the box's own.
        fn focus(&self, direction: gtk::DirectionType) -> bool {
            let forward = match direction {
                gtk::DirectionType::TabForward => true,
                gtk::DirectionType::TabBackward => false,
                _ => return self.parent_focus(direction),
            };
            let stops = self.obj().tab_stops();
            let current = stops.iter().position(gtk::prelude::WidgetExt::has_focus);
            super::keys::tab_stop(current, stops.len(), forward)
                .and_then(|i| stops.get(i))
                .is_some_and(gtk::prelude::WidgetExt::grab_focus)
        }

        fn grab_focus(&self) -> bool {
            self.text
                .get()
                .is_some_and(gtk::prelude::WidgetExt::grab_focus)
        }
    }

    impl BoxImpl for TagEntry {}
}

glib::wrapper! {
    pub struct TagEntry(ObjectSubclass<imp::TagEntry>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for TagEntry {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TagEntry {
    /// Calls `f` whenever the tags or the typed text change, by the user or
    /// by [`Self::set_tags`]. There is one such callback: setting another
    /// replaces it.
    pub fn set_on_changed(&self, f: impl Fn() + 'static) {
        let f: Rc<OnChanged> = Rc::new(f);
        self.imp().on_changed.replace(Some(Callback::new(f)));
    }

    /// Every tag there is, for the suggestions.
    pub fn set_all_tags(&self, all: Vec<Tag>) {
        self.imp().all_tags.replace(all);
    }

    /// Shows `names` as the pills, with nothing typed after them, and
    /// enables the field.
    pub fn set_tags(&self, names: &[String]) {
        let imp = self.imp();
        self.close_suggestions();
        imp.unavailable.set(false);
        imp.frame.set_sensitive(true);
        imp.names.replace(names.to_vec());
        self.refresh_pills();
        self.fill_text("");
        self.update_error();
        self.emit_changed();
    }

    /// Disables the field and shows `message` under it, when the tags
    /// could not be read. [`Self::check`] then has nothing to check.
    pub fn set_unavailable(&self, message: &str) {
        let imp = self.imp();
        self.close_suggestions();
        imp.unavailable.set(true);
        imp.names.borrow_mut().clear();
        self.refresh_pills();
        self.fill_text("");
        imp.frame.set_sensitive(false);
        imp.error.set_label(message);
        imp.error.set_visible(true);
        self.emit_changed();
    }

    /// The names to save (the pills, then the name being typed), or what
    /// is wrong with them.
    pub fn check(&self) -> TagsCheck {
        let imp = self.imp();
        if imp.unavailable.get() {
            return TagsCheck {
                names: Vec::new(),
                error: None,
            };
        }
        field::check(&imp.names.borrow(), &self.text().text())
    }

    /// The tags shown as pills.
    #[cfg(test)]
    pub fn pill_names(&self) -> Vec<String> {
        self.imp().names.borrow().clone()
    }

    /// The error shown under the field, if any.
    #[cfg(test)]
    pub fn shown_error(&self) -> Option<String> {
        let error = &self.imp().error;
        error.is_visible().then(|| error.label().to_string())
    }

    /// Whether the tags could be read and can be changed.
    #[cfg(test)]
    pub fn is_available(&self) -> bool {
        self.imp().frame.is_sensitive() && !self.imp().unavailable.get()
    }

    /// The text after the pills.
    #[cfg(test)]
    pub fn typed(&self) -> String {
        self.text().text().to_string()
    }

    /// Types `text` as the user would (it counts as typing), and runs the
    /// split a comma queues.
    #[cfg(test)]
    pub fn type_text(&self, text: &str) {
        let field = self.text();
        field.set_text(text);
        field.set_position(-1);
        if self.imp().split_queued.replace(false) {
            self.split(false);
        }
    }

    /// Types `keys` one character at a time at the cursor, running the
    /// split each one queues, as the idle would before the next key.
    #[cfg(test)]
    pub fn type_keys(&self, keys: &str) {
        let field = self.text();
        for key in keys.chars() {
            let mut position = field.position();
            field.insert_text(&key.to_string(), &mut position);
            field.set_position(position);
            if self.imp().split_queued.replace(false) {
                self.split(false);
            }
        }
    }

    /// The text widget the names are typed in.
    #[expect(
        clippy::expect_used,
        reason = "an invariant: `constructed` sets the text before anything can call this"
    )]
    pub fn text(&self) -> gtk::Text {
        self.imp()
            .text
            .get()
            .cloned()
            .expect("set in `constructed`, which runs before any other method")
    }

    fn setup(&self) {
        let imp = self.imp();
        // Made here rather than in the template: it goes into the pills'
        // wrapping lines, after them.
        let text = gtk::Text::builder()
            .placeholder_text("Add a tag")
            .width_chars(12)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .margin_start(6)
            .margin_end(6)
            .build();
        text.update_property(&[
            gtk::accessible::Property::Label("Tags"),
            gtk::accessible::Property::Autocomplete(gtk::AccessibleAutocomplete::List),
        ]);
        text.update_relation(&[gtk::accessible::Relation::DescribedBy(&[imp
            .error
            .upcast_ref()])]);
        imp.pills.set_end_child(Some(text.upcast_ref()));
        if imp.text.set(text.clone()).is_err() {
            tracing::warn!("the tag entry was set up twice");
        }

        imp.pills.set_on_remove(glib::clone!(
            #[weak(rename_to = entry)]
            self,
            move |index, backspace| entry.remove_pill(index, backspace)
        ));

        let popover = TagSuggestions::default();
        popover.set_parent(&*imp.frame);
        popover.connect_chosen(glib::clone!(
            #[weak(rename_to = entry)]
            self,
            move |index| entry.accept_suggestion(index)
        ));
        if imp.suggestions.set(popover).is_err() {
            tracing::warn!("the tag suggestions were set up twice");
        }

        text.connect_changed(glib::clone!(
            #[weak(rename_to = entry)]
            self,
            move |text| entry.text_changed(text)
        ));
        text.connect_notify_local(
            Some("cursor-position"),
            glib::clone!(
                #[weak(rename_to = entry)]
                self,
                move |_, _| {
                    let imp = entry.imp();
                    // The cursor moves after every typed character: that
                    // is not a move. A move by itself (arrows, a click)
                    // closes the list, so a later Tab cannot take a
                    // suggestion for a name the user did not touch.
                    if !imp.filling.get() && !imp.edited.replace(false) {
                        entry.close_suggestions();
                    }
                }
            ),
        );

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = entry)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| entry.key(key, state)
        ));
        text.add_controller(keys);

        // Tab or a click elsewhere: the suggestions go with the focus.
        let text_focus = gtk::EventControllerFocus::new();
        text_focus.connect_leave(glib::clone!(
            #[weak(rename_to = entry)]
            self,
            move |_| entry.close_suggestions()
        ));
        text.add_controller(text_focus);

        // The focus leaving the whole field (from the text or a pill) makes
        // the typed name a pill, so what the field shows is what a save
        // writes. The controller is on the field, and GTK emits `leave` only
        // when the focus leaves the hierarchy rooted at its widget (the
        // `leave` docs, gtk/gtkeventcontrollerfocus.c), so moving between
        // the text, the pills and the text's own popovers (context menu,
        // emoji chooser, parented to it) does not count. `leave` also comes
        // when only the window stops being active: the controller handles
        // GTK_CROSSING_ACTIVE as well as GTK_CROSSING_FOCUS
        // (`gtk_event_controller_focus_handle_crossing`, GTK 4.20). So the
        // decision waits for the focus to settle, in an idle, and is
        // `keys::should_split`.
        let field_focus = gtk::EventControllerFocus::new();
        field_focus.connect_leave(glib::clone!(
            #[weak(rename_to = entry)]
            self,
            move |_| {
                glib::idle_add_local_once(glib::clone!(
                    #[weak]
                    entry,
                    move || entry.focus_left()
                ));
            }
        ));
        self.add_controller(field_focus);

        // A click on the frame around the pills is a click in the field.
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = entry)]
            self,
            move |_, _, x, y| {
                let frame = &*entry.imp().frame;
                let on_button = frame.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| {
                    w.is::<gtk::Button>() || w.ancestor(gtk::Button::static_type()).is_some()
                });
                let text = entry.text();
                if !on_button && !text.has_focus() {
                    text.grab_focus();
                }
            }
        ));
        imp.frame.add_controller(click);
    }

    /// After the focus left the text: makes the typed name a pill if the
    /// focus really went elsewhere in an active window.
    fn focus_left(&self) {
        let Some(window) = self.root().and_downcast::<gtk::Window>() else {
            return;
        };
        self.after_leave(
            window.is_active(),
            gtk::prelude::GtkWindowExt::focus(&window),
        );
    }

    /// Makes the typed name a pill if `keys::should_split` says so, for a
    /// window that is `active` or not and the widget with the `focus`.
    fn after_leave(&self, active: bool, focus: Option<gtk::Widget>) {
        if keys::should_split(active, self.has_focus_inside(focus)) {
            self.imp().split_queued.set(false);
            self.split(true);
        }
    }

    /// Whether `focus` is in this field (the text, a pill, a popover of
    /// the text).
    fn has_focus_inside(&self, focus: Option<gtk::Widget>) -> bool {
        focus.is_some_and(|focus| {
            focus == *self.upcast_ref::<gtk::Widget>() || focus.is_ancestor(self)
        })
    }

    fn emit_changed(&self) {
        let callback = self.imp().on_changed.borrow().as_ref().map(Callback::get);
        if let Some(callback) = callback {
            callback();
        }
    }

    /// The text's own change: typing, pasting, deleting.
    fn text_changed(&self, text: &gtk::Text) {
        let imp = self.imp();
        if imp.filling.get() {
            return;
        }
        imp.edited.set(true);
        let typed = text.text();
        // A comma, or spaces before the name (after a comma, or typed into
        // the empty text), which are not part of it.
        let tidy = typed.contains(',') || typed.starts_with(char::is_whitespace);
        if tidy && !imp.split_queued.replace(true) {
            // Not in this handler: GtkText is still inserting the text
            // that emitted it. Run before the next frame is drawn, so the
            // comma is never seen: GDK redraws at GDK_PRIORITY_REDRAW,
            // G_PRIORITY_HIGH_IDLE + 20 (gdk/gdkevents.h), and this runs
            // at G_PRIORITY_HIGH_IDLE.
            glib::idle_add_local_full(
                glib::Priority::HIGH_IDLE,
                glib::clone!(
                    #[weak(rename_to = entry)]
                    self,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        if entry.imp().split_queued.replace(false) {
                            entry.split(false);
                        }
                        glib::ControlFlow::Break
                    }
                ),
            );
        }
        self.update_error();
        self.emit_changed();
        self.update_suggestions();
    }

    /// Makes the names a comma ended (all of them if `whole`: Enter) pills,
    /// and leaves the rest in the text.
    fn split(&self, whole: bool) {
        let imp = self.imp();
        let text = self.text().text();
        let split = field::split_typed(&imp.names.borrow(), &text, whole);
        if split.add.is_empty() && split.rest == text.as_str() {
            return;
        }
        self.close_suggestions();
        if let Some(message) = keys::added_announcement(&split.add) {
            self.text()
                .announce(&message, gtk::AccessibleAnnouncementPriority::Medium);
        }
        imp.names.borrow_mut().extend(split.add);
        self.refresh_pills();
        self.fill_text(&split.rest);
        self.update_error();
        self.emit_changed();
    }

    /// Puts `text` in the field with the cursor at its end, without it
    /// counting as typing.
    fn fill_text(&self, text: &str) {
        let imp = self.imp();
        let field = self.text();
        imp.filling.set(true);
        field.set_text(text);
        field.set_position(-1);
        imp.filling.set(false);
    }

    fn refresh_pills(&self) {
        let imp = self.imp();
        let shown: Vec<String> = imp.names.borrow().iter().map(|n| one_line(n)).collect();
        imp.pills.set_tags(&shown);
    }

    fn remove_pill(&self, index: usize, backspace: bool) {
        let imp = self.imp();
        let removed = {
            let mut names = imp.names.borrow_mut();
            (index < names.len()).then(|| names.remove(index))
        };
        let Some(removed) = removed else {
            return;
        };
        self.refresh_pills();
        let remaining = imp.names.borrow().len();
        let focused = keys::after_removal(index, remaining, backspace)
            .is_some_and(|i| imp.pills.focus_remove_button(i));
        let text = self.text();
        if !focused {
            text.grab_focus();
        }
        text.announce(
            &keys::removed_announcement(&one_line(&removed)),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
        self.update_error();
        self.emit_changed();
    }

    /// The text, then each pill's remove button: where Tab stops.
    fn tab_stops(&self) -> Vec<gtk::Widget> {
        let imp = self.imp();
        std::iter::once(self.text().upcast::<gtk::Widget>())
            .chain(imp.pills.remove_buttons().into_iter().map(Cast::upcast))
            .filter(|w| w.is_visible() && w.is_sensitive())
            .collect()
    }

    /// Shows the error of the field under it, announcing it when it
    /// appears or changes.
    fn update_error(&self) {
        let imp = self.imp();
        if imp.unavailable.get() {
            return;
        }
        match self.check().error {
            Some(message) => {
                if !imp.error.is_visible() || imp.error.label() != message {
                    self.text()
                        .announce(&message, gtk::AccessibleAnnouncementPriority::Medium);
                }
                imp.error.set_label(&message);
                imp.error.set_visible(true);
            }
            None => imp.error.set_visible(false),
        }
    }

    /// Lists the tags that match the name being typed, under the field.
    fn update_suggestions(&self) {
        let imp = self.imp();
        let Some(popover) = imp.suggestions.get() else {
            return;
        };
        let text = self.text();
        // Only while the user is typing in it.
        if !text.has_focus() || imp.unavailable.get() {
            self.close_suggestions();
            return;
        }
        let cursor = usize::try_from(text.position()).unwrap_or(0);
        let found = field::suggestions(
            &imp.all_tags.borrow(),
            &imp.names.borrow(),
            &text.text(),
            cursor,
        );
        if found.is_empty() {
            self.close_suggestions();
            return;
        }
        let same = found
            .iter()
            .map(|t| &t.id)
            .eq(imp.suggested.borrow().iter().map(|t| &t.id));
        if !same || !popover.is_visible() {
            let names: Vec<&str> = found.iter().map(|t| t.name.as_str()).collect();
            let count = names.len();
            popover.set_names(&names);
            popover.set_width_request(imp.frame.width());
            // Nothing is highlighted until Down or Up: Tab takes the
            // first, Enter makes the typed name a pill.
            imp.highlighted.set(None);
            popover.highlight(None);
            let opening = !popover.is_visible();
            imp.suggested.replace(found);
            if opening {
                popover.popup();
            } else {
                // Fewer rows than before: lay the popover out again at its
                // new natural size, or its surface keeps the old height
                // (seen under X11).
                popover.present();
            }
            if opening {
                text.announce(
                    &keys::suggestions_announcement(count),
                    gtk::AccessibleAnnouncementPriority::Low,
                );
            }
        }
    }

    fn close_suggestions(&self) {
        let imp = self.imp();
        if let Some(popover) = imp.suggestions.get()
            && popover.is_visible()
        {
            popover.popdown();
        }
        imp.suggested.borrow_mut().clear();
        imp.highlighted.set(None);
    }

    /// The text's keys, as `keys::key_action` decides. Keys with Ctrl, Alt
    /// or Shift are never taken: Ctrl+Enter saves, Shift+Tab moves back.
    fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let imp = self.imp();
        let plain = !state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SHIFT_MASK,
        );
        if !plain {
            return glib::Propagation::Proceed;
        }
        let Some(&(_, key)) = KEYS.iter().find(|(bound, _)| *bound == key) else {
            return glib::Propagation::Proceed;
        };
        let text = self.text();
        let typed = text.text();
        let entry_state = EntryState {
            showing: imp
                .suggestions
                .get()
                .is_some_and(gtk::prelude::WidgetExt::is_visible),
            highlighted: imp.highlighted.get(),
            blank: typed.trim().is_empty(),
            at_start: text.position() == 0 && text.selection_bounds().is_none(),
            pills: imp.names.borrow().len(),
        };
        match keys::key_action(key, entry_state) {
            KeyAction::Highlight(movement) => self.move_highlight(movement),
            KeyAction::Accept(index) => self.accept_suggestion(index),
            KeyAction::Close => self.close_suggestions(),
            KeyAction::Commit => {
                self.close_suggestions();
                imp.split_queued.set(false);
                self.split(true);
            }
            KeyAction::FocusLastPill => {
                let last = imp.names.borrow().len().saturating_sub(1);
                imp.pills.focus_remove_button(last);
            }
            KeyAction::Pass => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    fn move_highlight(&self, movement: Move) {
        let imp = self.imp();
        let suggested = imp.suggested.borrow();
        let Some(index) = keys::highlight_after(movement, imp.highlighted.get(), suggested.len())
        else {
            return;
        };
        imp.highlighted.set(Some(index));
        if let Some(popover) = imp.suggestions.get() {
            popover.highlight(Some(index));
        }
        if let Some(tag) = suggested.get(index) {
            self.text().announce(
                &keys::highlight_announcement(&tag.name, index, suggested.len()),
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        }
    }

    /// Makes suggestion `index` a pill in place of the name being typed.
    pub(crate) fn accept_suggestion(&self, index: usize) {
        let imp = self.imp();
        let Some(tag) = imp.suggested.borrow().get(index).cloned() else {
            return;
        };
        let text = self.text();
        let typed_text = text.text();
        let cursor = usize::try_from(text.position()).unwrap_or(0);
        let typed = field::name_at(&typed_text, cursor);
        let rest = field::without_piece(&typed_text, &typed);
        self.close_suggestions();
        // Suggestions leave out the tags that are pills already.
        let added = vec![tag.name];
        imp.names.borrow_mut().extend(added.iter().cloned());
        self.refresh_pills();
        self.fill_text(&rest);
        if let Some(message) = keys::added_announcement(&added) {
            text.announce(&message, gtk::AccessibleAnnouncementPriority::Medium);
        }
        self.update_error();
        self.emit_changed();
    }

    /// Shows the suggestions for what is typed, as typing would (tests: the
    /// text has no focus without a window).
    #[cfg(test)]
    pub fn suggest_for_test(&self) -> Vec<Tag> {
        let imp = self.imp();
        let text = self.text();
        let found = field::suggestions(
            &imp.all_tags.borrow(),
            &imp.names.borrow(),
            &text.text(),
            usize::try_from(text.position()).unwrap_or(0),
        );
        imp.suggested.replace(found.clone());
        found
    }

    /// Removes pill `index` as its button would.
    #[cfg(test)]
    pub fn remove_for_test(&self, index: usize) {
        self.remove_pill(index, false);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn a_comma_or_enter_makes_pills_and_the_typed_name_counts() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            let changes = Rc::new(std::cell::Cell::new(0));
            let count = Rc::clone(&changes);
            entry.set_on_changed(move || count.set(count.get() + 1));
            entry.set_tags(&strings(&["maths"]));
            assert_eq!(entry.pill_names(), ["maths"]);

            entry.type_text("algebra, geo");
            assert_eq!(entry.pill_names(), ["maths", "algebra"]);
            assert_eq!(entry.typed(), "geo");
            // What is typed is saved too.
            assert_eq!(entry.check().names, ["maths", "algebra", "geo"]);
            assert!(changes.get() >= 2);

            entry.split(true);
            assert_eq!(entry.pill_names(), ["maths", "algebra", "geo"]);
            assert_eq!(entry.typed(), "");
            // A name already there is dropped, in any spelling.
            entry.type_text("MATHS,");
            assert_eq!(entry.pill_names(), ["maths", "algebra", "geo"]);
            assert_eq!(entry.typed(), "");
        });
    }

    #[test]
    fn a_name_that_cannot_be_a_tag_stays_with_its_error() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            let long = "x".repeat(300);
            entry.type_text(&format!("{long},"));
            assert_eq!(entry.pill_names(), Vec::<String>::new());
            assert_eq!(entry.typed(), format!("{long},"));
            let check = entry.check();
            assert_eq!(
                check.error.as_deref(),
                Some("This tag is too long (the limit is 256 bytes)")
            );
            assert!(entry.imp().error.is_visible());
            entry.type_text("ok");
            assert!(!entry.imp().error.is_visible());
        });
    }

    #[test]
    fn removing_a_pill_relabels_the_buttons() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            entry.set_tags(&strings(&["a", "b", "c"]));
            let buttons = entry.imp().pills.remove_buttons();
            assert_eq!(buttons.len(), 3);
            entry.remove_for_test(1);
            assert_eq!(entry.pill_names(), ["a", "c"]);
            let buttons = entry.imp().pills.remove_buttons();
            assert_eq!(buttons.len(), 2);
            assert_eq!(buttons[1].tooltip_text().as_deref(), Some("Remove Tag"));
            assert_eq!(entry.check().names, ["a", "c"]);
        });
    }

    #[test]
    fn a_suggestion_becomes_a_pill() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            entry.set_all_tags(vec![Tag {
                id: "id".to_owned(),
                name: "Algèbre".to_owned(),
            }]);
            entry.set_tags(&strings(&["maths"]));
            entry.type_text("alg");
            assert_eq!(entry.suggest_for_test().len(), 1);
            entry.accept_suggestion(0);
            assert_eq!(entry.pill_names(), ["maths", "Algèbre"]);
            assert_eq!(entry.typed(), "");
        });
    }

    #[test]
    fn unavailable_tags_disable_the_field_and_check_nothing() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            entry.set_unavailable("The tags could not be read.");
            assert!(!entry.imp().frame.is_sensitive());
            assert!(entry.imp().error.is_visible());
            assert_eq!(entry.check().names, Vec::<String>::new());
            entry.set_tags(&strings(&["a"]));
            assert!(entry.imp().frame.is_sensitive());
            assert!(!entry.imp().error.is_visible());
        });
    }

    #[test]
    fn leaving_the_field_makes_the_typed_name_a_pill_only_if_the_focus_went_elsewhere() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            let other = gtk::Button::with_label("Other");
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.append(&entry);
            column.append(&other);
            // The field watches the focus itself, not only its text, so
            // text → pill → elsewhere is one leave.
            let watches = entry
                .observe_controllers()
                .into_iter()
                .filter_map(Result::ok)
                .any(|c| c.is::<gtk::EventControllerFocus>());
            assert!(watches);
            entry.set_tags(&strings(&["a"]));
            entry.type_text("geo");
            let pill_button = entry.imp().pills.remove_buttons()[0].clone();
            // The focus on a pill, or in the text: still inside.
            entry.after_leave(true, Some(pill_button.upcast()));
            entry.after_leave(true, Some(entry.text().upcast()));
            assert_eq!(entry.pill_names(), ["a"]);
            assert_eq!(entry.typed(), "geo");
            // The window went to the background: the name stays.
            entry.after_leave(false, Some(other.clone().upcast()));
            assert_eq!(entry.typed(), "geo");
            // Elsewhere in an active window: a pill.
            entry.after_leave(true, Some(other.upcast()));
            assert_eq!(entry.pill_names(), ["a", "geo"]);
            assert_eq!(entry.typed(), "");
        });
    }

    #[test]
    fn a_huge_pasted_list_makes_at_most_the_limit_of_pills_quickly() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            // About 180 KB; GTK keeps the first 65,535 bytes of a paste in
            // the text (`GTK_ENTRY_BUFFER_MAX_SIZE`), about 7,000 names.
            let names: Vec<String> = (0..20_000).map(|i| format!("tag {i}")).collect();
            let pasted = format!("{},", names.join(","));
            let (paste, ()) = crate::testing::cpu_time_of(|| entry.type_text(&pasted));
            assert_eq!(entry.pill_names().len(), field::MAX_TAGS_PER_CARD);
            assert_eq!(
                entry.imp().pills.remove_buttons().len(),
                field::MAX_TAGS_PER_CARD
            );
            // The rest waits in the text, with the error under the field.
            assert!(entry.typed().starts_with("tag 100,tag 101,"));
            assert_eq!(
                entry.shown_error().as_deref(),
                Some("A card can have at most 100 tags")
            );
            // A key typed after it.
            let (key, ()) = crate::testing::cpu_time_of(|| entry.type_keys("x"));
            assert_eq!(entry.pill_names().len(), field::MAX_TAGS_PER_CARD);
            crate::testing::print_timing("tag entry: paste 20,000 names", paste);
            crate::testing::print_timing("tag entry: a key after them", key);
            crate::testing::assert_cpu_within(paste, Duration::from_secs(1), "paste");
            crate::testing::assert_cpu_within(key, Duration::from_millis(500), "key");
        });
    }

    #[test]
    fn a_card_with_thousands_of_imported_tags_opens_in_time() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            let names: Vec<String> = (0..5_000).map(|i| format!("tag {i}")).collect();
            let (load, ()) = crate::testing::cpu_time_of(|| entry.set_tags(&names));
            // Laid out at a dialog's width, as when the editor opens.
            let (measure, (_, height, _, _)) =
                crate::testing::cpu_time_of(|| entry.measure(gtk::Orientation::Vertical, 600));
            assert!(height > 1_000, "every pill is laid out: {height} px");
            crate::testing::print_timing("tag entry: measure 5,000 tags", measure);
            assert_eq!(entry.pill_names().len(), 5_000);
            assert_eq!(entry.check().names.len(), 5_000);
            assert_eq!(entry.check().error, None);
            crate::testing::print_timing("tag entry: 5,000 tags", load);
            crate::testing::assert_cpu_within(load, Duration::from_secs(2), "5,000 tags");
            crate::testing::assert_cpu_within(measure, Duration::from_secs(4), "measure");
        });
    }

    #[test]
    fn hostile_names_are_plain_single_lines() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            let names = strings(&[
                "two\nlines",
                "tab\there",
                "line\u{2028}separator",
                "<b>bold</b> &amp; <span size='99999'>",
                "\u{202e}reversed\u{202c}",
                "zero\u{200b}width",
            ]);
            entry.set_tags(&names);
            let labels = entry.imp().pills.pill_widgets();
            let shown: Vec<String> = labels.iter().map(|l| l.label().to_string()).collect();
            assert_eq!(
                shown,
                [
                    "two lines",
                    "tab here",
                    "line separator",
                    "<b>bold</b> &amp; <span size='99999'>",
                    "\u{202e}reversed\u{202c}",
                    "zero\u{200b}width",
                ]
            );
            for label in &labels {
                assert!(!label.uses_markup());
                assert!(label.is_single_line_mode());
                assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::End);
            }
            // Saved as typed: only the pills are drawn on one line.
            assert_eq!(entry.check().names, names);
        });
    }

    #[test]
    fn spaces_typed_before_a_name_are_dropped() {
        crate::testing::with_widgets(|| {
            let entry = TagEntry::default();
            // Key by key, the split running between keys as the idle does.
            entry.type_keys("algebra, geo");
            assert_eq!(entry.pill_names(), ["algebra"]);
            assert_eq!(entry.typed(), "geo");
            entry.type_keys(" ");
            assert_eq!(entry.typed(), "geo ");
            entry.set_tags(&[]);
            entry.type_keys("  maths");
            assert_eq!(entry.typed(), "maths");
        });
    }
}
