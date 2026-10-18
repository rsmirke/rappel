//! Tests for restoring backups: over healthy, damaged and missing
//! databases, with stale journal files, and refusals that change nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::*;
use crate::domain::CardState;
use crate::scheduling::Scheduler;
use crate::storage::backup::{check_rows, create_backup, list_backups, utc_stamp};
use crate::storage::latest;
use crate::storage::tests::{
    CARD, CARD2, DAY_MS, DAY0, DECK, FAILING_V2, FAKE_V1, FAKE_V2, REVIEWS, corrupt_btree, count,
    insert_events, insert_sample, mode,
};
use crate::test_support::with_suffix;

const DECK2: &str = "01920000-0000-7000-8000-000000000009";
/// `utc_stamp(DAY0 + DAY_MS)`, the time every restore below runs at.
const RESTORE_STAMP: &str = "20261016-123456";

struct Env {
    _tmp: tempfile::TempDir,
    data: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        Self { _tmp: tmp, data }
    }

    fn db(&self) -> PathBuf {
        self.data.join(DB_FILE_NAME)
    }

    fn backups(&self) -> PathBuf {
        self.data.join(BACKUP_DIR_NAME)
    }

    fn open(&self) -> Result<Store> {
        Store::open_with(&self.db(), &self.backups(), DAY0, MIGRATIONS)
    }

    fn restore(&self, backup: &Path) -> Result<Restored> {
        restore_with(&self.data, backup, DAY0 + DAY_MS, MIGRATIONS)
    }

    fn replaced(&self) -> PathBuf {
        self.data
            .join(format!("{DB_FILE_NAME}.replaced-{RESTORE_STAMP}"))
    }

    /// A database with one deck ("Maths"), backed up; then a second deck
    /// ("Physics") is added that only the current database has. Returns the
    /// still-open store and the backup path.
    fn diverged(&self) -> (Store, PathBuf) {
        let store = self.open().unwrap();
        insert_sample(&store.conn);
        let backup = create_backup(&store.conn, &self.backups(), DAY0).unwrap();
        add_deck(&store.conn);
        (store, backup)
    }
}

/// Adds deck "Physics" with raw SQL, at schema v1 or later (`name_key`
/// exists from v4).
fn add_deck(conn: &Connection) {
    let has_key: bool = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('decks') WHERE name = 'name_key'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let sql = if has_key {
        format!(
            "INSERT INTO decks (id, name, name_key, created_at, updated_at)
             VALUES ('{DECK2}', 'Physics', 'physics', 2, 2);"
        )
    } else {
        format!(
            "INSERT INTO decks (id, name, created_at, updated_at) VALUES ('{DECK2}', 'Physics', 2, 2);"
        )
    };
    conn.execute_batch(&sql).unwrap();
}

fn deck_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM decks ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// Every file and directory under `dir` with its bytes (directories map to
/// an empty vector), for "nothing changed" checks.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (bool, Vec<u8>)> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.insert(path.clone(), (true, Vec::new()));
            out.extend(snapshot(&path));
        } else {
            out.insert(path.clone(), (false, fs::read(&path).unwrap()));
        }
    }
    out
}

/// No scratch copy, and no `-journal`/`-wal`/`-shm` next to one, is left.
fn no_partial_left(e: &Env) {
    for copy in [
        restore_partial_path(&e.data),
        restore_checking_path(&e.data),
    ] {
        for p in std::iter::once(copy.clone()).chain(sidecar_paths(&copy)) {
            assert!(!p.exists(), "{} left behind", p.display());
        }
    }
}

// --- Successful restores --------------------------------------------------

#[test]
fn restore_over_a_healthy_database() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    let before = fs::read(e.db()).unwrap();

    let restored = e.restore(&backup).unwrap();

    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    assert_eq!(count(&restored.store.conn, "review_events"), 1);
    assert_eq!(restored.replaced.as_deref(), Some(e.replaced().as_path()));
    // The previous database is intact, byte for byte, and private.
    assert_eq!(fs::read(e.replaced()).unwrap(), before);
    assert_eq!(mode(&e.replaced()), 0o600);
    assert_eq!(mode(&e.db()), 0o600);
    let old = Connection::open(e.replaced()).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
    // The backup itself is untouched and still listed.
    assert!(backup.exists());
    no_partial_left(&e);
}

/// #8: restoring the oldest backup must not prune it away when the open that
/// follows makes the day's first backup.
#[test]
fn the_restored_backup_survives_the_prune_after_it() {
    let e = Env::new();
    let (store, _) = e.diverged();
    // One backup a day for 14 days, ending on DAY0; the restore is on the
    // next day, which has none yet, so the oldest is out of the window.
    let mut oldest = PathBuf::new();
    for k in 0..14 {
        oldest = create_backup(&store.conn, &e.backups(), DAY0 - k * DAY_MS).unwrap();
    }
    drop(store);

    let restored = e.restore(&oldest).unwrap();

    assert!(oldest.exists(), "the backup just restored is still there");
    // The day's first backup was made too.
    assert!(
        e.backups()
            .join(format!("rappel-{RESTORE_STAMP}.sqlite3"))
            .exists()
    );
    assert_eq!(deck_names(&restored.store.conn), ["Maths", "Physics"]);
}

#[test]
fn restore_through_the_public_api_closes_the_open_store() {
    let e = Env::new();
    // Still open, with the Physics deck only in its WAL.
    let (store, _) = e.diverged();
    let list = list_backups(&e.data).unwrap();
    assert_eq!(list.len(), 1);

    let restored = restore_backup(&e.data, Some(store), &list[0]).unwrap();

    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    // Closing the store checkpointed its WAL before the move, so the
    // moved-aside database has everything.
    let replaced = restored.replaced.unwrap();
    let old = Connection::open(&replaced).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
}

#[test]
fn restore_over_a_corrupted_database() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    corrupt_btree(&e.db(), "decks");
    let damaged = fs::read(e.db()).unwrap();
    assert!(matches!(e.open(), Err(Error::IntegrityCheckFailed { .. })));

    let restored = e.restore(&backup).unwrap();

    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    assert_eq!(fs::read(e.replaced()).unwrap(), damaged);
    no_partial_left(&e);
}

#[test]
fn restore_over_a_file_that_is_not_a_database() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    fs::write(e.db(), vec![0x42; 8192]).unwrap();

    let restored = e.restore(&backup).unwrap();

    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    assert_eq!(fs::read(e.replaced()).unwrap(), vec![0x42; 8192]);
}

#[test]
fn restore_when_there_is_no_database() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    fs::remove_file(e.db()).unwrap();

    let restored = e.restore(&backup).unwrap();

    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    assert_eq!(restored.replaced, None);
}

/// Copies the database files as they are on disk while `store` still has
/// uncheckpointed rows in the `-wal`, closes it, and puts the copies back:
/// the state a crash leaves behind.
fn crash_with_rows_in_wal(e: &Env, store: Store) -> Vec<u8> {
    let wal = with_suffix(&e.db(), "-wal");
    let main = fs::read(e.db()).unwrap();
    let wal_bytes = fs::read(&wal).unwrap();
    let shm_bytes = fs::read(with_suffix(&e.db(), "-shm")).unwrap();
    assert!(!wal_bytes.is_empty(), "rows are in the WAL");
    drop(store);
    fs::write(e.db(), main).unwrap();
    fs::write(&wal, &wal_bytes).unwrap();
    fs::write(with_suffix(&e.db(), "-shm"), shm_bytes).unwrap();
    wal_bytes
}

