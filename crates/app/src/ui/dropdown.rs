//! Drop-downs over a list of names.

use gtk::glib;
use gtk::prelude::*;

/// Shows `labels` in `dropdown`, as plain text (they are the user's deck and
/// tag names), and makes the search in its popup work: `GtkDropDown` searches
/// by `expression`, which a model set by hand does not have
/// (`GtkDropDown:enable-search`).
pub fn set_labels(dropdown: &gtk::DropDown, labels: &[String]) {
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    dropdown.set_model(Some(&gtk::StringList::new(&refs)));
    if dropdown.expression().is_none() {
        set_expression(dropdown);
    }
}

/// Searches the popup by each entry's string. Setting an expression puts
/// GTK's own factories back on the drop-down and its popup (seen with GTK
/// 4.24; `the_factories_survive_new_labels_and_the_popup_stays_narrow`),
/// so it is set once, before [`shrinkable`]'s factories.
fn set_expression(dropdown: &gtk::DropDown) {
    dropdown.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
}

/// Lets `dropdown` be narrower than its chosen entry: the button shows the
/// entry on one line ending in "…" when it does not fit, and the popup
/// keeps every entry whole (wrapped past [`POPUP_CHARS`]), with a check mark on the chosen one as
/// `GtkDropDown`'s own factory has. That factory makes a label that does
/// not ellipsize, so the button would be at least as wide as the entry
/// (gtkdropdown.c, `setup_item`); `factory` is used for the button and
/// `list-factory` for the popup (`GtkDropDown:list-factory` docs). The
/// entries must be `GtkStringObject`s (a `GtkStringList`, as [`set_labels`]
/// makes). Plain text, never markup.
pub fn shrinkable(dropdown: &gtk::DropDown) {
    if dropdown.expression().is_none() {
        set_expression(dropdown);
    }
    dropdown.set_factory(Some(&button_factory()));
    dropdown.set_list_factory(Some(&popup_factory(dropdown)));
}

/// A plain label for a `GtkStringObject`, on one line.
fn string_label() -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.0)
        .use_markup(false)
        .single_line_mode(true)
        .build()
}

/// The widest an entry of the popup gets, in characters, before it wraps.
/// Deck names can be 1 KiB and tag names 256 bytes: unwrapped, the popup
/// would be wider than any window. Thirty characters and the popup's
/// padding fit a 360 px window.
const POPUP_CHARS: i32 = 30;

/// A popup entry: whole. One of at most [`POPUP_CHARS`] characters stays
/// on one line, so the popup is as wide as it; a longer one wraps at about
/// that width, between words or, in a longer word, between characters
/// ([`set_popup_text`]).
fn popup_label() -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.0)
        .use_markup(false)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(POPUP_CHARS)
        .hexpand(true)
        .build()
}

/// Whether a popup entry of `text` wraps: past [`POPUP_CHARS`] characters.
fn wraps_in_popup(text: &str) -> bool {
    text.chars().count() > usize::try_from(POPUP_CHARS).unwrap_or(usize::MAX)
}

/// Shows `text` in a [`popup_label`]. The popup is as wide as its widest
/// entry's minimum width (GTK's own entries never wrap, so their minimum is
/// their whole width): a short entry does not wrap, and a long one wraps
/// with a minimum of [`POPUP_CHARS`] characters, so it neither widens the
/// popup past that nor makes it as narrow as its longest word.
fn set_popup_text(label: &gtk::Label, text: &str) {
    let wraps = wraps_in_popup(text);
    label.set_wrap(wraps);
    label.set_width_chars(if wraps { POPUP_CHARS } else { -1 });
    label.set_label(text);
}

fn bind_label(item: &gtk::ListItem, label: &gtk::Label) {
    if let Some(string) = item.item().and_downcast::<gtk::StringObject>() {
        label.set_label(&string.string());
    }
}

/// The button: the chosen entry, ellipsized.
fn button_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = string_label();
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(label) = item.child().and_downcast::<gtk::Label>() {
            bind_label(item, &label);
        }
    });
    factory
}

/// Whether the popup's check mark shows on `entry`: it is the drop-down's
/// `chosen` item (none is never chosen). Compared by item, as GTK's own
/// factory does (gtkdropdown.c, `selected_item_changed`), not by
/// `GtkListItem:selected`, which follows the pointer and the arrow keys in
/// the popup, nor by position, which a search in the popup changes.
fn same_item(chosen: Option<&glib::Object>, entry: Option<&glib::Object>) -> bool {
    entry.is_some() && chosen == entry
}

