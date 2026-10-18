//! Tests of unused media and the media trash (ADR 0057): what is unused,
//! the clean-up and every way it can fail or be cut off, undo, bringing
//! files back (open, restore, import, card writes), the purge, a second
//! connection, hostile names, and a time bound.

use std::cell::Cell;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::trash::{Disk, Mover, folder_day, folder_name, purgeable};
use super::*;
use crate::storage::{Store, create_backup, list_backups, restore_backup};
use crate::test_support::{DAY_MS, T0, assert_cpu_within, fastest, png_bytes, wav_bytes};
use crate::{Error, Result};

/// A store in a new data directory.
fn store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&data(&tmp)).unwrap();
    (tmp, store)
}

fn data(tmp: &tempfile::TempDir) -> PathBuf {
    tmp.path().join("data")
}

fn media(tmp: &tempfile::TempDir) -> PathBuf {
    data(tmp).join("media")
}

fn trash(tmp: &tempfile::TempDir) -> PathBuf {
    data(tmp).join("media-trash")
}

/// The trash folder of the UTC day of `now`.
fn day_folder(tmp: &tempfile::TempDir, now: i64) -> PathBuf {
    trash(tmp).join(folder_name(now, 1))
}

/// No names, files or rows.
fn none() -> Vec<String> {
    Vec::new()
}

/// The names in `dir`, sorted; none for a missing folder.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn row_hashes(store: &Store) -> Vec<String> {
    store
        .conn
        .prepare("SELECT hash FROM media ORDER BY hash")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn rows(store: &Store) -> Vec<(String, String, i64, i64)> {
    store
        .conn
        .prepare("SELECT hash, mime, bytes, created_at FROM media ORDER BY hash")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The reference of a PNG made from `n`, without adding it.
fn png_ref(n: u32) -> MediaRef {
    let bytes = png_bytes(n);
    MediaRef::new(Sha256::digest(&bytes).into(), MediaKind::Png)
}

fn image(media: &MediaRef) -> String {
    format!("![]({})", media.reference())
}

/// A clean-up at `now` with the real renames.
fn clean_up_at(store: &mut Store, now: i64) -> Result<CleanedUp> {
    store.clean_up_media_with(now, &Disk, &|| Ok(()))
}

/// Hostile or foreign things in the media folder, which are never
/// touched: a temporary file, a damaged copy, an upper-case name, a foreign
/// name, a link with a media name to a file outside, a folder with a media
/// name. Returns the plain files' names.
fn plant_hostile(tmp: &tempfile::TempDir) -> Vec<String> {
    let outside = tmp.path().join("outside.png");
    fs::write(&outside, png_bytes(10)).unwrap();
    let hostile = vec![
        ".incoming-0123456789abcdef".to_owned(),
        format!("{}.damaged-0011223344556677", png_ref(11).file_name()),
        png_ref(12).file_name().to_uppercase(),
        "notes.txt".to_owned(),
    ];
    for name in &hostile {
        fs::write(media(tmp).join(name), b"keep me").unwrap();
    }
    symlink(&outside, media(tmp).join(png_ref(10).file_name())).unwrap();
    let folder = media(tmp).join(png_ref(13).file_name());
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("inner"), b"x").unwrap();
    hostile
}

/// What [`plant_hostile`] planted is all there.
fn assert_untouched(tmp: &tempfile::TempDir, hostile: &[String]) {
    for name in hostile {
        assert_eq!(fs::read(media(tmp).join(name)).unwrap(), b"keep me");
    }
    assert_eq!(
        fs::read(tmp.path().join("outside.png")).unwrap(),
        png_bytes(10)
    );
    let link = fs::symlink_metadata(media(tmp).join(png_ref(10).file_name())).unwrap();
    assert!(link.file_type().is_symlink());
    assert!(
        media(tmp)
            .join(png_ref(13).file_name())
            .join("inner")
            .exists()
    );
}