#[test]
fn stale_wal_does_not_leak_into_the_restored_database() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    let wal_bytes = crash_with_rows_in_wal(&e, store);

    let restored = e.restore(&backup).unwrap();

    // Only the backup's data: the Physics deck lived in the stale WAL.
    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    assert_eq!(
        check_rows(&restored.store.conn, "integrity_check").unwrap(),
        ["ok"]
    );
    // The WAL moved with its database, unchanged and private…
    let replaced_wal = with_suffix(&e.replaced(), "-wal");
    assert_eq!(fs::read(&replaced_wal).unwrap(), wal_bytes);
    assert_eq!(mode(&replaced_wal), 0o600);
    assert_eq!(mode(&with_suffix(&e.replaced(), "-shm")), 0o600);
    drop(restored);
    // …so the moved-aside database still has the rows that were only in it.
    let old = Connection::open(e.replaced()).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
}

#[test]
fn orphaned_wal_is_refused_by_open_and_moved_aside_by_restore() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    let wal_bytes = crash_with_rows_in_wal(&e, store);
    // A crash after the main file was moved but before its -wal was.
    fs::remove_file(e.db()).unwrap();

    match e.open() {
        Err(Error::OrphanedSidecar(path)) => assert_eq!(path, with_suffix(&e.db(), "-wal")),
        other => panic!("expected OrphanedSidecar, got {other:?}"),
    }
    assert!(!e.db().exists(), "open created nothing");

    let restored = e.restore(&backup).unwrap();

    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    let stem = restored.replaced.unwrap();
    assert!(!stem.exists(), "there was no main file to move");
    assert_eq!(fs::read(with_suffix(&stem, "-wal")).unwrap(), wal_bytes);
}

/// A crash in `move_aside` after it recorded the set and renamed the main
/// file, before it renamed the `-wal`. Returns the set's name and the
/// `-wal`'s bytes.
fn crash_between_the_renames(e: &Env, store: Store, at: i64) -> (PathBuf, Vec<u8>) {
    let wal_bytes = crash_with_rows_in_wal(e, store);
    let stem = e
        .data
        .join(format!("{DB_FILE_NAME}.replaced-{}", utc_stamp(at)));
    write_moving_aside(&e.db(), &stem).unwrap();
    fs::rename(e.db(), &stem).unwrap();
    (stem, wal_bytes)
}

/// #9: a retry moves the stray `-wal` next to its main file, not under a new
/// name, so the rows that were only in it are readable there.
#[test]
fn a_retry_after_a_crash_between_the_renames_keeps_the_set_together() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    let (stem, wal_bytes) = crash_between_the_renames(&e, store, DAY0);
    assert_ne!(
        utc_stamp(DAY0),
        RESTORE_STAMP,
        "the retry is at another time"
    );

    match e.open() {
        Err(Error::OrphanedSidecar(_)) => {}
        other => panic!("expected OrphanedSidecar, got {other:?}"),
    }
    let restored = e.restore(&backup).unwrap();

    assert_eq!(restored.replaced.as_deref(), Some(stem.as_path()));
    assert_eq!(fs::read(suffixed(&stem, "-wal")).unwrap(), wal_bytes);
    assert!(!e.replaced().exists(), "no second set was started");
    assert!(!moving_aside_path(&e.db()).exists(), "the marker is gone");
    let old = Connection::open(&stem).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
    no_partial_left(&e);
}

/// #9 at the source: `move_aside` itself records the set before its first
/// rename, so a failure after the main file leaves a marker (mode 0600) that
/// a plain retry follows.
#[test]
fn move_aside_records_its_set_before_renaming_and_a_retry_finishes_it() {
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;

    let e = Env::new();
    let (store, _) = e.diverged();
    let wal_bytes = crash_with_rows_in_wal(&e, store);
    let calls = Cell::new(0);

    let err = move_aside_with(&e.db(), DAY0, &|from, to| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            return Err(io::Error::other("injected failure"));
        }
        fs::rename(from, to)
    })
    .unwrap_err();

    assert!(matches!(err, Error::Io { .. }), "{err:?}");
    let stem = e
        .data
        .join(format!("{DB_FILE_NAME}.replaced-{}", utc_stamp(DAY0)));
    let marker = moving_aside_path(&e.db());
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim_end(),
        stem.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(
        fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(stem.exists(), "the main file was moved");
    assert!(suffixed(&e.db(), "-wal").exists(), "the -wal is still here");

    // A retry at another time still uses the recorded name.
    let moved = move_aside(&e.db(), DAY0 + 10 * 86_400_000).unwrap();

    assert_eq!(moved.as_deref(), Some(stem.as_path()));
    assert_eq!(fs::read(suffixed(&stem, "-wal")).unwrap(), wal_bytes);
    assert!(!marker.exists());
}

/// Start Fresh moves the same set.
#[test]
fn start_fresh_after_a_crash_between_the_renames_keeps_the_set_together() {
    let e = Env::new();
    let (store, _) = e.diverged();
    let (stem, _) = crash_between_the_renames(&e, store, DAY0);
    fs::remove_dir_all(e.backups()).unwrap();

    let fresh = e.start_fresh().unwrap();

    assert_eq!(fresh.replaced.as_deref(), Some(stem.as_path()));
    let old = Connection::open(&stem).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
}

/// A marker left by a finished move must not capture a later database.
#[test]
fn a_marker_left_after_a_finished_move_is_not_reused() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    let old_stem = e
        .data
        .join(format!("{DB_FILE_NAME}.replaced-20200101-000000"));
    fs::write(&old_stem, b"an older set").unwrap();
    write_moving_aside(&e.db(), &old_stem).unwrap();

    let restored = e.restore(&backup).unwrap();

    assert_eq!(restored.replaced.as_deref(), Some(e.replaced().as_path()));
    assert_eq!(fs::read(&old_stem).unwrap(), b"an older set");
    assert!(!moving_aside_path(&e.db()).exists());
}

#[test]
fn a_marker_naming_another_file_is_refused_and_nothing_changes() {
    for content in [
        "../elsewhere",
        "rappel.sqlite3",
        "rappel.sqlite3.replaced-1/../x",
        "",
    ] {
        let e = Env::new();
        let (store, backup) = e.diverged();
        drop(store);
        fs::write(moving_aside_path(&e.db()), content).unwrap();
        let before = snapshot(&e.data);

        let err = e.restore(&backup).unwrap_err();

        assert!(matches!(err, Error::Io { .. }), "{content:?}: {err:?}");
        // The validated copy waits, as after any failure past that point;
        // the database and the marker are as they were.
        let mut after = snapshot(&e.data);
        after.remove(&restore_partial_path(&e.data));
        let mut before = before;
        before.remove(&restore_partial_path(&e.data));
        assert_eq!(after, before, "{content:?}");
    }
}

/// Runs the first half of a restore by hand: copy and validate, then move
/// the current database aside. Returns the validated copy.
fn copy_validate_and_move_aside(e: &Env, backup: &Path) -> PathBuf {
    let partial = restore_partial_path(&e.data);
    copy_private(backup, &partial).unwrap();
    validate(&partial, MIGRATIONS).unwrap();
    move_aside(&e.db(), DAY0 + DAY_MS).unwrap();
    partial
}