/// The popup: every entry whole, a check mark on the chosen one (shown by
/// opacity, as GTK's, so the popup keeps its width).
///
/// The check mark's opacity is bound once per row, when the row is made,
/// to an expression of the drop-down's `selected-item` and the row's
/// `item`: GTK re-evaluates it when either changes (`gtk_expression_bind`
/// docs), so a recycled row follows its new item with no handler added on
/// a bind. The drop-down is held weakly (`GtkObjectExpression` docs: "meant
/// to break reference cycles"), and the watch goes with the check image,
/// so rows that are dropped leave nothing connected to the drop-down.
fn popup_factory(dropdown: &gtk::DropDown) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let weak = dropdown.downgrade();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let label = popup_label();
        row.append(&label);
        let check = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .opacity(0.0)
            .build();
        row.append(&check);
        item.set_child(Some(&row));
        if let Some(dropdown) = weak.upgrade() {
            let chosen = gtk::PropertyExpression::new(
                gtk::DropDown::static_type(),
                Some(gtk::ObjectExpression::new(&dropdown)),
                "selected-item",
            );
            let entry = gtk::PropertyExpression::new(
                gtk::ListItem::static_type(),
                None::<gtk::Expression>,
                "item",
            );
            let opacity = gtk::ClosureExpression::with_callback([chosen, entry], |values| {
                // values[0] is the row's list item (`this`), then the
                // drop-down's choice and the row's entry.
                let object = |i: usize| {
                    values
                        .get(i)
                        .and_then(|v| v.get::<Option<glib::Object>>().ok())
                        .flatten()
                };
                check_opacity(same_item(object(1).as_ref(), object(2).as_ref()))
            });
            opacity.bind(&check, "opacity", Some(item));
        }
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(label) = item
            .child()
            .and_then(|row| row.first_child())
            .and_downcast::<gtk::Label>()
            && let Some(string) = item.item().and_downcast::<gtk::StringObject>()
        {
            set_popup_text(&label, &string.string());
        }
    });
    factory
}

