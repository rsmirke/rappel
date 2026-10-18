//! `AppState`: the one owner of the open database. The UI calls its methods
//! and never sees SQL or rusqlite (ADR 0011).

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use rappel_core::domain::{
    Card, CardState, Deck, DeckSummary, Grade, ReviewEvent, SettingKey, Settings, Tag, TaggedCard,
};
use rappel_core::interchange::{ExportSummary, ImportPlan, ImportReport};
use rappel_core::media::MediaRef;
use rappel_core::scheduling::StudyInput;
use rappel_core::stats::{CardInfo, DayGrid, Stats};
use rappel_core::storage::{
    self, BackupInfo, BrowseQuery, BrowseRow, Moved, OpenWarning, Restored, Scope, Store, TagsAdded,
};

use crate::clock::{local_day_bounds_ms, local_day_start_ms, local_next_day_start_ms};

/// How many past days the statistics cover, today last (ADR 0032).
pub const STATS_PAST_DAYS: usize = 30;
/// How many days ahead the due forecast covers, today first.
pub const STATS_AHEAD_DAYS: usize = 14;

/// The statistics of a deck or of everything, with the study days they
/// are bucketed by.
#[derive(Debug, Clone, PartialEq)]
pub struct StatsData {
    pub stats: Stats,
    /// Where each of the past days begins, UTC milliseconds
    /// (`STATS_PAST_DAYS` of them, oldest first).
    pub past_starts: Vec<i64>,
    /// Where each of the days ahead begins, today first
    /// (`STATS_AHEAD_DAYS` of them).
    pub ahead_starts: Vec<i64>,
}

/// A cheap-to-clone handle on the open database, shared by the window and
/// its widgets.
///
/// `Rc` because GTK runs every widget on the main thread, so there is no
/// second thread to share with. `RefCell` because writes need `&mut Store`.
/// Every method borrows the store and releases it before returning; none
/// returns a borrow or calls back into GTK while holding one, so a borrow
/// can never fail at run time.
#[derive(Clone)]
pub struct AppState(Rc<Inner>);

struct Inner {
    data_dir: PathBuf,
    store: RefCell<Store>,
}

