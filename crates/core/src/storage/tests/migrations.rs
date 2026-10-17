//! Migrations.

use super::*;

/// For every real migration N: a database at N-1 (built by the real code
/// with only the first N-1 migrations) opens and reaches N. This covers
/// "migrate a DB created by the previous version" for every future
/// migration too; migrations that transform data add their own tests.
#[test]
fn every_migration_upgrades_the_previous_version() {
    for n in 1..=MIGRATIONS.len() {
        let e = env();
        let prev = e.open_with(DAY0, &MIGRATIONS[..n - 1]).unwrap();
        assert_eq!(prev.schema_version().unwrap() as usize, n - 1);
        drop(prev);

        let store = e.open_with(DAY0, &MIGRATIONS[..n]).unwrap();
        assert_eq!(store.schema_version().unwrap() as usize, n);
        assert_eq!(check_rows(&store.conn, "integrity_check").unwrap(), ["ok"]);
    }
}

#[test]
fn migration_0001_applies_to_an_existing_empty_file() {
    let e = env();
    fs::create_dir_all(e.db.parent().unwrap()).unwrap();
    drop(Connection::open(&e.db).unwrap());
    assert_eq!(fs::metadata(&e.db).unwrap().len(), 0);

    let store = e.open(DAY0).unwrap();
    assert_eq!(store.schema_version().unwrap(), latest(MIGRATIONS));
    insert_sample(&store.conn);
}

const FK_VIOLATING_V2: Migration = Migration {
    version: 2,
    name: "fk-violating v2",
    sql: "PRAGMA defer_foreign_keys = ON;
          CREATE TABLE p(id INTEGER PRIMARY KEY) STRICT;
          CREATE TABLE c(p INTEGER REFERENCES p(id)) STRICT;
          INSERT INTO c VALUES (99);",
    rebuild_schedule: false,
    backfill: None,
};

#[test]
fn upgrade_keeps_data_and_backs_up_the_old_version_first() {
    let e = env();
    db_at_fake_v1(&e);

    let store = e.open_with(DAY0, &[FAKE_V1, FAKE_V2]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 2);
    let sum: i64 = store
        .conn
        .query_row("SELECT sum(x + y) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sum, 1 + 7 + 2 + 7);

    // The backup holds the database as it was before the migration.
    let backups = e.backups();
    assert_eq!(backups.len(), 1);
    let old = Connection::open(e.backups.join(&backups[0])).unwrap();
    let v: u32 = old
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1);
    assert_eq!(count(&old, "t"), 2);
    let has_y: bool = old
        .query_row(
            "SELECT count(*) FROM pragma_table_info('t') WHERE name = 'y'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!has_y);
}

#[test]
fn failed_migration_rolls_back_and_keeps_the_backup() {
    let e = env();
    db_at_fake_v1(&e);

    let err = e.open_with(DAY0, &[FAKE_V1, FAILING_V2]).unwrap_err();
    assert!(matches!(err, Error::Migration { version: 2, .. }), "{err}");

    let conn = Connection::open(&e.db).unwrap();
    let v: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1);
    assert_eq!(tables(&conn), ["t"], "table u was rolled back");
    assert_eq!(count(&conn, "t"), 2);
    assert_eq!(e.backups().len(), 1);
}

#[test]
fn foreign_key_violation_rolls_back_the_migration() {
    let e = env();
    db_at_fake_v1(&e);

    let err = e.open_with(DAY0, &[FAKE_V1, FK_VIOLATING_V2]).unwrap_err();
    assert!(
        matches!(err, Error::ForeignKeyViolation { version: 2 }),
        "{err}"
    );
    let conn = Connection::open(&e.db).unwrap();
    assert_eq!(tables(&conn), ["t"]);
}

#[test]
fn newer_database_is_refused_untouched() {
    let e = env();
    drop(e.open(DAY0).unwrap());
    {
        // Not in WAL mode, like a backup copied back by hand: switching it
        // to WAL would rewrite the header, so this catches any write that
        // happens before the version check.
        let conn = Connection::open(&e.db).unwrap();
        conn.pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get::<_, String>(0))
            .unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
    }
    let before = fs::read(&e.db).unwrap();

    let err = e.open(DAY0 + DAY_MS).unwrap_err();
    assert!(
        matches!(
            err,
            Error::UnsupportedVersion {
                found: 99,
                supported
            } if supported == latest(MIGRATIONS)
        ),
        "{err}"
    );
    assert_eq!(fs::read(&e.db).unwrap(), before);
    assert!(!with_suffix(&e.db, "-wal").exists(), "no WAL file created");
    assert!(e.backups().is_empty(), "no backup of a database we refuse");
}

