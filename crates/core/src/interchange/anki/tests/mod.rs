//! Tests of the Anki import (ADR 0061). `fixtures.rs` reads the packages
//! Anki itself made (`../fixtures/`, see its README); `hostile.rs` builds
//! packages by hand to attack every part; `convert.rs` pins the HTML to
//! Markdown conversion.

mod builder;
mod convert;
mod fixtures;
mod hostile;

use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::package::AnkiLimits;
use super::{AnkiPlan, AnkiReport, Skip, Skipped};
use crate::storage::Store;
use crate::test_support::T0;

/// A store in a temporary data directory.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("data")).unwrap();
        Self { tmp, store }
    }

    fn data_dir(&self) -> std::path::PathBuf {
        self.tmp.path().join("data")
    }

    fn plan(&self, bytes: &[u8]) -> crate::Result<AnkiPlan> {
        self.store.plan_anki_bytes(bytes, &AnkiLimits::DEFAULT)
    }

    fn import(&mut self, bytes: &[u8]) -> crate::Result<AnkiReport> {
        let plan = self.plan(bytes)?;
        self.store.apply_anki_at(
            &plan,
            T0 + 400 * crate::test_support::DAY_MS,
            &AtomicBool::new(false),
        )
    }

    /// Every live card: (deck name, front, back, suspended), in creation
    /// order.
    fn cards(&self) -> Vec<(String, String, String, bool)> {
        let mut stmt = self
            .store
            .conn
            .prepare(
                "SELECT d.name, c.front_md, c.back_md, c.suspended FROM cards c
                 JOIN decks d ON d.id = c.deck_id WHERE c.deleted_at IS NULL ORDER BY c.seq",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// No temporary collection is left in the data directory.
    fn assert_no_temp(&self) {
        let left: Vec<_> = std::fs::read_dir(self.data_dir())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(super::package::TEMP_PREFIX)
            })
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

/// A committed fixture.
fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/interchange/anki/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

/// The count of `reason` in `skipped`, 0 if absent.
fn count(skipped: &[Skipped], reason: Skip) -> usize {
    skipped
        .iter()
        .find(|s| s.reason == reason)
        .map_or(0, |s| s.count)
}

/// The examples of `reason`.
fn examples(skipped: &[Skipped], reason: Skip) -> Vec<String> {
    skipped
        .iter()
        .find(|s| s.reason == reason)
        .map(|s| s.examples.clone())
        .unwrap_or_default()
}
