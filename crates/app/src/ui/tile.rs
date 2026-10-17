//! A headline figure in a tile (a `card` box with a big value over a
//! dimmed caption), as Statistics and the review page's summary show them.
//! The layout is in their Blueprint files; this fills one in.

use gtk::prelude::*;

/// One headline figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    /// The big text, "91%" or "—" without data.
    pub value: String,
    pub caption: String,
    /// What a screen reader says for the whole tile.
    pub accessible: String,
}

/// Shows `tile` in its `value` and `caption` labels. The screen reader's
/// reading of both goes on the value label and the caption is hidden from
/// it, so the tile is read once, and "—" can be read as words ("no
/// reviews").
pub fn set_tile(value: &gtk::Label, caption: &gtk::Label, tile: &Tile) {
    value.set_label(&tile.value);
    caption.set_label(&tile.caption);
    value.update_property(&[gtk::accessible::Property::Label(&tile.accessible)]);
    caption.update_state(&[gtk::accessible::State::Hidden(true)]);
}