#[test]
fn foreign_database_is_refused_untouched() {
    let e = env();
    fs::create_dir_all(e.db.parent().unwrap()).unwrap();
    Connection::open(&e.db)
        .unwrap()
        .execute_batch("CREATE TABLE other_app(x); INSERT INTO other_app VALUES (1);")
        .unwrap();
    let before = fs::read(&e.db).unwrap();

    assert!(matches!(e.open(DAY0), Err(Error::NotARappelDatabase)));
    assert_eq!(fs::read(&e.db).unwrap(), before);
    assert_eq!(
        e.backups(),
        Vec::<String>::new(),
        "no backup of a foreign file"
    );
}

#[test]
fn failed_pre_migration_backup_stops_the_migration() {
    let e = env();
    db_at_fake_v1(&e);
    // A regular file where the backup directory should be.
    fs::write(&e.backups, b"").unwrap();

    let err = e.open_with(DAY0, &[FAKE_V1, FAKE_V2]).unwrap_err();
    assert!(matches!(err, Error::NotADirectory(_)), "{err}");

    let conn = Connection::open(&e.db).unwrap();
    let v: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1, "not migrated without a backup");
    let leftovers: Vec<_> = fs::read_dir(e.db.parent().unwrap())
        .unwrap()
        .map(|d| d.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("cards-"))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());
}

#[test]
fn failed_daily_backup_still_opens_a_healthy_database() {
    let e = env();
    let store = e.open(DAY0).unwrap();
    insert_sample(&store.conn);
    drop(store);
    fs::write(&e.backups, b"").unwrap();

    let mut store = e.open(DAY0 + DAY_MS).unwrap();
    assert_eq!(count(&store.conn, "review_events"), 1);
    assert_eq!(store.schema_version().unwrap(), latest(MIGRATIONS));
    let warnings = store.take_open_warnings();
    assert!(
        matches!(warnings.as_slice(), [OpenWarning::DailyBackupFailed(_)]),
        "one warning for the failed backup, not a second for the prune: {warnings:?}"
    );
    assert!(store.take_open_warnings().is_empty(), "taken once");
}

#[test]
fn healthy_opens_have_no_warnings() {
    let e = env();
    let mut store = e.open(DAY0).unwrap();
    assert!(store.take_open_warnings().is_empty(), "new database");
    drop(store);
    let mut store = e.open(DAY0 + DAY_MS).unwrap();
    assert!(store.take_open_warnings().is_empty(), "daily backup made");
    assert_eq!(e.backups().len(), 1);
}

#[test]
fn failed_prune_is_a_warning() {
    let e = env();
    drop(e.open(DAY0).unwrap());
    drop(e.open(DAY0 + DAY_MS).unwrap());
    // Today's backup exists, so none is attempted; a stale partial backup
    // that cannot be removed (a non-empty directory) makes the prune fail.
    let stuck = e.backups.join("rappel-20261001-000000.sqlite3.partial");
    fs::create_dir(&stuck).unwrap();
    fs::write(stuck.join("x"), b"").unwrap();

    let mut store = e.open(DAY0 + DAY_MS + 1000).unwrap();
    let warnings = store.take_open_warnings();
    assert!(
        matches!(warnings.as_slice(), [OpenWarning::PruneFailed(_)]),
        "{warnings:?}"
    );
}

/// The app restores on a worker thread and sends the new store back.
#[test]
fn store_can_move_between_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<Store>();
}

#[test]
fn pre_migration_backup_survives_future_dated_backups() {
    let e = env();
    db_at_fake_v1(&e);
    fs::remove_dir_all(&e.backups).ok();
    fs::create_dir(&e.backups).unwrap();
    for day in 1..=15 {
        fs::write(
            e.backups
                .join(format!("rappel-203001{day:02}-120000.sqlite3")),
            b"",
        )
        .unwrap();
    }

    let store = e.open_with(DAY0, &[FAKE_V1, FAKE_V2]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 2);
    let fresh = e.backups.join("rappel-20261015-123456.sqlite3");
    let v: u32 = Connection::open(&fresh)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1, "the pre-migration backup is still there");
    // The fresh backup, and the newest of each of the 14 latest days of
    // the future-dated ones (#50; before, only the newest of them).
    let mut expected: Vec<String> = (2..=15)
        .map(|day| format!("rappel-203001{day:02}-120000.sqlite3"))
        .collect();
    expected.insert(0, "rappel-20261015-123456.sqlite3".into());
    assert_eq!(e.backups(), expected);
}

#[test]
fn migration_that_commits_itself_is_not_recorded() {
    const COMMITTING_V2: Migration = Migration {
        version: 2,
        name: "committing v2",
        sql: "CREATE TABLE u(x INTEGER) STRICT; COMMIT;",
        rebuild_schedule: false,
        backfill: None,
    };
    let e = env();
    db_at_fake_v1(&e);
    let err = e.open_with(DAY0, &[FAKE_V1, COMMITTING_V2]).unwrap_err();
    assert!(
        matches!(err, Error::MigrationEndedTransaction { version: 2 }),
        "{err}"
    );
    let v: u32 = Connection::open(&e.db)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(v, 1);
}

