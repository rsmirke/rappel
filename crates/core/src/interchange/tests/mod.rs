use std::fs;
use std::io::{Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use proptest::prelude::*;
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::archive::{JSON_ENTRY, read_cards_json, write_archive};
use super::*;
use crate::Error;
use crate::domain::Grade;
use crate::scheduling::Scheduler;
use crate::storage::{BACKUP_DIR_NAME, Store};
use crate::test_support::{DAY_MS, T0, schedule_table};

/// 2026-10-15T12:34:56.789Z
/// The export time used by every test, so exports compare byte for byte.
const T_EXPORT: i64 = T0 + 100 * DAY_MS;

/// A store in a temporary data directory, so it can be backed up.
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

    fn backups(&self) -> usize {
        fs::read_dir(self.tmp.path().join("data").join(BACKUP_DIR_NAME)).map_or(0, Iterator::count)
    }

    fn export(&self) -> Vec<u8> {
        export(&self.store)
    }

    fn import(&mut self, bytes: &[u8]) -> crate::Result<ImportReport> {
        let plan = self.store.plan_import_bytes(bytes, &Limits::DEFAULT)?;
        self.store.apply_import_at(&plan, T_EXPORT)
    }

    fn plan(&self, bytes: &[u8]) -> crate::Result<ImportPlan> {
        self.store.plan_import_bytes(bytes, &Limits::DEFAULT)
    }
}

fn export(store: &Store) -> Vec<u8> {
    store.export_bytes_at(T_EXPORT).unwrap().0
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(&read_cards_json(bytes, &Limits::DEFAULT).unwrap()).unwrap()
}

fn archive_of(value: &Value) -> Vec<u8> {
    write_archive(&serde_json::to_vec(value).unwrap()).unwrap()
}

/// `bytes` with every `from` replaced by `to`, the same length; `from`
/// must occur.
fn replace_all(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut n = 0;
    while i + from.len() <= out.len() {
        if &out[i..i + from.len()] == from {
            out[i..i + from.len()].copy_from_slice(to);
            n += 1;
        }
        i += 1;
    }
    assert!(n > 0);
    out
}

/// A small but varied database: a renamed deck, a deleted deck, edited,
/// deleted and undeleted cards, reviews on live and deleted cards, a
/// reset and an undo, tags (one on two cards, one on a deleted card, one
/// unused). Returns the store and the ids of the live deck and its first
/// card. 2 decks, 4 cards, 3 tags, 3 card tags, 8 log entries.
fn sample(store: &mut Store) -> (String, String) {
    let maths = store.create_deck("Maths").unwrap().id;
    store.rename_deck(&maths, "Mathématiques").unwrap();
    let physics = store.create_deck("Physics").unwrap().id;
    let q1 = store.add_card(&maths, "2 + 2 ?", "4").unwrap().id;
    let q2 = store
        .add_card(&maths, "√2 ?", "≈ 1.414 \"quoted\"\n")
        .unwrap()
        .id;
    let q3 = store.add_card(&maths, "π ?", "3.14").unwrap().id;
    let p1 = store
        .add_card(&physics, "c ?", "299 792 458 m/s")
        .unwrap()
        .id;
    store.edit_card(&q2, "√2 ≈ ?", "1.414").unwrap();
    let mut t = T0;
    for (card, grade) in [
        (&q1, Grade::Fail),
        (&q1, Grade::Pass),
        (&q2, Grade::Pass),
        (&p1, Grade::Pass),
        (&q3, Grade::Pass),
    ] {
        store
            .record_review(card, grade, Duration::from_millis(1_500), t)
            .unwrap();
        t += DAY_MS;
    }
    // q3: reset, reviewed again, and that review undone.
    store.reset_card(&q3, t).unwrap();
    let again = store
        .record_review(&q3, Grade::Pass, Duration::from_millis(900), t + 1)
        .unwrap();
    store.undo(&q3, &again.id, t + 2).unwrap();
    store.add_tag(&q1, "Algebra").unwrap();
    store.add_tag(&q2, "algebra").unwrap();
    store.add_tag(&p1, "Light").unwrap();
    let unused = store.add_tag(&q3, "Unused").unwrap();
    store.remove_tag(&q3, &unused.id).unwrap();
    store.delete_card(&q3).unwrap();
    store.undelete_card(&q3).unwrap();
    store.delete_card(&q2).unwrap();
    store.delete_deck(&physics).unwrap();
    (maths, q1)
}

