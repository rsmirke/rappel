//! Following an `AdwBreakpoint` in code, for what its setters cannot do
//! (a property of the page that its rows follow, a style class).

use std::rc::Rc;

/// Calls `f(true)` each time `breakpoint` starts to apply and `f(false)`
/// each time it stops. libadwaita emits `apply` and `unapply` as the
/// condition starts and stops matching, the first time too when the
/// widget is first sized, and after the breakpoint's setters (`AdwBreakpoint`
/// docs). `f` should hold its widget weakly (`glib::clone!` with `#[weak]`).
pub fn follow(breakpoint: &adw::Breakpoint, f: impl Fn(bool) + 'static) {
    let f = Rc::new(f);
    let applied = Rc::clone(&f);
    breakpoint.connect_apply(move |_| applied(true));
    breakpoint.connect_unapply(move |_| f(false));
}

/// Makes `status` `compact` (libadwaita's style class for a status page in
/// a small space: less spacing) without its icon while `breakpoint`
/// applies, and gives `icon` back after, so a short window (a phone held
/// sideways, 640 × 360) shows the page's text and button rather than a
/// large icon (ADR 0045, 0052, 0058). A setter can neither add a style
/// class nor unset the icon, hence the code.
pub fn compact_status_page(
    breakpoint: &adw::Breakpoint,
    status: &adw::StatusPage,
    icon: &'static str,
) {
    use adw::prelude::*;
    let status = status.downgrade();
    follow(breakpoint, move |short| {
        if let Some(status) = status.upgrade() {
            if short {
                status.add_css_class("compact");
            } else {
                status.remove_css_class("compact");
            }
            status.set_icon_name((!short).then_some(icon));
        }
    });
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use adw::prelude::*;

    use super::*;

    #[test]
    fn apply_and_unapply_reach_the_closure() {
        crate::testing::with_widgets(|| {
            let breakpoint =
                adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 400sp").unwrap());
            let seen = Rc::new(RefCell::new(Vec::new()));
            let sink = Rc::clone(&seen);
            follow(&breakpoint, move |on| sink.borrow_mut().push(on));
            breakpoint.emit_by_name::<()>("apply", &[]);
            breakpoint.emit_by_name::<()>("unapply", &[]);
            breakpoint.emit_by_name::<()>("apply", &[]);
            assert_eq!(*seen.borrow(), [true, false, true]);
        });
    }

    #[test]
    fn a_status_page_is_compact_without_its_icon_while_short() {
        crate::testing::with_widgets(|| {
            let breakpoint =
                adw::Breakpoint::new(adw::BreakpointCondition::parse("max-height: 450sp").unwrap());
            let status = adw::StatusPage::builder()
                .icon_name("dialog-warning-symbolic")
                .build();
            compact_status_page(&breakpoint, &status, "dialog-warning-symbolic");
            breakpoint.emit_by_name::<()>("apply", &[]);
            assert!(status.has_css_class("compact"));
            assert_eq!(status.icon_name(), None);
            breakpoint.emit_by_name::<()>("unapply", &[]);
            assert!(!status.has_css_class("compact"));
            assert_eq!(
                status.icon_name().as_deref(),
                Some("dialog-warning-symbolic")
            );
        });
    }
}
