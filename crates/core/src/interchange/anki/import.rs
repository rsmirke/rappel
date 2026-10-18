//! Planning and applying an Anki import (ADR 0061).
//!
//! Both read the package the same way ([`Run`]): the archive, the media
//! list, the collection (copied to a temporary file in the data directory,
//! removed afterwards), then every media file a note names (checked like
//! any file added to Rappel: its kind from its first bytes, its size, the
//! whole structure of audio), then every card, converted, compared with
//! the database and either counted (the plan) or written (the apply, in
//! one transaction, after a backup). The apply must make exactly the
//! plan's decisions (a digest of them), else nothing is written.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, Statement, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::collection::{CardRow, CardWalkError, Collection, Column, Notetype, RevRow};
use super::convert::{CardInfo, Parsed, Report, card_sides, math_drawable, parse_templates};
use super::html::{MediaUse, Resolver, media_names};
use super::package::{AnkiLimits, Package, from_archive, remove_stale_temps};
use super::{AnkiPlan, AnkiProblem, AnkiReport, AnkiSummary, FileKind, Skip};
use crate::domain::{EventKind, Grade, MAX_DECK_NAME_BYTES, ReviewEvent, check_tag_name, name_key};
use crate::error::{Error, Result};
use crate::id::IdGen;
use crate::interchange::archive::Archive;
use crate::interchange::source::{Input, Source};
use crate::interchange::validate::timestamp_ok;
use crate::media::{
    CopyFailure, Ingested, MediaProblem, MediaRef, Sync, copy_capped, ingest, insert_media_row,
    sync_folder,
};
use crate::storage::{
    Store, create_backup, find_or_create_tag, index_card_in, observe_max_id, prune_backups,
    rebuild_cards, scheduler_in,
};
use crate::time::now_ms;

// The most tags a card keeps from its note.
use crate::domain::MAX_TAGS_PER_CARD as MAX_TAGS;
/// The longest time a review may have taken: an hour (Anki caps answers
/// at a minute by default; longer ones are a clock or a lunch break).
const MAX_DURATION_MS: i64 = 3_600_000;
/// The separator of deck name parts in Rappel (U+203A with spaces).
pub(super) const DECK_SEPARATOR: &str = " \u{203a} ";

impl Store {
    /// Reads the Anki package `path` (`.apkg` or `.colpkg`, legacy or
    /// current), converts every card and compares it with the database.
    /// Writes nothing to the database; the collection is copied to a
    /// temporary file in the data directory, removed before this returns.
    ///
    /// # Errors
    /// [`Error::AnkiRefused`] for a package Rappel cannot read,
    /// [`Error::ImportCancelled`] if `cancel` became true,
    /// [`Error::NoMediaDir`] for a store without a data directory, IO and
    /// SQLite errors.
    pub fn plan_anki_import(&self, path: &Path, cancel: &AtomicBool) -> Result<AnkiPlan> {
        self.plan_anki_source(
            Source::File(path.to_path_buf()),
            &AnkiLimits::DEFAULT,
            cancel,
            now_ms(),
        )
    }

    /// [`Self::plan_anki_import`] on bytes, with explicit limits, for tests.
    #[cfg(test)]
    pub(crate) fn plan_anki_bytes(
        &self,
        bytes: &[u8],
        limits: &AnkiLimits,
        now: i64,
    ) -> Result<AnkiPlan> {
        self.plan_anki_source(
            Source::Bytes(std::sync::Arc::from(bytes)),
            limits,
            &AtomicBool::new(false),
            now,
        )
    }

    pub(super) fn plan_anki_source(
        &self,
        source: Source,
        limits: &AnkiLimits,
        cancel: &AtomicBool,
        now: i64,
    ) -> Result<AnkiPlan> {
        let data_dir = self.data_dir()?;
        let opened = source.open().map_err(refusal)?;
        let stamp = opened.stamp;
        let tx = self.conn.unchecked_transaction()?;
        let mut run = Run::open(opened.input, opened.len, limits, &data_dir, cancel)?;
        let outcome = run.convert(&tx, None, now, now)?;
        tx.finish()?;
        Ok(AnkiPlan {
            planned_at: now,
            source,
            stamp,
            limits: *limits,
            collection: run.collection_sha256,
            digest: outcome.digest,
            summary: outcome.summary,
        })
    }

    /// Adds the plan's cards to the database: their decks (made when no
    /// live deck has the name), tags, suspension and review history (its
    /// schedule rebuilt from it), and the media files they use. Backs up
    /// first, then writes in one transaction; nothing is written on error
    /// or if `cancel` becomes true. Nothing at all happens (no backup) if
    /// the plan adds no card.
    ///
    /// # Errors
    /// [`Error::ImportFileChanged`] (the package is not the plan's),
    /// [`Error::ImportPlanStale`] (the database changed since the plan),
    /// [`Error::ImportCancelled`], [`Error::NoBackupDir`],
    /// [`Error::NoMediaDir`], IO, backup, scheduler and SQLite errors.
    pub fn apply_anki_import(
        &mut self,
        plan: &AnkiPlan,
        cancel: &AtomicBool,
    ) -> Result<AnkiReport> {
        self.apply_anki_at(plan, now_ms(), cancel)
    }

