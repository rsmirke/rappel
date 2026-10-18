//! The streaming reader (ADR 0053): for any file, exactly the summary or
//! exactly the refusal that reading the whole document gave (the old
//! reader, kept in [`super::oracle`]); a file in another key order; the
//! file read again by the apply, and refused if it changed; hostile JSON.

use std::fs::{self, File};
use std::time::{Duration, SystemTime};

use proptest::collection::vec;

use super::property::{op, run};
use super::*;
use crate::interchange::notes::COLLIDING_NAMES;
use crate::test_support::within;

/// The top-level keys in the order an export writes them.
const KEYS: [&str; 9] = [
    "format_version",
    "exported_at",
    "decks",
    "cards",
    "tags",
    "card_tags",
    "review_events",
    "settings",
    "media",
];

/// The lists, as damage picks them.
const TABLES: [&str; 5] = ["decks", "cards", "tags", "card_tags", "review_events"];

/// Orders of the top-level keys: an export's, sorted by name (what
/// `serde_json::Value` writes), reversed, and the version last.
fn orders() -> [Vec<&'static str>; 4] {
    let mut sorted = KEYS.to_vec();
    sorted.sort_unstable();
    let mut reversed = KEYS.to_vec();
    reversed.reverse();
    let mut version_last = KEYS[1..].to_vec();
    version_last.push(KEYS[0]);
    [KEYS.to_vec(), sorted, reversed, version_last]
}

/// `doc` as JSON text with its top-level keys in `order` (keys not in it
/// last).
fn text_in_order(doc: &Value, order: &[&str]) -> Vec<u8> {
    let object = doc.as_object().unwrap();
    let mut keys: Vec<&str> = order
        .iter()
        .copied()
        .filter(|k| object.contains_key(*k))
        .collect();
    keys.extend(
        object
            .keys()
            .map(String::as_str)
            .filter(|k| !order.contains(k)),
    );
    let mut out = b"{\n".to_vec();
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(b",\n");
        }
        out.extend(serde_json::to_vec(key).unwrap());
        out.extend_from_slice(b": ");
        out.extend(serde_json::to_vec_pretty(&object[*key]).unwrap());
    }
    out.extend_from_slice(b"\n}\n");
    out
}

/// `cards.json` stored without compression, then `media/`: a file whose
/// length does not depend on how its JSON compresses.
fn stored(json: &[u8]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
    zip.start_file(JSON_ENTRY, options).unwrap();
    zip.write_all(json).unwrap();
    zip.add_directory("media/", SimpleFileOptions::DEFAULT)
        .unwrap();
    zip.finish().unwrap().into_inner()
}

/// Values damage puts into fields: each kind of value, ids of every
/// form, the words of kinds and grades, names that break rules, and
/// settings and media that are refused.
fn palette() -> Vec<Value> {
    vec![
        json!(null),
        json!(""),
        json!(" x"),
        json!("x"),
        json!("a,b"),
        json!("a\u{0}b"),
        json!(-1),
        json!(0),
        json!(1),
        json!(253_402_300_800_000_i64),
        json!("review"),
        json!("reset"),
        json!("undo"),
        json!("pass"),
        json!("fail"),
        json!("Pass"),
        json!(true),
        json!([]),
        json!({}),
        json!(0.5),
        json!("01920000-0000-7000-8000-0000000000ff"),
        json!("01920000-0000-4000-8000-000000000001"),
        json!({"desired_retention": 0.99}),
        json!({"max_new_per_day": 10}),
        json!([{"hash": "x"}]),
        json!(3),
    ]
}

/// Where a damaged field's new value comes from.
#[derive(Debug, Clone)]
enum Pick {
    /// Another field of any row: an id of another table, a name, a time.
    Copy {
        table: usize,
        row: usize,
        field: usize,
    },
    /// A value of [`palette`].
    Fixed(usize),
}

/// One change to a valid document.
#[derive(Debug, Clone)]
enum Damage {
    Set {
        table: usize,
        row: usize,
        field: usize,
        value: Pick,
    },
    Swap {
        table: usize,
        a: usize,
        b: usize,
    },
    Repeat {
        table: usize,
        row: usize,
    },
    Remove {
        table: usize,
        row: usize,
    },
    /// `format_version`, `exported_at`, `settings` or `media`.
    Top {
        key: usize,
        value: Pick,
    },
    DropKey(usize),
    UnknownKey,
}

