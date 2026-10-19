use proptest::prelude::*;

use rusqlite::OptionalExtension;

use super::*;
use crate::Error;
use crate::domain::{CardState, Grade, MAX_DAILY_LIMIT};
use crate::storage::list_backups;
use crate::test_support::{DAY_MS, T0, answer, schedule_table};

fn bad_stored(store: &Store, key: &str, value: &str) -> Error {
    store
        .conn
        .execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )
        .unwrap();
    store.settings().unwrap_err()
}

/// A card that was learned and then passed twice more, so its due date
/// depends on the retention: `(store, card ids)`.
fn store_with_history(cards: usize) -> (Store, Vec<String>) {
    let mut store = Store::in_memory();
    let deck = store.create_deck_at("Maths", T0).unwrap().id;
    let ids: Vec<String> = (0..cards)
        .map(|i| {
            store
                .add_card_at(&deck, &format!("q{i}"), "a", T0)
                .unwrap()
                .id
        })
        .collect();
    for (i, id) in ids.iter().enumerate() {
        let at = T0 + i64::try_from(i).unwrap() * DAY_MS;
        answer(&mut store, id, Grade::Pass, at);
        answer(&mut store, id, Grade::Pass, at + 11 * 60 * 1000);
        answer(&mut store, id, Grade::Pass, at + 5 * DAY_MS);
    }
    (store, ids)
}

/// Every card's cache row equals a replay of its log with `retention`.
fn assert_cache_is_replay(store: &Store, ids: &[String], retention: f32) {
    let scheduler = Scheduler::new(retention).unwrap();
    for id in ids {
        let events = store.review_events(id).unwrap();
        assert_eq!(
            store.card_state(id).unwrap(),
            scheduler.rebuild_state(id, &events).unwrap(),
            "card {id} at retention {retention}"
        );
    }
}

#[test]
fn a_fresh_database_has_the_defaults() {
    let store = Store::in_memory();
    assert_eq!(store.settings().unwrap(), Settings::default());
    assert_eq!(
        (
            store.settings().unwrap().desired_retention,
            store.settings().unwrap().max_new_per_day,
            store.settings().unwrap().max_reviews_per_day,
            store.settings().unwrap().day_start_hour
        ),
        (0.90, 20, 200, 4)
    );
}

#[test]
fn each_setting_is_kept_across_a_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    {
        let mut store = Store::open(&dir).unwrap();
        store.set_desired_retention(0.85).unwrap();
        store.set_max_new_per_day(7).unwrap();
        store.set_max_reviews_per_day(0).unwrap();
        store.set_day_start_hour(23).unwrap();
    }
    let store = Store::open(&dir).unwrap();
    assert_eq!(
        store.settings().unwrap(),
        Settings {
            desired_retention: 0.85,
            max_new_per_day: 7,
            max_reviews_per_day: 0,
            day_start_hour: 23,
        }
    );
}

#[test]
fn a_changed_setting_leaves_the_others_alone() {
    let mut store = Store::in_memory();
    store.set_max_new_per_day(5).unwrap();
    assert_eq!(
        store.settings().unwrap(),
        Settings {
            max_new_per_day: 5,
            ..Settings::default()
        }
    );
}

