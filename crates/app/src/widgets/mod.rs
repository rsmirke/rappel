//! Reusable widgets. Each is a `.rs` file; the row widgets have their
//! layout in `data/*.blp`, and `CardObject` is a list item with none.

mod backup_row;
pub mod bar_chart;
mod browse_item;
mod card_object;
pub mod card_row;
pub mod chart_card;
pub mod deck_row;
mod history_row;
pub mod tag_entry;
pub mod tag_pills;
mod tag_suggestions;

pub use backup_row::BackupRow;
pub use browse_item::BrowseItem;
pub use card_object::CardObject;
pub use card_row::CardRow;
pub use deck_row::DeckRow;
pub use history_row::HistoryRow;
pub use tag_entry::TagEntry;
pub use tag_pills::TagPills;
pub use tag_suggestions::TagSuggestions;

/// Registers every widget type of this module with `GObject`, so a
/// `GtkBuilder` can make them by name. A composite template registers the
/// types it names in its `class_init`; a plain layout loaded by
/// `ui::dialog::builder` has nothing that does, and `GtkBuilder` aborts on an
/// unknown type name ("Invalid object type"). Called at startup and by
/// `ui::dialog::builder`; cheap after the first call.
pub fn register_types() {
    use gtk::prelude::StaticTypeExt;

    BackupRow::ensure_type();
    bar_chart::BarChart::ensure_type();
    BrowseItem::ensure_type();
    CardObject::ensure_type();
    CardRow::ensure_type();
    chart_card::ChartCard::ensure_type();
    DeckRow::ensure_type();
    HistoryRow::ensure_type();
    TagEntry::ensure_type();
    TagPills::ensure_type();
    TagSuggestions::ensure_type();
}