/// Unused = no card names it, live or deleted, whatever deck. Rows without
/// a card, and media-named files without a row, are unused; anything else
/// in the folder is never looked at.
#[test]
fn unused_files_are_those_no_card_names() {
    let (tmp, mut store) = store();
    let live = store.create_deck("Live").unwrap().id;
    let gone = store.create_deck("Gone").unwrap().id;
    let add = |store: &Store, n: u32| store.add_media_bytes(&png_bytes(n)).unwrap();
    let live_a = add(&store, 1); // a live card
    let deleted_b = add(&store, 2); // a deleted card
    let in_gone_c = add(&store, 3); // a card of live_a deleted deck
    let unused_d = add(&store, 4); // no card: unused
    let in_code_f = add(&store, 6); // only named in code: kept, to be safe
    let linked_g = store.add_media_bytes(&wav_bytes(7)).unwrap(); // a link
    let other_ext_h = add(&store, 8); // named as another kind: the hash counts
    let entity_i = add(&store, 9); // named through an entity reference
    store.add_card(&live, &image(&live_a), "back").unwrap();
    let deleted = store
        .add_card(&live, "front", &image(&deleted_b))
        .unwrap()
        .id;
    store.delete_card(&deleted).unwrap();
    store.add_card(&gone, &image(&in_gone_c), "").unwrap();
    store.delete_deck(&gone).unwrap();
    store
        .add_card(&live, &format!("`{}`", in_code_f.reference()), "")
        .unwrap();
    store
        .add_card(&live, &format!("[listen]({})", linked_g.reference()), "")
        .unwrap();
    store
        .add_card(&live, &format!("![](media:{}.jpg)", other_ext_h.hex()), "")
        .unwrap();
    store
        .add_card(
            &live,
            &format!("![](&#109;edia:{}.png)", entity_i.hex()),
            "",
        )
        .unwrap();

    // An orphan: a media name, no row, no card (a failed import).
    let orphan_e = png_ref(5);
    fs::write(media(&tmp).join(orphan_e.file_name()), png_bytes(5)).unwrap();
    let hostile = plant_hostile(&tmp);

    let size = |n| png_bytes(n).len() as u64;
    assert_eq!(
        store.unused_media().unwrap(),
        UnusedMedia {
            files: 2,
            bytes: size(4) + size(5)
        }
    );
    let before = names(&media(&tmp));
    let cleaned = clean_up_at(&mut store, T0).unwrap();
    assert_eq!((cleaned.files, cleaned.bytes), (2, size(4) + size(5)));
    let mut moved = vec![unused_d.file_name(), orphan_e.file_name()];
    moved.sort();
    assert_eq!(names(&day_folder(&tmp, T0)), moved);
    let mut left = before.clone();
    left.retain(|n| !moved.contains(n));
    assert_eq!(names(&media(&tmp)), left);
    assert_untouched(&tmp, &hostile);
    // Only the unused row went.
    let mut kept: Vec<String> = [
        live_a,
        deleted_b,
        in_gone_c,
        in_code_f,
        linked_g,
        other_ext_h,
        entity_i,
    ]
    .iter()
    .map(MediaRef::hex)
    .collect();
    kept.sort();
    assert_eq!(row_hashes(&store), kept);
    assert_eq!(store.unused_media().unwrap(), UnusedMedia::default());
    // Nothing more to do.
    let again = clean_up_at(&mut store, T0).unwrap();
    assert_eq!((again.files, again.bytes), (0, 0));
}

/// A row whose file is missing (lost, or in the trash already) goes on its
/// own; a row beside a folder of its name stays.
#[test]
fn rows_without_files_go_and_rows_beside_folders_stay() {
    let (tmp, mut store) = store();
    let a = store.add_media_bytes(&png_bytes(1)).unwrap();
    let b = store.add_media_bytes(&png_bytes(2)).unwrap();
    fs::remove_file(media(&tmp).join(a.file_name())).unwrap();
    fs::remove_file(media(&tmp).join(b.file_name())).unwrap();
    fs::create_dir(media(&tmp).join(b.file_name())).unwrap();
    assert_eq!(store.unused_media().unwrap(), UnusedMedia::default());
    let cleaned = clean_up_at(&mut store, T0).unwrap();
    assert_eq!(cleaned.files, 0);
    assert_eq!(row_hashes(&store), [b.hex()]);
    assert!(media(&tmp).join(b.file_name()).is_dir());
}