fn pick() -> impl Strategy<Value = Pick> {
    prop_oneof![
        (0..TABLES.len(), 0..64usize, 0..16usize).prop_map(|(table, row, field)| Pick::Copy {
            table,
            row,
            field
        }),
        (0..palette().len()).prop_map(Pick::Fixed),
    ]
}

fn damage() -> impl Strategy<Value = Damage> {
    let table = || 0..TABLES.len();
    let row = || 0..64usize;
    prop_oneof![
        8 => (table(), row(), 0..16usize, pick())
            .prop_map(|(table, row, field, value)| Damage::Set { table, row, field, value }),
        2 => (table(), row(), row()).prop_map(|(table, a, b)| Damage::Swap { table, a, b }),
        1 => (table(), row()).prop_map(|(table, row)| Damage::Repeat { table, row }),
        1 => (table(), row()).prop_map(|(table, row)| Damage::Remove { table, row }),
        2 => (0..4usize, pick()).prop_map(|(key, value)| Damage::Top { key, value }),
        1 => (0..KEYS.len()).prop_map(Damage::DropKey),
        1 => Just(Damage::UnknownKey),
    ]
}

/// The value `pick` names in `doc`, if there is one.
fn picked(doc: &Value, pick: &Pick) -> Option<Value> {
    match *pick {
        Pick::Fixed(i) => Some(palette()[i].clone()),
        Pick::Copy { table, row, field } => {
            let rows = doc.get(TABLES[table])?.as_array()?;
            let record = rows.get(row % rows.len().max(1))?.as_object()?;
            let key = record.keys().nth(field % record.len().max(1))?;
            Some(record[key].clone())
        }
    }
}

fn apply(doc: &mut Value, damage: &Damage) {
    match *damage {
        Damage::Set {
            table,
            row,
            field,
            ref value,
        } => {
            let Some(value) = picked(doc, value) else {
                return;
            };
            let Some(rows) = doc[TABLES[table]].as_array_mut() else {
                return;
            };
            let n = rows.len();
            if let Some(record) = rows.get_mut(row % n.max(1)).and_then(Value::as_object_mut) {
                let n = record.len().max(1);
                if let Some(key) = record.keys().nth(field % n).cloned() {
                    record.insert(key, value);
                }
            }
        }
        Damage::Swap { table, a, b } => {
            if let Some(rows) = doc[TABLES[table]].as_array_mut()
                && !rows.is_empty()
            {
                let n = rows.len();
                rows.swap(a % n, b % n);
            }
        }
        Damage::Repeat { table, row } => {
            if let Some(rows) = doc[TABLES[table]].as_array_mut()
                && !rows.is_empty()
            {
                let copy = rows[row % rows.len()].clone();
                rows.insert(row % rows.len(), copy);
            }
        }
        Damage::Remove { table, row } => {
            if let Some(rows) = doc[TABLES[table]].as_array_mut()
                && !rows.is_empty()
            {
                let n = rows.len();
                rows.remove(row % n);
            }
        }
        Damage::Top { key, ref value } => {
            if let Some(value) = picked(doc, value) {
                doc[["format_version", "exported_at", "settings", "media"][key]] = value;
            }
        }
        Damage::DropKey(key) => {
            doc.as_object_mut().unwrap().remove(KEYS[key]);
        }
        Damage::UnknownKey => doc["folders"] = json!([]),
    }
}

