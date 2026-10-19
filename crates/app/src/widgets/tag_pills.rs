//! `TagPills`: a card's tags as small rounded pills, neutral (the text
//! colour at a low opacity, `.tag-pill` in `data/style.css`) so they read in
//! light, dark and high-contrast styles. Layout in `data/tag_pills.blp`.
//!
//! Two forms, by the `single-line` property:
//! - wrapping (the default): every tag, in an `AdwWrapBox` that breaks the
//!   pills onto more lines when the width runs out;
//! - single line, for list rows: at most `max-shown` pills, then one "+N"
//!   pill for the rest, whose tooltip names them.
//!
//! In the wrapping form the pills can be `removable` (the tag entry, ADR
//! 0043): each then has a remove button, focusable, with "Remove Tag" as
//! its tooltip and "Remove tag NAME" as its accessible label, and an
//! `end-child` (the entry's text) can follow the pills in the same lines.
//! A removable pill is a touch target: libadwaita's own circular button
//! (34 px) in a pill as tall, and its name is never cut: it wraps onto
//! more lines when wider than the field, so no name is tooltip-only
//! (ADR 0058).
//!
//! Built to be bound often: a recycled list row calls [`TagPills::set_tags`]
//! on every bind, so the pill labels are kept and relabelled, never made
//! again, and nothing is done when the tags are the ones already shown.
//! Pills are plain labels, never focusable; a list row says its tags in its
//! own accessible label (`text::spoken`). Only a removable pill's button
//! takes the focus.

pub mod text;

use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};

use crate::i18n::pgettext;
use crate::ui::callback::Callback;

/// Called with the index of a pill whose remove button was used, and
/// whether by Backspace (which walks back) rather than a click, Enter,
/// Space or Delete.
type OnRemove = dyn Fn(usize, bool);

/// One pill: its label and, when removable, the box it shares with its
/// remove button.
#[derive(Debug, Clone)]
pub struct Pill {
    /// The widget in the box: the label, or the removable pill's box.
    root: gtk::Widget,
    label: gtk::Label,
    remove: Option<gtk::Button>,
}

/// The keys that remove the pill whose button has the focus, without Ctrl,
/// Alt or Shift, and whether the focus then walks back (Backspace) rather
/// than staying in place. The Keyboard Shortcuts dialog lists them (a test
/// checks it).
pub(crate) const REMOVE_KEYS: [(gdk::Key, bool); 3] = [
    (gdk::Key::BackSpace, true),
    (gdk::Key::Delete, false),
    (gdk::Key::KP_Delete, false),
];

mod imp {
    use std::cell::{Cell, RefCell};

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use super::{OnRemove, Pill};
    use crate::ui::callback::Callback;

    #[derive(Debug, gtk::CompositeTemplate, glib::Properties)]
    #[properties(wrapper_type = super::TagPills)]
    #[template(resource = "/io/github/rsmirke/Rappel/tag_pills.ui")]
    pub struct TagPills {
        #[template_child]
        pub wrap_box: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub line_box: TemplateChild<gtk::Box>,
        /// One line with a "+N" pill, instead of wrapping.
        #[property(get, set = Self::set_single_line, explicit_notify)]
        pub single_line: Cell<bool>,
        /// The most pills on a single line before the "+N" pill.
        #[property(get, set = Self::set_max_shown, explicit_notify)]
        pub max_shown: Cell<u32>,
        /// Each pill has a remove button (wrapping form only).
        #[property(get, set = Self::set_removable, explicit_notify)]
        pub removable: Cell<bool>,
        /// The tag names shown, each already one line.
        pub names: RefCell<Vec<String>>,
        /// The pills of the box in use, kept for the next tags.
        pub pills: RefCell<Vec<Pill>>,
        /// A widget after the pills in the wrap box, kept last.
        pub end_child: RefCell<Option<gtk::Widget>>,
        pub on_remove: RefCell<Option<Callback<OnRemove>>>,
        /// The "+N" pill, made the first time it is needed.
        pub overflow: RefCell<Option<gtk::Label>>,
    }