impl fmt::Debug for AppState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppState")
            .field("data_dir", &self.0.data_dir)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Opens the database in `data_dir` (see [`Store::open`]).
    pub fn open(data_dir: PathBuf) -> rappel_core::Result<Self> {
        let store = Store::open(&data_dir)?;
        Ok(Self::new(data_dir, store))
    }

    /// Wraps a store that is already open, e.g. after a restore.
    pub fn new(data_dir: PathBuf, store: Store) -> Self {
        Self(Rc::new(Inner {
            data_dir,
            store: RefCell::new(store),
        }))
    }

    /// The backups in `data_dir`, newest first. An associated function: it
    /// is used by the recovery page, before any database is open.
    pub fn list_backups(data_dir: &Path) -> rappel_core::Result<Vec<BackupInfo>> {
        storage::list_backups(data_dir)
    }

    /// Restores `backup` in `data_dir` when no database could be opened
    /// (see [`storage::restore_backup`]). Blocking; runs on a worker thread.
    pub fn restore_unopened(data_dir: &Path, backup: &BackupInfo) -> rappel_core::Result<Restored> {
        storage::restore_backup(data_dir, None, backup)
    }

    /// Installs the checked copy an interrupted restore left waiting in
    /// `data_dir` (see [`storage::finish_restore`]). Blocking; runs on a
    /// worker thread.
    pub fn finish_restore_unopened(data_dir: &Path) -> rappel_core::Result<Restored> {
        storage::finish_restore(data_dir, None)
    }

    /// Moves the database in `data_dir` aside and starts an empty one, when
    /// none could be opened (see [`storage::start_fresh`]). Blocking; runs
    /// on a worker thread.
    pub fn start_fresh_unopened(data_dir: &Path) -> rappel_core::Result<Restored> {
        storage::start_fresh(data_dir, None)
    }

    /// Puts the default back for the damaged stored setting `key` in
    /// `data_dir`, after a backup, and opens the database (see
    /// [`storage::reset_setting`], ADR 0052). Blocking; runs on a worker
    /// thread, before any database is open.
    pub fn reset_setting_unopened(
        data_dir: &Path,
        key: SettingKey,
    ) -> rappel_core::Result<storage::SettingReset> {
        storage::reset_setting(data_dir, key)
    }

    /// Exports the database in `data_dir` to `path` (see
    /// [`Store::export_to`]). Blocking; runs on a worker thread, on its own
    /// connection ([`Store::open_concurrent`], ADR 0018), so the window's
    /// store stays on the main thread.
    pub fn export_on_worker(data_dir: &Path, path: &Path) -> rappel_core::Result<ExportSummary> {
        Store::open_concurrent(data_dir)?.export_to(path)
    }

    /// Checks the `.rappel` file `path` against the database in `data_dir`
    /// (see [`Store::plan_import`]). Blocking; on its own connection like
    /// [`Self::export_on_worker`].
    pub fn plan_import_on_worker(data_dir: &Path, path: &Path) -> rappel_core::Result<ImportPlan> {
        Store::open_concurrent(data_dir)?.plan_import(path)
    }

    /// Applies `plan` to the database in `data_dir` (see
    /// [`Store::apply_import`]). Blocking; on its own connection like
    /// [`Self::export_on_worker`]. A change made meanwhile on the main
    /// connection refuses it with `ImportPlanStale`, and a change to the
    /// file since it was checked with `ImportFileChanged`.
    pub fn apply_import_on_worker(
        data_dir: &Path,
        plan: &ImportPlan,
    ) -> rappel_core::Result<ImportReport> {
        Store::open_concurrent(data_dir)?.apply_import(plan)
    }

    /// Sets the desired retention in the database in `data_dir` and rebuilds
    /// every card's schedule from the review log (see
    /// [`Store::set_desired_retention`]). Blocking, as long as the log is
    /// big; runs on a worker thread, on its own connection like
    /// [`Self::export_on_worker`].
    pub fn set_desired_retention_on_worker(data_dir: &Path, value: f32) -> rappel_core::Result<()> {
        Store::open_concurrent(data_dir)?.set_desired_retention(value)
    }

    /// Copies the image `file` gives into the media store of the database
    /// in `data_dir` (see [`Store::add_image`]) and returns its reference.
    /// Blocking (a file of up to 10 MB is hashed, copied and synced); runs
    /// on a worker thread, on its own connection like
    /// [`Self::export_on_worker`]. `file` may be any `GFile` the file
    /// chooser, a drop or the clipboard gives (a portal's too).
    pub fn add_image_on_worker(
        data_dir: &Path,
        file: &gtk::gio::File,
    ) -> rappel_core::Result<MediaRef> {
        use gtk::prelude::*;
        let stream = file
            .read(gtk::gio::Cancellable::NONE)
            .map_err(|err| rappel_core::Error::MediaRead(std::io::Error::other(err)))?;
        Store::open_concurrent(data_dir)?.add_image(stream.into_read())
    }

    /// [`Self::add_image_on_worker`] for bytes (a pasted picture, encoded
    /// as PNG).
    pub fn add_image_bytes_on_worker(
        data_dir: &Path,
        bytes: &[u8],
    ) -> rappel_core::Result<MediaRef> {
        Store::open_concurrent(data_dir)?.add_image(bytes)
    }

    /// The media folder, where card images are read from (it may not
    /// exist yet). `None` for a store without a data directory.
    pub fn media_folder(&self) -> Option<PathBuf> {
        self.0.store.borrow().media_folder().map(Path::to_path_buf)
    }

    /// The data directory the database lives in.
    pub fn data_dir(&self) -> &Path {
        &self.0.data_dir
    }

    /// The live decks with what each has to study at `now`, counted like
    /// the deck page (see [`Store::list_decks`]).
    pub fn list_decks(&self, now: i64) -> rappel_core::Result<Vec<DeckSummary>> {
        let store = self.0.store.borrow();
        let hour = store.settings()?.day_start_hour;
        store.list_decks(now, local_day_start_ms(now, hour))
    }

    /// The live decks, without counting their cards (see [`Store::decks`]).
    /// For names and ids; the deck list's rows use [`Self::list_decks`].
    pub fn decks(&self) -> rappel_core::Result<Vec<Deck>> {
        self.0.store.borrow().decks()
    }

    /// Live deck `id`, without its counts (see [`Store::deck`]).
    pub fn deck(&self, id: &str) -> rappel_core::Result<Deck> {
        self.0.store.borrow().deck(id)
    }

    /// Creates a deck (see [`Store::create_deck`]).
    pub fn create_deck(&self, name: &str) -> rappel_core::Result<Deck> {
        self.0.store.borrow_mut().create_deck(name)
    }

    /// Renames a deck (see [`Store::rename_deck`]).
    pub fn rename_deck(&self, id: &str, name: &str) -> rappel_core::Result<()> {
        self.0.store.borrow_mut().rename_deck(id, name)
    }

    /// Soft-deletes a deck and its cards (see [`Store::delete_deck`]).
    pub fn delete_deck(&self, id: &str) -> rappel_core::Result<()> {
        self.0.store.borrow_mut().delete_deck(id)
    }

    /// Undoes [`Self::delete_deck`] (see [`Store::undelete_deck`]).
    pub fn undelete_deck(&self, id: &str) -> rappel_core::Result<Deck> {
        self.0.store.borrow_mut().undelete_deck(id)
    }

    /// The live cards of a deck, oldest first (see [`Store::list_cards`]).
    pub fn list_cards(&self, deck_id: &str) -> rappel_core::Result<Vec<Card>> {
        self.0.store.borrow().list_cards(deck_id)
    }

    /// The live cards of a deck, oldest first, with their tags (see
    /// [`Store::list_tagged_cards`]).
    pub fn list_tagged_cards(&self, deck_id: &str) -> rappel_core::Result<Vec<TaggedCard>> {
        self.0.store.borrow().list_tagged_cards(deck_id)
    }

    /// Adds a card with exactly these tags, in one write (see
    /// [`Store::add_card_with_tags`]).
    pub fn add_card_with_tags(
        &self,
        deck_id: &str,
        front: &str,
        back: &str,
        tag_names: &[String],
    ) -> rappel_core::Result<TaggedCard> {
        self.0
            .store
            .borrow_mut()
            .add_card_with_tags(deck_id, front, back, tag_names)
    }

    /// Replaces a card's front and back and, with `tag_names`, its tags, in
    /// one write (see [`Store::edit_card_with_tags`]).
    pub fn edit_card_with_tags(
        &self,
        id: &str,
        front: &str,
        back: &str,
        tag_names: Option<&[String]>,
    ) -> rappel_core::Result<TaggedCard> {
        self.0
            .store
            .borrow_mut()
            .edit_card_with_tags(id, front, back, tag_names)
    }

    /// The tags a live card has, for the suggestions and the tag filter
    /// (see [`Store::used_tags`], ADR 0052).
    pub fn used_tags(&self) -> rappel_core::Result<Vec<Tag>> {
        self.0.store.borrow().used_tags()
    }

    /// Every tag, unused ones too (see [`Store::tags`]).
    #[cfg(test)]
    pub fn tags(&self) -> rappel_core::Result<Vec<Tag>> {
        self.0.store.borrow().tags()
    }

    /// The tags of a live card (see [`Store::card_tags`]).
    pub fn card_tags(&self, card_id: &str) -> rappel_core::Result<Vec<Tag>> {
        self.0.store.borrow().card_tags(card_id)
    }

    /// Card `id` (see [`Store::card`]).
    pub fn card(&self, id: &str) -> rappel_core::Result<Card> {
        self.0.store.borrow().card(id)
    }

    /// What a deck has to study at `now`, with the study day starting at
    /// the settings' day start hour, local time (`local_day_start_ms`; see
    /// [`Store::study_input`]).
    pub fn study_input(&self, deck_id: &str, now: i64) -> rappel_core::Result<StudyInput> {
        let store = self.0.store.borrow();
        let hour = store.settings()?.day_start_hour;
        store.study_input(deck_id, now, local_day_start_ms(now, hour))
    }

    /// When the next study day begins after `now`, UTC milliseconds (the
    /// day start hour of the settings, local time).
    pub fn next_day_start(&self, now: i64) -> rappel_core::Result<i64> {
        let hour = self.0.store.borrow().settings()?.day_start_hour;
        Ok(local_next_day_start_ms(now, hour))
    }

    /// The settings (see [`Store::settings`]).
    pub fn settings(&self) -> rappel_core::Result<Settings> {
        self.0.store.borrow().settings()
    }

    /// Sets the new cards per deck and day (see
    /// [`Store::set_max_new_per_day`]).
    pub fn set_max_new_per_day(&self, value: u32) -> rappel_core::Result<()> {
        self.0.store.borrow_mut().set_max_new_per_day(value)
    }

    /// Sets the reviews per deck and day (see
    /// [`Store::set_max_reviews_per_day`]).
    pub fn set_max_reviews_per_day(&self, value: u32) -> rappel_core::Result<()> {
        self.0.store.borrow_mut().set_max_reviews_per_day(value)
    }

    /// Sets the hour a study day begins (see [`Store::set_day_start_hour`]).
    pub fn set_day_start_hour(&self, value: u8) -> rappel_core::Result<()> {
        self.0.store.borrow_mut().set_day_start_hour(value)
    }

    /// The folder backups are kept in.
    pub fn backup_dir(&self) -> PathBuf {
        storage::backup_dir(&self.0.data_dir)
    }

    /// When what a deck has to study next changes (see
    /// [`Store::next_study_change`]).
    pub fn next_study_change(&self, deck_id: &str, now: i64) -> rappel_core::Result<Option<i64>> {
        self.0.store.borrow().next_study_change(deck_id, now)
    }

    /// Records an answer (see [`Store::record_review`]).
    pub fn record_review(
        &self,
        card_id: &str,
        grade: Grade,
        duration: Duration,
        now: i64,
    ) -> rappel_core::Result<ReviewEvent> {
        self.0
            .store
            .borrow_mut()
            .record_review(card_id, grade, duration, now)
    }

    /// Makes a card new again, keeping its history (see
    /// [`Store::reset_card`]).
    pub fn reset_card(&self, card_id: &str, now: i64) -> rappel_core::Result<ReviewEvent> {
        self.0.store.borrow_mut().reset_card(card_id, now)
    }

    /// Cancels the card's latest review or reset (see [`Store::undo`]).
    pub fn undo(
        &self,
        card_id: &str,
        event_id: &str,
        now: i64,
    ) -> rappel_core::Result<ReviewEvent> {
        self.0.store.borrow_mut().undo(card_id, event_id, now)
    }

    /// A card's whole log (see [`Store::review_events`]); tests check it.
    #[cfg(test)]
    pub fn review_events(&self, card_id: &str) -> rappel_core::Result<Vec<ReviewEvent>> {
        self.0.store.borrow().review_events(card_id)
    }

    /// The statistics of `deck_id`, or of every deck for `None`, at `now`.
    /// The days follow the settings' day start hour in local time: the last
    /// `STATS_PAST_DAYS` ending with today, and `STATS_AHEAD_DAYS` starting
    /// with today (see [`Store::stats`]).
    ///
    /// # Errors
    /// A missing or deleted deck, or a failed read; a day grid that cannot
    /// be built (it always can, but core checks it).
    pub fn stats(&self, deck_id: Option<&str>, now: i64) -> rappel_core::Result<StatsData> {
        let store = self.0.store.borrow();
        let hour = store.settings()?.day_start_hour;
        let first_past = 1 - i32::try_from(STATS_PAST_DAYS).unwrap_or(30);
        let mut past_bounds = local_day_bounds_ms(now, hour, first_past, STATS_PAST_DAYS + 1);
        let mut ahead_bounds = local_day_bounds_ms(now, hour, 0, STATS_AHEAD_DAYS + 1);
        let scope = deck_id.map_or(Scope::All, Scope::Deck);
        let stats = store.stats(
            scope,
            now,
            &DayGrid::new(past_bounds.clone())?,
            &DayGrid::new(ahead_bounds.clone())?,
        )?;
        past_bounds.pop();
        ahead_bounds.pop();
        Ok(StatsData {
            stats,
            past_starts: past_bounds,
            ahead_starts: ahead_bounds,
        })
    }

    /// A card's properties and history at `now` (see [`Store::card_info`]).
    pub fn card_info(&self, card_id: &str, now: i64) -> rappel_core::Result<CardInfo> {
        self.0.store.borrow().card_info(card_id, now)
    }

    /// The ids of a deck's new cards (see [`Store::new_card_ids`]).
    pub fn new_card_ids(&self, deck_id: &str) -> rappel_core::Result<HashSet<String>> {
        self.0.store.borrow().new_card_ids(deck_id)
    }

    /// A card's scheduling state (see [`Store::card_state`]).
    pub fn card_state(&self, card_id: &str) -> rappel_core::Result<CardState> {
        self.0.store.borrow().card_state(card_id)
    }

    /// Soft-deletes a card (see [`Store::delete_card`]).
    pub fn delete_card(&self, id: &str) -> rappel_core::Result<()> {
        self.0.store.borrow_mut().delete_card(id)
    }

    /// Undoes [`Self::delete_card`] (see [`Store::undelete_card`]).
    pub fn undelete_card(&self, id: &str) -> rappel_core::Result<Card> {
        self.0.store.borrow_mut().undelete_card(id)
    }

    /// The cards the All Cards page lists for `query`, at `now` (see
    /// [`Store::browse`]).
    pub fn browse(&self, query: &BrowseQuery, now: i64) -> rappel_core::Result<Vec<BrowseRow>> {
        self.0.store.borrow().browse(query, now)
    }

    /// One row of the All Cards page, `None` if the card is gone (see
    /// [`Store::browse_row`]).
    pub fn browse_row(&self, card_id: &str, now: i64) -> rappel_core::Result<Option<BrowseRow>> {
        self.0.store.borrow().browse_row(card_id, now)
    }

    /// Suspends or unsuspends cards; returns those that changed (see
    /// [`Store::set_suspended`]).
    pub fn set_suspended(
        &self,
        ids: &[String],
        suspended: bool,
    ) -> rappel_core::Result<Vec<String>> {
        self.0.store.borrow_mut().set_suspended(ids, suspended)
    }

    /// Undoes [`Self::set_suspended`] (see [`Store::restore_suspended`]).
    pub fn restore_suspended(&self, ids: &[String], suspended: bool) -> rappel_core::Result<usize> {
        self.0.store.borrow_mut().restore_suspended(ids, suspended)
    }

    /// Moves cards to a deck (see [`Store::move_cards`]).
    pub fn move_cards(&self, ids: &[String], deck_id: &str) -> rappel_core::Result<Vec<Moved>> {
        self.0.store.borrow_mut().move_cards(ids, deck_id)
    }

    /// Undoes [`Self::move_cards`] (see [`Store::unmove_cards`]).
    pub fn unmove_cards(&self, moved: &[Moved]) -> rappel_core::Result<usize> {
        self.0.store.borrow_mut().unmove_cards(moved)
    }

    /// Puts tags, created if need be, on cards, leaving out those that would
    /// have more than `max_per_card` (see [`Store::add_tags_to_cards`]).
    pub fn add_tags_to_cards(
        &self,
        ids: &[String],
        names: &[String],
        max_per_card: usize,
    ) -> rappel_core::Result<TagsAdded> {
        self.0
            .store
            .borrow_mut()
            .add_tags_to_cards(ids, names, max_per_card)
    }

    /// Undoes [`Self::add_tags_to_cards`] (see [`Store::release_tags`]).
    pub fn release_tags(&self, links: &[(String, String)]) -> rappel_core::Result<usize> {
        self.0.store.borrow_mut().release_tags(links)
    }

    /// Takes a tag off cards (see [`Store::remove_tag_from_cards`]).
    pub fn remove_tag_from_cards(
        &self,
        ids: &[String],
        tag_id: &str,
    ) -> rappel_core::Result<Vec<String>> {
        self.0.store.borrow_mut().remove_tag_from_cards(ids, tag_id)
    }

    /// Undoes [`Self::remove_tag_from_cards`] (see
    /// [`Store::restore_tag_on_cards`]).
    pub fn restore_tag_on_cards(
        &self,
        card_ids: &[String],
        tag_id: &str,
    ) -> rappel_core::Result<usize> {
        self.0
            .store
            .borrow_mut()
            .restore_tag_on_cards(card_ids, tag_id)
    }

    /// Makes cards new again (see [`Store::reset_cards`]).
    pub fn reset_cards(&self, ids: &[String], now: i64) -> rappel_core::Result<Vec<ReviewEvent>> {
        self.0.store.borrow_mut().reset_cards(ids, now)
    }

    /// Undoes [`Self::reset_cards`]; returns the cards left alone (see
    /// [`Store::undo_entries`]).
    pub fn undo_entries(
        &self,
        entries: &[ReviewEvent],
        now: i64,
    ) -> rappel_core::Result<Vec<String>> {
        self.0.store.borrow_mut().undo_entries(entries, now)
    }

    /// Soft-deletes cards (see [`Store::delete_cards`]).
    pub fn delete_cards(&self, ids: &[String]) -> rappel_core::Result<()> {
        self.0.store.borrow_mut().delete_cards(ids)
    }

    /// Undoes [`Self::delete_cards`] (see [`Store::undelete_cards`]).
    pub fn undelete_cards(&self, ids: &[String]) -> rappel_core::Result<usize> {
        self.0.store.borrow_mut().undelete_cards(ids)
    }

    /// The problems met while opening that did not stop it. Empty after
    /// the first call.
    pub fn take_open_warnings(&self) -> Vec<OpenWarning> {
        self.0.store.borrow_mut().take_open_warnings()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statistics_cover_thirty_past_days_ending_today_and_fourteen_ahead() {
        let (_tmp, state) = crate::testing::temporary_state();
        let now = rappel_core::time::now_ms();
        let data = state.stats(None, now).unwrap();
        assert_eq!(data.past_starts.len(), STATS_PAST_DAYS);
        assert_eq!(data.ahead_starts.len(), STATS_AHEAD_DAYS);
        assert_eq!(data.stats.days.len(), STATS_PAST_DAYS);
        assert_eq!(data.stats.forecast.len(), STATS_AHEAD_DAYS);
        // Today is the last past day and the first day ahead.
        let today = data.past_starts[STATS_PAST_DAYS - 1];
        assert_eq!(data.ahead_starts[0], today);
        assert!(today <= now && now < data.ahead_starts[1]);
        assert!(data.past_starts.windows(2).all(|w| w[0] < w[1]));
    }
}
