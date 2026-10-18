//! Numbered schema migrations.
//!
//! The schema version is `PRAGMA user_version`. Migration N moves a database
//! from version N-1 to N. **Never edit a migration that has been merged**;
//! add a new one. `tests::migrations_are_unchanged` pins every migration's
//! SQL so an edit cannot slip through unnoticed.
//!
//! Migrations run with `foreign_keys=ON`, which cannot be changed inside a
//! transaction. A migration that rebuilds a table (SQLite's "12-step
//! ALTER") would therefore cascade or be blocked by `DROP TABLE`: dropping
//! `cards` would delete `card_schedule` rows, and `review_events` would
//! refuse. Such a migration needs foreign keys switched off around its
//! transaction first; add that to the framework when the first one is
//! written, together with a test. A table that no other table references
//! can be rebuilt with foreign keys on, as 0007 does with `review_events`.

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use crate::domain::deck_name_key;
use crate::error::{Error, Result};
use crate::storage::schedule::write_schedule_cache;
use crate::storage::settings::scheduler_in;

/// One schema migration.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Migration {
    /// The `user_version` after this migration. Contiguous from 1.
    pub version: u32,
    /// Short description, for logs.
    pub name: &'static str,
    /// The SQL to run. Must not contain `BEGIN`/`COMMIT`: it runs inside a
    /// transaction. SQL that ends the transaction is detected and reported
    /// as [`Error::MigrationEndedTransaction`], but whatever it committed
    /// before that point stays committed.
    pub sql: &'static str,
    /// Rebuild the `card_schedule` cache from `review_events` after the
    /// SQL (and the backfill), in the same transaction. For migrations that
    /// reshape the cache or change the scheduler's output (ADR 0009).
    pub rebuild_schedule: bool,
    /// Rust code run after the SQL, in the same transaction, for values
    /// SQL cannot compute (e.g. Unicode normalization). Its error rolls the
    /// migration back. Like the SQL, it must never change once merged: a
    /// different result needs a new migration.
    pub backfill: Option<Backfill>,
}

/// A migration's Rust step; see [`Migration::backfill`].
pub(crate) type Backfill = fn(&Transaction<'_>) -> Result<()>;

/// All migrations, in order.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("0001_initial.sql"),
        rebuild_schedule: false,
        backfill: None,
    },
    Migration {
        version: 2,
        name: "card_schedule phase",
        sql: include_str!("0002_card_schedule_phase.sql"),
        rebuild_schedule: true,
        backfill: None,
    },
    Migration {
        version: 3,
        name: "soft delete",
        sql: include_str!("0003_soft_delete.sql"),
        rebuild_schedule: false,
        backfill: None,
    },
    Migration {
        version: 4,
        name: "deck name key",
        sql: include_str!("0004_deck_name_key.sql"),
        rebuild_schedule: false,
        backfill: Some(backfill_deck_name_keys),
    },
    Migration {
        version: 5,
        name: "rebuild for interval floor",
        sql: include_str!("0005_rebuild_for_interval_floor.sql"),
        rebuild_schedule: true,
        backfill: None,
    },
    Migration {
        version: 6,
        name: "settings",
        sql: include_str!("0006_settings.sql"),
        rebuild_schedule: false,
        backfill: None,
    },
    Migration {
        version: 7,
        name: "log entry kinds",
        sql: include_str!("0007_log_entry_kinds.sql"),
        // Every existing entry is a review, which replays as before.
        rebuild_schedule: false,
        backfill: None,
    },
    Migration {
        version: 8,
        name: "tags",
        sql: include_str!("0008_tags.sql"),
        rebuild_schedule: false,
        backfill: None,
    },
    Migration {
        version: 9,
        name: "suspended",
        sql: include_str!("0009_suspended.sql"),
        rebuild_schedule: false,
        backfill: None,
    },
    Migration {
        version: 10,
        name: "search",
        sql: include_str!("0010_search.sql"),
        rebuild_schedule: false,
        backfill: Some(backfill_search_index),
    },
    Migration {
        version: 11,
        name: "media",
        sql: include_str!("0011_media.sql"),
        rebuild_schedule: false,
        backfill: None,
    },
];

/// Migration 0010: the search index of every card, deleted ones included.
fn backfill_search_index(tx: &Transaction<'_>) -> Result<()> {
    crate::storage::search::rebuild_search_index(tx)
}

