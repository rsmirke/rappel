//! Small helpers shared by the pages, dialogs and rows: toasts, alert
//! dialogs, drop-downs, navigation, row menus, focus, one-time cells, texts,
//! dates, headline tiles, debouncing, worker threads and what an undo that failed means. No layouts.
//! Use these instead of writing a local copy.

pub mod callback;
pub mod debounce;
pub mod dialog;
pub mod dropdown;
pub mod focus;
pub mod nav;
pub mod rows;
pub mod text;
pub mod tile;
pub mod time;
pub mod toast;
pub mod undo;
pub mod worker;

use std::cell::OnceCell;

/// Sets a cell of a widget being constructed. Its cells are still empty,
/// so this cannot fail; if it ever does, the old value is kept and the
/// error is logged instead of panicking.
pub fn init_once<T>(cell: &OnceCell<T>, value: T) {
    if cell.set(value).is_err() {
        tracing::error!("a one-time cell was set twice; keeping the first value");
    }
}