/// The same file cleaned up twice in one day goes to a second folder of
/// that day: nothing is ever written over.
#[test]
fn a_name_taken_in_the_trash_goes_to_another_folder() {
    let (tmp, mut store) = store();
    let a = store.add_media_bytes(&png_bytes(1)).unwrap();
    clean_up_at(&mut store, T0).unwrap();
    store.add_media_bytes(&png_bytes(1)).unwrap();
    // A link in the way of the next folder is skipped, never followed.
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, trash(&tmp).join(folder_name(T0, 2))).unwrap();
    let cleaned = clean_up_at(&mut store, T0 + 1000).unwrap();
    assert_eq!(cleaned.files, 1);
    let first = day_folder(&tmp, T0).join(a.file_name());
    let third = trash(&tmp).join(folder_name(T0, 3)).join(a.file_name());
    assert_eq!(fs::read(first).unwrap(), png_bytes(1));
    assert_eq!(fs::read(third).unwrap(), png_bytes(1));
    assert_eq!(names(&elsewhere), none());
    assert_eq!(names(&media(&tmp)), none());
}

/// A trash that is not a real folder refuses the clean-up, with nothing
/// moved; so does a clock set before Rappel could clean up.
#[test]
fn a_linked_trash_or_a_wrong_clock_refuses_the_clean_up() {
    let (tmp, mut store) = store();
    let a = store.add_media_bytes(&png_bytes(1)).unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, trash(&tmp)).unwrap();
    assert!(matches!(clean_up_at(&mut store, T0), Err(Error::Io { .. })));
    assert_eq!(names(&elsewhere), none());
    assert_eq!(names(&media(&tmp)), [a.file_name()]);
    assert_eq!(row_hashes(&store), [a.hex()]);
    fs::remove_file(trash(&tmp)).unwrap();
    for now in [0, T0 - 400 * DAY_MS, i64::MAX] {
        assert!(matches!(
            clean_up_at(&mut store, now),
            Err(Error::ClockWrong)
        ));
    }
    assert_eq!(names(&media(&tmp)), [a.file_name()]);
    assert!(!trash(&tmp).exists());
}

/// Renames that fail from call `from` to call `to` (counted from 0).
struct Failing {
    calls: Cell<usize>,
    from: usize,
    to: usize,
}

impl Failing {
    fn new(from: usize, to: usize) -> Self {
        Self {
            calls: Cell::new(0),
            from,
            to,
        }
    }
}

impl Mover for Failing {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let call = self.calls.replace(self.calls.get() + 1);
        if (self.from..=self.to).contains(&call) {
            return Err(io::Error::other("injected"));
        }
        fs::rename(from, to)
    }
}

/// Three unused files, for the failure tests.
fn three_unused() -> (tempfile::TempDir, Store, Vec<String>) {
    let (tmp, store) = store();
    for n in 1..=3 {
        store.add_media_bytes(&png_bytes(n)).unwrap();
    }
    let files = names(&media(&tmp));
    (tmp, store, files)
}

fn trashed(tmp: &tempfile::TempDir) -> Vec<String> {
    let mut out = Vec::new();
    for folder in names(&trash(tmp)) {
        out.extend(names(&trash(tmp).join(folder)));
    }
    out.sort();
    out
}

/// A rename that fails half way: the files already moved come back, and
/// every row stays.
#[test]
fn a_failed_move_puts_everything_back() {
    let (tmp, mut store, files) = three_unused();
    let rows_before = rows(&store);
    // The third move fails; the two moves back work.
    let mover = Failing::new(2, 2);
    let err = store.clean_up_media_with(T0, &mover, &|| Ok(()));
    assert!(matches!(err, Err(Error::Io { .. })), "{err:?}");
    assert_eq!(names(&media(&tmp)), files);
    assert_eq!(trashed(&tmp), none());
    assert_eq!(rows(&store), rows_before);
}

/// A failure after the moves, just before the commit: the rows roll back
/// and the files come back.
#[test]
fn a_failure_before_the_commit_puts_everything_back() {
    let (tmp, mut store, files) = three_unused();
    let rows_before = rows(&store);
    let err = store.clean_up_media_with(T0, &Disk, &|| Err(Error::NoMediaDir));
    assert!(matches!(err, Err(Error::NoMediaDir)));
    assert_eq!(names(&media(&tmp)), files);
    assert_eq!(trashed(&tmp), none());
    assert_eq!(rows(&store), rows_before);
}