/// After an interrupted restore, a retry that fails must leave the
/// validated copy in place, so `Store::open` keeps refusing.
fn assert_failed_retry_keeps_the_guard(make_bad: impl FnOnce(&Env, &Path) -> PathBuf) {
    let e = Env::new();
    let (store, good) = e.diverged();
    drop(store);
    let partial = copy_validate_and_move_aside(&e, &good);
    let bad = make_bad(&e, &good);
    let before = snapshot(&e.data);

    assert!(e.restore(&bad).is_err());

    assert_eq!(snapshot(&e.data), before, "nothing changed");
    assert!(partial.exists());
    assert!(matches!(e.open(), Err(Error::InterruptedRestore(_))));
    assert!(!e.db().exists(), "no empty database was started");
    // A good backup still finishes the job.
    let _ = fs::remove_dir(restore_checking_path(&e.data));
    let restored = e.restore(&good).unwrap();
    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
}

#[test]
fn refused_retry_after_an_interrupted_restore_keeps_the_guard() {
    assert_failed_retry_keeps_the_guard(|e, good| {
        let bad = bad_path(e);
        fs::copy(good, &bad).unwrap();
        corrupt_btree(&bad, "decks");
        bad
    });
}

#[test]
fn failed_copy_after_an_interrupted_restore_keeps_the_guard() {
    assert_failed_retry_keeps_the_guard(|e, good| {
        // A directory in the way of the scratch copy makes copying fail
        // (also when the tests run as root, unlike a permission trick).
        fs::create_dir(restore_checking_path(&e.data)).unwrap();
        good.to_path_buf()
    });
}

#[test]
fn interrupted_restore_is_refused_by_open_and_finished_by_a_new_restore() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    // A crash between moving the old database aside and installing the copy.
    let partial = copy_validate_and_move_aside(&e, &backup);

    match e.open() {
        Err(Error::InterruptedRestore(path)) => assert_eq!(path, partial),
        other => panic!("expected InterruptedRestore, got {other:?}"),
    }
    assert!(!e.db().exists(), "open did not start an empty database");

    let restored = e.restore(&backup).unwrap();

    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    assert!(!partial.exists());
    let old = Connection::open(e.replaced()).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
}

#[test]
fn failed_install_keeps_the_copy_and_never_starts_an_empty_database() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    let partial = copy_validate_and_move_aside(&e, &backup);
    // Something appears at the target between moving aside and installing.
    let intruder = with_suffix(&e.db(), "-wal");
    fs::write(&intruder, b"intruder").unwrap();

    let err = install(&partial, &e.db()).unwrap_err();

    assert!(
        matches!(&err, Error::Io { path, .. } if *path == intruder),
        "{err}"
    );
    assert!(partial.exists(), "the validated copy is kept");
    assert!(!e.db().exists());
    // The waiting copy is what open reports, even with the stray -wal next
    // to it, so the user is offered Finish Restore, not Start Fresh (#50).
    assert!(matches!(e.open(), Err(Error::InterruptedRestore(_))));
    // Without the copy, the stray -wal alone is refused.
    let aside = with_suffix(&partial, ".aside");
    fs::rename(&partial, &aside).unwrap();
    assert!(matches!(e.open(), Err(Error::OrphanedSidecar(_))));
    fs::rename(&aside, &partial).unwrap();
    assert!(!e.db().exists(), "open created nothing");

    // A new restore moves the stray file aside and finishes.
    let restored = e.restore(&backup).unwrap();
    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    let stem = restored.replaced.unwrap();
    assert_eq!(stem, with_suffix(&e.replaced(), "-2"));
    assert_eq!(fs::read(with_suffix(&stem, "-wal")).unwrap(), b"intruder");
    let old = Connection::open(e.replaced()).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
}

// --- Finishing an interrupted restore (#22) -------------------------------

/// The state of #22: a restore moved the database aside and was interrupted,
/// and the backup it came from is gone. Returns the waiting copy.
fn interrupted_without_its_backup(e: &Env) -> PathBuf {
    let (store, backup) = e.diverged();
    drop(store);
    let partial = copy_validate_and_move_aside(e, &backup);
    fs::remove_dir_all(e.backups()).unwrap();
    partial
}

#[test]
fn finishing_installs_exactly_the_waiting_copy() {
    let e = Env::new();
    let partial = interrupted_without_its_backup(&e);
    assert!(matches!(e.open(), Err(Error::InterruptedRestore(_))));
    // Something strayed into the place meanwhile. The waiting copy is still
    // what open reports, so the user is offered Finish Restore (#50).
    fs::write(with_suffix(&e.db(), "-wal"), b"intruder").unwrap();
    assert!(matches!(e.open(), Err(Error::InterruptedRestore(_))));

    let finished = finish_restore_with(&e.data, DAY0 + DAY_MS, MIGRATIONS).unwrap();

    assert_eq!(deck_names(&finished.store.conn), ["Maths"]);
    assert_eq!(count(&finished.store.conn, "review_events"), 1);
    assert!(!partial.exists());
    assert_eq!(mode(&e.db()), 0o600);
    // The stray file is moved aside, next to the set the interruption made.
    let stem = finished.replaced.clone().unwrap();
    assert_eq!(stem, with_suffix(&e.replaced(), "-2"));
    assert_eq!(fs::read(with_suffix(&stem, "-wal")).unwrap(), b"intruder");
    let old = Connection::open(e.replaced()).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
    // What was installed was the waiting copy: same rows as the backup it
    // came from, which was a valid database of the current version.
    drop(finished);
    let installed = Connection::open(e.db()).unwrap();
    assert_eq!(deck_names(&installed), ["Maths"]);
    no_partial_left(&e);
}

/// #50: the backup an interrupted restore came from may still be there and
/// be the oldest one. Finishing does not know which one it was, so it
/// prunes nothing; the next open prunes as usual.
#[test]
fn finishing_keeps_every_backup_the_copy_may_have_come_from() {
    let e = Env::new();
    let (store, _) = e.diverged();
    // One backup a day for 20 days ending on DAY0: six of them are outside
    // the 14 days of the finish on the next day.
    let mut oldest = PathBuf::new();
    for k in 0..20 {
        oldest = create_backup(&store.conn, &e.backups(), DAY0 - k * DAY_MS).unwrap();
    }
    drop(store);
    copy_validate_and_move_aside(&e, &oldest);
    let before: Vec<PathBuf> = list_backups(&e.data)
        .unwrap()
        .into_iter()
        .map(|b| b.path().to_path_buf())
        .collect();

    let finished = finish_restore_with(&e.data, DAY0 + DAY_MS, MIGRATIONS).unwrap();

    assert!(oldest.exists(), "the backup the copy came from is kept");
    for path in &before {
        assert!(path.exists(), "{} was pruned", path.display());
    }
    // The day's backup was made too.
    assert_eq!(list_backups(&e.data).unwrap().len(), before.len() + 1);
    drop(finished);
    // The next open prunes as usual.
    Store::open_with(&e.db(), &e.backups(), DAY0 + 2 * DAY_MS, MIGRATIONS).unwrap();
    assert!(!oldest.exists());
}

#[test]
fn finishing_through_the_public_api_works_without_any_backup() {
    let e = Env::new();
    interrupted_without_its_backup(&e);

    let finished = finish_restore(&e.data, None).unwrap();

    assert_eq!(deck_names(&finished.store.conn), ["Maths"]);
    assert_eq!(finished.replaced, None, "the database was already aside");
}