#[test]
fn values_out_of_range_are_refused_and_nothing_is_written() {
    let mut store = Store::in_memory();
    for bad in [0.79, 0.98, f32::NAN, f32::INFINITY, -0.9] {
        assert!(
            matches!(
                store.set_desired_retention(bad),
                Err(Error::InvalidSetting { .. })
            ),
            "{bad}"
        );
    }
    assert!(store.set_max_new_per_day(MAX_DAILY_LIMIT + 1).is_err());
    assert!(store.set_max_reviews_per_day(MAX_DAILY_LIMIT + 1).is_err());
    assert!(store.set_day_start_hour(24).is_err());
    assert!(store.set_day_start_hour(u8::MAX).is_err());
    let rows: i64 = store
        .conn
        .query_row("SELECT count(*) FROM settings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    // The boundaries are fine.
    store.set_desired_retention(0.80).unwrap();
    store.set_desired_retention(0.97).unwrap();
    store.set_max_new_per_day(MAX_DAILY_LIMIT).unwrap();
    store.set_day_start_hour(0).unwrap();
}

#[test]
fn a_bad_stored_value_is_an_error_not_a_default() {
    let store = Store::in_memory();
    for (key, value) in [
        ("desired_retention", "0.5"),
        ("desired_retention", "NaN"),
        ("desired_retention", "high"),
        ("max_new_per_day", "-1"),
        ("max_new_per_day", "10000"),
        ("max_reviews_per_day", "lots"),
        ("day_start_hour", "24"),
        ("day_start_hour", "4.5"),
    ] {
        let err = bad_stored(&store, key, value);
        assert!(
            matches!(&err, Error::DamagedSetting { key: k, .. } if k.as_str() == key),
            "{key} = {value}: {err}"
        );
        store.conn.execute("DELETE FROM settings", []).unwrap();
    }
}

/// Hostile values a damaged or hand-edited database may hold (ADR 0050):
/// each is refused with an error that quotes only the start of the value.
#[test]
fn hostile_stored_values_are_refused_and_never_quoted_whole() {
    let store = Store::in_memory();
    let huge = "9".repeat(1 << 20);
    for (key, value) in [
        ("desired_retention", "inf"),
        ("desired_retention", "-inf"),
        ("desired_retention", "1e39"),
        ("desired_retention", "-0"),
        ("desired_retention", " 0.9"),
        ("desired_retention", "0.9\0"),
        ("desired_retention", ""),
        ("max_new_per_day", "4294967296"),
        ("max_new_per_day", "0x10"),
        ("max_new_per_day", "٣"),
        ("max_reviews_per_day", huge.as_str()),
        ("day_start_hour", "256"),
        ("day_start_hour", "-0"),
    ] {
        let err = bad_stored(&store, key, value);
        let Error::DamagedSetting {
            key: damaged,
            value: shown,
        } = &err
        else {
            panic!("{key} = {value:?}: {err}");
        };
        assert_eq!(damaged.as_str(), key);
        assert!(
            shown.chars().count() <= 65,
            "{key}: {} characters",
            shown.len()
        );
        store.conn.execute("DELETE FROM settings", []).unwrap();
    }
    // A number where text belongs: the STRICT table stores it as text, so
    // it reads like the text a write makes.
    store.conn.execute("DELETE FROM settings", []).unwrap();
    store
        .conn
        .execute("INSERT INTO settings VALUES ('day_start_hour', 5)", [])
        .unwrap();
    assert_eq!(store.settings().unwrap().day_start_hour, 5);
    // A blob is refused by the table itself.
    assert!(
        store
            .conn
            .execute("INSERT INTO settings VALUES ('max_new_per_day', x'00')", [])
            .is_err()
    );
}

#[test]
fn an_unknown_key_is_ignored() {
    let store = Store::in_memory();
    store
        .conn
        .execute("INSERT INTO settings VALUES ('from_the_future', 'x')", [])
        .unwrap();
    assert_eq!(store.settings().unwrap(), Settings::default());
}

#[test]
fn the_retention_changes_what_the_scheduler_does() {
    let (mut store, ids) = store_with_history(3);
    assert_cache_is_replay(&store, &ids, 0.90);
    let before = schedule_table(&store);

    store.set_desired_retention(0.80).unwrap();
    assert_cache_is_replay(&store, &ids, 0.80);
    assert_ne!(
        schedule_table(&store),
        before,
        "a lower retention must give other intervals"
    );

    // The log did not change.
    for id in &ids {
        assert_eq!(store.review_events(id).unwrap().len(), 3);
    }

    // Back to the default: the original schedule, exactly.
    store.set_desired_retention(0.90).unwrap();
    assert_eq!(schedule_table(&store), before);
}

#[test]
fn a_review_after_the_change_uses_the_new_retention() {
    let (mut store, ids) = store_with_history(1);
    store.set_desired_retention(0.97).unwrap();
    answer(&mut store, &ids[0], Grade::Pass, T0 + 30 * DAY_MS);
    assert_cache_is_replay(&store, &ids, 0.97);
    let default = Scheduler::default()
        .rebuild_state(&ids[0], &store.review_events(&ids[0]).unwrap())
        .unwrap();
    assert_ne!(store.card_state(&ids[0]).unwrap(), default);
}

#[test]
fn a_failed_rebuild_changes_neither_the_value_nor_the_cache() {
    let (mut store, _) = store_with_history(3);
    let before = schedule_table(&store);
    // The rebuild empties the cache and then inserts: fail the first insert.
    store
        .conn
        .execute_batch(
            "CREATE TEMP TRIGGER fail_rebuild BEFORE INSERT ON card_schedule
             BEGIN SELECT RAISE(ABORT, 'no'); END;",
        )
        .unwrap();
    assert!(store.set_desired_retention(0.80).is_err());
    store.conn.execute("DROP TRIGGER fail_rebuild", []).unwrap();
    assert_eq!(store.settings().unwrap().desired_retention, 0.90);
    assert_eq!(schedule_table(&store), before);
}

#[test]
fn a_bad_stored_retention_can_be_written_over() {
    let (mut store, ids) = store_with_history(2);
    bad_stored(&store, "desired_retention", "garbage");
    assert!(store.settings().is_err());
    store.set_desired_retention(0.90).unwrap();
    assert_eq!(store.settings().unwrap().desired_retention, 0.90);
    assert_cache_is_replay(&store, &ids, 0.90);
}

#[test]
fn the_same_value_does_not_rebuild() {
    let (mut store, _) = store_with_history(2);
    store.set_desired_retention(0.85).unwrap();
    // A rebuild would delete cache rows.
    store
        .conn
        .execute_batch(
            "CREATE TEMP TRIGGER no_rebuild BEFORE DELETE ON card_schedule
             BEGIN SELECT RAISE(ABORT, 'rebuilt'); END;",
        )
        .unwrap();
    store.set_desired_retention(0.85).unwrap();
    assert!(store.set_desired_retention(0.86).is_err());
}

#[test]
fn another_connection_schedules_with_the_stored_retention() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let mut main = Store::open(&dir).unwrap();
    let deck = main.create_deck_at("Maths", T0).unwrap().id;
    let card = main.add_card_at(&deck, "q", "a", T0).unwrap().id;
    main.set_desired_retention(0.80).unwrap();

    // A worker connection opened afterwards, and one opened before.
    let mut later = Store::open_concurrent(&dir).unwrap();
    answer(&mut later, &card, Grade::Pass, T0);
    answer(&mut later, &card, Grade::Pass, T0 + 11 * 60 * 1000);
    main.set_desired_retention(0.95).unwrap();
    answer(&mut later, &card, Grade::Pass, T0 + 6 * DAY_MS);

    // The last review used 0.95, the committed value at that moment.
    let events = later.review_events(&card).unwrap();
    let expected = Scheduler::new(0.95)
        .unwrap()
        .rebuild_state(&card, &events)
        .unwrap();
    assert_eq!(main.card_state(&card).unwrap(), expected);
    assert!(matches!(expected, CardState::Scheduled(_)));
}