    pub(crate) fn apply_anki_at(
        &mut self,
        plan: &AnkiPlan,
        now: i64,
        cancel: &AtomicBool,
    ) -> Result<AnkiReport> {
        let summary = &plan.summary;
        if summary.is_noop() {
            return Ok(AnkiReport {
                new_decks: 0,
                cards: 0,
                suspended: 0,
                reviews: 0,
                new_media: 0,
                skipped: summary.skipped.clone(),
                backup: None,
            });
        }
        let backup_dir = self.backup_dir.clone().ok_or(Error::NoBackupDir)?;
        let media_dir = self.media_dir.clone().ok_or(Error::NoMediaDir)?;
        let data_dir = self.data_dir()?;
        let opened = plan.source.open().map_err(|err| match err {
            Error::ImportRefused(_) | Error::AnkiRefused(_) => Error::ImportFileChanged,
            other => other,
        })?;
        if opened.stamp != plan.stamp {
            return Err(Error::ImportFileChanged);
        }
        let mut run = Run::open(opened.input, opened.len, &plan.limits, &data_dir, cancel)
            .map_err(|err| match err {
                Error::AnkiRefused(_) => Error::ImportFileChanged,
                other => other,
            })?;
        if run.collection_sha256 != plan.collection {
            return Err(Error::ImportFileChanged);
        }

        let backup = create_backup(&self.conn, &backup_dir, now)?;
        tracing::info!(path = %backup.display(), "database backed up before an Anki import");
        if let Err(err) = prune_backups(&backup_dir, now, &[&backup]) {
            tracing::warn!("pruning old backups failed: {err}");
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for column in ["decks.id", "cards.id", "tags.id", "review_events.id"] {
            observe_max_id(&tx, &mut self.ids, column)?;
        }
        // The plan's clock decides what the plan decided (card times, the
        // reviews kept); only the ids written take this one.
        let outcome = run.convert(&tx, Some(&mut self.ids), plan.planned_at, now)?;
        if outcome.digest != plan.digest || outcome.summary != *summary {
            return Err(Error::ImportPlanStale);
        }
        // The media files, into the media folder, before the rows commit.
        let new_media = run.store_media(&tx, &outcome.used_media, &media_dir, now)?;
        rebuild_cards(
            &tx,
            &scheduler_in(&tx)?,
            outcome.reviewed.iter().map(String::as_str),
        )?;
        match crate::media::recover_all(&tx, &media_dir, now) {
            Ok(0) => {}
            Ok(n) => tracing::info!(files = n, "media files an Anki import uses brought back"),
            Err(err) => tracing::warn!("cannot bring media back from the trash: {err}"),
        }
        before_commit();
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::ImportCancelled);
        }
        tx.commit()?;
        tracing::info!(
            decks = summary.new_decks,
            cards = summary.cards,
            reviews = summary.reviews,
            media = new_media,
            "Anki import applied"
        );
        Ok(AnkiReport {
            new_decks: summary.new_decks,
            cards: summary.cards,
            suspended: summary.suspended,
            reviews: summary.reviews,
            new_media,
            skipped: summary.skipped.clone(),
            backup: Some(backup),
        })
    }

    /// The data directory: where the media folder is.
    fn data_dir(&self) -> Result<PathBuf> {
        self.media_dir
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or(Error::NoMediaDir)
    }
}

/// An archive refusal as an Anki refusal.
fn refusal(err: Error) -> Error {
    match err {
        Error::ImportRefused(problem) => Error::AnkiRefused(from_archive(problem)),
        other => other,
    }
}

/// Whether `path` is an Anki package or a Rappel export by its content.
pub(super) fn sniff(path: &Path) -> Option<FileKind> {
    let open = || Source::File(path.to_path_buf()).open().ok();
    let opened = open()?;
    if Archive::open_anki(opened.input, opened.len, &AnkiLimits::DEFAULT.archive).is_ok() {
        return Some(FileKind::Anki);
    }
    let opened = open()?;
    Archive::open(opened.input, opened.len, &AnkiLimits::DEFAULT.archive)
        .is_ok()
        .then_some(FileKind::Rappel)
}

/// A media file of the package, as the cards may use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaState {
    /// Listed, not (yet) read: no note names it.
    Listed { file: u32 },
    /// Read and accepted.
    Ok { media: MediaRef, file: u32 },
    /// Read and refused, or its name is not usable.
    Bad(Skip),
}

/// One read of the package.
struct Run<'c> {
    package: Package<Input>,
    /// Removed when the run ends.
    _temp: super::package::TempCollection,
    collection: Collection,
    collection_sha256: [u8; 32],
    limits: AnkiLimits,
    notetypes: HashMap<i64, Notetype>,
    decks: HashMap<i64, String>,
    media: HashMap<String, MediaState>,
    report: Report,
    cancel: &'c AtomicBool,
}