/// Plans `text` (deflated into an archive) with the streaming reader and
/// with the oracle, against the same database, and checks they agree.
/// Returns the plan when both accept the file.
fn agree(env: &Env, text: &[u8]) -> Result<Option<ImportPlan>, TestCaseError> {
    let new = env.plan(&write_archive(text).unwrap());
    let old = oracle::plan(&env.store.conn, text, &Limits::DEFAULT).unwrap();
    match (new, old) {
        (Ok(plan), Ok(summary)) => {
            prop_assert_eq!(plan.summary(), &summary);
            Ok(Some(plan))
        }
        (Err(Error::ImportRefused(new)), Err(old)) => {
            // serde_json reading from a stream counts the byte it has
            // looked at next, so its place can be one byte later.
            if let (Some((new_kind, new_at)), Some((old_kind, old_at))) =
                (json_place(text, &new), json_place(text, &old))
            {
                prop_assert_eq!(new_kind, old_kind);
                prop_assert!(
                    new_at == old_at || new_at == old_at + 1,
                    "{:?} against {:?}",
                    new,
                    old
                );
            } else {
                prop_assert_eq!(new, old);
            }
            Ok(None)
        }
        (new, old) => {
            let new = new.map(|p| p.summary().clone());
            Err(TestCaseError::fail(format!("new {new:?}, old {old:?}")))
        }
    }
}

/// A JSON problem's kind and the byte of `text` its line and column name.
fn json_place(text: &[u8], problem: &ImportProblem) -> Option<(String, usize)> {
    let ImportProblem::Json(message) = problem else {
        return None;
    };
    let (kind, place) = message.rsplit_once(" at line ")?;
    let (line, column) = place.split_once(", column ")?;
    let (line, column): (usize, usize) = (line.parse().ok()?, column.parse().ok()?);
    let line_start: usize = text
        .split(|&b| b == b'\n')
        .take(line.saturating_sub(1))
        .map(|l| l.len() + 1)
        .sum();
    Some((kind.to_owned(), line_start + column))
}

/// A new environment whose database is a copy of `env`'s.
fn copy_of(env: &Env) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    fs::create_dir(&data).unwrap();
    let file = data.join(crate::storage::DB_FILE_NAME);
    env.store
        .conn
        .execute("VACUUM INTO ?1", [file.to_str().unwrap()])
        .unwrap();
    let store = Store::open(&data).unwrap();
    Env { tmp, store }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(160))]

    /// For random databases, damaged in random ways and written with the
    /// keys in any order: the same summary or the same refusal as the
    /// old reader, into an empty database or into the source database
    /// changed since. An accepted file without conflicts applies, and
    /// applying it again changes nothing.
    #[test]
    fn streaming_gives_what_reading_the_whole_document_gave(
        ops in vec(op(), 0..60),
        later in vec(op(), 0..30),
        damages in vec(damage(), 0..4),
        order in 0..4usize,
        into_source in any::<bool>(),
    ) {
        let mut source = Env::new();
        run(&mut source.store, &ops);
        let mut doc = json_of(&source.export());
        for d in &damages {
            apply(&mut doc, d);
        }
        let text = text_in_order(&doc, &orders()[order]);
        let mut env = if into_source {
            run(&mut source.store, &later);
            source
        } else {
            Env::new()
        };
        if let Some(plan) = agree(&env, &text)?
            && plan.summary().conflicts.is_empty()
        {
            // The old apply on a copy of the same database writes the same.
            let mut copy = copy_of(&env);
            let applied = oracle::apply(&mut copy.store.conn, &text, &Limits::DEFAULT).unwrap();
            prop_assert!(applied);
            env.store.apply_import_at(&plan, T_EXPORT).unwrap();
            prop_assert_eq!(env.export(), copy.export());
            prop_assert_eq!(schedule_table(&env.store), schedule_table(&copy.store));
            let again = env.plan(&write_archive(&text).unwrap()).unwrap();
            prop_assert!(again.summary().is_noop(), "{:?}", again.summary());
        }
    }

    /// Damaged JSON text (bytes changed, removed or added): the same
    /// refusal, at the same line and column, or the same summary.
    #[test]
    fn damaged_text_is_refused_as_reading_the_whole_document_refused_it(
        ops in vec(op(), 0..30),
        edits in vec((any::<prop::sample::Index>(), prop_oneof![
            Just(b"".to_vec()),
            Just(b"\"".to_vec()),
            Just(b"\\".to_vec()),
            Just(b",".to_vec()),
            Just(b"[".to_vec()),
            Just(b"}".to_vec()),
            Just(b"\xff".to_vec()),
            Just(b"1".to_vec()),
            Just(b" ".to_vec()),
            prop::collection::vec(any::<u8>(), 1..3),
        ]), 1..3),
        order in 0..4usize,
    ) {
        let mut source = Env::new();
        run(&mut source.store, &ops);
        let mut text = text_in_order(&json_of(&source.export()), &orders()[order]);
        for (at, bytes) in edits {
            let at = at.index(text.len());
            text.splice(at..=at, bytes);
        }
        agree(&Env::new(), &text)?;
    }
}

