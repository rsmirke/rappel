//! Hostile files: random damage to a valid archive, zip tricks, and
//! random or malformed JSON inside a valid archive. Whatever the file,
//! planning returns (never panics, never hangs), and a refusal never
//! quotes the file's text.

use std::sync::OnceLock;

use super::*;
use crate::test_support::within;

/// Text planted in every card side and deck name of [`base`]: no message
/// may contain it.
const MARKER: &str = "MARKER7Q";

/// Bound on planning one hostile file of the sample's size, in a debug
/// test build (ADR 0025).
const PLAN_LIMIT: Duration = Duration::from_secs(5);

/// The sample database's export, with [`MARKER`] in every text, as an
/// archive with its JSON stored (not compressed), so byte damage lands in
/// readable places; and the summary planning it gives.
fn base() -> &'static (Vec<u8>, ImportSummary) {
    static BASE: OnceLock<(Vec<u8>, ImportSummary)> = OnceLock::new();
    BASE.get_or_init(|| {
        let (env, _, _) = sample_env();
        let mut doc = json_of(&env.export());
        for deck in doc["decks"].as_array_mut().unwrap() {
            deck["name"] = json!(format!("{MARKER} deck {}", deck["id"]));
        }
        for card in doc["cards"].as_array_mut().unwrap() {
            card["front_md"] = json!(format!("{MARKER} front"));
            card["back_md"] = json!(format!("{MARKER} back"));
        }
        let bytes = stored_archive(&serde_json::to_vec_pretty(&doc).unwrap());
        let summary = plan(&bytes).unwrap().summary().clone();
        (bytes, summary)
    })
}

/// `cards.json` stored without compression, then `media/`.
fn stored_archive(json: &[u8]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
    zip.start_file(JSON_ENTRY, stored).unwrap();
    zip.write_all(json).unwrap();
    zip.add_directory("media/", SimpleFileOptions::DEFAULT)
        .unwrap();
    zip.finish().unwrap().into_inner()
}

/// Plans `bytes` against an empty in-memory store, on a small stack and
/// within [`PLAN_LIMIT`].
fn plan(bytes: &[u8]) -> crate::Result<ImportPlan> {
    let bytes = bytes.to_vec();
    within(PLAN_LIMIT, "planning a hostile file", move || {
        Store::in_memory().plan_import_bytes(&bytes, &Limits::DEFAULT)
    })
}

/// Plans a damaged copy of the base archive: either it is refused without
/// quoting the file, or (damage the format ignores, such as a timestamp)
/// it plans exactly as the undamaged file.
fn check_damaged(bytes: &[u8]) -> Result<(), TestCaseError> {
    match plan(bytes) {
        Ok(plan) => prop_assert_eq!(plan.summary(), &base().1),
        Err(err) => {
            let message = err.to_string();
            prop_assert!(!message.contains(MARKER), "{}", message);
            prop_assert!(
                matches!(err, Error::ImportRefused(_)),
                "not a refusal: {}",
                message
            );
        }
    }
    Ok(())
}

/// The offsets of every `signature` in `bytes`.
fn offsets_of(bytes: &[u8], signature: [u8; 4]) -> Vec<usize> {
    bytes
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == signature)
        .map(|(i, _)| i)
        .collect()
}

const LOCAL: [u8; 4] = *b"PK\x03\x04";
const CENTRAL: [u8; 4] = *b"PK\x01\x02";
const END: [u8; 4] = *b"PK\x05\x06";

/// A header field: which record, its offset in the record, its width.
#[derive(Debug, Clone, Copy)]
struct Field {
    record: [u8; 4],
    offset: usize,
    width: usize,
}

/// Every fixed field of the three record kinds, after the signature:
/// versions, flags (encryption, data descriptor), method, time, CRC,
/// sizes, name / extra / comment lengths, disks, attributes, offsets,
/// entry counts.
fn fields() -> Vec<Field> {
    let f = |record, offset, width| Field {
        record,
        offset,
        width,
    };
    let mut all = Vec::new();
    for (offset, width) in [
        (4, 2),
        (6, 2),
        (8, 2),
        (10, 2),
        (12, 2),
        (14, 4),
        (18, 4),
        (22, 4),
        (26, 2),
        (28, 2),
    ] {
        all.push(f(LOCAL, offset, width));
    }
    for (offset, width) in [
        (4, 2),
        (6, 2),
        (8, 2),
        (10, 2),
        (12, 2),
        (14, 2),
        (16, 4),
        (20, 4),
        (24, 4),
        (28, 2),
        (30, 2),
        (32, 2),
        (34, 2),
        (36, 2),
        (38, 4),
        (42, 4),
    ] {
        all.push(f(CENTRAL, offset, width));
    }
    for (offset, width) in [(4, 2), (6, 2), (8, 2), (10, 2), (12, 4), (16, 4), (20, 2)] {
        all.push(f(END, offset, width));
    }
    all
}