/// What converting every card found.
struct Outcome {
    summary: AnkiSummary,
    digest: [u8; 32],
    /// The media files the imported cards use, with their entries.
    used_media: Vec<(MediaRef, u32)>,
    /// The cards given log entries (applying).
    reviewed: Vec<String>,
}

impl<'c> Run<'c> {
    fn open(
        input: Input,
        len: u64,
        limits: &AnkiLimits,
        data_dir: &Path,
        cancel: &'c AtomicBool,
    ) -> Result<Self> {
        remove_stale_temps(data_dir);
        let mut package = Package::open(input, len, limits).map_err(Error::AnkiRefused)?;
        let listed = package.media_list().map_err(Error::AnkiRefused)?;
        let mut report = Report::default();
        let media = media_table(listed, limits, &mut report);
        check_cancel(cancel)?;
        let temp = package.extract_collection(data_dir)?;
        check_cancel(cancel)?;
        let collection = Collection::open(&temp.path, limits).map_err(Error::AnkiRefused)?;
        let notetypes = collection.notetypes(limits).map_err(Error::AnkiRefused)?;
        let decks = collection.decks(limits).map_err(Error::AnkiRefused)?;
        let mut run = Self {
            package,
            collection_sha256: temp.sha256,
            _temp: temp,
            collection,
            limits: *limits,
            notetypes,
            decks,
            media,
            report,
            cancel,
        };
        run.check_named_media()?;
        Ok(run)
    }

    /// Reads every listed media file a note or a template names; the others
    /// are unused.
    fn check_named_media(&mut self) -> Result<()> {
        let mut named: HashSet<String> = HashSet::new();
        let mut add = |name: &str| {
            if !named.contains(name) {
                named.insert(name.to_owned());
                if let Some(decoded) = percent_decoded(name) {
                    named.insert(decoded);
                }
            }
        };
        for notetype in self.notetypes.values() {
            for template in &notetype.templates {
                if let Some((q, a)) = &template.formats {
                    media_names(q, &mut add);
                    media_names(a, &mut add);
                }
            }
        }
        let cancel = self.cancel;
        let mut count = 0_usize;
        let finished = self
            .collection
            .each_note_fields(&self.limits, |fields| {
                media_names(fields, &mut add);
                count += 1;
                !count.is_multiple_of(1024) || !cancel.load(Ordering::Relaxed)
            })
            .map_err(Error::AnkiRefused)?;
        if !finished {
            return Err(Error::ImportCancelled);
        }
        let mut listed: Vec<(String, u32)> = self
            .media
            .iter()
            .filter_map(|(name, state)| match state {
                MediaState::Listed { file } => Some((name.clone(), *file)),
                _ => None,
            })
            .collect();
        listed.sort_by_key(|(_, file)| *file);
        let mut unused: Vec<String> = Vec::new();
        for (i, (name, file)) in listed.into_iter().enumerate() {
            if i % 64 == 0 {
                check_cancel(self.cancel)?;
            }
            if !named.contains(&name) {
                unused.push(name);
                continue;
            }
            let state = match self.read_media(file) {
                Ok(media) => MediaState::Ok { media, file },
                Err(skip) => MediaState::Bad(skip),
            };
            self.media.insert(name, state);
        }
        let examples: Vec<&str> = unused.iter().map(String::as_str).collect();
        self.report
            .add_n(Skip::UnusedMedia, unused.len(), &examples);
        for name in &unused {
            self.media.remove(name);
        }
        Ok(())
    }

    /// Reads media file `file` without keeping it: its kind, size,
    /// structure and hash.
    fn read_media(&mut self, file: u32) -> std::result::Result<MediaRef, Skip> {
        let cap = crate::media::AUDIO_MAX_BYTES.max(crate::media::IMAGE_MAX_BYTES);
        let mut reader = match self.package.media_reader(file, cap) {
            None => return Err(Skip::MissingMedia),
            Some(Err(AnkiProblem::EntryTooLarge { .. })) => return Err(Skip::MediaTooLarge),
            Some(Err(_)) => return Err(Skip::DamagedMedia),
            Some(Ok(reader)) => reader,
        };
        match copy_capped(&mut reader, &mut std::io::sink()) {
            Ok(Ingested { media, .. }) => match reader.finish() {
                Ok(()) => Ok(media),
                Err(super::package::ReadFailure::TooLarge(_)) => Err(Skip::MediaTooLarge),
                Err(_) => Err(Skip::DamagedMedia),
            },
            Err(CopyFailure::Refused(problem)) => Err(match problem {
                MediaProblem::TooLarge { .. } => Skip::MediaTooLarge,
                MediaProblem::Malformed | MediaProblem::NotPlainAudio | MediaProblem::Empty => {
                    Skip::DamagedMedia
                }
                MediaProblem::NotMedia | MediaProblem::NotImage | MediaProblem::NotAudio => {
                    Skip::UnsupportedMedia
                }
            }),
            Err(CopyFailure::Read(_)) => match reader.finish() {
                Err(super::package::ReadFailure::TooLarge(_)) => Err(Skip::MediaTooLarge),
                _ => Err(Skip::DamagedMedia),
            },
            Err(CopyFailure::Write(_)) => Err(Skip::DamagedMedia),
        }
    }

