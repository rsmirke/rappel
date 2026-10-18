//! Dialogs. Each is a `.rs` file with its layout in `data/*.blp`.

pub mod about;
pub mod browser_dialogs;
pub mod card_editor;
pub mod card_info;
pub mod clean_up;
pub mod deck_name_dialog;
pub mod preferences;
#[cfg(test)]
pub(crate) mod shortcuts;