/// Values that make headers lie: zero, one, all ones (the zip64 marker),
/// the high bit, and an arbitrary value.
fn field_value() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(0),
        Just(1),
        Just(u32::MAX),
        Just(0xFFFF),
        Just(0x8000_0000),
        Just(0x0001),
        any::<u32>(),
    ]
}

/// Writes `value` (little-endian, cut to the field's width) into the
/// field of the `nth` record of its kind, if there is one.
fn patch(bytes: &mut [u8], field: Field, nth: usize, value: u32) {
    let records = offsets_of(bytes, field.record);
    if records.is_empty() {
        return;
    }
    let at = records[nth % records.len()] + field.offset;
    if at + field.width <= bytes.len() {
        bytes[at..at + field.width].copy_from_slice(&value.to_le_bytes()[..field.width]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn lying_headers_are_refused_or_harmless(
        patches in prop::collection::vec((0..fields().len(), 0..3usize, field_value()), 1..4),
    ) {
        let mut bytes = base().0.clone();
        let fields = fields();
        for (field, nth, value) in patches {
            patch(&mut bytes, fields[field], nth, value);
        }
        check_damaged(&bytes)?;
    }

    #[test]
    fn flipped_bytes_are_refused_or_harmless(
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
    ) {
        let mut bytes = base().0.clone();
        for (index, byte) in flips {
            let i = index.index(bytes.len());
            bytes[i] ^= byte | 1;
        }
        check_damaged(&bytes)?;
    }

    #[test]
    fn cut_or_padded_files_are_refused_or_harmless(
        cut in any::<prop::sample::Index>(),
        insert in prop::collection::vec(any::<u8>(), 0..64),
        at in any::<prop::sample::Index>(),
    ) {
        let base = &base().0;
        let mut bytes = base[..cut.index(base.len() + 1)].to_vec();
        let at = at.index(bytes.len() + 1);
        bytes.splice(at..at, insert);
        check_damaged(&bytes)?;
    }

    #[test]
    fn random_bytes_are_refused(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        prop_assert!(plan(&bytes).is_err());
    }
}

/// A local header pointing at the same data as another one, or a central
/// record pointing at the other entry's local header (overlapping
/// entries, as in "better zip bombs"); entry names
/// that are paths, absolute, or have a NUL or invalid UTF-8.
#[test]
fn zip_tricks_are_refused() {
    let (bytes, _) = base();
    let central = offsets_of(bytes, CENTRAL);
    assert_eq!(central.len(), 2, "cards.json and media/");

    // Both central records point at the first local header.
    let mut overlapping = bytes.clone();
    overlapping[central[1] + 42..central[1] + 46].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        refused(plan(&overlapping)),
        ImportProblem::InvalidArchive("entries overlap or are out of place".to_owned())
    );

    let mut tricks = Vec::new();

    // Renamed entries, the same length as the real names so the offsets
    // stay valid.
    for (from, to) in [
        (&b"cards.json"[..], &b"../ds.json"[..]),
        (b"cards.json", b"/ards.json"),
        (b"cards.json", b"cards.jso\0"),
        (b"cards.json", b"cards.js\xff\xfe"),
        (b"cards.json", b"CARDS.JSON"),
        (b"cards.json", b"cards\\json"),
        (b"media/", b"media\\"),
        (b"media/", b"../me/"),
    ] {
        tricks.push(replace_all(bytes, from, to));
    }

    for trick in tricks {
        let problem = refused(plan(&trick));
        assert!(
            matches!(problem, ImportProblem::UnexpectedEntry(_)),
            "{problem}"
        );
    }
}

/// An entry whose data, by its central record, reaches into the central
/// directory: `media/` first, then `cards.json` claiming a few bytes
/// more than it has.
#[test]
fn an_entry_reaching_into_the_central_directory_is_refused() {
    let json = read_cards_json(&base().0, &Limits::DEFAULT).unwrap();
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.add_directory("media/", SimpleFileOptions::DEFAULT)
        .unwrap();
    let stored = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
    zip.start_file(JSON_ENTRY, stored).unwrap();
    zip.write_all(&json).unwrap();
    let mut bytes = zip.finish().unwrap().into_inner();
    assert_eq!(plan(&bytes).unwrap().summary(), &base().1);

    // The second central record is cards.json's: its compressed size.
    let central = offsets_of(&bytes, CENTRAL);
    let size = u32::try_from(json.len() + 4).unwrap();
    bytes[central[1] + 20..central[1] + 24].copy_from_slice(&size.to_le_bytes());
    assert_eq!(
        refused(plan(&bytes)),
        ImportProblem::InvalidArchive("entries overlap or are out of place".to_owned())
    );
}

/// Any JSON value, nested up to a few levels, with strings that carry
/// the marker.
fn any_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        "[a-z_]{0,12}".prop_map(Value::from),
        any::<String>().prop_map(|s| Value::from(format!("{MARKER}{s}"))),
    ];
    leaf.prop_recursive(6, 256, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::from),
            prop::collection::btree_map(
                prop_oneof![
                    Just("format_version".to_owned()),
                    Just("exported_at".to_owned()),
                    Just("decks".to_owned()),
                    Just("cards".to_owned()),
                    Just("review_events".to_owned()),
                    Just("settings".to_owned()),
                    Just("media".to_owned()),
                    Just("id".to_owned()),
                    Just("name".to_owned()),
                    Just("front_md".to_owned()),
                    "[a-z_]{1,8}",
                ],
                inner,
                0..8,
            )
            .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn random_json_is_refused_without_quoting_it(value in any_json()) {
        let bytes = archive_of(&value);
        let problem = refused(plan(&bytes));
        prop_assert!(!problem.to_string().contains(MARKER), "{}", problem);
    }

    #[test]
    fn damaged_json_is_refused_or_harmless(
        edits in prop::collection::vec(
            (any::<prop::sample::Index>(), prop_oneof![
                Just(b"\0".to_vec()),
                Just(b"\xff".to_vec()),
                Just(b"\xc3".to_vec()),
                Just(b"\xed\xa0\x80".to_vec()), // a UTF-16 surrogate in UTF-8
                Just(b"\"".to_vec()),
                Just(b"\\".to_vec()),
                Just(b"\\u0000".to_vec()),
                Just(b"\\ud800".to_vec()),
                Just(b"[".to_vec()),
                Just(b"{".to_vec()),
                Just(b",".to_vec()),
                Just(b"1e999999".to_vec()),
                Just(b"-0".to_vec()),
                Just(b"99999999999999999999999".to_vec()),
                prop::collection::vec(any::<u8>(), 1..4),
            ]),
            1..4,
        ),
    ) {
        let json = read_cards_json(&base().0, &Limits::DEFAULT).unwrap();
        let mut json = json;
        for (index, bytes) in edits {
            let at = index.index(json.len());
            json.splice(at..=at, bytes);
        }
        let bytes = stored_archive(&json);
        match plan(&bytes) {
            // A harmless edit: inside a string, or a digit for a digit in
            // a time. The rows are the same in number.
            Ok(plan) => {
                let (got, base) = (plan.summary(), &base().1);
                prop_assert_eq!(got.decks.new, base.decks.new);
                prop_assert_eq!(got.cards.new, base.cards.new);
                prop_assert_eq!(got.review_events.new, base.review_events.new);
            }
            Err(err) => {
                let message = err.to_string();
                prop_assert!(!message.contains(MARKER), "{}", message);
            }
        }
    }
}

