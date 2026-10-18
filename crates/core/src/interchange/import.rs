//! Importing a `.rappel` file: plan (read and check the file, compare it
//! with the database, no writes), then apply (back up, then read the file
//! again and write, in one transaction). Both read the file as a stream
//! and keep only compact notes, so memory stays bounded whatever the file
//! holds (ADR 0053).
//!
//! A read is done in one pass when the file is as an export writes it
//! (`format_version` first, then the lists in file order) and breaks no
//! rule. Otherwise it is done again one step at a time, in the order the
//! checks have always come: the archive and the version, the shape of the
//! whole document, the top level, then each list on its own, and the
//! settings last. That finds exactly the problem a check of the whole
//! document found, and reads any valid file whatever the order of its
//! keys.

use std::io::{Read, Seek};
use std::path::Path;
#[cfg(test)]
use std::sync::Arc;

use rusqlite::{Connection, TransactionBehavior};

use super::archive::Archive;
use super::engine::{Engine, Found, Stop};
use super::format::SettingsRecord;
use super::source::Source;
use super::stream::{Focus, ReadEnd, Table, read_document, read_header};
use super::validate::version_problem;
use super::{ImportPlan, ImportProblem, ImportReport, Limits, Mode, SettingsImport};
use crate::error::{Error, Result};
use crate::storage::{
    Store, StoredSettings, create_backup, prune_backups, rebuild_cards, replace_settings,
    scheduler_in,
};
use crate::time::now_ms;

impl Store {
    /// Reads and checks the `.rappel` file `path` and compares it with the
    /// database. Writes nothing.
    ///
    /// A file that is not a valid export is refused with
    /// [`Error::ImportRefused`], saying what is wrong and where. A valid
    /// file gives a plan whose [`ImportPlan::summary`] counts the rows that
    /// would be added, those that would be brought back (deleted here,
    /// live in the file, otherwise the same) and those already present,
    /// and lists every conflict (a row with the same id but different
    /// content, a live deck name already used by another live deck, or an
    /// undo of an entry the database has already undone). A tag with the
    /// name of a database tag is mapped onto it, not a conflict. It also
    /// says whether the file's settings would be applied: only into a
    /// database without decks. Pass the plan to [`Self::apply_import`].
    ///
    /// The file is read as a stream: the plan holds the summary and a
    /// fingerprint of the file, not its content.
    ///
    /// # Errors
    /// [`Error::ImportRefused`], [`Error::ImportFileChanged`] (the file
    /// changed while it was read), IO errors reading `path`, SQLite errors.
    pub fn plan_import(&self, path: &Path) -> Result<ImportPlan> {
        self.plan_import_with(path, &Limits::DEFAULT)
    }

    /// [`Self::plan_import`] with explicit limits, for tests.
    pub(crate) fn plan_import_with(&self, path: &Path, limits: &Limits) -> Result<ImportPlan> {
        self.plan_source(Source::File(path.to_path_buf()), limits)
    }

    /// [`Self::plan_import`] on archive bytes in memory.
    #[cfg(test)]
    pub(crate) fn plan_import_bytes(&self, bytes: &[u8], limits: &Limits) -> Result<ImportPlan> {
        self.plan_source(Source::Bytes(Arc::from(bytes)), limits)
    }

    fn plan_source(&self, source: Source, limits: &Limits) -> Result<ImportPlan> {
        let opened = source.open()?;
        let mut archive =
            Archive::open(opened.input, opened.len, limits).map_err(Error::ImportRefused)?;
        let tx = self.conn.unchecked_transaction()?;
        let (mode, sha256, found) =
            if let Some((sha256, found)) = read_in_one_pass(&tx, &mut archive, limits, None)? {
                (Mode::OnePass, sha256, found)
            } else {
                let (sha256, found) = read_step_by_step(&tx, &mut archive, limits, None)?;
                (Mode::StepByStep, sha256, found)
            };
        // Read only: ending the transaction cannot lose anything.
        tx.finish()?;
        Ok(ImportPlan {
            source,
            limits: *limits,
            stamp: opened.stamp,
            mode,
            sha256,
            digest: found.digest,
            summary: found.summary,
        })
    }