#[test]
fn finishing_without_a_waiting_copy_is_refused_and_changes_nothing() {
    let e = Env::new();
    let (store, _) = e.diverged();
    drop(store);
    let before = snapshot(&e.data);

    assert!(matches!(
        finish_restore_with(&e.data, DAY0 + DAY_MS, MIGRATIONS),
        Err(Error::NoRestoreWaiting)
    ));

    assert_eq!(snapshot(&e.data), before);
}

#[test]
fn finishing_refuses_a_waiting_symlink_and_changes_nothing() {
    let e = Env::new();
    let partial = interrupted_without_its_backup(&e);
    let real = e.data.join("elsewhere");
    fs::rename(&partial, &real).unwrap();
    std::os::unix::fs::symlink(&real, &partial).unwrap();
    let before = snapshot(&e.data);

    assert!(matches!(
        finish_restore_with(&e.data, DAY0 + DAY_MS, MIGRATIONS),
        Err(Error::NoRestoreWaiting)
    ));

    assert_eq!(snapshot(&e.data), before);
}

#[test]
fn finishing_refuses_a_damaged_copy_and_keeps_it() {
    let e = Env::new();
    let partial = interrupted_without_its_backup(&e);
    fs::write(&partial, vec![0x42; 8192]).unwrap();
    fs::write(with_suffix(&e.db(), "-wal"), b"stray").unwrap();
    let before = snapshot(&e.data);

    let err = finish_restore_with(&e.data, DAY0 + DAY_MS, MIGRATIONS).unwrap_err();

    assert!(matches!(err, Error::RestoreRefused { .. }), "{err:?}");
    assert_eq!(snapshot(&e.data), before, "nothing changed, copy kept");
    // The copy still waits, and is reported before the stray -wal (#50).
    assert!(matches!(e.open(), Err(Error::InterruptedRestore(_))));
}

#[test]
fn finishing_migrates_a_copy_from_an_older_build() {
    let e = Env::new();
    let store = Store::open_with(&e.db(), &e.backups(), DAY0, &[FAKE_V1]).unwrap();
    store
        .conn
        .execute_batch("INSERT INTO t VALUES (1);")
        .unwrap();
    let backup = create_backup(&store.conn, &e.backups(), DAY0).unwrap();
    drop(store);
    let partial = restore_partial_path(&e.data);
    copy_private(&backup, &partial).unwrap();
    move_aside(&e.db(), DAY0 + DAY_MS).unwrap();
    fs::remove_dir_all(e.backups()).unwrap();

    let finished = finish_restore_with(&e.data, DAY0 + DAY_MS, &[FAKE_V1, FAKE_V2]).unwrap();

    assert_eq!(finished.store.schema_version().unwrap(), 2);
    let y: i64 = finished
        .store
        .conn
        .query_row("SELECT y FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(y, 7);
}

#[test]
fn replaced_name_gets_a_suffix_when_taken() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    fs::write(with_suffix(&e.replaced(), "-wal"), b"someone else's").unwrap();

    let restored = e.restore(&backup).unwrap();

    let expected = with_suffix(&e.replaced(), "-2");
    assert_eq!(restored.replaced.as_deref(), Some(expected.as_path()));
    assert_eq!(
        fs::read(with_suffix(&e.replaced(), "-wal")).unwrap(),
        b"someone else's"
    );
}

#[test]
fn older_backup_is_migrated_before_install() {
    let e = Env::new();
    let store = Store::open_with(&e.db(), &e.backups(), DAY0, &[FAKE_V1]).unwrap();
    store
        .conn
        .execute_batch("INSERT INTO t VALUES (1);")
        .unwrap();
    let backup = create_backup(&store.conn, &e.backups(), DAY0).unwrap();
    drop(store);

    let restored = restore_with(&e.data, &backup, DAY0 + DAY_MS, &[FAKE_V1, FAKE_V2]).unwrap();

    assert_eq!(restored.store.schema_version().unwrap(), 2);
    let y: i64 = restored
        .store
        .conn
        .query_row("SELECT y FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(y, 7);
}

/// Writes a database at schema v1 (the real migration 0001) with two
/// cards, one of them reviewed, and a cache row in the v1 shape; backs it
/// up; then adds a deck only the current database has. The current
/// database stays at v1. Returns the backup path.
fn diverged_v1(e: &Env) -> PathBuf {
    let v1 = Store::open_with(&e.db(), &e.backups(), DAY0, &MIGRATIONS[..1]).unwrap();
    v1.conn
        .execute_batch(&format!(
            "INSERT INTO decks (id, name, created_at, updated_at) VALUES ('{DECK}', 'Maths', 1, 1);
             INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at)
                 VALUES ('{CARD}', '{DECK}', 'front', 'back', 1, 1),
                        ('{CARD2}', '{DECK}', 'new card', 'back', 1, 1);
             INSERT INTO card_schedule VALUES ('{CARD}', 3, 1.5, 5.0, 2, 1, 0);"
        ))
        .unwrap();
    insert_events(&v1.conn, CARD, 0, &REVIEWS);
    let backup = create_backup(&v1.conn, &e.backups(), DAY0).unwrap();
    add_deck(&v1.conn);
    backup
}

/// A backup from schema v1 is migrated to the current version before it is
/// installed: the review log is kept and the schedule cache is rebuilt
/// from it.
#[test]
fn v1_backup_is_restored_and_migrated_to_the_current_version() {
    let e = Env::new();
    let backup = diverged_v1(&e);
    let backup_bytes = fs::read(&backup).unwrap();

    let restored = e.restore(&backup).unwrap();

    let store = &restored.store;
    assert_eq!(
        store.schema_version().unwrap(),
        crate::storage::migrations::latest(MIGRATIONS)
    );
    assert_eq!(deck_names(&store.conn), ["Maths"]);
    assert_eq!(count(&store.conn, "cards"), 2);
    assert_eq!(count(&store.conn, "review_events"), 4);
    let events = store.review_events(CARD).unwrap();
    let expected = Scheduler::default().rebuild_state(CARD, &events).unwrap();
    assert!(matches!(expected, CardState::Scheduled(_)));
    assert_eq!(store.card_state(CARD).unwrap(), expected);
    assert_eq!(store.card_state(CARD2).unwrap(), CardState::New);
    // The old v1 database is moved aside, the backup is left at v1.
    let old = Connection::open(e.replaced()).unwrap();
    assert_eq!(deck_names(&old), ["Maths", "Physics"]);
    assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
    no_partial_left(&e);
}