/// The sample's export as JSON text with its keys in `order`.
fn sample_text(order: &[&str]) -> (Env, Vec<u8>) {
    let (source, _, _) = sample_env();
    let text = text_in_order(&json_of(&source.export()), order);
    (source, text)
}

#[test]
fn a_file_in_any_key_order_plans_and_applies_as_an_export() {
    let (source, _, _) = sample_env();
    let export = source.export();
    let expected = Env::new().plan(&export).unwrap().summary().clone();
    for order in orders() {
        let text = text_in_order(&json_of(&export), &order);
        let mut env = Env::new();
        let plan = env.plan(&write_archive(&text).unwrap()).unwrap();
        assert_eq!(plan.summary(), &expected, "{order:?}");
        env.store.apply_import_at(&plan, T_EXPORT).unwrap();
        assert_eq!(env.export(), export, "{order:?}");
    }
}

/// An undo before the entry it cancels, and a log in no order at all:
/// the undos are written after every other entry, so the schema's checks
/// hold, and the database ends as the source.
#[test]
fn a_log_in_any_order_imports() {
    let (source, _, _) = sample_env();
    let export = source.export();
    let mut doc = json_of(&export);
    doc["review_events"].as_array_mut().unwrap().reverse();
    for order in [orders()[0].clone(), orders()[1].clone()] {
        let mut env = Env::new();
        let plan = env
            .plan(&write_archive(&text_in_order(&doc, &order)).unwrap())
            .unwrap();
        assert_eq!(plan.summary().review_events.new, 8);
        env.store.apply_import_at(&plan, T_EXPORT).unwrap();
        assert_eq!(env.export(), export);
        assert_eq!(schedule_table(&env.store), schedule_table(&source.store));
    }
}

/// Resets `COLLIDING_NAMES` even when the test fails.
struct Colliding;

impl Colliding {
    fn on() -> Self {
        COLLIDING_NAMES.set(true);
        Self
    }
}

impl Drop for Colliding {
    fn drop(&mut self) {
        COLLIDING_NAMES.set(false);
    }
}

/// Two names whose digests collide are not taken for the same name: the
/// names themselves are compared. A real repeat is still found, at its
/// row, and an apply reads the file the same way.
#[test]
fn colliding_name_digests_are_confirmed_on_the_names() {
    let (source, _, _) = sample_env();
    let export = source.export();
    let expected = Env::new().plan(&export).unwrap().summary().clone();
    let _colliding = Colliding::on();
    let mut env = Env::new();
    let plan = env.plan(&export).unwrap();
    assert_eq!(plan.summary(), &expected);
    env.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(env.export(), export);

    let mut doc = json_of(&export);
    doc["tags"][2]["name"] = json!("ALGEBRA");
    for order in [orders()[0].clone(), orders()[1].clone()] {
        let text = text_in_order(&doc, &order);
        assert_eq!(
            refused(Env::new().plan(&write_archive(&text).unwrap())),
            ImportProblem::InvalidField {
                table: "tags",
                index: 2,
                field: "name",
                reason: FieldProblem::DuplicateTagName
            }
        );
    }
}

/// A repeated id in a file as an export writes it (read in one pass, which
/// then reads it again to find where) is reported at its row, before a
/// later problem.
#[test]
fn a_repeated_id_in_an_export_is_reported_at_its_row() {
    let (source, _, _) = sample_env();
    let mut doc = json_of(&source.export());
    doc["review_events"][4]["id"] = doc["review_events"][1]["id"].clone();
    doc["review_events"][6]["kind"] = json!("again");
    let text = text_in_order(&doc, &KEYS);
    assert_eq!(
        refused(Env::new().plan(&write_archive(&text).unwrap())),
        ImportProblem::InvalidField {
            table: "review_events",
            index: 4,
            field: "id",
            reason: FieldProblem::DuplicateId
        }
    );
}