    /// Adds the plan's new decks, cards, tags, card tags and log entries
    /// to the database, brings back the deleted decks and cards that are
    /// live in the file, and applies the file's settings if the plan says
    /// so (before the schedules are rebuilt, so they use them). A deck
    /// brought back also brings back the cards deleted with it, as
    /// [`Self::undelete_deck`] does, unless the file has them.
    ///
    /// Nothing is written if the plan has conflicts, or if every row is
    /// already present (then no backup is made either, so importing a file
    /// twice changes nothing). Otherwise the file must still be the one
    /// the plan read (same size, time and file, checked before anything
    /// else, and the same `cards.json`, checked as it is read again), the
    /// database is backed up (no backup, no import), then, in one
    /// transaction, the file is read again, compared again and written row
    /// by row, and refused if any decision differs from the plan's (the
    /// database changed since). Last, the schedule cache of every card
    /// that got log entries is rebuilt from its log. A card brought back
    /// keeps its schedule, which deleting it did not touch.
    ///
    /// # Errors
    /// [`Error::ImportConflicts`], [`Error::ImportPlanStale`],
    /// [`Error::ImportFileChanged`], [`Error::NoBackupDir`], IO, backup,
    /// scheduler and SQLite errors. On error nothing is written.
    pub fn apply_import(&mut self, plan: &ImportPlan) -> Result<ImportReport> {
        self.apply_import_at(plan, now_ms())
    }

    /// [`Self::apply_import`] at time `now` (names the backup).
    pub(crate) fn apply_import_at(&mut self, plan: &ImportPlan, now: i64) -> Result<ImportReport> {
        let summary = &plan.summary;
        if !summary.conflicts.is_empty() {
            return Err(Error::ImportConflicts(summary.conflicts.len()));
        }
        if summary.is_noop() {
            return Ok(ImportReport {
                decks: 0,
                cards: 0,
                tags: 0,
                card_tags: 0,
                review_events: 0,
                settings_applied: false,
                revived_decks: 0,
                revived_cards: 0,
                backup: None,
            });
        }
        let backup_dir = self.backup_dir.clone().ok_or(Error::NoBackupDir)?;

        // The same file, before anything is written.
        let opened = plan.source.open().map_err(|err| match err {
            Error::ImportRefused(_) => Error::ImportFileChanged,
            other => other,
        })?;
        if opened.stamp != plan.stamp {
            return Err(Error::ImportFileChanged);
        }
        let mut archive = Archive::open(opened.input, opened.len, &plan.limits)
            .map_err(|_| Error::ImportFileChanged)?;

        let backup = create_backup(&self.conn, &backup_dir, now)?;
        tracing::info!(path = %backup.display(), "database backed up before an import");
        if let Err(err) = prune_backups(&backup_dir, now, &[&backup]) {
            // As at startup: a failed prune only leaves extra backups.
            tracing::warn!("pruning old backups failed: {err}");
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Rows are written as they are read; any error from here on drops
        // the transaction, which rolls every one of them back.
        let found = match plan.mode {
            Mode::OnePass => read_in_one_pass(&tx, &mut archive, &plan.limits, Some(plan))?
                .map(|(_, found)| found)
                .ok_or(Error::ImportFileChanged)?,
            Mode::StepByStep => read_step_by_step(&tx, &mut archive, &plan.limits, Some(plan))?.1,
        };
        // The same file, so equal decisions in the same order are the
        // plan the user confirmed. Any change in the database since (a row
        // added, edited, deleted or brought back) that changes what is
        // written refuses the import.
        if found.digest != plan.digest || found.summary != *summary {
            return Err(Error::ImportPlanStale);
        }
        let settings_applied = summary.settings == SettingsImport::Applied;
        if settings_applied {
            replace_settings(&tx, &stored_of(&found.settings))?;
        }
        rebuild_cards(
            &tx,
            &scheduler_in(&tx)?,
            found.reviewed.iter().map(String::as_str),
        )?;
        tx.commit()?;
        tracing::info!(
            decks = summary.decks.new,
            cards = summary.cards.new,
            tags = summary.tags.new,
            card_tags = summary.card_tags.new,
            review_events = summary.review_events.new,
            settings_applied,
            revived_decks = summary.decks.revived,
            revived_cards = summary.cards.revived,
            "import applied"
        );
        Ok(ImportReport {
            decks: summary.decks.new,
            cards: summary.cards.new,
            tags: summary.tags.new,
            card_tags: summary.card_tags.new,
            review_events: summary.review_events.new,
            settings_applied,
            revived_decks: summary.decks.revived,
            revived_cards: summary.cards.revived,
            backup: Some(backup),
        })
    }
}

#[cfg(test)]
thread_local! {
    /// Runs before each read of a step-by-step read, so tests can change
    /// the file between them.
    pub(super) static BEFORE_READ: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Before a read of a step-by-step read (a test hook; nothing otherwise).
fn before_read() {
    #[cfg(test)]
    BEFORE_READ.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook();
        }
    });
}

