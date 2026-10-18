//! Malformed / tampered.

use super::*;

/// The sample's export as JSON, to tamper with.
fn sample_doc() -> Value {
    let (env, _, _) = sample_env();
    json_of(&env.export())
}

/// A change made to a valid document.
type Mutation = fn(&mut Value);

fn plan_doc(doc: &Value) -> ImportProblem {
    refused(Env::new().plan(&archive_of(doc)))
}

fn field(
    table: &'static str,
    index: usize,
    name: &'static str,
    reason: FieldProblem,
) -> ImportProblem {
    ImportProblem::InvalidField {
        table,
        index,
        field: name,
        reason,
    }
}

#[test]
fn files_that_are_not_cards_archives_are_refused() {
    let env = Env::new();
    let good = sample_env().0.export();
    for bytes in [
        Vec::new(),
        b"hello".to_vec(),
        FIXTURE.as_bytes().to_vec(),
        good[..good.len() - 1].to_vec(),
        good[good.len() / 2..].to_vec(),
        [good.as_slice(), b"trailing"].concat(),
    ] {
        assert!(
            matches!(refused(env.plan(&bytes)), ImportProblem::InvalidArchive(_)),
            "{} bytes",
            bytes.len()
        );
    }
}

/// A zip with the given entries: `Some(data)` is a stored file, `None` a
/// directory.
fn zip_with(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        match data {
            Some(data) => {
                let options =
                    SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
                zip.start_file(*name, options).unwrap();
                zip.write_all(data).unwrap();
            }
            None => zip
                .add_directory(*name, SimpleFileOptions::DEFAULT)
                .unwrap(),
        }
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn archive_entries_must_be_exactly_cards_json_and_media() {
    let json = FIXTURE.as_bytes();
    let env = Env::new();
    let check = |entries: &[(&str, Option<&[u8]>)], expected: ImportProblem| {
        assert_eq!(
            refused(env.plan(&zip_with(entries))),
            expected,
            "{entries:?}"
        );
    };
    let unexpected = |s: &str| ImportProblem::UnexpectedEntry(format!("\"{s}\""));

    check(
        &[(JSON_ENTRY, Some(json))],
        ImportProblem::MissingEntry("media/"),
    );
    check(
        &[("media/", None)],
        ImportProblem::MissingEntry("cards.json"),
    );
    check(
        &[("../cards.json", Some(json)), ("media/", None)],
        unexpected("../cards.json"),
    );
    check(
        &[("/cards.json", Some(json)), ("media/", None)],
        unexpected("/cards.json"),
    );
    check(
        &[("Cards.json", Some(json)), ("media/", None)],
        unexpected("Cards.json"),
    );
    check(
        &[
            (JSON_ENTRY, Some(json)),
            ("media/", None),
            ("media/x.png", Some(b"x")),
        ],
        unexpected("media/x.png"),
    );
    check(
        &[(JSON_ENTRY, Some(json)), ("media/", None), ("extra/", None)],
        unexpected("extra/"),
    );
    check(
        &[(JSON_ENTRY, None), ("media/", None)],
        // A directory named "cards.json/" is not the entry.
        unexpected("cards.json/"),
    );
    check(
        &[(JSON_ENTRY, Some(json)), ("media", Some(b""))],
        unexpected("media"),
    );
    // The right entries are accepted, stored or deflated, in any order.
    assert!(
        env.plan(&zip_with(&[("media/", None), (JSON_ENTRY, Some(json))]))
            .is_ok()
    );
    assert!(env.plan(&write_archive(json).unwrap()).is_ok());
}

#[test]
fn a_repeated_entry_name_is_refused() {
    // The writer refuses duplicate names, so rename the second entry in
    // place (local header and central directory).
    let json = FIXTURE.as_bytes();
    let bytes = zip_with(&[
        (JSON_ENTRY, Some(json)),
        ("cards.jsoX", Some(json)),
        ("media/", None),
    ]);
    let patched = replace_all(&bytes, b"cards.jsoX", b"cards.json");
    assert!(matches!(
        refused(Env::new().plan(&patched)),
        ImportProblem::UnexpectedEntry(_)
    ));
}

#[test]
fn a_corrupted_entry_fails_its_crc() {
    let bytes = zip_with(&[(JSON_ENTRY, Some(FIXTURE.as_bytes())), ("media/", None)]);
    // Change one character of the stored JSON; the CRC no longer matches.
    let tampered = replace_all(&bytes, b"\"Maths\"", b"\"Mathz\"");
    assert!(matches!(
        refused(Env::new().plan(&tampered)),
        ImportProblem::InvalidArchive(_)
    ));
}

#[test]
fn size_limits_are_enforced_whatever_the_entry_claims() {
    let env = Env::new();
    let bytes = sample_env().0.export();
    let json_len = read_cards_json(&bytes, &Limits::DEFAULT).unwrap().len() as u64;

    let tight = Limits {
        json: json_len - 1,
        ..Limits::DEFAULT
    };
    assert_eq!(
        refused(env.store.plan_import_bytes(&bytes, &tight)),
        ImportProblem::EntryTooLarge {
            entry: "cards.json",
            limit: json_len - 1
        }
    );
    let exact = Limits {
        json: json_len,
        ..Limits::DEFAULT
    };
    assert!(env.store.plan_import_bytes(&bytes, &exact).is_ok());

    // A highly compressible entry (a zip bomb in miniature).
    let bomb = write_archive(&vec![b' '; 1 << 20]).unwrap();
    assert!(bomb.len() < 10_000);
    let small = Limits {
        json: 1 << 16,
        ..Limits::DEFAULT
    };
    assert!(matches!(
        refused(env.store.plan_import_bytes(&bomb, &small)),
        ImportProblem::EntryTooLarge { .. }
    ));

    // The file itself.
    let path = env.tmp.path().join("big.rappel");
    fs::write(&path, &bytes).unwrap();
    let tiny = Limits {
        archive: bytes.len() as u64 - 1,
        ..Limits::DEFAULT
    };
    assert_eq!(
        refused(env.store.plan_import_with(&path, &tiny)),
        ImportProblem::FileTooLarge {
            limit: bytes.len() as u64 - 1
        }
    );
}

#[test]
fn text_limits_are_enforced() {
    let mut doc = sample_doc();
    let limits = Limits {
        card_text: 8,
        deck_name: 4,
        ..Limits::DEFAULT
    };
    let env = Env::new();
    let plan = |doc: &Value| refused(env.store.plan_import_bytes(&archive_of(doc), &limits));
    assert_eq!(plan(&doc), field("decks", 0, "name", FieldProblem::TooLong));
    doc["decks"][0]["name"] = json!("Abc");
    doc["decks"][1]["name"] = json!("Def");
    doc["cards"][0]["front_md"] = json!("123456789");
    assert_eq!(
        plan(&doc),
        field("cards", 0, "front_md", FieldProblem::TooLong)
    );
    doc["cards"][0]["front_md"] = json!("q");
    doc["cards"][0]["back_md"] = json!("123456789");
    assert_eq!(
        plan(&doc),
        field("cards", 0, "back_md", FieldProblem::TooLong)
    );
}

#[test]
fn a_nul_in_a_name_or_card_side_is_refused() {
    let env = Env::new();
    let plan = |doc: &Value| {
        refused(
            env.store
                .plan_import_bytes(&archive_of(doc), &Limits::DEFAULT),
        )
    };
    for (table, name) in [
        ("decks", "name"),
        ("cards", "front_md"),
        ("cards", "back_md"),
    ] {
        let mut doc = sample_doc();
        doc[table][0][name] = json!("a\u{0}b");
        assert_eq!(plan(&doc), field(table, 0, name, FieldProblem::ContainsNul));
    }
    // Nothing was written.
    assert_eq!(env.store.list_decks(T_EXPORT, 0).unwrap().len(), 0);
}

#[test]
fn unsupported_format_versions_are_refused() {
    for version in [json!(0), json!(3), json!(-1), json!(1_000_000)] {
        let mut doc = sample_doc();
        doc["format_version"] = version.clone();
        assert_eq!(
            plan_doc(&doc),
            ImportProblem::UnsupportedFormatVersion {
                found: version.as_i64().unwrap()
            }
        );
    }
    // Checked before the rest: a newer file with new fields still says
    // "unsupported version".
    let mut doc = sample_doc();
    doc["format_version"] = json!(3);
    doc["folders"] = json!([]);
    assert!(matches!(
        plan_doc(&doc),
        ImportProblem::UnsupportedFormatVersion { found: 3 }
    ));
}

#[test]
fn json_that_does_not_match_the_format_is_refused() {
    let env = Env::new();
    let json_problem = |bytes: &[u8]| {
        matches!(
            refused(env.plan(&write_archive(bytes).unwrap())),
            ImportProblem::Json(_)
        )
    };
    assert!(json_problem(b"not json"));
    assert!(json_problem(b"{}"));
    assert!(json_problem(br#"{"format_version": "1"}"#));
    assert!(json_problem(br#"{"format_version": 2}"#));

    let mutations: [Mutation; 9] = [
        |d| d["unknown"] = json!(1),
        |d| d["decks"][0]["extra"] = json!(true),
        |d| d["cards"][0]["seq"] = json!(1),
        |d| d["review_events"][0]["duration_ms"] = json!(1.5),
        |d| d["review_events"][0]["reviewed_at"] = json!("yesterday"),
        |d| d["cards"][0]["deck_id"] = json!(null),
        |d| _ = d.as_object_mut().unwrap().remove("cards"),
        // Missing, not null: must not read as "not deleted".
        |d| _ = d["decks"][1].as_object_mut().unwrap().remove("deleted_at"),
        |d| _ = d["cards"][1].as_object_mut().unwrap().remove("deleted_at"),
    ];
    for (i, mutate) in mutations.into_iter().enumerate() {
        let mut doc = sample_doc();
        mutate(&mut doc);
        assert!(
            matches!(plan_doc(&doc), ImportProblem::Json(_)),
            "mutation {i}"
        );
    }
}

#[test]
fn ids_must_be_lowercase_uuidv7() {
    let bad_ids = [
        "01920000-0000-4000-8000-000000000001", // v4
        "01920000-0000-8000-8000-000000000001", // v8
        "01920000-0000-7000-c000-000000000001", // variant 110
        "01920000-0000-7000-8000-00000000000A", // uppercase
        "01920000000070008000000000000001",     // no hyphens
        "x",
        "",
    ];
    for bad in bad_ids {
        for (table, index) in [("decks", 1), ("cards", 2), ("review_events", 3)] {
            let mut doc = sample_doc();
            doc[table][index]["id"] = json!(bad);
            assert_eq!(
                plan_doc(&doc),
                field(table, index, "id", FieldProblem::NotUuidV7),
                "{table} {bad}"
            );
        }
    }
}

#[test]
fn ids_dated_after_the_year_9999_are_refused() {
    // The largest id would leave the id generator no room (#12 review):
    // every later deck, card or review would fail with IdsExhausted.
    let past_9999 = format!("{:012x}", 253_402_300_800_000_i64);
    let just_past = format!(
        "{}-{}-7000-8000-000000000001",
        &past_9999[..8],
        &past_9999[8..]
    );
    for bad in ["ffffffff-ffff-7fff-bfff-ffffffffffff", just_past.as_str()] {
        for (table, index) in [("decks", 0), ("cards", 1), ("review_events", 2)] {
            let mut doc = sample_doc();
            doc[table][index]["id"] = json!(bad);
            assert_eq!(
                plan_doc(&doc),
                field(table, index, "id", FieldProblem::TimestampOutOfRange),
                "{table} {bad}"
            );
        }
    }
    // The last millisecond of 9999 is fine.
    let last = format!("{:012x}", 253_402_300_799_999_i64);
    let mut doc = sample_doc();
    doc["decks"][1]["id"] = json!(format!(
        "{}-{}-7fff-bfff-ffffffffffff",
        &last[..8],
        &last[8..]
    ));
    let deck = doc["decks"][1]["id"].clone();
    doc["cards"][3]["deck_id"] = deck;
    assert!(Env::new().plan(&archive_of(&doc)).is_ok());
}

#[test]
fn ids_must_be_unique_within_a_table() {
    for table in ["decks", "cards", "review_events"] {
        let mut doc = sample_doc();
        doc[table][1]["id"] = doc[table][0]["id"].clone();
        assert_eq!(
            plan_doc(&doc),
            field(table, 1, "id", FieldProblem::DuplicateId),
            "{table}"
        );
    }
}

#[test]
fn references_must_resolve_within_the_file() {
    let missing = json!("01920000-0000-7000-8000-0000000000ff");
    let mut doc = sample_doc();
    doc["cards"][1]["deck_id"] = missing.clone();
    assert_eq!(
        plan_doc(&doc),
        field("cards", 1, "deck_id", FieldProblem::UnknownReference)
    );

    let mut doc = sample_doc();
    doc["review_events"][4]["card_id"] = missing;
    assert_eq!(
        plan_doc(&doc),
        field(
            "review_events",
            4,
            "card_id",
            FieldProblem::UnknownReference
        )
    );

    // A card id is not a deck id.
    let mut doc = sample_doc();
    doc["cards"][0]["deck_id"] = doc["cards"][1]["id"].clone();
    assert_eq!(
        plan_doc(&doc),
        field("cards", 0, "deck_id", FieldProblem::UnknownReference)
    );
}

#[test]
fn values_are_checked() {
    let cases: [(Mutation, ImportProblem); 12] = [
        (
            |d| d["review_events"][0]["grade"] = json!("easy"),
            field("review_events", 0, "grade", FieldProblem::InvalidGrade),
        ),
        (
            |d| d["review_events"][0]["grade"] = json!("Pass"),
            field("review_events", 0, "grade", FieldProblem::InvalidGrade),
        ),
        (
            |d| d["review_events"][1]["duration_ms"] = json!(-1),
            field("review_events", 1, "duration_ms", FieldProblem::Negative),
        ),
        (
            |d| d["review_events"][2]["reviewed_at"] = json!(-1),
            field(
                "review_events",
                2,
                "reviewed_at",
                FieldProblem::TimestampOutOfRange,
            ),
        ),
        (
            |d| d["cards"][0]["updated_at"] = json!(253_402_300_800_000_i64),
            field("cards", 0, "updated_at", FieldProblem::TimestampOutOfRange),
        ),
        (
            |d| d["decks"][1]["deleted_at"] = json!(i64::MIN),
            field("decks", 1, "deleted_at", FieldProblem::TimestampOutOfRange),
        ),
        (
            |d| d["exported_at"] = json!(-5),
            ImportProblem::InvalidTopLevel {
                field: "exported_at",
                reason: FieldProblem::TimestampOutOfRange,
            },
        ),
        (
            |d| d["decks"][0]["name"] = json!("  "),
            field("decks", 0, "name", FieldProblem::Empty),
        ),
        (
            |d| d["decks"][0]["name"] = json!(" Maths"),
            field("decks", 0, "name", FieldProblem::NotTrimmed),
        ),
        (
            |d| d["cards"][3]["front_md"] = json!("\n"),
            field("cards", 3, "front_md", FieldProblem::Empty),
        ),
        (
            |d| d["settings"] = json!({"desired_retention": 0.99}),
            ImportProblem::InvalidTopLevel {
                field: "settings.desired_retention",
                reason: FieldProblem::OutOfRange,
            },
        ),
        (
            |d| {
                d["media"] = json!([{
                    "hash": "f".repeat(64),
                    "mime": "image/png",
                    "bytes": 3,
                    "created_at": 0
                }]);
            },
            field("media", 0, "hash", FieldProblem::NoMediaFile),
        ),
    ];
    for (i, (mutate, expected)) in cases.into_iter().enumerate() {
        let mut doc = sample_doc();
        mutate(&mut doc);
        assert_eq!(plan_doc(&doc), expected, "case {i}");
    }
}

#[test]
fn states_the_app_never_produces_are_refused() {
    // The sample's decks: [0] live, [1] deleted. Cards [0..3) are in [0].
    let mut doc = sample_doc();
    let deleted_deck = doc["decks"][1]["id"].clone();
    doc["cards"][0]["deck_id"] = deleted_deck;
    assert_eq!(
        plan_doc(&doc),
        field(
            "cards",
            0,
            "deleted_at",
            FieldProblem::LiveCardInDeletedDeck
        )
    );

    let mut doc = sample_doc();
    doc["decks"][1]["deleted_at"] = json!(null);
    doc["decks"][1]["name"] = json!("MATHÉMATIQUES");
    assert_eq!(
        plan_doc(&doc),
        field("decks", 1, "name", FieldProblem::DuplicateDeckName)
    );
}

#[test]
fn a_refused_file_writes_nothing() {
    let (mut env, _, _) = sample_env();
    let before = env.export();
    let backups = env.backups();
    let mut doc = json_of(&before);
    doc["review_events"][0]["grade"] = json!("again");
    assert!(env.import(&archive_of(&doc)).is_err());
    assert_eq!(env.export(), before);
    assert_eq!(env.backups(), backups);
}

#[test]
fn json_errors_give_a_place_not_the_text() {
    let mut doc = sample_doc();
    doc["review_events"][0]["duration_ms"] = json!(format!("SECRET{}", "x".repeat(100_000)));
    let problem = plan_doc(&doc);
    let ImportProblem::Json(message) = &problem else {
        panic!("{problem:?}");
    };
    assert!(!message.contains("SECRET"), "{message}");
    assert!(message.len() < 100, "{message}");
    assert!(message.contains("line "), "{message}");

    let mut doc = sample_doc();
    doc["cards"][0]["SECRET field"] = json!(1);
    assert!(!plan_doc(&doc).to_string().contains("SECRET"));
}

#[test]
fn an_archive_comment_is_refused() {
    let mut bytes = write_archive(FIXTURE.as_bytes()).unwrap();
    // Set the end record's comment length and append the comment.
    let n = bytes.len();
    bytes[n - 2..].copy_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(b"abc");
    assert!(matches!(
        refused(Env::new().plan(&bytes)),
        ImportProblem::InvalidArchive(_)
    ));
}

#[test]
fn problems_never_quote_card_text() {
    let mut doc = sample_doc();
    doc["review_events"][0]["grade"] = json!("SECRET");
    doc["cards"][0]["front_md"] = json!("SECRET");
    let message = Error::ImportRefused(plan_doc(&doc)).to_string();
    assert!(!message.contains("SECRET"), "{message}");
    assert_eq!(
        message,
        "cannot import this file: review_events[0].grade: not a grade"
    );
}

#[test]
fn a_plan_debug_prints_only_the_summary() {
    let (env, _, _) = sample_env();
    let plan = Env::new().plan(&env.export()).unwrap();
    let shown = format!("{plan:?}");
    assert!(shown.contains("summary"), "{shown}");
    assert!(!shown.contains("2 + 2"), "{shown}");
}

/// Only a regular file is read. A FIFO would block the import forever (on
/// open, waiting for a writer), and a device such as `/dev/zero` is
/// endless; a directory cannot be read at all.
#[test]
fn only_regular_files_are_imported() {
    let env = Env::new();
    let plan = |path: PathBuf| {
        let store = Store::open_concurrent(&env.tmp.path().join("data")).unwrap();
        crate::test_support::within(
            Duration::from_secs(5),
            "planning a special file",
            move || store.plan_import(&path),
        )
    };
    let mut special = vec![env.tmp.path().to_path_buf(), PathBuf::from("/dev/null")];
    if Path::new("/dev/zero").exists() {
        special.push(PathBuf::from("/dev/zero"));
    }
    let fifo = env.tmp.path().join("pipe.rappel");
    match std::process::Command::new("mkfifo").arg(&fifo).status() {
        Ok(status) if status.success() => special.push(fifo),
        _ => eprintln!("mkfifo is not available: the FIFO case is skipped"),
    }
    for path in special {
        assert_eq!(
            refused(plan(path.clone())),
            ImportProblem::NotAFile,
            "{}",
            path.display()
        );
    }
    // A symbolic link to a regular file is followed.
    let file = env.tmp.path().join("real.rappel");
    fs::write(&file, sample_env().0.export()).unwrap();
    let link = env.tmp.path().join("link.rappel");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(plan(link).is_ok());
}