    /// Converts every card; with `ids`, writes it too.
    fn convert(
        &mut self,
        conn: &Connection,
        ids: Option<&mut IdGen>,
        planned_at: i64,
        now: i64,
    ) -> Result<Outcome> {
        let parsed: HashMap<i64, Parsed<'_>> = self
            .notetypes
            .iter()
            .map(|(id, nt)| (*id, parse_templates(nt)))
            .collect();
        let mut walk = Walk {
            existing: existing_cards(conn)?,
            live_decks: live_decks(conn)?,
            writer: match ids {
                Some(ids) => Some(Writer::new(conn, ids, now)?),
                None => None,
            },
            notetypes: &self.notetypes,
            parsed: &parsed,
            anki_decks: &self.decks,
            resolver: MediaResolver {
                media: &self.media,
                used: Vec::new(),
                noted: HashSet::new(),
                report: Report::default(),
            },
            report: self.report.clone(),
            digest: Sha256::new(),
            decks: HashMap::new(),
            shortened: HashSet::new(),
            used_media: Vec::new(),
            used_set: HashSet::new(),
            tags_seen: HashSet::new(),
            reviewed: Vec::new(),
            tally: Tally::default(),
            invalid_tags: HashSet::new(),
            now: planned_at,
        };
        let cancel = self.cancel;
        let walked = self.collection.each_card(&self.limits, |card, take_revs| {
            // Every card: one card's work is bounded (its template's work
            // budget), so this is the longest wait for a cancel.
            check_cancel(cancel)?;
            let revs = take_revs(card.id).map_err(Error::AnkiRefused)?;
            walk.card(&card, &revs)?;
            after_card();
            Ok(())
        });
        match walked {
            Ok(()) => {}
            Err(CardWalkError::Problem(problem)) => return Err(Error::AnkiRefused(problem)),
            Err(CardWalkError::Stopped(err)) => return Err(err),
        }
        check_cancel(self.cancel)?;
        walk.finish(conn, self.package.format)
    }

    /// Applying: copies each used media file into the media folder and
    /// records it. Returns how many were not there before.
    fn store_media(
        &mut self,
        conn: &Connection,
        used: &[(MediaRef, u32)],
        dir: &Path,
        now: i64,
    ) -> Result<usize> {
        let mut added = 0;
        for (i, (media, file)) in used.iter().enumerate() {
            if i % 64 == 0 {
                check_cancel(self.cancel)?;
            }
            let had: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM media WHERE hash = ?1)",
                [media.hex()],
                |r| r.get(0),
            )?;
            let cap = crate::media::AUDIO_MAX_BYTES.max(crate::media::IMAGE_MAX_BYTES);
            let Some(Ok(mut reader)) = self.package.media_reader(*file, cap) else {
                return Err(Error::ImportFileChanged);
            };
            let got = ingest(dir, &mut reader, Sync::FileOnly).map_err(|err| match err {
                Error::MediaRefused(_) | Error::MediaRead(_) => Error::ImportFileChanged,
                other => other,
            })?;
            reader.finish().map_err(|_| Error::ImportFileChanged)?;
            if got.media != *media {
                return Err(Error::ImportFileChanged);
            }
            insert_media_row(conn, &got, now)?;
            added += usize::from(!had);
        }
        if !used.is_empty() {
            sync_folder(dir)?;
        }
        Ok(added)
    }
}

/// The state of a walk over the cards.
struct Walk<'a, 't> {
    /// The keys of the database's live cards.
    existing: HashSet<[u8; 16]>,
    /// The database's live decks by name key.
    live_decks: HashMap<String, String>,
    /// Applying: what writes.
    writer: Option<Writer<'t>>,
    notetypes: &'a HashMap<i64, Notetype>,
    parsed: &'a HashMap<i64, Parsed<'a>>,
    anki_decks: &'a HashMap<i64, String>,
    resolver: MediaResolver<'a>,
    report: Report,
    /// Every decision, in order.
    digest: Sha256,
    /// Where each deck name's cards go, by name key.
    decks: HashMap<String, DeckChoice>,
    /// Anki decks whose name was shortened (reported once each).
    shortened: HashSet<i64>,
    used_media: Vec<(MediaRef, u32)>,
    used_set: HashSet<MediaRef>,
    tags_seen: HashSet<String>,
    reviewed: Vec<String>,
    tally: Tally,
    /// Tags already reported as invalid (each is counted once).
    invalid_tags: HashSet<String>,
    /// The plan's clock.
    now: i64,
}

/// A card as it will be added.
struct Planned {
    deck: String,
    front: String,
    back: String,
    tags: Vec<String>,
    suspended: bool,
    created: i64,
    updated: i64,
    history: History,
}

