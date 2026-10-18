//! Full-window pages. Each is a `.rs` file with its layout in `data/*.blp`.

mod browser_page;
mod deck_page;
mod decks_page;
mod recovery_page;
mod review_page;
mod stats_page;

pub use browser_page::BrowserPage;
pub(crate) use browser_page::text as browser_text;
pub use deck_page::DeckPage;
pub use decks_page::DecksPage;
/// A data folder as an interrupted restore leaves it, for the window's tests.
#[cfg(test)]
pub(crate) use recovery_page::tests::interrupted as interrupted_restore_folder;
pub use recovery_page::{Recovered, RecoveryPage};
pub use review_page::ReviewPage;
#[cfg(test)]
pub(crate) use review_page::key_triggers as review_key_triggers;
pub use stats_page::StatsPage;
