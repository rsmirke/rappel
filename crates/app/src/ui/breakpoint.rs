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
}