impl Walk<'_, '_> {
    /// One card: converted, compared, counted, and written when applying.
    fn card(&mut self, card: &CardRow, revs: &[RevRow]) -> Result<()> {
        let Some(planned) = self.plan_card(card, revs) else {
            return Ok(());
        };
        self.add_to_digest(&planned);
        let key = name_key(&planned.deck);
        let choice = if let Some(choice) = self.decks.get(&key) {
            choice.clone()
        } else {
            let choice = match self.live_decks.get(&key) {
                Some(id) => DeckChoice::Existing(id.clone()),
                None => DeckChoice::New(match &mut self.writer {
                    Some(w) => w.deck(&planned.deck)?,
                    None => String::new(),
                }),
            };
            self.decks.insert(key, choice.clone());
            choice
        };
        // Where the cards go is a decision too: a deck renamed since the
        // plan would send them elsewhere.
        match &choice {
            DeckChoice::Existing(id) => self.digest.update(id.as_bytes()),
            DeckChoice::New(_) => self.digest.update(b"new"),
        }
        self.digest.update([0]);
        for (media, file) in self.resolver.used.drain(..) {
            if self.used_set.insert(media) {
                self.used_media.push((media, file));
            }
        }
        self.tally.cards += 1;
        self.tally.suspended += usize::from(planned.suspended);
        self.tally.reviews += planned.history.reviews();
        for tag in &planned.tags {
            self.tags_seen.insert(name_key(tag));
        }
        if let Some(w) = &mut self.writer {
            let p = &planned;
            let card_id = w.card(
                choice.id(),
                &p.front,
                &p.back,
                p.created,
                p.updated,
                p.suspended,
            )?;
            for tag in &p.tags {
                w.tag(&card_id, tag)?;
            }
            if !p.history.entries.is_empty() {
                for entry in &p.history.entries {
                    match *entry {
                        Entry::Review(at, grade, ms) => w.review(&card_id, at, grade, ms)?,
                        Entry::Reset(at) => w.reset(&card_id, at)?,
                    }
                }
                self.reviewed.push(card_id);
            }
        }
        Ok(())
    }

    /// The card as it would be added, or `None` (reported) if it is not.
    fn plan_card(&mut self, card: &CardRow, revs: &[RevRow]) -> Option<Planned> {
        let deck_id = if card.odid != 0 { card.odid } else { card.did };
        let anki_deck = self
            .anki_decks
            .get(&deck_id)
            .map_or("Default", String::as_str);
        let (deck, cut) = rappel_deck_name(anki_deck);
        if cut && self.shortened.insert(deck_id) {
            self.report.add(Skip::DeckNameShortened, &[anki_deck]);
        }
        let report = &mut self.report;
        let Some(note) = &card.note else {
            report.add(Skip::BrokenNote, &[]);
            return None;
        };
        let (Some(notetype), Some(parsed)) =
            (self.notetypes.get(&note.mid), self.parsed.get(&note.mid))
        else {
            report.add(Skip::BrokenNote, &[]);
            return None;
        };
        let fields = match &note.fields {
            Column::Kept(fields) => fields,
            Column::TooLong => {
                report.add(Skip::TooLong, &[]);
                return None;
            }
            Column::Missing => {
                report.add(Skip::BrokenNote, &[]);
                return None;
            }
        };
        let fields: Vec<&str> = fields.split('\u{1f}').collect();
        let example = super::html::plain_text(fields.first().copied().unwrap_or(""));
        let tags = match &note.tags {
            Column::Kept(tags) => tags,
            Column::TooLong => {
                report.add(Skip::TooLong, &[&example]);
                return None;
            }
            Column::Missing => {
                report.add(Skip::BrokenNote, &[&example]);
                return None;
            }
        };
        self.resolver.used.clear();
        // Templates see the name as Anki shows it.
        let shown_deck = anki_deck.replace('\u{1f}', "::");
        let info = CardInfo {
            ord: card.ord,
            tags,
            deck: &shown_deck,
            flag: 0,
        };
        let sides = match card_sides(notetype, parsed, &fields, info, &mut self.resolver, report) {
            Ok(sides) => sides,
            Err(skip) => {
                report.add(skip, &[&example]);
                return None;
            }
        };
        if self
            .existing
            .contains(&card_key(&name_key(&deck), &sides.front, &sides.back))
        {
            report.add(Skip::AlreadyInRappel, &[&example]);
            return None;
        }
        let created = card_time(card.id, self.now);
        let updated = note
            .modified
            .checked_mul(1000)
            .filter(|t| timestamp_ok(*t) && *t >= created && *t <= self.now.max(created))
            .unwrap_or(created);
        Some(Planned {
            deck,
            front: sides.front,
            back: sides.back,
            tags: card_tags(tags, report, &mut self.invalid_tags),
            suspended: card.queue == -1,
            created,
            updated,
            history: history(revs, card.kind == 0, self.now, report),
        })
    }

    fn add_to_digest(&mut self, p: &Planned) {
        let d = &mut self.digest;
        for text in [&p.deck, &p.front, &p.back] {
            d.update(text.as_bytes());
            d.update([0]);
        }
        for tag in &p.tags {
            d.update(tag.as_bytes());
            d.update([0]);
        }
        d.update([u8::from(p.suspended)]);
        d.update(p.created.to_le_bytes());
        d.update(p.updated.to_le_bytes());
        for entry in &p.history.entries {
            match *entry {
                Entry::Review(at, grade, ms) => {
                    d.update([1]);
                    d.update(at.to_le_bytes());
                    d.update([u8::from(grade == Grade::Pass)]);
                    d.update(ms.to_le_bytes());
                }
                Entry::Reset(at) => {
                    d.update([2]);
                    d.update(at.to_le_bytes());
                }
            }
        }
        d.update([0]);
    }