/// How the file changes between the plan and the apply.
#[derive(Debug, Clone, Copy)]
enum Change {
    /// Another export written over it.
    Rewritten,
    /// Only its time changed.
    Touched,
    /// Other content of the same length, its time put back: only the hash
    /// of `cards.json` tells.
    SameStamp,
    /// Another file moved over it, with the same time.
    Replaced,
}

#[test]
fn a_file_changed_between_plan_and_apply_is_refused_with_nothing_written() {
    let (source, deck, _) = sample_env();
    let mut other = Env::new();
    other.store.create_deck("Other").unwrap();
    for change in [
        Change::Rewritten,
        Change::Touched,
        Change::SameStamp,
        Change::Replaced,
    ] {
        // A target that already has the sample, and one more card in the
        // file, so the apply has something to write.
        let mut env = Env::new();
        env.import(&source.export()).unwrap();
        let mut grown = Env::new();
        grown.import(&source.export()).unwrap();
        grown.store.add_card(&deck, "new", "card").unwrap();
        let grown_json = read_cards_json(&grown.export(), &Limits::DEFAULT).unwrap();
        let path = env.tmp.path().join("file.rappel");
        fs::write(&path, stored(&grown_json)).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let plan = env.store.plan_import(&path).unwrap();
        assert_eq!(plan.summary().cards.new, 1, "{change:?}");

        let set_time = |path: &Path, time: SystemTime| {
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(time)
                .unwrap();
        };
        match change {
            Change::Rewritten => fs::write(&path, other.export()).unwrap(),
            Change::Touched => set_time(&path, modified + Duration::from_secs(10)),
            Change::SameStamp => {
                let same_length = replace_all(&grown_json, b"\"new\"", b"\"old\"");
                fs::write(&path, stored(&same_length)).unwrap();
                set_time(&path, modified);
            }
            Change::Replaced => {
                let elsewhere = env.tmp.path().join("elsewhere.rappel");
                fs::write(&elsewhere, stored(&grown_json)).unwrap();
                set_time(&elsewhere, modified);
                fs::rename(&elsewhere, &path).unwrap();
            }
        }
        let before = env.export();
        let backups = env.backups();
        assert!(
            matches!(
                env.store.apply_import_at(&plan, T_EXPORT),
                Err(Error::ImportFileChanged)
            ),
            "{change:?}"
        );
        assert_eq!(env.export(), before, "{change:?}");
        // Only a change the stamp does not show is found after the backup.
        let backed_up = usize::from(matches!(change, Change::SameStamp));
        assert_eq!(env.backups(), backups + backed_up, "{change:?}");
    }
}

/// A file changed between two reads of one plan read step by step (its
/// keys in another order): refused as changed, whichever read sees it.
#[test]
fn a_file_changed_while_it_is_planned_is_refused() {
    use crate::interchange::import::BEFORE_READ;
    let (source, _, _) = sample_env();
    let doc = json_of(&source.export());
    let text = text_in_order(&doc, &orders()[1]);
    let changed = replace_all(&text, b"2 + 2 ?", b"2 + 3 ?");
    // The structure read, five lists to check, five to compare.
    for at in 1..=11 {
        let env = Env::new();
        let path = env.tmp.path().join("file.rappel");
        fs::write(&path, stored(&text)).unwrap();
        let (hook_path, hook_bytes) = (path.clone(), stored(&changed));
        let mut reads = 0;
        BEFORE_READ.set(Some(Box::new(move || {
            reads += 1;
            if reads == at {
                fs::write(&hook_path, &hook_bytes).unwrap();
            }
        })));
        let result = env.store.plan_import(&path);
        BEFORE_READ.set(None);
        assert!(
            matches!(result, Err(Error::ImportFileChanged)),
            "read {at}: {:?}",
            result.map(|p| p.summary().clone())
        );
        assert_eq!(env.store.decks().unwrap(), []);
    }
}