/// A backup from schema v2 (cache with `phase`, no soft delete, no name
/// keys) is migrated through 0003–0005 before it is installed: its data is
/// kept, every deck gets its name key, and the cache equals the rebuild.
#[test]
fn v2_backup_is_restored_and_migrated_to_the_current_version() {
    let e = Env::new();
    // "Économie" with a combining accent: the key is NFC and lower case.
    let name = "E\u{301}conomie";
    let backup = {
        let v2 = Store::open_with(&e.db(), &e.backups(), DAY0, &MIGRATIONS[..2]).unwrap();
        v2.conn
            .execute(
                "INSERT INTO decks (id, name, created_at, updated_at) VALUES (?1, ?2, 1, 1)",
                rusqlite::params![DECK, name],
            )
            .unwrap();
        v2.conn
            .execute_batch(&format!(
                "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at)
                     VALUES ('{CARD}', '{DECK}', 'front', 'back', 1, 1),
                            ('{CARD2}', '{DECK}', 'new card', 'back', 1, 1);
                 INSERT INTO card_schedule VALUES ('{CARD}', 'learning', 3, 1.5, 5.0, 2, 1, 0);"
            ))
            .unwrap();
        insert_events(&v2.conn, CARD, 0, &REVIEWS);
        assert_eq!(v2.schema_version().unwrap(), 2);
        create_backup(&v2.conn, &e.backups(), DAY0).unwrap()
    };
    let backup_bytes = fs::read(&backup).unwrap();

    let restored = e.restore(&backup).unwrap();

    let store = &restored.store;
    assert_eq!(store.schema_version().unwrap(), latest(MIGRATIONS));
    assert_eq!(deck_names(&store.conn), [name]);
    let key: String = store
        .conn
        .query_row("SELECT name_key FROM decks WHERE id = ?1", [DECK], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(key, crate::domain::deck_name_key(name));
    assert_eq!(key, "\u{e9}conomie");
    assert_eq!(count(&store.conn, "cards"), 2);
    assert_eq!(count(&store.conn, "review_events"), 4);
    let events = store.review_events(CARD).unwrap();
    let expected = Scheduler::default().rebuild_state(CARD, &events).unwrap();
    assert!(matches!(expected, CardState::Scheduled(_)));
    assert_eq!(
        store.card_state(CARD).unwrap(),
        expected,
        "stale cache replaced"
    );
    assert_eq!(store.card_state(CARD2).unwrap(), CardState::New);
    // Live after the soft-delete migration, and the backup is left at v2.
    assert_eq!(store.list_decks(DAY0, DAY0).unwrap().len(), 1);
    assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
    no_partial_left(&e);
}

/// A backup at the current schema version is installed as it is.
#[test]
fn current_version_backup_is_restored() {
    let e = Env::new();
    let (mut store, backup) = e.diverged();
    insert_events(&store.conn, CARD, 0, &REVIEWS);
    store.rebuild_schedule_cache(&Scheduler::default()).unwrap();
    let backup_current = create_backup(&store.conn, &e.backups(), DAY0 + 1).unwrap();
    let expected = store.card_state(CARD).unwrap();
    assert!(matches!(expected, CardState::Scheduled(_)));
    drop(store);
    fs::remove_file(backup).unwrap();
    let backups_before = list_backups(&e.data).unwrap().len();

    let restored = e.restore(&backup_current).unwrap();

    let store = &restored.store;
    assert_eq!(
        store.schema_version().unwrap(),
        crate::storage::migrations::latest(MIGRATIONS)
    );
    assert_eq!(deck_names(&store.conn), ["Maths", "Physics"]);
    assert_eq!(count(&store.conn, "review_events"), 5);
    assert_eq!(store.card_state(CARD).unwrap(), expected);
    // Nothing to migrate, so no pre-migration backup; the daily backup for
    // the restore day is the only new one.
    assert_eq!(list_backups(&e.data).unwrap().len(), backups_before + 1);
    no_partial_left(&e);
}

/// A backup whose migration fails is refused before anything is moved,
/// so the current database stays in place and opens.
#[test]
fn backup_whose_migration_fails_is_refused_untouched() {
    let e = Env::new();
    let backup = diverged_v1(&e);
    let before = snapshot(&e.data);

    let err = restore_with(
        &e.data,
        &backup,
        DAY0 + DAY_MS,
        &[MIGRATIONS[0], FAILING_V2],
    )
    .unwrap_err();

    assert!(
        matches!(&err, Error::RestoreRefused { source, .. }
            if matches!(**source, Error::Migration { version: 2, .. })),
        "{err:?}"
    );
    assert_eq!(snapshot(&e.data), before, "nothing changed");
    no_partial_left(&e);
    let store = Store::open_with(&e.db(), &e.backups(), DAY0, &MIGRATIONS[..1]).unwrap();
    assert_eq!(deck_names(&store.conn), ["Maths", "Physics"]);
}

// --- Refusals -------------------------------------------------------------

/// Sets up a healthy database plus a bad backup made by `make_bad`, and
/// checks that restoring it is refused for `expected` reasons with every
/// file in the data directory unchanged.
fn assert_refused(
    make_bad: impl FnOnce(&Env, &Path) -> PathBuf,
    expected: impl FnOnce(&Error) -> bool,
) {
    let e = Env::new();
    let (store, good) = e.diverged();
    drop(store);
    let bad = make_bad(&e, &good);
    let before = snapshot(&e.data);

    let err = e.restore(&bad).unwrap_err();

    match &err {
        Error::RestoreRefused { path, source } => {
            assert_eq!(path, &bad);
            assert!(expected(source), "wrong reason: {source}");
        }
        other => panic!("expected RestoreRefused, got {other:?}"),
    }
    assert_eq!(snapshot(&e.data), before, "nothing changed");
    no_partial_left(&e);
    // The current database still opens with its own data.
    assert_eq!(deck_names(&e.open().unwrap().conn), ["Maths", "Physics"]);
}

/// A backup-named path in the backups directory for a bad file.
fn bad_path(e: &Env) -> PathBuf {
    e.backups().join("rappel-20261014-000000.sqlite3")
}

/// A copy of the good backup, changed by `change` through SQLite.
fn altered_copy(e: &Env, good: &Path, change: &str) -> PathBuf {
    let bad = bad_path(e);
    fs::copy(good, &bad).unwrap();
    Connection::open(&bad)
        .unwrap()
        .execute_batch(change)
        .unwrap();
    bad
}

#[test]
fn corrupted_backup_is_refused() {
    assert_refused(
        |e, good| {
            let bad = bad_path(e);
            fs::copy(good, &bad).unwrap();
            corrupt_btree(&bad, "decks");
            bad
        },
        |err| matches!(err, Error::IntegrityCheckFailed { .. }),
    );
}

#[test]
fn garbage_backup_is_refused() {
    assert_refused(
        |e, _| {
            let bad = bad_path(e);
            fs::write(&bad, vec![0x42; 8192]).unwrap();
            bad
        },
        |err| matches!(err, Error::IntegrityCheckFailed { .. }),
    );
}

#[test]
fn too_new_backup_is_refused() {
    let too_new = latest(MIGRATIONS) + 1;
    assert_refused(
        |e, good| altered_copy(e, good, &format!("PRAGMA user_version = {too_new};")),
        |err| matches!(err, Error::UnsupportedVersion { found, .. } if *found == i64::from(too_new)),
    );
}

#[test]
fn foreign_database_is_refused() {
    assert_refused(
        |e, _| {
            let bad = bad_path(e);
            Connection::open(&bad)
                .unwrap()
                .execute_batch("CREATE TABLE notes(x TEXT);")
                .unwrap();
            bad
        },
        |err| matches!(err, Error::NotARappelDatabase),
    );
}

#[test]
fn empty_database_is_refused() {
    assert_refused(
        |e, _| {
            let bad = bad_path(e);
            fs::write(&bad, b"").unwrap();
            bad
        },
        |err| matches!(err, Error::NotARappelDatabase),
    );
}

#[test]
fn backup_missing_a_table_is_refused() {
    assert_refused(
        |e, good| altered_copy(e, good, "DROP TABLE card_schedule;"),
        |err| matches!(err, Error::NotARappelDatabase),
    );
}

#[test]
fn backup_with_an_extra_object_is_refused() {
    assert_refused(
        |e, good| altered_copy(e, good, "CREATE VIEW sneaky AS SELECT 1;"),
        |err| matches!(err, Error::NotARappelDatabase),
    );
}

/// A v1 database relabelled as v2 has v2's table and index names but
/// v1's `card_schedule` columns. Comparing names alone would accept it.
#[test]
fn v1_schema_labelled_v2_is_refused() {
    assert_refused(
        |e, _good| {
            let v1 = e.backups().join("v1.sqlite3");
            {
                let conn = Connection::open(&v1).unwrap();
                migrate(&mut { conn }, &MIGRATIONS[..1], 0).unwrap();
            }
            let bad = bad_path(e);
            fs::rename(&v1, &bad).unwrap();
            Connection::open(&bad)
                .unwrap()
                .execute_batch("PRAGMA user_version = 2;")
                .unwrap();
            bad
        },
        |err| matches!(err, Error::NotARappelDatabase),
    );
}

#[test]
fn wal_mode_backup_is_refused() {
    assert_refused(
        |e, good| altered_copy(e, good, "PRAGMA journal_mode = WAL;"),
        |err| matches!(err, Error::WalModeCopy { .. }),
    );
}

#[test]
fn file_outside_the_backups_directory_is_refused() {
    assert_refused(
        |e, good| {
            let elsewhere = e.data.join("rappel-20261014-000000.sqlite3");
            fs::copy(good, &elsewhere).unwrap();
            elsewhere
        },
        |err| matches!(err, Error::Io { .. }),
    );
}

#[test]
fn badly_named_file_is_refused() {
    assert_refused(
        |e, good| {
            let bad = e.backups().join("my-copy.sqlite3");
            fs::copy(good, &bad).unwrap();
            bad
        },
        |err| matches!(err, Error::Io { .. }),
    );
}

#[test]
fn symlinked_backup_is_refused() {
    assert_refused(
        |e, good| {
            let link = bad_path(e);
            std::os::unix::fs::symlink(good, &link).unwrap();
            link
        },
        |err| matches!(err, Error::Io { .. }),
    );
}

#[test]
fn leftover_partial_from_an_interrupted_restore_is_replaced() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    let partial = restore_checking_path(&e.data);
    fs::write(&partial, b"junk").unwrap();
    let stale_wal = with_suffix(&partial, "-wal");
    fs::write(&stale_wal, b"junk").unwrap();

    let restored = e.restore(&backup).unwrap();
    assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
    assert!(!partial.exists());
    assert!(!stale_wal.exists());
}