    /// The summary and the digest, with the media the cards use.
    fn finish(mut self, conn: &Connection, format: super::AnkiFormat) -> Result<Outcome> {
        self.used_media.sort_by_key(|(media, _)| *media);
        for (media, _) in &self.used_media {
            self.digest.update(media.hash());
            self.digest.update(media.kind().extension().as_bytes());
        }
        let present = present_media(conn, &self.used_media)?;
        for reason in self.resolver.report.skipped() {
            let examples: Vec<&str> = reason.examples.iter().map(String::as_str).collect();
            self.report.add_n(reason.reason, reason.count, &examples);
        }
        let new_decks = self
            .decks
            .values()
            .filter(|c| matches!(c, DeckChoice::New(_)))
            .count();
        let summary = AnkiSummary {
            format,
            new_decks,
            existing_decks: self.decks.len() - new_decks,
            cards: self.tally.cards,
            suspended: self.tally.suspended,
            reviews: self.tally.reviews,
            new_media: self.used_media.len() - present,
            present_media: present,
            tags: self.tags_seen.len(),
            skipped: self.report.skipped(),
        };
        Ok(Outcome {
            summary,
            digest: self.digest.finalize().into(),
            used_media: self.used_media,
            reviewed: self.reviewed,
        })
    }
}

#[cfg(test)]
thread_local! {
    /// Runs just before an apply's last cancel check and its commit, so
    /// tests can cancel there.
    pub(super) static BEFORE_COMMIT: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Runs after each card of a walk, so tests can cancel in the middle.
    pub(super) static AFTER_CARD: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// After each card of a walk (a test hook; nothing otherwise).
fn after_card() {
    #[cfg(test)]
    AFTER_CARD.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook();
        }
    });
}

/// Before an apply commits (a test hook; nothing otherwise).
fn before_commit() {
    #[cfg(test)]
    BEFORE_COMMIT.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook();
        }
    });
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::ImportCancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Tally {
    cards: usize,
    suspended: usize,
    reviews: usize,
}

/// Where a deck's cards go.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeckChoice {
    /// A live deck of the database with the same name.
    Existing(String),
    /// A new deck (its id when applying, else empty).
    New(String),
}

impl DeckChoice {
    fn id(&self) -> &str {
        match self {
            Self::Existing(id) | Self::New(id) => id,
        }
    }
}

/// The media list by name: names that cannot be file names, and names
/// used twice, are refused.
fn media_table(
    listed: Vec<super::package::ListedFile>,
    limits: &AnkiLimits,
    report: &mut Report,
) -> HashMap<String, MediaState> {
    let mut table: HashMap<String, MediaState> = HashMap::with_capacity(listed.len());
    let mut bad = Vec::new();
    for file in listed {
        let name = file.name;
        let usable = !name.is_empty()
            && name.len() <= limits.media_name
            && !name.contains(['/', '\\', '\0'])
            && name != "."
            && !name.starts_with("..");
        if !usable || table.contains_key(&name) {
            if table.insert(name.clone(), MediaState::Bad(Skip::BadMediaName))
                != Some(MediaState::Bad(Skip::BadMediaName))
            {
                bad.push(name);
            }
            continue;
        }
        table.insert(name, MediaState::Listed { file: file.file });
    }
    let examples: Vec<&str> = bad.iter().map(String::as_str).collect();
    report.add_n(Skip::BadMediaName, bad.len(), &examples);
    table
}

/// `%XX` sequences decoded, if there are any and the result is UTF-8.
fn percent_decoded(name: &str) -> Option<String> {
    if !name.contains('%') {
        return None;
    }
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Resolves the media names of one card, noting what cannot be used.
struct MediaResolver<'m> {
    media: &'m HashMap<String, MediaState>,
    /// The files this card uses.
    used: Vec<(MediaRef, u32)>,
    /// (reason, name) already noted, so each name counts once.
    noted: HashSet<(Skip, String)>,
    report: Report,
}

impl MediaResolver<'_> {
    fn note(&mut self, reason: Skip, name: &str) {
        if self.noted.len() < 1_000_000 && self.noted.insert((reason, name.to_owned())) {
            self.report.add(reason, &[name]);
        }
    }
}

