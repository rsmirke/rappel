//! The list of tag suggestions under the card editor's tags field, a popover
//! parented to the entry. Layout in `data/tag_suggestions.blp`.
//!
//! It never takes the keyboard focus (the entry keeps it; the editor moves
//! the highlight with the arrow keys): the popover is not `autohide`, which
//! GTK documents as the setting that makes a popover modal, grabbing the
//! keyboard focus and closing on Esc (`gtk_popover_set_autohide`). Its rows
//! are not focusable either, or a click would take the focus from the entry.

use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::ui::callback::Callback;

/// Called with the index of the suggestion the user clicked.
type OnChosen = dyn Fn(usize);

mod imp {
    use std::cell::RefCell;

    use adw::prelude::*;
    use adw::subclass::prelude::*;
    use gtk::glib;

    use crate::ui::callback::Callback;

    use super::OnChosen;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/tag_suggestions.ui")]
    pub struct TagSuggestions {
        #[template_child]
        pub list: TemplateChild<gtk::ListBox>,
        /// Called with the row index of a row the user clicked or tapped.
        pub on_chosen: RefCell<Option<Callback<OnChosen>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TagSuggestions {
        const NAME: &'static str = "RappelTagSuggestions";
        type Type = super::TagSuggestions;
        type ParentType = gtk::Popover;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TagSuggestions {
        fn constructed(&self) {
            self.parent_constructed();
            let popover = self.obj();
            self.list.connect_row_activated(glib::clone!(
                #[weak]
                popover,
                move |_, row| {
                    let index = usize::try_from(row.index()).ok();
                    let callback = popover.imp().on_chosen.borrow().as_ref().map(Callback::get);
                    if let (Some(index), Some(callback)) = (index, callback) {
                        callback(index);
                    }
                }
            ));
        }
    }
    impl WidgetImpl for TagSuggestions {}
    impl PopoverImpl for TagSuggestions {}
}

glib::wrapper! {
    pub struct TagSuggestions(ObjectSubclass<imp::TagSuggestions>)
        @extends gtk::Popover, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::ShortcutManager;
}

impl Default for TagSuggestions {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TagSuggestions {
    /// Calls `f` with the index of the suggestion the user clicked.
    pub fn connect_chosen(&self, f: impl Fn(usize) + 'static) {
        let f: Rc<OnChosen> = Rc::new(f);
        self.imp().on_chosen.replace(Some(Callback::new(f)));
    }

    /// Replaces the rows with `names`, none highlighted. Each is shown on
    /// one line (`ui::text::one_line`): an imported name can hold a line
    /// break or a tab.
    pub fn set_names(&self, names: &[&str]) {
        let list = &self.imp().list;
        list.remove_all();
        for name in names {
            let name = crate::ui::text::one_line(name);
            let label = gtk::Label::builder()
                .label(&name)
                .use_markup(false)
                .single_line_mode(true)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let row = gtk::ListBoxRow::builder()
                .child(&label)
                .focusable(false)
                .build();
            row.update_property(&[gtk::accessible::Property::Label(&name)]);
            list.append(&row);
        }
    }

    /// Highlights row `index`, or none.
    pub fn highlight(&self, index: Option<usize>) {
        let list = &self.imp().list;
        match index.and_then(|i| list.row_at_index(i32::try_from(i).ok()?)) {
            Some(row) => list.select_row(Some(&row)),
            None => list.unselect_all(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_are_plain_single_lines() {
        crate::testing::with_widgets(|| {
            let popover = TagSuggestions::default();
            popover.set_names(&["two\nlines", "<b>x</b> &amp;", "tab\there"]);
            let list = &popover.imp().list;
            let labels: Vec<gtk::Label> = (0..3)
                .filter_map(|i| list.row_at_index(i))
                .filter_map(|row| row.child().and_downcast::<gtk::Label>())
                .collect();
            let texts: Vec<String> = labels.iter().map(|l| l.label().to_string()).collect();
            assert_eq!(texts, ["two lines", "<b>x</b> &amp;", "tab here"]);
            for label in &labels {
                assert!(!label.uses_markup());
                assert!(label.is_single_line_mode());
            }
        });
    }
}