/// A failure before the commit whose moving back fails too: the files stay
/// in the trash with their rows, and the next open brings them back.
#[test]
fn files_that_cannot_go_back_return_at_the_next_open() {
    let (tmp, mut store, files) = three_unused();
    let rows_before = rows(&store);
    // Three moves succeed, then every rename (the moves back) fails.
    let mover = Failing::new(3, usize::MAX);
    let err = store.clean_up_media_with(T0, &mover, &|| Err(Error::NoMediaDir));
    assert!(err.is_err());
    assert_eq!(names(&media(&tmp)), none());
    assert_eq!(trashed(&tmp), files);
    assert_eq!(rows(&store), rows_before);
    drop(store);
    let store = Store::open(&data(&tmp)).unwrap();
    assert_eq!(names(&media(&tmp)), files);
    assert_eq!(trashed(&tmp), none());
    assert_eq!(rows(&store), rows_before, "the rows are untouched");
}

/// A crash between the moves and the commit (some or all files moved, the
/// rows still there): the next open brings the files back.
#[test]
fn a_crash_before_the_commit_is_repaired_at_the_next_open() {
    for moved in [1, 3] {
        let (tmp, store, files) = three_unused();
        let rows_before = rows(&store);
        drop(store);
        // What the crash left: the folder made, `moved` files renamed.
        let folder = day_folder(&tmp, T0);
        fs::create_dir_all(&folder).unwrap();
        for name in &files[..moved] {
            fs::rename(media(&tmp).join(name), folder.join(name)).unwrap();
        }
        let store = Store::open(&data(&tmp)).unwrap();
        assert_eq!(names(&media(&tmp)), files, "{moved} moved");
        assert_eq!(trashed(&tmp), none());
        assert_eq!(rows(&store), rows_before);
    }
}

/// A crash after the commit: the rows are gone and the files are in the
/// trash, which the open leaves as it is.
#[test]
fn a_crash_after_the_commit_leaves_the_trash_alone() {
    let (tmp, mut store, files) = three_unused();
    clean_up_at(&mut store, T0).unwrap();
    drop(store);
    let store = Store::open(&data(&tmp)).unwrap();
    assert_eq!(names(&media(&tmp)), none());
    assert_eq!(trashed(&tmp), files);
    assert_eq!(row_hashes(&store), none());
}

/// Undo puts back the files and their rows as they were, and keeps what
/// was written since: a file added again stays, its row too.
#[test]
fn undo_puts_back_what_is_still_free() {
    let (tmp, mut store, files) = three_unused();
    let rows_before = rows(&store);
    let cleaned = clean_up_at(&mut store, T0).unwrap();
    assert_eq!(cleaned.files, 3);
    assert_eq!(store.undo_clean_up(&cleaned).unwrap(), 3);
    assert_eq!(names(&media(&tmp)), files);
    assert_eq!(trashed(&tmp), none());
    assert_eq!(rows(&store), rows_before, "created_at kept");

    // Cleaned again; one file comes back by an add before the undo.
    let cleaned = clean_up_at(&mut store, T0 + 1000).unwrap();
    store.add_media_bytes(&png_bytes(1)).unwrap();
    let readded = rows(&store);
    assert_eq!(store.undo_clean_up(&cleaned).unwrap(), 2);
    assert_eq!(names(&media(&tmp)), files);
    assert_eq!(trashed(&tmp), [png_ref(1).file_name()]);
    let after = rows(&store);
    assert_eq!(after.len(), 3);
    assert!(after.contains(&readded[0]), "the add's row is kept");
}