/// Migration 0004: `decks.name_key` for every deck, deleted ones included.
/// Two live decks whose names now share a key violate the unique index, so
/// the migration fails and rolls back.
fn backfill_deck_name_keys(tx: &Transaction<'_>) -> Result<()> {
    let decks: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT id, name FROM decks")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut update = tx.prepare("UPDATE decks SET name_key = ?2 WHERE id = ?1")?;
    for (id, name) in decks {
        update.execute(params![id, deck_name_key(&name)])?;
    }
    Ok(())
}

/// The schema version a database has after all of `migrations`.
pub(crate) fn latest(migrations: &[Migration]) -> u32 {
    migrations.last().map_or(0, |m| m.version)
}

/// Applies every migration newer than `current`, each in its own
/// transaction that also sets `user_version` and checks foreign keys (and
/// rebuilds the schedule cache if the migration asks for it). On the
/// first failure that migration is rolled back and the error returned; the
/// database stays at the last migration that succeeded.
pub(crate) fn migrate(conn: &mut Connection, migrations: &[Migration], current: u32) -> Result<()> {
    for m in migrations.iter().filter(|m| m.version > current) {
        tracing::info!(version = m.version, name = m.name, "applying migration");
        let as_migration_err = |source| Error::Migration {
            version: m.version,
            source,
        };
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(as_migration_err)?;
        tx.execute_batch(m.sql).map_err(as_migration_err)?;
        if tx.is_autocommit() {
            // The SQL ended the transaction itself (COMMIT/END/ROLLBACK).
            // Whatever it committed cannot be undone here, but the version
            // is not bumped, so the migration is not recorded as done.
            return Err(Error::MigrationEndedTransaction { version: m.version });
        }
        if let Some(backfill) = m.backfill {
            // An error drops `tx`, which rolls the migration back.
            backfill(&tx).map_err(|e| Error::MigrationBackfill {
                version: m.version,
                source: Box::new(e),
            })?;
        }
        if m.rebuild_schedule {
            // The stored desired retention, or the default before the
            // settings table exists (ADR 0027). An error drops `tx`, which
            // rolls the migration back.
            let scheduler = scheduler_in(&tx).map_err(|e| Error::MigrationRebuild {
                version: m.version,
                source: Box::new(e),
            })?;
            let cards =
                write_schedule_cache(&tx, &scheduler).map_err(|e| Error::MigrationRebuild {
                    version: m.version,
                    source: Box::new(e),
                })?;
            tracing::info!(cards, "schedule cache rebuilt");
        }
        tx.pragma_update(None, "user_version", m.version)
            .map_err(as_migration_err)?;
        let violations = tx
            .prepare("PRAGMA foreign_key_check")
            .and_then(|mut stmt| stmt.exists([]))
            .map_err(as_migration_err)?;
        if violations {
            // Dropping `tx` rolls back.
            return Err(Error::ForeignKeyViolation { version: m.version });
        }
        tx.commit().map_err(as_migration_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_contiguous_from_one() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.version as usize, i + 1, "migration {}", m.name);
        }
    }

    #[test]
    fn migrations_do_not_manage_transactions() {
        for m in MIGRATIONS {
            let upper = m.sql.to_uppercase();
            for word in ["BEGIN TRANSACTION", "COMMIT", "ROLLBACK"] {
                assert!(
                    !upper.contains(word),
                    "migration {} contains {word}",
                    m.version
                );
            }
        }
    }

    /// 64-bit FNV-1a. Not cryptographic; it only has to notice edits.
    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// Pins the SQL of every merged migration. If this fails you edited an
    /// existing migration: revert it and add a new migration instead. Only
    /// append to this list, when adding a migration.
    #[test]
    fn migrations_are_unchanged() {
        const PINNED: &[(u32, u64)] = &[
            (1, 2_537_882_282_965_863_491),
            (2, 11_540_844_169_458_386_709),
            (3, 12_700_174_011_840_410_413),
            (4, 274_116_458_028_196_935),
            (5, 16_265_802_780_356_427_840),
            (6, 4_814_555_124_323_229_936),
            (7, 5_098_997_871_159_998_159),
            (8, 5_300_822_826_126_250_477),
            (9, 1_539_123_956_118_822_870),
            (10, 15_621_161_985_399_297_379),
            (11, 8_376_656_448_889_747_473),
        ];
        assert_eq!(
            MIGRATIONS.len(),
            PINNED.len(),
            "pin every new migration here"
        );
        for (m, &(version, hash)) in MIGRATIONS.iter().zip(PINNED) {
            assert_eq!(m.version, version);
            assert_eq!(
                fnv1a(m.sql.as_bytes()),
                hash,
                "migration {version} was edited; add a new migration instead"
            );
        }
    }
}