impl Resolver for MediaResolver<'_> {
    fn media(&mut self, name: &str, use_: MediaUse) -> Option<MediaRef> {
        let state = self
            .media
            .get(name)
            .or_else(|| percent_decoded(name).and_then(|d| self.media.get(&d)))
            .copied();
        match state {
            None | Some(MediaState::Listed { .. }) => {
                self.note(Skip::MissingMedia, name);
                None
            }
            // A bad name is counted once, with the list.
            Some(MediaState::Bad(Skip::BadMediaName)) => None,
            Some(MediaState::Bad(reason)) => {
                self.note(reason, name);
                None
            }
            Some(MediaState::Ok { media, file }) => {
                let fits = match use_ {
                    MediaUse::Image => media.kind().is_image(),
                    MediaUse::Audio => !media.kind().is_image(),
                };
                if fits {
                    self.used.push((media, file));
                    Some(media)
                } else {
                    self.note(Skip::UnsupportedMedia, name);
                    None
                }
            }
        }
    }

    fn remote_image(&mut self, src: &str) {
        let shown = if src
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case("data:"))
        {
            "data:…"
        } else {
            src
        };
        self.note(Skip::RemoteImage, shown);
    }

    fn math_ok(&mut self, source: &str, display: bool) -> bool {
        math_drawable(source, display)
    }
}

/// An Anki deck name as a Rappel one: its parts (separated by `::`, or
/// U+001F in a current collection) trimmed, empty ones dropped, joined by
/// " › "; control characters are spaces; "Default" if nothing is left.
/// Shortened to Rappel's limit (with "…") if longer; the flag says so.
pub(super) fn rappel_deck_name(anki: &str) -> (String, bool) {
    let cleaned: String = anki
        .chars()
        .map(|c| match c {
            '\u{1f}' => '\u{1f}',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let parts: Vec<&str> = cleaned
        .split(['\u{1f}'])
        .flat_map(|p| p.split("::"))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let name = if parts.is_empty() {
        "Default".to_owned()
    } else {
        parts.join(DECK_SEPARATOR)
    };
    if name.len() <= MAX_DECK_NAME_BYTES {
        return (name, false);
    }
    let mut cut = MAX_DECK_NAME_BYTES - '…'.len_utf8();
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    (format!("{}…", name[..cut].trim_end()), true)
}

/// A note's tags as Rappel tags: split at whitespace, each checked; at most
/// [`MAX_TAGS`], each name once. An invalid tag is reported once in the
/// whole import (`reported`), a card with too many once.
fn card_tags(tags: &str, report: &mut Report, reported: &mut HashSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut keys: HashSet<String> = HashSet::new();
    let mut extra = 0;
    for tag in tags.split_whitespace() {
        let tag = tag.replace('\0', "\u{FFFD}");
        if check_tag_name(&tag).is_err() {
            // A bounded set: past it, tags are counted again.
            if reported.len() >= 100_000 || reported.insert(tag.clone()) {
                report.add(Skip::InvalidTag, &[&tag]);
            }
            continue;
        }
        if !keys.insert(name_key(&tag)) {
            continue;
        }
        if out.len() == MAX_TAGS {
            extra += 1;
            continue;
        }
        out.push(tag);
    }
    if extra > 0 {
        report.add(Skip::TooManyTags, &[]);
    }
    out
}

/// One entry of a card's log in Rappel's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Entry {
    /// A review: time, grade, duration in ms.
    Review(i64, Grade, i64),
    /// A reset (Anki's "Forget").
    Reset(i64),
}

/// A card's review history in Rappel's terms, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct History {
    pub(super) entries: Vec<Entry>,
}

impl History {
    /// How many reviews it has.
    pub(super) fn reviews(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, Entry::Review(..)))
            .count()
    }
}

/// Maps Anki's log entries of one card (oldest first) to Rappel's. Again
/// is Fail; Hard, Good and Easy are Pass. Kept: learning, review and
/// relearning entries (types 0 to 2), and filtered-deck entries that
/// rescheduled (type 3 with an ease factor; Anki 26.9 logs a review
/// answered early in a filtered deck that reschedules so, and a preview
/// or cram that does not with factor 0, checked with its library). A
/// "Forget" (a manual entry, type 4 or 5, with ease, interval and factor 0)
/// after a review is a reset at its time. Left out and counted: other
/// manual entries ("Set Due Date"), filtered entries without a factor,
/// anything out of range or after `now` (the plan's clock). A card that is
/// new in Anki (`is_new`) whose history ends with a review was forgotten
/// otherwise: a reset follows it, so it is new in Rappel too.
pub(super) fn history(revs: &[RevRow], is_new: bool, now: i64, report: &mut Report) -> History {
    let mut out = History::default();
    let (mut manual, mut filtered, mut bad) = (0, 0, 0);
    let mut reviewed = false;
    for rev in revs {
        if !timestamp_ok(rev.id) || rev.id >= super::super::validate::MAX_TIMESTAMP || rev.id > now
        {
            bad += 1;
            continue;
        }
        let grade = match rev.ease {
            1 => Some(Grade::Fail),
            2..=4 => Some(Grade::Pass),
            _ => None,
        };
        let forget = rev.ease == 0 && rev.ivl == 0 && rev.factor == 0;
        let manual_kind = matches!(rev.kind, 4 | 5);
        match grade {
            Some(grade) if (0..=2).contains(&rev.kind) || (rev.kind == 3 && rev.factor != 0) => {
                out.entries
                    .push(Entry::Review(rev.id, grade, duration(rev.time)));
                reviewed = true;
            }
            _ if rev.kind == 3 => filtered += 1,
            _ if manual_kind && forget && reviewed => {
                out.entries.push(Entry::Reset(rev.id));
                reviewed = false;
            }
            _ if manual_kind || (rev.ease == 0 && (0..=2).contains(&rev.kind)) => manual += 1,
            _ => bad += 1,
        }
    }
    report.add_n(Skip::ManualEntry, manual, &[]);
    report.add_n(Skip::FilteredReview, filtered, &[]);
    report.add_n(Skip::BadReview, bad, &[]);
    if is_new
        && reviewed
        && let Some(Entry::Review(last, _, _)) = out.entries.last()
    {
        out.entries.push(Entry::Reset(last + 1));
    }
    out
}