#[test]
fn negative_user_version_is_refused() {
    let e = env();
    fs::create_dir_all(e.db.parent().unwrap()).unwrap();
    Connection::open(&e.db)
        .unwrap()
        .pragma_update(None, "user_version", -1)
        .unwrap();
    assert!(matches!(
        e.open(DAY0),
        Err(Error::UnsupportedVersion { found: -1, .. })
    ));
}

/// The cards of `DECK` with the columns every schema version has, for the
/// tests of migrations older than a column the current code reads.
fn old_cards(conn: &Connection) -> Vec<(String, String, String, String, i64, i64)> {
    let mut stmt = conn
        .prepare(
            "SELECT id, deck_id, front_md, back_md, created_at, updated_at FROM cards
             WHERE deck_id = ?1 AND deleted_at IS NULL ORDER BY seq",
        )
        .unwrap();
    stmt.query_map([DECK], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// Migration 0006 only adds the empty `settings` table: data and the
/// schedule cache stay as they were, and every setting has its default.
#[test]
fn migration_0006_adds_settings_and_keeps_everything_else() {
    use crate::domain::Settings;
    use crate::test_support::schedule_table;

    let e = env();
    let before = {
        let v5 = e.open_with(DAY0, &MIGRATIONS[..5]).unwrap();
        insert_sample(&v5.conn);
        assert!(!tables(&v5.conn).contains(&"settings".to_owned()));
        (
            schedule_table(&v5),
            v5.review_events(CARD).unwrap(),
            old_cards(&v5.conn),
        )
    };
    let store = e.open_with(DAY0, &MIGRATIONS[..6]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 6);
    assert_eq!(count(&store.conn, "settings"), 0);
    assert_eq!(store.settings().unwrap(), Settings::default());
    assert_eq!(
        (
            schedule_table(&store),
            store.review_events(CARD).unwrap(),
            old_cards(&store.conn)
        ),
        before
    );
    // The pre-migration backup is the version-5 database.
    assert_eq!(e.backups().len(), 1);
}

/// A later migration that rebuilds the schedule cache uses the stored
/// desired retention, not the default (ADR 0006, 0027).
#[test]
fn a_rebuilding_migration_uses_the_stored_retention() {
    let e = env();
    {
        let mut current = e.open(DAY0).unwrap();
        insert_sample(&current.conn);
        insert_events(
            &current.conn,
            CARD,
            10,
            &[(DAY_MS, "pass"), (9 * DAY_MS, "pass")],
        );
        current.set_desired_retention(0.80).unwrap();
    }
    let next = latest(MIGRATIONS) + 1;
    let rebuild = Migration {
        version: next,
        name: "fake rebuild",
        sql: "SELECT 1;",
        rebuild_schedule: true,
        backfill: None,
    };
    let mut migrations = MIGRATIONS.to_vec();
    migrations.push(rebuild);
    let store = e.open_with(DAY0, &migrations).unwrap();
    assert_eq!(store.schema_version().unwrap(), next);
    let events = store.review_events(CARD).unwrap();
    let at_80 = Scheduler::new(0.80)
        .unwrap()
        .rebuild_state(CARD, &events)
        .unwrap();
    let at_90 = Scheduler::default().rebuild_state(CARD, &events).unwrap();
    assert_ne!(at_80, at_90, "the test needs the retentions to differ");
    assert_eq!(store.card_state(CARD).unwrap(), at_80);
}

/// The `CREATE` statement of `name` in the schema.
fn schema_sql(conn: &Connection, name: &str) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE name = ?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

/// Migration 0007 rebuilds `review_events` with entry kinds (ADR 0028):
/// every row comes across as a review, unchanged; the schedule cache is
/// untouched; the append-only triggers are back; the self-reference names
/// the renamed table.
#[test]
fn migration_0007_keeps_every_review_and_the_cache() {
    use crate::domain::{EventKind, Grade};
    use crate::test_support::schedule_table;

    let e = env();
    let before = {
        let mut v6 = e.open_with(DAY0, &MIGRATIONS[..6]).unwrap();
        insert_sample(&v6.conn);
        v6.conn
            .execute_batch(&format!(
                "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at)
                     VALUES ('{CARD2}', '{DECK}', 'f', 'b', 1, 1);"
            ))
            .unwrap();
        insert_events(&v6.conn, CARD2, 10, &REVIEWS);
        // The cache of both cards, as version 6 builds it.
        v6.rebuild_schedule_cache(&Scheduler::default()).unwrap();
        let rows: Vec<(String, String, i64, String, i64)> = v6
            .conn
            .prepare("SELECT id, card_id, reviewed_at, grade, duration_ms FROM review_events ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        (rows, schedule_table(&v6))
    };
    let store = e.open_with(DAY0, &MIGRATIONS[..7]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 7);
    let after: Vec<(String, String, i64, String, i64)> = store
        .conn
        .prepare(
            "SELECT id, card_id, reviewed_at, grade, duration_ms FROM review_events
             WHERE kind = 'review' AND undoes IS NULL ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(after, before.0);
    assert_eq!(count(&store.conn, "review_events"), 5);
    assert_eq!(schedule_table(&store), before.1);
    let events = store.review_events(CARD).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Review(Grade::Pass));
    // The triggers came back with the table.
    for sql in [
        "UPDATE review_events SET grade = 'fail'",
        "DELETE FROM review_events",
    ] {
        let err = store.conn.execute(sql, []).unwrap_err();
        assert!(err.to_string().contains("append-only"), "{sql}: {err}");
    }
    let table = schema_sql(&store.conn, "review_events");
    assert!(
        table.contains("REFERENCES \"review_events\"(id)"),
        "{table}"
    );
    assert!(!table.contains("review_events_v7"), "{table}");
    for name in [
        "review_events_card_reviewed",
        "review_events_undoes",
        "review_events_no_update",
        "review_events_no_delete",
        "review_events_undo_target",
    ] {
        schema_sql(&store.conn, name);
    }
    // The pre-migration backup is the version-6 database.
    assert_eq!(e.backups().len(), 1);
}

/// Migration 0008 only adds the empty tag tables.
#[test]
fn migration_0008_adds_tags_and_keeps_everything_else() {
    use crate::test_support::schedule_table;

    let e = env();
    let before = {
        let v7 = e.open_with(DAY0, &MIGRATIONS[..7]).unwrap();
        insert_sample(&v7.conn);
        assert!(!tables(&v7.conn).contains(&"tags".to_owned()));
        (
            schedule_table(&v7),
            v7.review_events(CARD).unwrap(),
            old_cards(&v7.conn),
        )
    };
    let store = e.open_with(DAY0, &MIGRATIONS[..8]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
    assert_eq!(count(&store.conn, "tags"), 0);
    assert_eq!(count(&store.conn, "card_tags"), 0);
    assert_eq!(store.tags().unwrap(), []);
    assert_eq!(
        (
            schedule_table(&store),
            store.review_events(CARD).unwrap(),
            old_cards(&store.conn)
        ),
        before
    );
    assert_eq!(e.backups().len(), 1);
}

/// Migrations 0009 and 0010 on a version 8 database: every card is
/// unsuspended, the data and the schedule stay as they were, and the
/// search index holds the plain text of every card, deleted ones too.
#[test]
fn migrations_0009_and_0010_add_suspension_and_index_every_card() {
    use crate::content::search_text;
    use crate::domain::Settings;
    use crate::test_support::schedule_table;

    const GONE: &str = "01920000-0000-7000-8000-00000000000a";
    let e = env();
    let before = {
        let v8 = e.open_with(DAY0, &MIGRATIONS[..8]).unwrap();
        insert_sample(&v8.conn);
        v8.conn
            .execute(
                "INSERT INTO cards (id, deck_id, front_md, back_md, created_at, updated_at, deleted_at)
                 VALUES (?1, ?2, '**Économie** $x^2$', '[lien](https://a.b)', 1, 1, 5)",
                rusqlite::params![GONE, DECK],
            )
            .unwrap();
        assert!(!tables(&v8.conn).contains(&"cards_fts".to_owned()));
        (
            schedule_table(&v8),
            v8.review_events(CARD).unwrap(),
            old_cards(&v8.conn),
            v8.settings().unwrap(),
        )
    };
    let store = e.open_with(DAY0, &MIGRATIONS[..10]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 10);
    assert_eq!(
        (
            schedule_table(&store),
            store.review_events(CARD).unwrap(),
            old_cards(&store.conn),
            store.settings().unwrap(),
        ),
        before
    );
    assert_eq!(before.3, Settings::default());
    assert_eq!(
        count(&store.conn, "cards WHERE suspended = 0"),
        count(&store.conn, "cards")
    );
    assert_eq!(count(&store.conn, "cards_fts"), 2);
    let rows: Vec<(String, String)> = store
        .conn
        .prepare(
            "SELECT f.front, f.back FROM cards_fts f JOIN cards c ON c.seq = f.rowid
             ORDER BY c.seq",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (search_text("front"), search_text("back")),
            ("economie x^2".to_owned(), "lien".to_owned()),
        ]
    );
    // The pre-migration backup is the version-8 database.
    assert_eq!(e.backups().len(), 1);
}