/// The same file applies, and a file moved away is an IO error with
/// nothing written.
#[test]
fn a_file_planned_from_disk_applies_and_a_removed_one_does_not() {
    let (source, _, _) = sample_env();
    let mut env = Env::new();
    let path = env.tmp.path().join("file.rappel");
    fs::write(&path, source.export()).unwrap();
    let plan = env.store.plan_import(&path).unwrap();
    env.store.apply_import_at(&plan, T_EXPORT).unwrap();
    assert_eq!(env.export(), source.export());

    let mut empty = Env::new();
    let plan = empty.store.plan_import(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        empty.store.apply_import_at(&plan, T_EXPORT),
        Err(Error::Io { .. })
    ));
    assert_eq!(empty.store.decks().unwrap(), []);
    assert_eq!(empty.backups(), 0);
}

/// A string longer than any field can be is refused before it is read
/// whole, wherever it is, without quoting it, and quickly.
#[test]
fn a_string_longer_than_any_field_is_refused_where_it_is() {
    let limit = super::super::stream::string_limit(&Limits::DEFAULT);
    let long = format!("SECRET{}", "x".repeat(usize::try_from(limit).unwrap()));
    let (_, text) = sample_text(&KEYS);
    let text = String::from_utf8(text).unwrap();
    for (from, to) in [
        ("\"2 + 2 ?\"", format!("\"{long}\"")),
        ("\"exported_at\"", format!("\"{long}\": 1, \"exported_at\"")),
        ("\"media\": []", format!("\"media\": [\"{long}\"]")),
    ] {
        assert!(text.contains(from), "{from}");
        let damaged = text.replacen(from, &to, 1);
        let bytes = write_archive(damaged.as_bytes()).unwrap();
        let problem = refused(within(
            Duration::from_secs(10),
            "planning a file with a long string",
            move || Store::in_memory().plan_import_bytes(&bytes, &Limits::DEFAULT),
        ));
        let ImportProblem::Json(message) = &problem else {
            panic!("{problem:?}");
        };
        assert!(message.starts_with("a string longer than"), "{message}");
        assert!(message.contains("line "), "{message}");
        assert!(!message.contains("SECRET"), "{message}");
    }
    // A side at the limit with every character escaped is not too long.
    let mut doc = json_of(&sample_env().0.export());
    doc["cards"][0]["front_md"] = json!("\u{1}".repeat(Limits::DEFAULT.card_text));
    let text = serde_json::to_vec(&doc).unwrap();
    assert!(text.len() as u64 > 6 * Limits::DEFAULT.card_text as u64 - 1);
    assert!(Env::new().plan(&write_archive(&text).unwrap()).is_ok());
}

/// Nesting, repeated keys and unknown fields inside a record, in a file as
/// an export writes it: refused as JSON errors, within the time bound, on
/// a small stack.
#[test]
fn hostile_records_are_refused() {
    let (_, text) = sample_text(&KEYS);
    let text = String::from_utf8(text).unwrap();
    let nested = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
    for (from, to) in [
        (
            "\"front_md\": \"2 + 2 ?\"",
            format!("\"front_md\": {nested}"),
        ),
        (
            "\"front_md\": \"2 + 2 ?\"",
            format!("\"extra\": {nested}, \"front_md\": \"2 + 2 ?\""),
        ),
        (
            "\"name\": \"Algebra\"",
            "\"name\": \"Algebra\", \"name\": \"Algebra\"".to_owned(),
        ),
        (
            "\"settings\": {",
            "\"settings\": {\"desired_retention\": 0.9, \"desired_retention\": 0.9, ".to_owned(),
        ),
        ("\"media\": []", "\"media\": [], \"media\": []".to_owned()),
        ("\"media\": []\n}", "\"media\": []\n} []".to_owned()),
    ] {
        assert!(text.contains(from), "{from}");
        let damaged = text.replacen(from, &to, 1);
        let bytes = write_archive(damaged.as_bytes()).unwrap();
        let problem = refused(within(
            Duration::from_secs(10),
            "planning a hostile record",
            move || Store::in_memory().plan_import_bytes(&bytes, &Limits::DEFAULT),
        ));
        assert!(
            matches!(problem, ImportProblem::Json(_)),
            "{to:.40}: {problem}"
        );
    }
}

/// The plan holds no content of the file, whatever its size.
#[test]
fn a_plan_is_small() {
    assert!(size_of::<ImportPlan>() < 512, "{}", size_of::<ImportPlan>());
}