fn duration(ms: i64) -> i64 {
    ms.clamp(0, MAX_DURATION_MS)
}

/// When a card was made: its Anki id is the time in milliseconds; one out
/// of range (or after the plan's clock, `now`) is the plan's time.
fn card_time(id: i64, now: i64) -> i64 {
    if timestamp_ok(id) && id <= now {
        id
    } else {
        now
    }
}

/// The key a card is compared by: its deck's name key, front and back.
fn card_key(deck_key: &str, front: &str, back: &str) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(deck_key.as_bytes());
    hasher.update([0]);
    hasher.update(front.as_bytes());
    hasher.update([0]);
    hasher.update(back.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    let mut key = [0; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

/// The keys of the database's live cards in live decks.
fn existing_cards(conn: &Connection) -> Result<HashSet<[u8; 16]>> {
    let mut stmt = conn.prepare(
        "SELECT d.name_key, c.front_md, c.back_md FROM cards c JOIN decks d ON d.id = c.deck_id
         WHERE c.deleted_at IS NULL AND d.deleted_at IS NULL",
    )?;
    let mut rows = stmt.query([])?;
    let mut keys = HashSet::new();
    while let Some(row) = rows.next()? {
        let (deck, front, back): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        keys.insert(card_key(&deck, &front, &back));
    }
    Ok(keys)
}

/// The live decks by name key.
fn live_decks(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT name_key, id FROM decks WHERE deleted_at IS NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How many of `used` the media table has.
fn present_media(conn: &Connection, used: &[(MediaRef, u32)]) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT EXISTS (SELECT 1 FROM media WHERE hash = ?1)")?;
    let mut present = 0;
    for (media, _) in used {
        let has: bool = stmt.query_row([media.hex()], |r| r.get(0))?;
        present += usize::from(has);
    }
    Ok(present)
}

/// Writes an import's rows, inside the caller's transaction.
struct Writer<'t> {
    conn: &'t Connection,
    ids: &'t mut IdGen,
    now: i64,
    card: Statement<'t>,
    link: Statement<'t>,
    event: Statement<'t>,
}

impl<'t> Writer<'t> {
    fn new(conn: &'t Connection, ids: &'t mut IdGen, now: i64) -> Result<Self> {
        Ok(Self {
            conn,
            ids,
            now,
            card: conn.prepare(
                "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at, suspended)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?,
            link: conn.prepare("INSERT OR IGNORE INTO card_tags (card_id, tag_id) VALUES (?1, ?2)")?,
            event: conn.prepare(
                "INSERT INTO review_events (id, card_id, kind, reviewed_at, grade, duration_ms, undoes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            )?,
        })
    }

    fn deck(&mut self, name: &str) -> Result<String> {
        let id = self.ids.next(self.now)?;
        self.conn.execute(
            "INSERT INTO decks (id, name, name_key, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, name, name_key(name), self.now],
        )?;
        Ok(id)
    }

    fn card(
        &mut self,
        deck_id: &str,
        front: &str,
        back: &str,
        created: i64,
        updated: i64,
        suspended: bool,
    ) -> Result<String> {
        let id = self.ids.next(self.now)?;
        self.card.execute(params![
            id, deck_id, front, back, created, updated, suspended
        ])?;
        index_card_in(self.conn, &id, front, back)?;
        Ok(id)
    }

    fn tag(&mut self, card_id: &str, name: &str) -> Result<()> {
        let tag = find_or_create_tag(self.conn, self.ids, name, self.now)?;
        self.link.execute(params![card_id, tag.id])?;
        Ok(())
    }

    fn review(&mut self, card_id: &str, at: i64, grade: Grade, duration_ms: i64) -> Result<()> {
        let event = ReviewEvent {
            id: self.ids.next(self.now)?,
            card_id: card_id.to_owned(),
            reviewed_at: at,
            kind: EventKind::Review(grade),
            duration_ms,
        };
        self.event.execute(params![
            event.id,
            event.card_id,
            event.kind.as_str(),
            event.reviewed_at,
            grade.as_str(),
            event.duration_ms
        ])?;
        Ok(())
    }

    fn reset(&mut self, card_id: &str, at: i64) -> Result<()> {
        let id = self.ids.next(self.now)?;
        self.event.execute(params![
            id,
            card_id,
            EventKind::Reset.as_str(),
            at,
            None::<&str>,
            0
        ])?;
        Ok(())
    }
}