/// Deep nesting inside `media`, whose items are typed records (ADR 0054;
/// before, they were free JSON values parsed recursively): refused as
/// JSON at any depth, on a small stack, without overflowing it. Nesting
/// far past `serde_json`'s recursion limit is refused by the depth limit
/// of the reader under it.
#[test]
fn deeply_nested_json_is_refused() {
    let text = String::from_utf8(read_cards_json(&base().0, &Limits::DEFAULT).unwrap()).unwrap();
    let empty = r#""media": []"#;
    assert!(text.contains(empty), "{empty}");
    for depth in [3, 200, 10_000, 1_000_000] {
        let value = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let nested = text.replacen(empty, &format!(r#""media": [{value}]"#), 1);
        let problem = refused(plan(&stored_archive(nested.as_bytes())));
        assert!(
            matches!(problem, ImportProblem::Json(_)),
            "{depth}: {problem}"
        );
    }
}

/// Invalid UTF-8 in a string is refused.
#[test]
fn invalid_utf8_is_refused() {
    let text = read_cards_json(&base().0, &Limits::DEFAULT).unwrap();
    let marker = text
        .windows(MARKER.len())
        .position(|w| w == MARKER.as_bytes())
        .unwrap();
    for bad in [&b"\xff"[..], b"\xc3", b"\xed\xa0\x80", b"\xc0\x80"] {
        let mut json = text.clone();
        json.splice(marker..marker, bad.iter().copied());
        let problem = refused(plan(&stored_archive(&json)));
        assert!(
            matches!(problem, ImportProblem::Json(_)),
            "{bad:?}: {problem}"
        );
    }
}

/// The same key twice: refused, never "last one wins" silently.
#[test]
fn duplicate_keys_are_refused() {
    let json = read_cards_json(&base().0, &Limits::DEFAULT).unwrap();
    let text = String::from_utf8(json).unwrap();
    let doubled = text.replacen(
        "\"format_version\": 2,",
        "\"format_version\": 2, \"format_version\": 2,",
        1,
    );
    assert_ne!(doubled, text);
    let problem = refused(plan(&stored_archive(doubled.as_bytes())));
    assert!(matches!(problem, ImportProblem::Json(_)), "{problem}");
}

/// A second end record after the first: the last one is read, and it
/// describes the same entries, so the file plans as before.
#[test]
fn a_second_end_record_is_harmless() {
    let (bytes, summary) = base();
    let end = *offsets_of(bytes, END).last().unwrap();
    let doubled = [bytes.as_slice(), &bytes[end..]].concat();
    assert_eq!(plan(&doubled).unwrap().summary(), summary);
}