    impl Default for TagPills {
        fn default() -> Self {
            Self {
                wrap_box: TemplateChild::default(),
                line_box: TemplateChild::default(),
                single_line: Cell::new(false),
                max_shown: Cell::new(super::text::ROW_PILLS),
                removable: Cell::new(false),
                names: RefCell::default(),
                pills: RefCell::default(),
                end_child: RefCell::default(),
                on_remove: RefCell::default(),
                overflow: RefCell::default(),
            }
        }
    }

    impl TagPills {
        fn set_single_line(&self, single_line: bool) {
            if self.single_line.replace(single_line) == single_line {
                return;
            }
            // The pills are in the box being left: start again in the other.
            self.obj().clear_pills(!single_line);
            self.wrap_box.set_visible(!single_line);
            self.line_box.set_visible(single_line);
            self.obj().refresh();
            self.obj().notify_single_line();
        }

        fn set_removable(&self, removable: bool) {
            if self.removable.replace(removable) == removable {
                return;
            }
            // Pills of the other kind: make them again.
            self.obj().clear_pills(self.single_line.get());
            self.obj().refresh();
            self.obj().notify_removable();
        }

        fn set_max_shown(&self, max_shown: u32) {
            if self.max_shown.replace(max_shown) == max_shown {
                return;
            }
            self.obj().refresh();
            self.obj().notify_max_shown();
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TagPills {
        const NAME: &'static str = "RappelTagPills";
        type Type = super::TagPills;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TagPills {}
    impl WidgetImpl for TagPills {}
    impl BoxImpl for TagPills {}
}

glib::wrapper! {
    pub struct TagPills(ObjectSubclass<imp::TagPills>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for TagPills {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TagPills {
    /// Shows `names` (each already one line, `ui::text::one_line`). Cheap
    /// when they are the tags already shown, and otherwise only relabels
    /// the kept pills.
    pub fn set_tags(&self, names: &[String]) {
        let imp = self.imp();
        if imp.names.borrow().as_slice() == names {
            return;
        }
        imp.names.replace(names.to_vec());
        self.refresh();
    }

    /// The tag names shown (all of them, the ones in the "+N" pill too).
    #[cfg(test)]
    pub fn tags(&self) -> Vec<String> {
        self.imp().names.borrow().clone()
    }

    /// Calls `f` with the index of a pill whose remove button was used,
    /// and whether by Backspace. There is one such callback: setting
    /// another replaces it.
    pub fn set_on_remove(&self, f: impl Fn(usize, bool) + 'static) {
        let f: Rc<OnRemove> = Rc::new(f);
        self.imp().on_remove.replace(Some(Callback::new(f)));
    }

    /// Puts `child` after the pills, in the same wrapping lines (wrapping
    /// form only), or takes it away.
    pub fn set_end_child(&self, child: Option<&gtk::Widget>) {
        let imp = self.imp();
        if let Some(old) = imp.end_child.take() {
            imp.wrap_box.remove(&old);
        }
        if let Some(child) = child {
            imp.wrap_box.append(child);
            imp.end_child.replace(Some(child.clone()));
        }
    }

    /// The remove buttons of the pills on screen, in order.
    pub fn remove_buttons(&self) -> Vec<gtk::Button> {
        self.imp()
            .pills
            .borrow()
            .iter()
            .filter(|p| p.root.is_visible())
            .filter_map(|p| p.remove.clone())
            .collect()
    }

    /// Moves the focus to pill `index`'s remove button. Whether there is
    /// one to focus.
    pub fn focus_remove_button(&self, index: usize) -> bool {
        self.remove_buttons()
            .get(index)
            .is_some_and(gtk::prelude::WidgetExt::grab_focus)
    }

    /// Every pill label made so far, the "+N" pill last, shown or not.
    #[cfg(test)]
    pub fn pill_widgets(&self) -> Vec<gtk::Label> {
        let imp = self.imp();
        let mut all: Vec<gtk::Label> = imp.pills.borrow().iter().map(|p| p.label.clone()).collect();
        all.extend(imp.overflow.borrow().clone());
        all
    }

    /// The labels of the pills on screen, in order, the "+N" pill last.
    #[cfg(test)]
    pub fn shown_texts(&self) -> Vec<String> {
        let imp = self.imp();
        let mut texts: Vec<String> = imp
            .pills
            .borrow()
            .iter()
            .filter(|p| p.root.is_visible())
            .map(|p| p.label.label().to_string())
            .collect();
        if let Some(overflow) = imp.overflow.borrow().as_ref()
            && overflow.is_visible()
        {
            texts.push(overflow.label().to_string());
        }
        texts
    }

    /// Lays the pills out for the current names, reusing the ones made
    /// before; the ones not needed are hidden, not destroyed.
    fn refresh(&self) {
        let imp = self.imp();
        let single_line = imp.single_line.get();
        let removable = imp.removable.get() && !single_line;
        let names = imp.names.borrow();
        let (shown, hidden) = text::split(names.len(), single_line.then(|| imp.max_shown.get()));
        let mut pills = imp.pills.borrow_mut();
        while pills.len() < shown {
            let pill = if removable {
                self.removable_pill()
            } else {
                let label = pill_label(false);
                Pill {
                    root: label.clone().upcast(),
                    label,
                    remove: None,
                }
            };
            self.container_append(&pill.root);
            pills.push(pill);
        }
        for (i, pill) in pills.iter().enumerate() {
            match names.get(i).filter(|_| i < shown) {
                Some(name) => {
                    pill.label.set_label(name);
                    // `set_tooltip_text` is plain text, like the label. A
                    // removable pill shows its whole name.
                    pill.label.set_tooltip_text(
                        (!removable && text::is_cut(name)).then_some(name.as_str()),
                    );
                    if let Some(remove) = &pill.remove {
                        remove.update_property(&[gtk::accessible::Property::Label(
                            &super::tag_entry::keys::remove_label(name),
                        )]);
                    }
                    pill.root.set_visible(true);
                }
                None => pill.root.set_visible(false),
            }
        }
        drop(pills);
        if hidden > 0 {
            let overflow = self.overflow_label().unwrap_or_else(|| {
                let overflow = pill_label(false);
                overflow.add_css_class("numeric");
                overflow.set_ellipsize(gtk::pango::EllipsizeMode::None);
                self.container_append(overflow.upcast_ref());
                imp.overflow.replace(Some(overflow.clone()));
                overflow
            });
            overflow.set_label(&text::overflow_text(hidden));
            overflow.set_tooltip_text(Some(&text::overflow_tooltip(&names[shown..])));
            overflow.set_visible(true);
        } else if let Some(overflow) = self.overflow_label() {
            overflow.set_visible(false);
        }
    }

    /// A pill with a remove button, which tells [`Self::set_on_remove`]'s
    /// callback which pill it belongs to when used.
    fn removable_pill(&self) -> Pill {
        let label = pill_label(true);
        let remove = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(pgettext("tag pill", "Remove Tag"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        let root = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["tag-pill", "removable"])
            .build();
        // The box has the pill's look, around the label and the button.
        label.remove_css_class("tag-pill");
        root.append(&label);
        root.append(&remove);
        remove.connect_clicked(glib::clone!(
            #[weak(rename_to = pills)]
            self,
            move |button| pills.removed(button, false)
        ));
        // Backspace and Delete on the focused button remove the pill too,
        // as they would a selected word. The button has no use for them.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = pills)]
            self,
            #[weak]
            remove,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let plain = !state.intersects(
                    gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK
                        | gdk::ModifierType::SHIFT_MASK,
                );
                match REMOVE_KEYS.iter().find(|(bound, _)| *bound == key) {
                    Some(&(_, backwards)) if plain => {
                        pills.removed(&remove, backwards);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        remove.add_controller(keys);
        Pill {
            root: root.upcast(),
            label,
            remove: Some(remove),
        }
    }

    /// Tells the callback that the pill of `button` is to be removed.
    fn removed(&self, button: &gtk::Button, backspace: bool) {
        let index = self.remove_buttons().iter().position(|b| b == button);
        let callback = self.imp().on_remove.borrow().as_ref().map(Callback::get);
        if let (Some(index), Some(callback)) = (index, callback) {
            callback(index, backspace);
        }
    }

    fn overflow_label(&self) -> Option<gtk::Label> {
        self.imp().overflow.borrow().clone()
    }

    /// Adds `pill` at the end of the box in use: before the "+N" pill on a
    /// single line, before the end child when wrapping.
    fn container_append(&self, pill: &gtk::Widget) {
        let imp = self.imp();
        if imp.single_line.get() {
            match self.overflow_label() {
                Some(overflow) => {
                    imp.line_box
                        .insert_child_after(pill, overflow.prev_sibling().as_ref());
                }
                None => imp.line_box.append(pill),
            }
        } else {
            match imp.end_child.borrow().as_ref() {
                Some(end) => imp
                    .wrap_box
                    .insert_child_after(pill, end.prev_sibling().as_ref()),
                None => imp.wrap_box.append(pill),
            }
        }
    }

    /// Removes every pill, from the single line if `in_line`, else from
    /// the wrap box. The end child stays.
    fn clear_pills(&self, in_line: bool) {
        let imp = self.imp();
        let pills = imp.pills.take();
        let overflow = imp.overflow.take();
        let widgets = pills
            .iter()
            .map(|p| p.root.clone())
            .chain(overflow.map(Cast::upcast));
        for widget in widgets {
            if in_line {
                imp.line_box.remove(&widget);
            } else {
                imp.wrap_box.remove(&widget);
            }
        }
    }
}

/// The widest a removable pill's name asks to be, in characters, before it
/// wraps: about the editor's widest field.
const REMOVABLE_PILL_CHARS: i32 = 80;

/// One pill: a plain label, cut with "…" past [`text::PILL_CHARS`], or
/// for a removable pill whole, wrapping where it is wider than the field.
fn pill_label(removable: bool) -> gtk::Label {
    let label = gtk::Label::builder()
        .use_markup(false)
        .valign(gtk::Align::Center)
        .css_classes(["tag-pill", "caption"])
        .build();
    if removable {
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_xalign(0.0);
        // A wrapping label's natural width is `max-width-chars` wide at
        // most; unset, GTK makes it narrow. This lets a name ask for its
        // whole line (the wrap box gives it at most the field's width).
        label.set_max_width_chars(REMOVABLE_PILL_CHARS);
        label.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
    } else {
        label.set_single_line_mode(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(text::PILL_CHARS);
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn pills_are_reused_and_the_rest_counted() {
        crate::testing::with_widgets(|| {
            let pills = TagPills::default();
            pills.set_single_line(true);
            pills.set_max_shown(2);
            pills.set_tags(&names(&["a", "b", "c", "d"]));
            assert_eq!(pills.shown_texts(), ["a", "b", "+2"]);
            let first = pills.imp().pills.borrow()[0].label.clone();
            pills.set_tags(&names(&["x"]));
            assert_eq!(pills.shown_texts(), ["x"]);
            // The same label, relabelled: nothing is made again on a bind.
            assert_eq!(pills.imp().pills.borrow()[0].label, first);
            pills.set_max_shown(3);
            pills.set_tags(&names(&["a", "b", "c", "d", "e"]));
            assert_eq!(pills.shown_texts(), ["a", "b", "c", "+2"]);
            // The "+N" pill stays last after a pill is added before it.
            let last = pills
                .imp()
                .line_box
                .last_child()
                .and_downcast::<gtk::Label>();
            assert_eq!(last.map(|l| l.label().to_string()).as_deref(), Some("+2"));
            pills.set_tags(&[]);
            assert_eq!(pills.shown_texts(), Vec::<String>::new());
        });
    }

    #[test]
    fn wrapping_pills_show_every_tag() {
        crate::testing::with_widgets(|| {
            let pills = TagPills::default();
            let many: Vec<String> = (0..10).map(|i| format!("tag {i}")).collect();
            pills.set_tags(&many);
            assert_eq!(pills.shown_texts(), many);
            // Switching form moves the pills to the other box.
            pills.set_single_line(true);
            assert_eq!(pills.shown_texts(), ["tag 0", "tag 1", "tag 2", "+7"]);
            assert!(pills.imp().wrap_box.first_child().is_none());
            // Back to wrapping: the single line, "+N" pill included, is
            // emptied, and every tag wraps again.
            pills.set_single_line(false);
            assert_eq!(pills.shown_texts(), many);
            assert!(pills.imp().line_box.first_child().is_none());
            // And to a single line again, with a new "+N" pill.
            pills.set_single_line(true);
            assert_eq!(pills.shown_texts(), ["tag 0", "tag 1", "tag 2", "+7"]);
            assert!(pills.imp().wrap_box.first_child().is_none());
            let last = pills
                .imp()
                .line_box
                .last_child()
                .and_downcast::<gtk::Label>();
            assert_eq!(last.map(|l| l.label().to_string()).as_deref(), Some("+7"));
        });
    }

    #[test]
    fn removable_pills_have_a_labelled_button_and_the_end_child_stays_last() {
        crate::testing::with_widgets(|| {
            let pills = TagPills::default();
            pills.set_removable(true);
            let end = gtk::Text::new();
            pills.set_end_child(Some(end.upcast_ref()));
            pills.set_tags(&names(&["a", "b"]));
            pills.set_tags(&names(&["a", "b", "c"]));
            assert_eq!(pills.shown_texts(), ["a", "b", "c"]);
            assert_eq!(
                pills.imp().wrap_box.last_child().as_ref(),
                Some(end.upcast_ref::<gtk::Widget>())
            );
            let buttons = pills.remove_buttons();
            assert_eq!(buttons.len(), 3);
            for button in &buttons {
                assert!(button.is_focusable());
                assert_eq!(button.tooltip_text().as_deref(), Some("Remove Tag"));
            }
            // The callback is told which pill.
            let removed = std::rc::Rc::new(std::cell::Cell::new(None));
            let sink = std::rc::Rc::clone(&removed);
            pills.set_on_remove(move |index, backspace| sink.set(Some((index, backspace))));
            buttons[1].emit_clicked();
            assert_eq!(removed.get(), Some((1, false)));
            // Fewer tags: the buttons of hidden pills are not stops.
            pills.set_tags(&names(&["a"]));
            assert_eq!(pills.remove_buttons().len(), 1);
            // A long name is shown whole in a removable pill, without a
            // tooltip: it wraps instead of being cut.
            let long = "a tag name far longer than a pill shows on one line";
            pills.set_tags(&names(&["a", long]));
            let label = pills.imp().pills.borrow()[1].label.clone();
            assert_eq!(label.label(), long);
            assert!(label.wraps());
            assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::None);
            assert_eq!(label.tooltip_text(), None);
            pills.set_tags(&names(&["a"]));
            // Not removable again: plain labels, no buttons.
            pills.set_removable(false);
            assert_eq!(pills.shown_texts(), ["a"]);
            assert_eq!(pills.remove_buttons(), Vec::<gtk::Button>::new());
        });
    }
}