// --- Starting fresh (ADR 0016) --------------------------------------------

/// Deletes every backup, for the tests below: Start Fresh is only for a
/// data folder with nothing to restore.
fn without_backups(e: &Env) {
    fs::remove_dir_all(e.backups()).unwrap();
}

impl Env {
    fn start_fresh(&self) -> Result<Restored> {
        start_fresh_with(&self.data, DAY0 + DAY_MS, MIGRATIONS)
    }
}

/// The new database is empty, at the latest version, and opens again.
fn assert_fresh(e: &Env, fresh: Restored) {
    assert_eq!(fresh.store.schema_version().unwrap(), latest(MIGRATIONS));
    assert_eq!(count(&fresh.store.conn, "decks"), 0);
    drop(fresh);
    let reopened = e.open().unwrap();
    assert_eq!(count(&reopened.conn, "decks"), 0);
}

#[test]
fn start_fresh_moves_a_damaged_database_aside_unchanged() {
    let e = Env::new();
    let (store, _) = e.diverged();
    drop(store);
    without_backups(&e);
    corrupt_btree(&e.db(), "decks");
    let damaged = fs::read(e.db()).unwrap();

    let fresh = e.start_fresh().unwrap();

    assert_eq!(fresh.replaced.as_deref(), Some(e.replaced().as_path()));
    assert_eq!(fs::read(e.replaced()).unwrap(), damaged);
    assert_eq!(mode(&e.replaced()), 0o600);
    assert_fresh(&e, fresh);
}

#[test]
fn start_fresh_moves_another_applications_database_aside() {
    let e = Env::new();
    fs::create_dir_all(&e.data).unwrap();
    {
        let conn = Connection::open(e.db()).unwrap();
        conn.execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('x');")
            .unwrap();
    }
    assert!(matches!(e.open(), Err(Error::NotARappelDatabase)));
    let foreign = fs::read(e.db()).unwrap();

    let fresh = e.start_fresh().unwrap();

    assert_eq!(fs::read(e.replaced()).unwrap(), foreign);
    assert_fresh(&e, fresh);
}

#[test]
fn start_fresh_moves_an_orphaned_wal_aside() {
    let e = Env::new();
    let (store, _) = e.diverged();
    let wal_bytes = crash_with_rows_in_wal(&e, store);
    without_backups(&e);
    fs::remove_file(e.db()).unwrap();
    assert!(matches!(e.open(), Err(Error::OrphanedSidecar(_))));

    let fresh = e.start_fresh().unwrap();

    let stem = fresh.replaced.clone().unwrap();
    assert!(!stem.exists(), "there was no main file to move");
    assert_eq!(fs::read(with_suffix(&stem, "-wal")).unwrap(), wal_bytes);
    assert_fresh(&e, fresh);
}

#[test]
fn start_fresh_is_refused_while_a_validated_copy_waits() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    copy_validate_and_move_aside(&e, &backup);
    let before = snapshot(&e.data);

    match e.start_fresh() {
        Err(Error::InterruptedRestore(path)) => assert_eq!(path, restore_partial_path(&e.data)),
        other => panic!("expected InterruptedRestore, got {other:?}"),
    }
    assert_eq!(snapshot(&e.data), before, "nothing changed");
}

#[test]
fn starting_fresh_twice_in_one_second_keeps_both_old_databases() {
    let e = Env::new();
    fs::create_dir_all(&e.data).unwrap();
    fs::write(e.db(), vec![0x42; 4096]).unwrap();
    let first = e.start_fresh().unwrap();
    drop(first);
    let second_old = fs::read(e.db()).unwrap();

    let second = e.start_fresh().unwrap();

    assert_eq!(fs::read(e.replaced()).unwrap(), vec![0x42; 4096]);
    let expected = with_suffix(&e.replaced(), "-2");
    assert_eq!(second.replaced.as_deref(), Some(expected.as_path()));
    assert_eq!(fs::read(&expected).unwrap(), second_old);
    assert_fresh(&e, second);
}

#[test]
fn start_fresh_without_any_database_just_creates_one() {
    let e = Env::new();
    let fresh = e.start_fresh().unwrap();
    assert_eq!(fresh.replaced, None);
    assert_fresh(&e, fresh);
}

#[test]
fn start_fresh_is_refused_while_backups_exist() {
    let e = Env::new();
    let (store, _) = e.diverged();
    drop(store);
    corrupt_btree(&e.db(), "decks");
    let before = snapshot(&e.data);

    assert!(matches!(e.start_fresh(), Err(Error::BackupsAvailable(1))));
    assert_eq!(snapshot(&e.data), before, "nothing changed");
}

