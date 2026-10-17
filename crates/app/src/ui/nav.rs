//! The `AdwNavigationView` that pages are pushed onto.
//!
//! Focus after a push: a `grab_focus()` before or during the push is lost,
//! and the view then focuses the first focusable widget (usually Back).
//! Set the focus in the pushed page's `shown` handler instead.

use adw::prelude::*;

/// The navigation view that contains `widget`, or `None` after logging:
/// every page lives in the window's view, so that is a bug.
pub fn navigation_view(widget: &impl IsA<gtk::Widget>) -> Option<adw::NavigationView> {
    let view = widget
        .ancestor(adw::NavigationView::static_type())
        .and_downcast::<adw::NavigationView>();
    if view.is_none() {
        tracing::error!("{} is not inside a navigation view", widget.type_().name());
    }
    view
}

/// Goes back from the page that contains `widget`.
pub fn pop(widget: &impl IsA<gtk::Widget>) {
    if let Some(view) = navigation_view(widget) {
        view.pop();
    }
}

/// Pushes the page `build` makes onto `view`, unless the visible page is
/// already a `P` (or a subclass): key repeat (Enter or a shortcut held) or a double click
/// can activate the same thing again before the first push shows, and two
/// deck pages, sessions or All Cards pages must never stack (#20). `build`
/// runs only when the page is pushed, so a repeat reads nothing.
///
/// Returns whether it pushed. The check holds at once: `push` appends to
/// the navigation stack and `visible_page` is its last item, both
/// synchronous (`push_to_stack` and `adw_navigation_view_get_visible_page`
/// in `adw-navigation-view.c`, libadwaita 1.8 and 1.9).
pub fn push_unless_on_top<P, E>(
    view: &adw::NavigationView,
    build: impl FnOnce() -> Result<P, E>,
) -> Result<bool, E>
where
    P: IsA<adw::NavigationPage>,
{
    if view.visible_page().is_some_and(|page| page.is::<P>()) {
        return Ok(false);
    }
    view.push(&build()?);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use adw::subclass::prelude::*;
    use gtk::glib;

    use super::*;

    mod imp {
        use super::*;

        #[derive(Debug, Default)]
        pub struct OtherPage;

        #[glib::object_subclass]
        impl ObjectSubclass for OtherPage {
            const NAME: &'static str = "RappelNavTestOtherPage";
            type Type = super::OtherPage;
            type ParentType = adw::NavigationPage;
        }

        impl ObjectImpl for OtherPage {}
        impl WidgetImpl for OtherPage {}
        impl NavigationPageImpl for OtherPage {}
    }

    glib::wrapper! {
        /// A page type of its own, to stand on top of a plain page.
        pub struct OtherPage(ObjectSubclass<imp::OtherPage>)
            @extends adw::NavigationPage, gtk::Widget,
            @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
    }

    #[test]
    fn a_page_of_the_type_on_top_is_not_pushed_again() {
        crate::testing::with_widgets(|| {
            let view = adw::NavigationView::new();
            view.push(&adw::NavigationPage::builder().title("Root").build());
            let made = Cell::new(0);
            let build = || {
                made.set(made.get() + 1);
                Ok::<_, ()>(glib::Object::new::<OtherPage>())
            };
            // Another type on top: pushed.
            assert_eq!(push_unless_on_top(&view, build), Ok(true));
            // The same type on top: not pushed, and nothing built.
            assert_eq!(push_unless_on_top(&view, build), Ok(false));
            assert_eq!(push_unless_on_top(&view, build), Ok(false));
            assert_eq!(made.get(), 1);
            assert_eq!(view.navigation_stack().n_items(), 2);
            assert!(view.visible_page().is_some_and(|p| p.is::<OtherPage>()));

            // Back on the root: pushed again.
            view.pop();
            assert_eq!(push_unless_on_top(&view, build), Ok(true));
            assert_eq!(made.get(), 2);
        });
    }

    #[test]
    fn a_failed_build_pushes_nothing() {
        crate::testing::with_widgets(|| {
            let view = adw::NavigationView::new();
            assert_eq!(
                push_unless_on_top::<adw::NavigationPage, _>(&view, || Err("unreadable")),
                Err("unreadable")
            );
            assert_eq!(view.visible_page(), None);
        });
    }
}