// --- A damaged stored setting at startup (ADR 0052) -------------------------

/// A data folder with cards, a review history, a tag and every setting
/// changed from its default, then closed. Returns the folder (inside the
/// temporary directory) and the card ids.
fn folder_with_history(tmp: &tempfile::TempDir) -> (std::path::PathBuf, Vec<String>) {
    let dir = tmp.path().join("data");
    let mut store = Store::open(&dir).unwrap();
    let deck = store.create_deck_at("Maths", T0).unwrap().id;
    let mut ids = Vec::new();
    for i in 0..3 {
        let id = store
            .add_card_at(&deck, &format!("q{i}"), "a", T0)
            .unwrap()
            .id;
        let at = T0 + i64::from(i) * DAY_MS;
        answer(&mut store, &id, Grade::Pass, at);
        answer(&mut store, &id, Grade::Pass, at + 11 * 60 * 1000);
        answer(&mut store, &id, Grade::Pass, at + 5 * DAY_MS);
        ids.push(id);
    }
    store.add_tag(&ids[0], "algebra").unwrap();
    store.set_desired_retention(0.85).unwrap();
    store.set_max_new_per_day(7).unwrap();
    store.set_max_reviews_per_day(70).unwrap();
    store.set_day_start_hour(6).unwrap();
    (dir, ids)
}

/// The open store after a reset that leaves no damaged setting, and
/// whether it reset anything.
fn opened(reset: SettingReset) -> (Store, bool) {
    match reset.then {
        AfterReset::Opened(store) => (*store, reset.was_reset),
        AfterReset::Damaged(key) => panic!("{key} is still damaged"),
    }
}

/// The settings `folder_with_history` stores.
const CHANGED: Settings = Settings {
    desired_retention: 0.85,
    max_new_per_day: 7,
    max_reviews_per_day: 70,
    day_start_hour: 6,
};