/// The dead end of ADR 0050, row 35: the waiting copy is damaged, so Finish
/// Restore refuses it, and there is no backup. Start Fresh renames the copy
/// aside, byte for byte, and starts over; nothing is deleted.
#[test]
fn start_fresh_sets_a_damaged_waiting_copy_aside_and_keeps_it() {
    let e = Env::new();
    let partial = interrupted_without_its_backup(&e);
    fs::write(&partial, vec![0x42; 8192]).unwrap();
    fs::write(with_suffix(&e.db(), "-wal"), b"stray").unwrap();
    assert!(matches!(
        finish_restore_with(&e.data, DAY0 + DAY_MS, MIGRATIONS),
        Err(Error::RestoreRefused { .. })
    ));

    let fresh = e.start_fresh().unwrap();

    let kept = e
        .data
        .join(format!("{DB_FILE_NAME}.restore.refused-{RESTORE_STAMP}"));
    assert_eq!(fresh.set_aside.as_deref(), Some(kept.as_path()));
    assert_eq!(fs::read(&kept).unwrap(), vec![0x42; 8192]);
    assert!(!partial.exists());
    assert!(!restore_checking_path(&e.data).exists());
    // The stray -wal is moved aside as for any start fresh.
    let stem = fresh.replaced.clone().expect("the -wal was moved aside");
    assert_eq!(fs::read(with_suffix(&stem, "-wal")).unwrap(), b"stray");
    assert_fresh(&e, fresh);
}

#[test]
fn a_damaged_waiting_copy_is_kept_in_place_while_backups_exist() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    let partial = copy_validate_and_move_aside(&e, &backup);
    fs::write(&partial, vec![0x42; 8192]).unwrap();
    let before = snapshot(&e.data);

    assert!(matches!(e.start_fresh(), Err(Error::BackupsAvailable(1))));
    assert_eq!(snapshot(&e.data), before, "nothing changed");
}

#[test]
fn a_second_refused_copy_never_overwrites_the_first() {
    let e = Env::new();
    fs::create_dir_all(&e.data).unwrap();
    let partial = restore_partial_path(&e.data);
    fs::write(&partial, b"first").unwrap();
    let first = e.start_fresh().unwrap();
    drop(first);
    fs::write(&partial, b"second").unwrap();

    let second = e.start_fresh().unwrap();

    let kept = e
        .data
        .join(format!("{DB_FILE_NAME}.restore.refused-{RESTORE_STAMP}"));
    assert_eq!(fs::read(&kept).unwrap(), b"first");
    let again = with_suffix(&kept, "-2");
    assert_eq!(second.set_aside.as_deref(), Some(again.as_path()));
    assert_eq!(fs::read(&again).unwrap(), b"second");
}

/// Only refusals about the copy's content make it unusable (ADR 0050,
/// row 35); a full disk, an IO error or a newer version say nothing
/// against it.
#[test]
fn only_a_refusal_of_the_content_makes_a_copy_unusable() {
    let sqlite = |code: i32| {
        Error::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ))
    };
    let io = |kind: io::ErrorKind| Error::Io {
        path: PathBuf::from("/x"),
        source: io::Error::new(kind, "x"),
    };
    for unusable in [
        Error::IntegrityCheckFailed {
            messages: vec!["bad".into()],
        },
        Error::NotARappelDatabase,
        Error::WalModeCopy {
            path: PathBuf::from("/x"),
        },
        Error::UnsupportedVersion {
            found: -1,
            supported: 5,
        },
        sqlite(rusqlite::ffi::SQLITE_NOTADB),
        sqlite(rusqlite::ffi::SQLITE_CORRUPT),
    ] {
        assert!(is_unusable_copy(&unusable), "{unusable:?}");
    }
    for kept in [
        sqlite(rusqlite::ffi::SQLITE_FULL),
        sqlite(rusqlite::ffi::SQLITE_IOERR),
        sqlite(rusqlite::ffi::SQLITE_NOMEM),
        sqlite(rusqlite::ffi::SQLITE_BUSY),
        io(io::ErrorKind::PermissionDenied),
        io(io::ErrorKind::StorageFull),
        // `read_moving_aside`'s refusal of its marker, not of the copy.
        io(io::ErrorKind::InvalidData),
        Error::UnsupportedVersion {
            found: 99,
            supported: 5,
        },
    ] {
        assert!(!is_unusable_copy(&kept), "{kept:?}");
    }
}

/// A check that fails for a reason other than the copy's content (here a
/// full disk while migrating the scratch copy) changes nothing: the copy
/// stays, nothing is renamed, and the error is returned.
#[test]
fn a_transient_check_failure_keeps_the_copy_and_changes_nothing() {
    let e = Env::new();
    let partial = interrupted_without_its_backup(&e);
    let before = snapshot(&e.data);
    for failure in [
        Error::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        )),
        Error::UnsupportedVersion {
            found: 99,
            supported: 5,
        },
    ] {
        let message = failure.to_string();
        let fail = move |_: &Path, _: &[Migration]| -> Result<()> {
            Err(match &failure {
                Error::UnsupportedVersion { found, supported } => Error::UnsupportedVersion {
                    found: *found,
                    supported: *supported,
                },
                _ => Error::Sqlite(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                    None,
                )),
            })
        };
        let err = start_fresh_checking(&e.data, DAY0 + DAY_MS, MIGRATIONS, &fail).unwrap_err();
        assert_eq!(err.to_string(), message);
        assert_eq!(snapshot(&e.data), before, "nothing changed");
        assert!(partial.exists());
    }
}

/// A folder at the waiting copy's name is no copy: Start Fresh neither
/// checks nor moves it, and refuses with nothing changed.
#[test]
fn a_folder_at_the_copys_name_is_left_alone() {
    let e = Env::new();
    fs::create_dir_all(restore_partial_path(&e.data)).unwrap();
    fs::write(e.db(), vec![0x42; 4096]).unwrap();
    let before = snapshot(&e.data);
    assert!(matches!(e.start_fresh(), Err(Error::InterruptedRestore(_))));
    assert_eq!(snapshot(&e.data), before, "nothing changed");
}

/// A copy with a negative version was not written by any Rappel: unusable,
/// so Start Fresh sets it aside. One from a newer Rappel is kept, and its
/// error returned with nothing changed.
#[test]
fn a_negative_version_is_unusable_but_a_newer_one_is_kept() {
    for (version, set_aside) in [(-1_i64, true), (i64::from(latest(MIGRATIONS)) + 1, false)] {
        let e = Env::new();
        let partial = interrupted_without_its_backup(&e);
        {
            let conn = Connection::open(&partial).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
        }
        let before = snapshot(&e.data);
        let result = e.start_fresh();
        if set_aside {
            let fresh = result.unwrap();
            assert!(fresh.set_aside.is_some(), "{version}");
            assert!(!partial.exists());
        } else {
            assert!(
                matches!(result, Err(Error::UnsupportedVersion { found, .. }) if found == version),
                "{version}"
            );
            assert_eq!(snapshot(&e.data), before, "nothing changed");
        }
    }
}

// --- Restoring past a waiting copy (ADR 0052) ------------------------------

impl Env {
    /// `rappel.sqlite3.restore.set-aside-<RESTORE_STAMP>`, the name the
    /// first copy a restore sets aside gets.
    fn set_aside(&self) -> PathBuf {
        self.data
            .join(format!("{DB_FILE_NAME}.restore.set-aside-{RESTORE_STAMP}"))
    }
}