/// The check mark's opacity: shown on the chosen entry only.
fn check_opacity(chosen: bool) -> f64 {
    if chosen { 1.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check mark is on the drop-down's chosen item, wherever it is in
    /// the popup, and on nothing when nothing is chosen.
    #[test]
    fn the_check_mark_is_on_the_chosen_entry() {
        crate::testing::with_widgets(|| {
            let dropdown = gtk::DropDown::builder().build();
            set_labels(
                &dropdown,
                &["All".to_owned(), "a".to_owned(), "b".to_owned()],
            );
            let model = dropdown.model().expect("set_labels sets a model");
            dropdown.set_selected(2);
            let chosen = dropdown.selected_item();
            assert!(same_item(chosen.as_ref(), model.item(2).as_ref()));
            assert!(!same_item(chosen.as_ref(), model.item(1).as_ref()));
            assert!(!same_item(None, model.item(1).as_ref()));
            assert!(!same_item(None, None));
        });
    }

    /// A long name wraps in the popup instead of widening it: a 1 KiB
    /// deck name is no wider than 31 characters, and asks for 30 at least.
    /// A short one stays on one line.
    #[test]
    fn a_long_entry_wraps_in_the_popup() {
        crate::testing::with_widgets(|| {
            let widths = |text: &str| {
                let label = popup_label();
                set_popup_text(&label, text);
                let (min, natural, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
                (min, natural, label.wraps())
            };
            let (_, limit, _) = widths(&"w".repeat(31));
            let (min, natural, wraps) = widths(&"wide ".repeat(205));
            assert!(wraps && natural <= limit, "{natural} > {limit}");
            assert!(min > limit / 2, "{min}: the popup would be too narrow");
            let (_, natural, wraps) = widths(&"W".repeat(1_024));
            assert!(wraps && natural <= widths(&"W".repeat(31)).1, "{natural}");
            let (min, natural, wraps) = widths("Markdown (one per construct)");
            assert!(
                !wraps && min == natural,
                "a short name stays whole on one line"
            );
            assert!(!wraps_in_popup(&"é".repeat(30)) && wraps_in_popup(&"é".repeat(31)));
            assert!(!popup_label().uses_markup());
        });
    }

    /// The factories of [`shrinkable`] are still those of the drop-down
    /// and of its own popup after new labels, and a long entry wraps in
    /// that popup: it is no wider than about 30 characters.
    #[test]
    fn the_factories_survive_new_labels_and_the_popup_stays_narrow() {
        crate::testing::with_widgets(|| {
            let dropdown = gtk::DropDown::builder().enable_search(true).build();
            shrinkable(&dropdown);
            let (button, popup) = (dropdown.factory(), dropdown.list_factory());
            assert!(button.is_some() && popup.is_some());
            for _ in 0..2 {
                set_labels(
                    &dropdown,
                    &["All".to_owned(), "wide ".repeat(205), "short".to_owned()],
                );
                assert_eq!(dropdown.factory(), button);
                assert_eq!(dropdown.list_factory(), popup);
            }
            let window = gtk::Window::builder()
                .default_width(600)
                .default_height(500)
                .child(&dropdown)
                .build();
            window.present();
            dropdown.activate();
            let popover = || descendant::<gtk::Popover>(dropdown.upcast_ref());
            assert!(crate::testing::settle_until(
                || popover().is_some_and(|p| p.is_mapped()),
                std::time::Duration::from_secs(5)
            ));
            let list = descendant::<gtk::ListView>(dropdown.upcast_ref())
                .expect("the popup lists the entries");
            assert_eq!(list.factory(), popup);
            // The rows' natural width: the label, its check mark and the
            // row's padding (the popover is at least as wide as the
            // drop-down's button besides). Unwrapped, it was 2,100 px.
            let width = list.measure(gtk::Orientation::Horizontal, -1).1;
            let label = popup_label();
            label.set_label(&"W".repeat(31));
            let limit = label.measure(gtk::Orientation::Horizontal, -1).1;
            assert!(
                width < limit + 100,
                "the rows are {width} px wide, {limit} px for 31 W"
            );
            window.destroy();
        });
    }

    /// The first `T` under `widget`, depth first.
    fn descendant<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<T> {
        if let Some(found) = widget.downcast_ref::<T>() {
            return Some(found.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = descendant::<T>(&c) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    /// Thousands of tags fill the filter at once.
    #[test]
    fn thousands_of_entries_are_set_quickly() {
        crate::testing::with_widgets(|| {
            let names: Vec<String> = (0..10_000).map(|i| format!("tag {i}")).collect();
            let dropdown = gtk::DropDown::builder().enable_search(true).build();
            shrinkable(&dropdown);
            let (cpu, ()) = crate::testing::cpu_time_of(|| set_labels(&dropdown, &names));
            assert_eq!(dropdown.model().map(|m| m.n_items()), Some(10_000));
            crate::testing::print_timing("drop-down: 10,000 entries", cpu);
            crate::testing::assert_cpu_within(
                cpu,
                std::time::Duration::from_millis(50),
                "set_labels",
            );
        });
    }

    /// All the check images under `widget`, in order.
    fn checks(widget: &gtk::Widget) -> Vec<gtk::Image> {
        let mut found = Vec::new();
        if let Some(image) = widget.downcast_ref::<gtk::Image>() {
            found.push(image.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            found.extend(checks(&c));
            child = c.next_sibling();
        }
        found
    }

    /// The popup's rows, made by the factory in a shown list: the check
    /// mark follows the drop-down's choice and the rows' items, through new
    /// models too, and once the rows are gone nothing is left connected to
    /// the drop-down.
    #[test]
    fn the_popup_rows_follow_the_choice_and_leave_no_handler() {
        crate::testing::with_widgets(|| {
            let names = |n: usize| (0..n).map(|i| format!("entry {i}")).collect::<Vec<_>>();
            let dropdown = gtk::DropDown::builder().build();
            set_labels(&dropdown, &names(3));
            let pending = |dropdown: &gtk::DropDown| {
                glib::signal::signal_has_handler_pending(
                    dropdown,
                    glib::subclass::signal::SignalId::lookup("notify", glib::Object::static_type())
                        .expect("GObject has notify"),
                    Some(glib::Quark::from_str("selected-item")),
                    true,
                )
            };
            assert!(!pending(&dropdown));
            let list = gtk::ListView::new(None::<gtk::NoSelection>, Some(popup_factory(&dropdown)));
            let show = |dropdown: &gtk::DropDown, list: &gtk::ListView| {
                let model = dropdown.model().expect("set_labels sets a model");
                list.set_model(Some(&gtk::NoSelection::new(Some(model))));
                assert!(crate::testing::settle_until(
                    || checks(list.upcast_ref()).len() >= 3,
                    std::time::Duration::from_secs(5)
                ));
            };
            let window = gtk::Window::builder()
                .default_width(300)
                .default_height(300)
                .child(&list)
                .build();
            window.present();
            let opacities = |list: &gtk::ListView| -> Vec<f64> {
                checks(list.upcast_ref())
                    .iter()
                    .map(WidgetExt::opacity)
                    .collect()
            };
            show(&dropdown, &list);
            dropdown.set_selected(1);
            assert_eq!(opacities(&list), [0.0, 1.0, 0.0]);
            dropdown.set_selected(2);
            assert_eq!(opacities(&list), [0.0, 0.0, 1.0]);
            // New models (a save, an import): the recycled rows follow.
            for _ in 0..5 {
                set_labels(&dropdown, &names(3));
                dropdown.set_selected(0);
                show(&dropdown, &list);
            }
            assert_eq!(opacities(&list), [1.0, 0.0, 0.0]);
            assert_eq!(checks(list.upcast_ref()).len(), 3, "one check per row");
            // The rows' watches are on the drop-down while they live...
            assert!(pending(&dropdown));
            let one = checks(list.upcast_ref())[0].downgrade();
            let lw = list.downgrade();
            window.destroy();
            drop(window);
            drop(list);
            assert!(
                crate::testing::settle_until(
                    || one.upgrade().is_none() && lw.upgrade().is_none(),
                    std::time::Duration::from_secs(5)
                ),
                "the rows are gone"
            );
            // ...and go with them.
            assert!(
                !pending(&dropdown),
                "a row's watch is still on the drop-down"
            );
        });
    }
}