fn sample_env() -> (Env, String, String) {
    let mut env = Env::new();
    let (deck, card) = sample(&mut env.store);
    (env, deck, card)
}

fn refused(result: crate::Result<ImportPlan>) -> ImportProblem {
    match result {
        Err(Error::ImportRefused(problem)) => problem,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A hand-written file with fixed ids pins the format: importing it into
/// an empty database and exporting again gives exactly this text.
const FIXTURE: &str = r#"{
  "format_version": 2,
  "exported_at": 1800707696789,
  "decks": [
    {
      "id": "01920000-0000-7000-8000-000000000001",
      "name": "Maths",
      "created_at": 1792067696789,
      "updated_at": 1792067696789,
      "deleted_at": null
    },
    {
      "id": "01920000-0000-7000-8000-000000000002",
      "name": "Old",
      "created_at": 1792067696789,
      "updated_at": 1792067696789,
      "deleted_at": 1792067700000
    }
  ],
  "cards": [
    {
      "id": "01920000-0000-7000-8000-000000000003",
      "deck_id": "01920000-0000-7000-8000-000000000001",
      "front_md": "2 + 2 ?",
      "back_md": "4",
      "created_at": 1792067696789,
      "updated_at": 1792067696789,
      "suspended": false,
      "deleted_at": null
    },
    {
      "id": "01920000-0000-7000-8000-000000000004",
      "deck_id": "01920000-0000-7000-8000-000000000002",
      "front_md": "gone",
      "back_md": "",
      "created_at": 1792067696789,
      "updated_at": 1792067696789,
      "suspended": true,
      "deleted_at": 1792067700000
    }
  ],
  "tags": [
    {
      "id": "01920000-0000-7000-8000-000000000009",
      "name": "Algebra"
    },
    {
      "id": "01920000-0000-7000-8000-00000000000a",
      "name": "Unused"
    }
  ],
  "card_tags": [
    {
      "card_id": "01920000-0000-7000-8000-000000000003",
      "tag_id": "01920000-0000-7000-8000-000000000009"
    },
    {
      "card_id": "01920000-0000-7000-8000-000000000004",
      "tag_id": "01920000-0000-7000-8000-000000000009"
    }
  ],
  "review_events": [
    {
      "id": "01920000-0000-7000-8000-000000000005",
      "card_id": "01920000-0000-7000-8000-000000000003",
      "kind": "review",
      "reviewed_at": 1792067696789,
      "grade": "pass",
      "duration_ms": 1200,
      "undoes": null
    },
    {
      "id": "01920000-0000-7000-8000-000000000006",
      "card_id": "01920000-0000-7000-8000-000000000004",
      "kind": "review",
      "reviewed_at": 1792067696789,
      "grade": "fail",
      "duration_ms": 0,
      "undoes": null
    },
    {
      "id": "01920000-0000-7000-8000-000000000007",
      "card_id": "01920000-0000-7000-8000-000000000003",
      "kind": "reset",
      "reviewed_at": 1792067700000,
      "grade": null,
      "duration_ms": 0,
      "undoes": null
    },
    {
      "id": "01920000-0000-7000-8000-000000000008",
      "card_id": "01920000-0000-7000-8000-000000000003",
      "kind": "undo",
      "reviewed_at": 1792067700001,
      "grade": null,
      "duration_ms": 0,
      "undoes": "01920000-0000-7000-8000-000000000007"
    }
  ],
  "settings": {
    "desired_retention": 0.85,
    "max_new_per_day": 10,
    "day_start_hour": 5
  },
  "media": []
}
"#;

/// A minimal file of format version 1, as early test versions wrote it.
const FIXTURE_V1: &str = r#"{
  "format_version": 1,
  "exported_at": 1800707696789,
  "decks": [],
  "cards": [],
  "review_events": [],
  "settings": {},
  "media": []
}
"#;

/// A target holding the sample's export, and the source with one more
/// card, so a refused import would have had something to add.
fn diverged() -> (Env, Env, String, String) {
    let (mut source, deck, card) = sample_env();
    let mut target = Env::new();
    target.import(&source.export()).unwrap();
    source.store.add_card(&deck, "new", "card").unwrap();
    (source, target, deck, card)
}

mod archive_reader;
mod concurrent;
mod conflicts;
mod format;
mod fuzz;
mod large;
mod limits;
mod log_and_tags;
mod media;
mod oracle;
mod property;
mod refused;
mod revive;
mod round_trip;
mod settings;
mod streaming;
mod suspended;
mod zip_attacks;