/// An add and a clean-up never interleave: the add names its file only
/// with the write lock, so a clean-up holding it never sees the file
/// without its row; and a clean-up waiting for an add's lock sees the file
/// with its row, and takes both, never one.
#[test]
fn an_add_and_a_clean_up_take_turns() {
    let (tmp, mut store) = store();
    let dir = data(&tmp);
    // The lock, as a clean-up holds it.
    store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let adding = std::thread::spawn({
        let dir = dir.clone();
        move || {
            let other = Store::open_concurrent(&dir).unwrap();
            other.add_media_bytes(&png_bytes(1)).unwrap()
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    let a = png_ref(1);
    assert!(
        !media(&tmp).join(a.file_name()).exists(),
        "no name while a clean-up holds the lock"
    );
    store.conn.execute_batch("COMMIT").unwrap();
    assert_eq!(adding.join().unwrap(), a);
    assert_eq!(row_hashes(&store), [a.hex()]);

    // Another connection holds the lock (an add between naming and its
    // row); the clean-up waits for it.
    let other = Store::open_concurrent(&dir).unwrap();
    other.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let b = png_ref(2);
    fs::write(media(&tmp).join(b.file_name()), png_bytes(2)).unwrap();
    let cleaning = std::thread::spawn(move || {
        let r = clean_up_at(&mut store, T0);
        (store, r)
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        media(&tmp).join(b.file_name()).exists(),
        "the clean-up waits"
    );
    other
        .conn
        .execute(
            "INSERT INTO media (hash, mime, bytes, created_at) VALUES (?1, 'image/png', ?2, 0)",
            rusqlite::params![b.hex(), i64::try_from(png_bytes(2).len()).unwrap()],
        )
        .unwrap();
    other.conn.execute_batch("COMMIT").unwrap();
    let (mut store, cleaned) = cleaning.join().unwrap();
    assert_eq!(cleaned.unwrap().files, 2);
    assert_eq!(row_hashes(&store), none(), "no row without its file");
    assert_eq!(names(&media(&tmp)), none());

    // The editor that held `b`'s reference saves its card: it comes back.
    let deck = store.create_deck("Deck").unwrap().id;
    store.add_card(&deck, &image(&b), "").unwrap();
    assert_eq!(names(&media(&tmp)), [b.file_name()]);
    assert_eq!(row_hashes(&store), [b.hex()]);
    assert_eq!(store.unused_media().unwrap().files, 0);
}

/// Every card write brings back the files it names from the trash, with a
/// row made from the file's name and size.
#[test]
fn a_card_write_brings_its_files_back() {
    let (tmp, mut store) = store();
    let a = store.add_media_bytes(&png_bytes(1)).unwrap();
    let w = store.add_media_bytes(&wav_bytes(2)).unwrap();
    clean_up_at(&mut store, T0).unwrap();
    assert_eq!(row_hashes(&store), none());
    let deck = store.create_deck("Deck").unwrap().id;
    let card = store.add_card(&deck, "plain", "").unwrap().id;
    assert_eq!(names(&media(&tmp)), none());
    store
        .edit_card(&card, &image(&a), &format!("[]({})", w.reference()))
        .unwrap();
    let mut both = vec![a.file_name(), w.file_name()];
    both.sort();
    assert_eq!(names(&media(&tmp)), both);
    let got = rows(&store);
    assert_eq!(got.len(), 2);
    for (hash, mime, bytes, _) in got {
        if hash == a.hex() {
            assert_eq!(
                (mime.as_str(), bytes),
                ("image/png", i64::try_from(png_bytes(1).len()).unwrap())
            );
        } else {
            assert_eq!(
                (mime.as_str(), bytes),
                ("audio/wav", i64::try_from(wav_bytes(2).len()).unwrap())
            );
        }
    }
    assert_eq!(trashed(&tmp), none());
}

/// Restoring a backup made before a clean-up brings its files back, with
/// their rows, and they are what the cards show.
#[test]
fn a_restore_brings_back_the_files_its_backup_uses() {
    let (tmp, mut store) = store();
    let deck = store.create_deck("Deck").unwrap().id;
    let a = store.add_media_bytes(&png_bytes(1)).unwrap();
    let card = store.add_card(&deck, &image(&a), "").unwrap().id;
    // A card that names a file with no row (typed by hand): its file is
    // in the trash already.
    let typed = png_ref(9);
    store.add_card(&deck, &image(&typed), "").unwrap();
    let backup_dir = data(&tmp).join("backups");
    create_backup(&store.conn, &backup_dir, T0 - DAY_MS).unwrap();
    store.edit_card(&card, "no image", "").unwrap();
    store.delete_card(&card).unwrap();
    // Deleted, but its last text names nothing: unused.
    clean_up_at(&mut store, T0).unwrap();
    let rows_before = rows(&store);
    assert_eq!(names(&media(&tmp)), none());
    let other_day = trash(&tmp).join("2026-10-05");
    fs::create_dir_all(&other_day).unwrap();
    fs::write(other_day.join(typed.file_name()), png_bytes(9)).unwrap();

    let backup = list_backups(&data(&tmp)).unwrap().remove(0);
    let restored = restore_backup(&data(&tmp), Some(store), &backup).unwrap();
    let mut both = vec![a.file_name(), typed.file_name()];
    both.sort();
    assert_eq!(names(&media(&tmp)), both);
    assert_eq!(trashed(&tmp), none());
    let back = rows(&restored.store);
    assert_eq!(back.len(), 2);
    assert_eq!(rows_before.len(), 0);
    let path = restored.store.media_path(&a).unwrap();
    assert_eq!(fs::read(path).unwrap(), png_bytes(1));
    assert!(
        back.iter()
            .any(|r| r.0 == typed.hex() && r.1 == "image/png")
    );
}

/// An import whose card names a file in the trash that the file does not
/// bring brings it back.
#[test]
fn an_import_brings_back_the_files_its_cards_name() {
    let (tmp, mut store) = store();
    let a = store.add_media_bytes(&png_bytes(1)).unwrap();
    clean_up_at(&mut store, T0).unwrap();
    // Another collection whose card names `a` without having it.
    let (other_tmp, mut other) = self::store();
    let deck = other.create_deck("Other").unwrap().id;
    other.add_card(&deck, &image(&a), "").unwrap();
    let file = other_tmp.path().join("other.rappel");
    other.export_to(&file).unwrap();
    let plan = store.plan_import(&file).unwrap();
    store.apply_import(&plan).unwrap();
    assert_eq!(names(&media(&tmp)), [a.file_name()]);
    assert_eq!(row_hashes(&store), [a.hex()]);
}

/// The trash at the UTC day of `now` with the PNGs `ns`, cleaned up.
fn trashed_at(now: i64, ns: &[u32]) -> (tempfile::TempDir, Store, Vec<MediaRef>) {
    let (tmp, mut store) = store();
    let refs: Vec<MediaRef> = ns
        .iter()
        .map(|n| store.add_media_bytes(&png_bytes(*n)).unwrap())
        .collect();
    clean_up_at(&mut store, now).unwrap();
    (tmp, store, refs)
}

/// Purged 31 days after the start of the folder's date, not a millisecond
/// before; the folder goes when empty. A clock set back never purges.
#[test]
fn the_purge_waits_thirty_days() {
    let day = folder_day(&folder_name(T0, 1)).unwrap();
    let (tmp, mut store, _) = trashed_at(T0, &[1, 2]);
    let at = day + 31 * DAY_MS;
    for now in [T0, T0 - 365 * DAY_MS, at - 1] {
        let tidied = store.tidy_media_trash_at(now).unwrap();
        assert_eq!(tidied, TrashTidied::default(), "{now}");
        assert_eq!(trashed(&tmp).len(), 2);
    }
    let tidied = store.tidy_media_trash_at(at).unwrap();
    assert_eq!((tidied.purged, tidied.kept, tidied.blocked), (2, 0, false));
    assert_eq!(tidied.purged_bytes, 2 * png_bytes(1).len() as u64);
    assert_eq!(names(&trash(&tmp)), none(), "the empty folder went");
    assert_eq!(names(&media(&tmp)), none());
}

#[test]
fn the_age_rule() {
    let day = folder_day("2026-10-18").unwrap();
    assert!(!purgeable(day, day + 31 * DAY_MS - 1));
    assert!(purgeable(day, day + 31 * DAY_MS));
    assert!(!purgeable(day, day - DAY_MS), "a clock set back");
    assert!(!purgeable(day, i64::MIN));
    // A folder no real clean-up could date.
    let early = folder_day("2025-01-01").unwrap();
    assert!(!purgeable(early, day + 365 * DAY_MS));
    assert!(purgeable(day, i64::MAX));
}

#[test]
fn folder_names_are_exact() {
    let day = folder_day("2026-10-18").unwrap();
    assert_eq!(folder_name(day + 5_000, 1), "2026-10-18");
    assert_eq!(folder_name(day, 2), "2026-10-18-2");
    assert_eq!(folder_day("2026-10-18-2"), Some(day));
    assert_eq!(folder_day("2026-10-18-999"), Some(day));
    for bad in [
        "2026-10-18-1",
        "2026-10-18-02",
        "2026-10-18-1000",
        "2026-10-18-",
        "2026-10-18x",
        "2026-02-30",
        "2026-13-01",
        "26-10-18",
        "2026_10_18",
        "+026-10-18",
        "2026-10-1é",
        "",
    ] {
        assert_eq!(folder_day(bad), None, "{bad}");
    }
}

/// Clean-up at `T0`, then the folder's date so a purge may run.
fn purge_time() -> i64 {
    folder_day(&folder_name(T0, 1)).unwrap() + 31 * DAY_MS
}

/// A file a backup uses (by a row or a card side, also in a backup made
/// before media existed) stays; the others go.
#[test]
fn the_purge_keeps_what_a_backup_uses() {
    let (tmp, mut store) = store();
    let deck = store.create_deck("Deck").unwrap().id;
    let a = store.add_media_bytes(&png_bytes(1)).unwrap();
    let card = store.add_card(&deck, "plain", "").unwrap().id;
    let backups = data(&tmp).join("backups");
    // A backup whose media row names `a`.
    create_backup(&store.conn, &backups, T0 - 2 * DAY_MS).unwrap();
    // `c` named only by a card of a backup, never a row.
    let c = png_ref(3);
    store.edit_card(&card, &image(&c), "").unwrap();
    store
        .conn
        .execute("DELETE FROM media WHERE hash = ?1", [a.hex()])
        .unwrap();
    create_backup(&store.conn, &backups, T0 - DAY_MS).unwrap();
    store.edit_card(&card, "plain", "").unwrap();
    // A backup of the oldest schema, cards only, naming `d`.
    let d = png_ref(4);
    let old = rusqlite::Connection::open(backups.join("rappel-20260101-000000.sqlite3")).unwrap();
    old.execute_batch("CREATE TABLE cards (front_md TEXT NOT NULL, back_md TEXT NOT NULL)")
        .unwrap();
    old.execute("INSERT INTO cards VALUES ('x', ?1)", [image(&d)])
        .unwrap();
    drop(old);
    fs::write(media(&tmp).join(c.file_name()), png_bytes(3)).unwrap();
    fs::write(media(&tmp).join(d.file_name()), png_bytes(4)).unwrap();
    store.add_media_bytes(&png_bytes(1)).unwrap();
    // `b`, added since the last backup: no backup uses it.
    let b = store.add_media_bytes(&png_bytes(2)).unwrap();
    let cleaned = clean_up_at(&mut store, T0).unwrap();
    assert_eq!(cleaned.files, 4);

    let tidied = store.tidy_media_trash_at(purge_time()).unwrap();
    assert_eq!((tidied.purged, tidied.kept, tidied.blocked), (1, 3, false));
    let mut kept = vec![a.file_name(), c.file_name(), d.file_name()];
    kept.sort();
    assert_eq!(trashed(&tmp), kept);
    assert!(!trashed(&tmp).contains(&b.file_name()));
}

/// A backup, or the copy a restore left waiting, that cannot be read keeps
/// everything; a damaged copy a restore replaced does not block.
#[test]
fn an_unreadable_backup_blocks_the_purge() {
    let blockers: [(&str, &[u8]); 3] = [
        (
            "backups/rappel-20261001-000000.sqlite3",
            b"not a database at all",
        ),
        (
            "backups/rappel-20261002-000000.sqlite3",
            b"SQLite format 3\0 and then garbage instead of pages, long enough to be read....",
        ),
        (
            "rappel.sqlite3.restore.partial",
            b"SQLite format 3\0garbage",
        ),
    ];
    for (name, bytes) in blockers {
        let (tmp, mut store, _) = trashed_at(T0, &[1]);
        let path = data(&tmp).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        let tidied = store.tidy_media_trash_at(purge_time()).unwrap();
        assert!(tidied.blocked, "{name}");
        assert_eq!(tidied.purged, 0);
        assert_eq!(trashed(&tmp), [png_ref(1).file_name()], "{name}");
    }
    let (tmp, mut store, _) = trashed_at(T0, &[1]);
    fs::write(
        data(&tmp).join("rappel.sqlite3.replaced-20261001-000000"),
        b"SQLite format 3\0damaged",
    )
    .unwrap();
    let tidied = store.tidy_media_trash_at(purge_time()).unwrap();
    assert_eq!((tidied.purged, tidied.blocked), (1, false));
}

/// The database's own use keeps a file: a trash copy whose name is in the
/// media folder again (so it is not brought back) stays while a row or
/// a card names it.
#[test]
fn the_purge_keeps_what_the_database_uses() {
    let (tmp, mut store, refs) = trashed_at(T0, &[1]);
    store.add_media_bytes(&png_bytes(1)).unwrap();
    let tidied = store.tidy_media_trash_at(purge_time()).unwrap();
    assert_eq!((tidied.recovered, tidied.purged, tidied.kept), (0, 0, 1));
    assert_eq!(trashed(&tmp), [refs[0].file_name()]);
}

/// At startup the files the database uses come back first, so the purge
/// never sees them.
#[test]
fn tidying_brings_back_what_cards_use_before_purging() {
    let (tmp, mut store, refs) = trashed_at(T0, &[1, 2]);
    let deck = store.create_deck("Deck").unwrap().id;
    // A card naming refs[0], written without the store (another version,
    // or a database swapped by hand), so no write brought it back.
    let id = store.add_card(&deck, "x", "").unwrap().id;
    store
        .conn
        .execute(
            "UPDATE cards SET back_md = ?1 WHERE id = ?2",
            [image(&refs[0]), id],
        )
        .unwrap();
    let tidied = store.tidy_media_trash_at(purge_time()).unwrap();
    assert_eq!((tidied.recovered, tidied.purged), (1, 1));
    assert_eq!(names(&media(&tmp)), [refs[0].file_name()]);
    assert_eq!(trashed(&tmp), none());
}

/// Links, folders and foreign names in the trash are never followed or
/// removed, by a purge or by bringing files back.
#[test]
fn hostile_things_in_the_trash_are_left_alone() {
    let (tmp, mut store, refs) = trashed_at(T0, &[1]);
    let folder = day_folder(&tmp, T0);
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let target = outside.join(png_ref(2).file_name());
    fs::write(&target, png_bytes(2)).unwrap();
    // In a purgeable folder: a link with a media name, a folder with one,
    // foreign names.
    symlink(&target, folder.join(png_ref(2).file_name())).unwrap();
    fs::create_dir(folder.join(png_ref(3).file_name())).unwrap();
    fs::write(folder.join("notes.txt"), b"keep").unwrap();
    fs::write(folder.join(png_ref(4).file_name().to_uppercase()), b"keep").unwrap();
    // A link to a folder, named like an old date, and a file named so.
    symlink(&outside, trash(&tmp).join("2026-10-02")).unwrap();
    fs::write(trash(&tmp).join("2026-10-03"), b"keep").unwrap();
    // A card naming the linked file: never brought back through the link.
    let deck = store.create_deck("Deck").unwrap().id;
    store.add_card(&deck, &image(&png_ref(2)), "").unwrap();

    let tidied = store.tidy_media_trash_at(purge_time()).unwrap();
    assert_eq!((tidied.purged, tidied.recovered), (1, 0));
    assert!(!folder.join(refs[0].file_name()).exists());
    assert_eq!(fs::read(&target).unwrap(), png_bytes(2));
    assert!(
        fs::symlink_metadata(folder.join(png_ref(2).file_name()))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(folder.join(png_ref(3).file_name()).is_dir());
    assert!(folder.join("notes.txt").exists());
    assert!(trash(&tmp).join("2026-10-02").exists());
    assert!(trash(&tmp).join("2026-10-03").exists());
    assert!(!media(&tmp).join(png_ref(2).file_name()).exists());
}

/// Counting the unused files of 20,000 cards, a tenth naming one of 500
/// files, beside 100 unused ones, stays within a bound (measured about
/// 40 ms in a debug build).
#[test]
fn counting_unused_media_at_twenty_thousand_cards_is_fast() {
    let (tmp, mut store) = store();
    let deck = store.create_deck("Big").unwrap().id;
    let files: Vec<MediaRef> = (0..600)
        .map(|n| store.add_media_bytes(&png_bytes(n)).unwrap())
        .collect();
    // Twenty thousand commits: not synced, or they would take minutes.
    store
        .conn
        .pragma_update(None, "synchronous", "OFF")
        .unwrap();
    for i in 0..20_000_usize {
        let front = if i % 10 == 0 {
            format!("Card {i} ![a picture]({})", files[i / 10 % 500].reference())
        } else {
            format!("Card {i} with **some** text and a [link](https://example.org)")
        };
        store.add_card(&deck, &front, "back").unwrap();
    }
    let (used, unused) = fastest("unused_media at 20,000 cards", 3, || {
        store.unused_media().unwrap()
    });
    assert_eq!(unused.files, 100);
    assert_cpu_within(used, std::time::Duration::from_millis(500), "unused_media");
    drop(tmp);
}