/// What to do with a problem found in a read: planning, refuse the file;
/// applying, the plan read the same file without it, so the file changed.
fn refuse(applying: Option<&ImportPlan>, problem: ImportProblem) -> Error {
    if applying.is_some() {
        Error::ImportFileChanged
    } else {
        Error::ImportRefused(problem)
    }
}

/// A read's hash against the one it must equal: the plan's when applying,
/// else the first read's (the file must not change between reads).
fn same_file<T>(end: &ReadEnd<T>, expected: &[u8; 32]) -> Result<()> {
    if end.sha256 == *expected {
        Ok(())
    } else {
        Err(Error::ImportFileChanged)
    }
}

fn stop_error(stop: Stop) -> Error {
    match stop {
        Stop::Db(err) => err,
        Stop::Stale => Error::ImportPlanStale,
        // Not stops of a read that cannot fall back.
        Stop::Fallback | Stop::Problem(_) | Stop::Locate => Error::ImportFileChanged,
    }
}

/// Reads the whole file in one pass, for a file as an export writes it.
/// `Ok(None)` if that did not work out for any reason but the database
/// (when applying, that is the file changing).
fn read_in_one_pass<R: Read + Seek>(
    conn: &Connection,
    archive: &mut Archive<R>,
    limits: &Limits,
    applying: Option<&ImportPlan>,
) -> Result<Option<([u8; 32], Found)>> {
    let mut engine = Engine::new(conn, *limits, applying.is_some(), false)?;
    let end =
        read_document(archive, limits, &mut engine, Focus::All).map_err(|p| refuse(applying, p))?;
    if let Some(plan) = applying {
        same_file(&end, &plan.sha256)?;
    }
    let mut fell_back = match engine.take_stop() {
        Some(Stop::Db(err)) => return Err(err),
        Some(Stop::Stale) => return Err(Error::ImportPlanStale),
        Some(Stop::Fallback | Stop::Problem(_) | Stop::Locate) => true,
        None => false,
    };
    fell_back = fell_back
        || end.problem(false).is_some()
        || engine.check_top_level().is_err()
        || engine.check_settings().is_err();
    if fell_back {
        return match applying {
            Some(_) => Err(Error::ImportFileChanged),
            None => Ok(None),
        };
    }
    Ok(Some((end.sha256, engine.finish())))
}

/// Reads the file one step at a time, in the order of the checks, and
/// reports the first problem exactly. Every read must see the same
/// `cards.json`.
fn read_step_by_step<R: Read + Seek>(
    conn: &Connection,
    archive: &mut Archive<R>,
    limits: &Limits,
    applying: Option<&ImportPlan>,
) -> Result<([u8; 32], Found)> {
    // The archive (its size, its CRC) and the version, before the rest.
    let header = read_header(archive, limits).map_err(|p| refuse(applying, p))?;
    if let Some(problem) = header.problem(false) {
        return Err(refuse(applying, problem));
    }
    let sha256 = header.sha256;
    if let Some(plan) = applying {
        same_file(&header, &plan.sha256)?;
    }
    let version = header.parsed.map_or(0, |h| h.format_version);
    if let Some(problem) = version_problem(version) {
        return Err(refuse(applying, problem));
    }

    // The shape of the whole document: its JSON errors come before any
    // rule.
    let mut checks = Engine::new(conn, *limits, false, true)?.checks_only();
    before_read();
    let end = read_document(archive, limits, &mut checks, Focus::Structure)
        .map_err(|p| refuse(applying, p))?;
    same_file(&end, &sha256)?;
    if let Some(problem) = end.problem(false) {
        return Err(refuse(applying, problem));
    }
    checks.check_top_level().map_err(|p| refuse(applying, p))?;

    // Every rule, list by list, before anything is compared with the
    // database: a file refused here costs no query.
    for table in Table::ALL {
        read_table(&mut checks, archive, limits, table, &sha256, applying)?;
    }
    checks.check_settings().map_err(|p| refuse(applying, p))?;

    // The file is valid: compare (and write) list by list.
    let mut engine = Engine::new(conn, *limits, applying.is_some(), true)?;
    engine.adopt_top_level(&checks);
    drop(checks);
    for table in Table::ALL {
        read_table(&mut engine, archive, limits, table, &sha256, applying)?;
    }
    Ok((sha256, engine.finish()))
}