/// Stores `value` under `key` with a plain connection, as a hand edit does.
fn damage(dir: &std::path::Path, key: SettingKey, value: &str) {
    let conn = Connection::open(dir.join(DB_FILE_NAME)).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        [key.as_str(), value],
    )
    .unwrap();
}

/// Every row of the tables a reset must leave alone, as text.
fn data_rows(conn: &Connection) -> Vec<String> {
    let mut out = Vec::new();
    for table in ["decks", "cards", "review_events", "tags", "card_tags"] {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))
            .unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..n)
                    .map(|i| {
                        r.get::<_, rusqlite::types::Value>(i)
                            .map(|v| format!("{v:?}"))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        for row in rows {
            out.push(format!("{table}: {}", row.unwrap().join(" | ")));
        }
    }
    out
}

/// The stored value of `key` in the database at `path`, if any.
fn stored_value(path: &std::path::Path, key: SettingKey) -> Option<String> {
    let conn = Connection::open(path).unwrap();
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [key.as_str()],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

/// `settings` with `key` at its default.
fn with_default(settings: Settings, key: SettingKey) -> Settings {
    let defaults = Settings::default();
    let mut out = settings;
    match key {
        SettingKey::DesiredRetention => out.desired_retention = defaults.desired_retention,
        SettingKey::MaxNewPerDay => out.max_new_per_day = defaults.max_new_per_day,
        SettingKey::MaxReviewsPerDay => out.max_reviews_per_day = defaults.max_reviews_per_day,
        SettingKey::DayStartHour => out.day_start_hour = defaults.day_start_hour,
    }
    out
}

#[test]
fn the_open_refuses_a_damaged_setting_and_names_its_key() {
    for key in SettingKey::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, _) = folder_with_history(&tmp);
        damage(&dir, key, &"7".repeat(1000));
        match Store::open(&dir) {
            Err(Error::DamagedSetting { key: found, value }) => {
                assert_eq!(found, key);
                assert!(value.chars().count() <= 65, "{key}: cut");
            }
            other => panic!("{key}: expected DamagedSetting, got {other:?}"),
        }
        // A database without decks is refused too: nothing depends on a
        // read that happens to come first.
        let empty = tmp.path().join("empty");
        drop(Store::open(&empty).unwrap());
        damage(&empty, key, "x");
        assert!(matches!(
            Store::open(&empty),
            Err(Error::DamagedSetting { key: k, .. }) if k == key
        ));
    }
}

#[test]
fn resetting_a_damaged_setting_puts_its_default_back_and_nothing_else() {
    for key in SettingKey::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, ids) = folder_with_history(&tmp);
        let (rows_before, cache_before) = {
            let store = Store::open(&dir).unwrap();
            (data_rows(&store.conn), schedule_table(&store))
        };
        damage(&dir, key, "damaged");
        let backups_before = list_backups(&dir).unwrap().len();

        let (store, was_reset) = opened(reset_setting(&dir, key).unwrap());

        assert!(was_reset, "{key}");
        assert_eq!(
            store.settings().unwrap(),
            with_default(CHANGED, key),
            "{key}"
        );
        assert_eq!(stored_value(&dir.join(DB_FILE_NAME), key), None, "{key}");
        assert_eq!(data_rows(&store.conn), rows_before, "{key}: data untouched");
        if key == SettingKey::DesiredRetention {
            assert_cache_is_replay(&store, &ids, DEFAULT_DESIRED_RETENTION);
            assert_ne!(schedule_table(&store), cache_before, "rebuilt");
        } else {
            assert_eq!(
                schedule_table(&store),
                cache_before,
                "{key}: cache untouched"
            );
        }
        // The backup made first holds the damaged value, as it was.
        let backups = list_backups(&dir).unwrap();
        assert_eq!(backups.len(), backups_before + 1, "{key}: one backup");
        let newest = backups.first().unwrap();
        assert_eq!(
            stored_value(newest.path(), key).as_deref(),
            Some("damaged"),
            "{key}"
        );
        drop(store);
        // And the next start opens normally.
        Store::open(&dir).unwrap();
    }
}

