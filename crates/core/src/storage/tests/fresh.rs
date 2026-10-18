//! Fresh database.

use super::*;

#[test]
fn fresh_database_is_created_private_migrated_and_not_backed_up() {
    let e = env();
    let store = e.open(DAY0).unwrap();

    assert_eq!(store.schema_version().unwrap(), latest(MIGRATIONS));
    assert_eq!(
        tables(&store.conn),
        [
            "card_schedule",
            "card_tags",
            "cards",
            "cards_fts",
            "cards_fts_config",
            "cards_fts_content",
            "cards_fts_data",
            "cards_fts_docsize",
            "cards_fts_idx",
            "decks",
            "media",
            "review_events",
            "settings",
            "tags"
        ]
    );
    assert_eq!(mode(&e.db), 0o600);
    assert_eq!(mode(e.db.parent().unwrap()), 0o700);

    let journal: String = store
        .conn
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    let fk: bool = store
        .conn
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    assert!(fk);
    let sync: i64 = store
        .conn
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    assert_eq!(sync, 2, "synchronous=FULL");

    assert!(
        e.backups().is_empty(),
        "nothing to back up in a new database"
    );
}

#[test]
fn wal_and_shm_files_are_private() {
    let e = env();
    let store = e.open(DAY0).unwrap();
    insert_sample(&store.conn);
    for suffix in ["-wal", "-shm"] {
        assert_eq!(mode(&with_suffix(&e.db, suffix)), 0o600, "{suffix}");
    }
}

#[test]
fn existing_loose_permissions_are_tightened() {
    let e = env();
    drop(e.open(DAY0).unwrap());
    fs::set_permissions(&e.db, fs::Permissions::from_mode(0o644)).unwrap();
    drop(e.open(DAY0).unwrap());
    assert_eq!(mode(&e.db), 0o600);
}