/// The folder outside `backups/` (the opens there make and prune backups).
fn files_outside_backups(e: &Env) -> BTreeMap<PathBuf, (bool, Vec<u8>)> {
    let backups = e.backups();
    snapshot(&e.data)
        .into_iter()
        .filter(|(path, _)| !path.starts_with(&backups))
        .collect()
}

/// A restore keeps the copy an interrupted restore left waiting, byte for
/// byte, under a new name, and every other file as it was (ADR 0007's
/// "Noted, not changed" in ADR 0050, row 35, now changed).
#[test]
fn a_restore_sets_a_waiting_copy_aside_byte_identical() {
    for main_file_left in [false, true] {
        let e = Env::new();
        let (store, backup) = e.diverged();
        drop(store);
        let partial = restore_partial_path(&e.data);
        if main_file_left {
            // A crash after the copy got its name, before the database
            // was moved aside: both are there.
            copy_private(&backup, &partial).unwrap();
        } else {
            copy_validate_and_move_aside(&e, &backup);
        }
        // Not the backup's bytes, so the test sees which file is which.
        {
            let conn = Connection::open(&partial).unwrap();
            add_deck(&conn);
        }
        let waiting = fs::read(&partial).unwrap();
        let before = files_outside_backups(&e);

        let restored = e.restore(&backup).unwrap();

        assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
        assert_eq!(restored.set_aside.as_deref(), Some(e.set_aside().as_path()));
        assert_eq!(fs::read(e.set_aside()).unwrap(), waiting);
        assert_eq!(mode(&e.set_aside()), 0o600);
        // Every file that was there is still there with the same bytes,
        // the waiting copy under its new name; what is new is the restored
        // database and the copy's new name.
        let after = files_outside_backups(&e);
        let db = e.db().to_string_lossy().into_owned();
        let replaced = e.replaced().to_string_lossy().into_owned();
        // The main file or one of its `-wal`/`-shm`/`-journal` files: the
        // suffix after the database's name, if `path` is one.
        let sidecar = |path: &Path| {
            let name = path.to_string_lossy();
            name.strip_prefix(&db)
                .filter(|rest| !rest.starts_with('.'))
                .map(str::to_owned)
        };
        for (path, content) in &before {
            if *path == partial {
                continue;
            }
            // The live database and its own files went to `replaced-…`.
            let moved = match sidecar(path) {
                Some(suffix) => PathBuf::from(format!("{replaced}{suffix}")),
                None => path.clone(),
            };
            assert_eq!(after.get(&moved), Some(content), "{} kept", path.display());
        }
        for path in after.keys() {
            assert!(
                before.contains_key(path)
                    || *path == e.set_aside()
                    || sidecar(path).is_some()
                    || path.to_string_lossy().starts_with(&replaced),
                "{} is new",
                path.display()
            );
        }
        no_partial_left(&e);
    }
}

#[test]
fn two_restores_in_a_row_never_overwrite_a_copy_set_aside() {
    let e = Env::new();
    let (store, backup) = e.diverged();
    drop(store);
    let partial = restore_partial_path(&e.data);
    fs::write(&partial, b"first").unwrap();
    let first = e.restore(&backup).unwrap();
    drop(first);
    fs::write(&partial, b"second").unwrap();

    let second = e.restore(&backup).unwrap();

    assert_eq!(fs::read(e.set_aside()).unwrap(), b"first");
    let again = with_suffix(&e.set_aside(), "-2");
    assert_eq!(second.set_aside.as_deref(), Some(again.as_path()));
    assert_eq!(fs::read(&again).unwrap(), b"second");
    // A third restore with nothing waiting sets nothing aside.
    drop(second);
    let third = e.restore(&backup).unwrap();
    assert_eq!(third.set_aside, None);
}

/// A crash right after the waiting copy got its second name, or right after
/// the new copy took its place, leaves a checked copy waiting: the open
/// still refuses to start an empty database, and the next restore or
/// Finish Restore goes on.
#[test]
fn a_crash_after_setting_the_copy_aside_is_recoverable() {
    for crash_after_new_copy in [false, true] {
        let e = Env::new();
        let (store, backup) = e.diverged();
        drop(store);
        let partial = copy_validate_and_move_aside(&e, &backup);
        let waiting = fs::read(&partial).unwrap();
        // The restore's first steps, by hand, then the crash.
        let kept = keep_waiting_copy(&e.data, &partial, DAY0 + DAY_MS)
            .unwrap()
            .unwrap();
        assert_eq!(kept, e.set_aside());
        if crash_after_new_copy {
            let checking = restore_checking_path(&e.data);
            copy_private(&backup, &checking).unwrap();
            check_copy(&checking, MIGRATIONS).unwrap();
            fs::rename(&checking, &partial).unwrap();
        }

        assert!(matches!(e.open(), Err(Error::InterruptedRestore(_))));
        assert!(!e.db().exists(), "no empty database was started");
        assert_eq!(fs::read(&kept).unwrap(), waiting, "the copy set aside");

        if crash_after_new_copy {
            let finished = finish_restore_with(&e.data, DAY0 + DAY_MS, MIGRATIONS).unwrap();
            assert_eq!(deck_names(&finished.store.conn), ["Maths"]);
        } else {
            let restored = e.restore(&backup).unwrap();
            assert_eq!(deck_names(&restored.store.conn), ["Maths"]);
            // The same file under a second name again: nothing overwritten.
            let again = with_suffix(&e.set_aside(), "-2");
            assert_eq!(restored.set_aside.as_deref(), Some(again.as_path()));
        }
        assert_eq!(fs::read(&kept).unwrap(), waiting, "still kept");
    }
}

/// Anything but a regular file at the waiting copy's name is no copy: a
/// restore leaves it alone and refuses, with nothing changed.
#[test]
fn a_restore_leaves_a_folder_or_link_at_the_copys_name_alone() {
    for link in [false, true] {
        let e = Env::new();
        let (store, backup) = e.diverged();
        drop(store);
        let partial = restore_partial_path(&e.data);
        if link {
            std::os::unix::fs::symlink(&backup, &partial).unwrap();
        } else {
            fs::create_dir(&partial).unwrap();
        }
        let before = snapshot(&e.data);

        let err = e.restore(&backup).unwrap_err();

        assert!(
            matches!(&err, Error::Io { path, source }
                if *path == partial && source.kind() == std::io::ErrorKind::AlreadyExists),
            "{err:?}"
        );
        assert_eq!(snapshot(&e.data), before, "nothing changed");
    }
}

#[test]
fn without_hard_links_the_copy_is_copied_aside_never_over_a_file() {
    let e = Env::new();
    fs::create_dir_all(&e.data).unwrap();
    let partial = restore_partial_path(&e.data);
    fs::write(&partial, b"waiting").unwrap();
    // A file that took the first free name meanwhile is not overwritten.
    fs::write(e.set_aside(), b"someone else's").unwrap();

    let first = copy_to_free_name(&e.data, &partial, ".set-aside-", DAY0 + DAY_MS).unwrap();

    assert_eq!(first, with_suffix(&e.set_aside(), "-2"));
    assert_eq!(fs::read(&first).unwrap(), b"waiting");
    assert_eq!(fs::read(e.set_aside()).unwrap(), b"someone else's");
    assert_eq!(mode(&first), 0o600);
    assert_eq!(fs::read(&partial).unwrap(), b"waiting", "left in place");
}