/// Reads one list: its rows checked and compared in order, then what
/// follows the list. Candidate duplicates are confirmed by reading it
/// again ([`super::notes::Locate`]).
fn read_table<R: Read + Seek>(
    engine: &mut Engine<'_>,
    archive: &mut Archive<R>,
    limits: &Limits,
    table: Table,
    sha256: &[u8; 32],
    applying: Option<&ImportPlan>,
) -> Result<()> {
    before_read();
    let end = read_document(archive, limits, engine, Focus::Table(table))
        .map_err(|p| refuse(applying, p))?;
    same_file(&end, sha256)?;
    // The shape was checked: a problem in the JSON now is a changed file.
    if end.problem(engine.stopped()).is_some() {
        return Err(Error::ImportFileChanged);
    }
    match engine.take_stop() {
        None => Ok(()),
        Some(Stop::Locate) => {
            let candidates = engine.take_candidates();
            if let Some(problem) = locate(engine, archive, limits, table, candidates, sha256)? {
                return Err(refuse(applying, problem));
            }
            // The candidates were digests that collide, not duplicates.
            engine.after_table(table).map_err(|stop| match stop {
                Stop::Problem(problem) => refuse(applying, problem),
                other => stop_error(other),
            })
        }
        Some(Stop::Problem(problem)) => {
            // A duplicate in an earlier row comes first: confirm the
            // candidates among the rows read, which finds the first
            // problem in row order.
            let candidates = engine.candidates_after_problem(table);
            let first = if candidates.is_empty() {
                problem
            } else {
                locate(engine, archive, limits, table, candidates, sha256)?.unwrap_or(problem)
            };
            Err(refuse(applying, first))
        }
        Some(other) => Err(stop_error(other)),
    }
}

/// Reads `table` again, checking each row with the candidates tracked
/// exactly; the first problem in row order, if any.
fn locate<R: Read + Seek>(
    engine: &mut Engine<'_>,
    archive: &mut Archive<R>,
    limits: &Limits,
    table: Table,
    candidates: super::notes::Candidates,
    sha256: &[u8; 32],
) -> Result<Option<ImportProblem>> {
    engine.start_locate(candidates);
    before_read();
    let end = read_document(archive, limits, engine, Focus::Table(table))
        .map_err(|_| Error::ImportFileChanged)?;
    engine.end_locate();
    same_file(&end, sha256)?;
    if end.problem(engine.stopped()).is_some() {
        return Err(Error::ImportFileChanged);
    }
    match engine.take_stop() {
        None => Ok(None),
        Some(Stop::Problem(problem)) => Ok(Some(problem)),
        Some(other) => Err(stop_error(other)),
    }
}

/// What to do with a deck or card of the file whose id is in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Nothing: it is already here.
    Present,
    /// Clear the database row's `deleted_at`, which is this stamp.
    Revive(i64),
    /// Refuse the whole import.
    Conflict,
}

/// Compares a row of the file with the database's row of the same id.
/// `rest_equal` says whether every field but `deleted_at` is the same.
/// Only a difference in `deleted_at` can be merged: deleted here and live
/// in the file brings the row back; live here and deleted in the file
/// leaves it live, as an import never deletes. Deleted on both sides is
/// already present, whatever the stamps: merging it would change nothing.
/// Any other difference is a conflict.
pub(super) fn compare(
    rest_equal: bool,
    stored_deleted: Option<i64>,
    file_deleted: Option<i64>,
) -> Outcome {
    if !rest_equal {
        return Outcome::Conflict;
    }
    match (stored_deleted, file_deleted) {
        (Some(stamp), None) => Outcome::Revive(stamp),
        // Live here, or deleted on both sides.
        (None, _) | (Some(_), Some(_)) => Outcome::Present,
    }
}

pub(super) fn stored_of(s: &SettingsRecord) -> StoredSettings {
    StoredSettings {
        desired_retention: s.desired_retention,
        max_new_per_day: s.max_new_per_day,
        max_reviews_per_day: s.max_reviews_per_day,
        day_start_hour: s.day_start_hour,
    }
}