#[test]
fn a_failed_backup_resets_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, _) = folder_with_history(&tmp);
    let rows_before = data_rows(&Store::open(&dir).unwrap().conn);
    damage(&dir, SettingKey::DesiredRetention, "damaged");
    // A file where the backup folder belongs: no backup can be made (also
    // when the tests run as root, unlike a permission trick).
    let backups = dir.join(BACKUP_DIR_NAME);
    std::fs::remove_dir_all(&backups).unwrap();
    std::fs::write(&backups, b"not a folder").unwrap();

    let err = reset_setting(&dir, SettingKey::DesiredRetention).unwrap_err();

    assert!(matches!(err, Error::NotADirectory(_)), "{err}");
    let db = dir.join(DB_FILE_NAME);
    assert_eq!(
        stored_value(&db, SettingKey::DesiredRetention).as_deref(),
        Some("damaged")
    );
    assert_eq!(data_rows(&Connection::open(&db).unwrap()), rows_before);
    assert!(matches!(
        Store::open(&dir),
        Err(Error::DamagedSetting {
            key: SettingKey::DesiredRetention,
            ..
        })
    ));
}

#[test]
fn a_second_damaged_setting_is_reported_after_the_first_is_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, _) = folder_with_history(&tmp);
    damage(&dir, SettingKey::MaxReviewsPerDay, "lots");
    damage(&dir, SettingKey::DesiredRetention, "high");
    // The first key in `SettingKey::ALL` order is named first.
    assert!(matches!(
        Store::open(&dir),
        Err(Error::DamagedSetting {
            key: SettingKey::DesiredRetention,
            ..
        })
    ));

    let reset = reset_setting(&dir, SettingKey::DesiredRetention).unwrap();

    assert!(reset.was_reset);
    assert!(
        matches!(
            reset.then,
            AfterReset::Damaged(SettingKey::MaxReviewsPerDay)
        ),
        "{:?}",
        reset.then
    );
    let db = dir.join(DB_FILE_NAME);
    assert_eq!(stored_value(&db, SettingKey::DesiredRetention), None);
    assert_eq!(
        stored_value(&db, SettingKey::MaxReviewsPerDay).as_deref(),
        Some("lots")
    );
    let (store, _) = opened(reset_setting(&dir, SettingKey::MaxReviewsPerDay).unwrap());
    assert_eq!(
        store.settings().unwrap(),
        Settings {
            desired_retention: DEFAULT_DESIRED_RETENTION,
            max_reviews_per_day: Settings::default().max_reviews_per_day,
            ..CHANGED
        }
    );
}

#[test]
fn resetting_a_setting_that_is_not_damaged_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, _) = folder_with_history(&tmp);
    drop(Store::open(&dir).unwrap());
    let backups = list_backups(&dir).unwrap().len();
    for key in SettingKey::ALL {
        let (store, was_reset) = opened(reset_setting(&dir, key).unwrap());
        assert!(!was_reset, "{key}");
        assert_eq!(store.settings().unwrap(), CHANGED, "{key}");
    }
    assert_eq!(list_backups(&dir).unwrap().len(), backups, "no backup");
}

/// The open checks the settings after migrating (ADR 0052), so a migration
/// that read them would meet a damaged value first and fail with no way
/// on. None does: only migrations before the settings table (0006) rebuild
/// the schedule cache, the one way a migration reads them (the backfill of
/// 0010 reads the cards' text only). A new one that must read the settings
/// has to handle a damaged value itself.
#[test]
fn no_migration_reads_the_settings() {
    for m in MIGRATIONS {
        assert!(
            m.version < 6 || !m.rebuild_schedule,
            "migration {} rebuilds the schedule after the settings table exists; see ADR 0052",
            m.version
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Whatever the logs, changing the retention leaves every cache row
    /// equal to a replay of its log at the new retention.
    #[test]
    fn a_retention_change_equals_a_replay(
        retention in 0.80f32..=0.97,
        logs in proptest::collection::vec(
            proptest::collection::vec((any::<bool>(), 0i64..40), 1..8), 1..5),
    ) {
        let mut store = Store::in_memory();
        let deck = store.create_deck_at("D", T0).unwrap().id;
        let mut ids = Vec::new();
        for (i, log) in logs.iter().enumerate() {
            let card = store.add_card_at(&deck, &format!("q{i}"), "a", T0).unwrap().id;
            let mut at = T0;
            for &(pass, gap_days) in log {
                at += gap_days * DAY_MS + 60_000;
                let grade = if pass { Grade::Pass } else { Grade::Fail };
                answer(&mut store, &card, grade, at);
            }
            ids.push(card);
        }
        store.set_desired_retention(retention).unwrap();
        assert_cache_is_replay(&store, &ids, retention);
    }
}
