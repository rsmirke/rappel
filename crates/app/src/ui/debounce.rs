//! Running something once input has paused, such as the card editor's
//! preview: each new keystroke restarts the wait, so typing fast in a long
//! card does not re-render it on every key.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;

/// One pending run at most. Cloning shares the pending run.
#[derive(Debug, Default, Clone)]
pub struct Debounce {
    pending: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Debounce {
    /// Runs `f` on the thread's main context after `delay`, dropping the
    /// run still waiting from an earlier call, if any.
    pub fn restart(&self, delay: Duration, f: impl FnOnce() + 'static) {
        self.cancel();
        let pending = Rc::clone(&self.pending);
        let id = glib::timeout_add_local_once(delay, move || {
            // Forgotten before `f` runs, so `f` may restart it, and so the
            // source (removed by GLib once run) is never removed again.
            pending.borrow_mut().take();
            f();
        });
        self.pending.replace(Some(id));
    }

    /// Drops the run still waiting, if any.
    pub fn cancel(&self) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// Runs the default main context, as the app's main loop would, until
    /// `done`. Fails after a few seconds rather than hanging the test run
    /// if the awaited source never fires.
    fn run_until(done: impl Fn() -> bool) {
        assert!(
            crate::testing::settle_until(done, Duration::from_secs(5)),
            "timed out waiting for the main context"
        );
    }

    /// `Debounce` uses the process-wide default main context
    /// (`glib::timeout_add_local_once`), which one thread at a time may own,
    /// and tests run on parallel threads: the body runs on the thread that
    /// owns it (`crate::testing`).
    #[test]
    fn debounce_on_the_default_main_context() {
        crate::testing::on_main_context(|| {
            only_the_last_restart_runs();
            a_cancelled_run_never_happens();
            a_run_may_restart_itself();
        });
    }

    fn only_the_last_restart_runs() {
        let runs = Rc::new(RefCell::new(Vec::new()));
        let debounce = Debounce::default();
        for i in 0..3 {
            let runs = Rc::clone(&runs);
            debounce.restart(Duration::from_millis(5), move || runs.borrow_mut().push(i));
        }
        run_until(|| !runs.borrow().is_empty());
        // Nothing else is waiting.
        assert!(debounce.pending.borrow().is_none());
        assert_eq!(*runs.borrow(), [2]);
    }

    fn a_cancelled_run_never_happens() {
        let ran = Rc::new(Cell::new(false));
        let debounce = Debounce::default();
        let flag = Rc::clone(&ran);
        debounce.restart(Duration::from_millis(1), move || flag.set(true));
        debounce.cancel();
        // A later run proves the context ran past the cancelled one.
        let later = Rc::new(Cell::new(false));
        let flag = Rc::clone(&later);
        glib::timeout_add_local_once(Duration::from_millis(10), move || flag.set(true));
        run_until(|| later.get());
        assert!(!ran.get());
    }

    fn a_run_may_restart_itself() {
        let count = Rc::new(Cell::new(0));
        let debounce = Debounce::default();
        let (again, counter) = (debounce.clone(), Rc::clone(&count));
        debounce.restart(Duration::from_millis(1), move || {
            counter.set(counter.get() + 1);
            let counter = Rc::clone(&counter);
            again.restart(Duration::from_millis(1), move || {
                counter.set(counter.get() + 1);
            });
        });
        run_until(|| count.get() == 2);
        assert!(debounce.pending.borrow().is_none());
    }
}
