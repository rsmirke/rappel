//! Archives built to make a zip reader work or hold memory in proportion
//! to what they declare (ADR 0053, after the review of slice 1): central
//! records with long comments behind a lying directory size, end records
//! repeated many times after a damaged directory, and a valid zip64 end
//! record hidden behind a classic one. Each is refused quickly, by
//! reading only what the last end record declares.

use std::time::Duration;

use super::*;
use crate::test_support::within;

/// A local header and its data for `name` (stored).
fn local(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = b"PK\x03\x04".to_vec();
    out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0x21, 0, 0]);
    out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    out
}

/// A central record for `name`, with a comment of `comment` bytes.
fn central(name: &[u8], comment: usize) -> Vec<u8> {
    let mut out = b"PK\x01\x02".to_vec();
    out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
    out.extend_from_slice(&crc32fast::hash(b"{}").to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&u16::try_from(comment).unwrap().to_le_bytes());
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(name);
    out.extend(std::iter::repeat_n(0xb0, comment));
    out
}

/// A classic end record.
fn end(entries: u16, size: u32, offset: u32) -> Vec<u8> {
    let mut out = b"PK\x05\x06\0\0\0\0".to_vec();
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Central records with 64 KiB comments, behind an end record that says
/// the directory is 1,000 bytes.
pub(super) fn long_comments(records: usize) -> Vec<u8> {
    let mut bytes = local(b"cards.json", b"{}");
    let offset = u32::try_from(bytes.len()).unwrap();
    for i in 0..records {
        bytes.extend(central(format!("x{i:07}").as_bytes(), 65_535));
    }
    bytes.extend(end(u16::try_from(records).unwrap(), 1_000, offset));
    bytes
}

/// A directory damaged after 65,533 records, then `ends` end records that
/// all name it: a reader that tries each end record in turn reads the
/// directory `ends` times.
pub(super) fn many_end_records(ends: usize) -> Vec<u8> {
    let mut bytes = local(b"cards.json", b"{}");
    let offset = u32::try_from(bytes.len()).unwrap();
    let record = central(b"", 0);
    for _ in 0..65_533 {
        bytes.extend_from_slice(&record);
    }
    bytes.extend_from_slice(b"XXXX");
    bytes.extend_from_slice(&[0; 42]);
    for _ in 0..ends {
        bytes.extend(end(65_534, 1_000, offset));
    }
    bytes
}

/// A valid zip64 end record declaring `records` entries, followed by a
/// classic end record that declares 2 entries in a directory that is not
/// there: a reader that falls back from the last end record to an earlier
/// one lists all of them.
pub(super) fn hidden_zip64(records: u64) -> Vec<u8> {
    let mut bytes = local(b"cards.json", b"{}");
    let offset = bytes.len() as u64;
    let record = central(b"", 0);
    for _ in 0..records {
        bytes.extend_from_slice(&record);
    }
    let size = bytes.len() as u64 - offset;
    let zip64_at = bytes.len() as u64;
    bytes.extend_from_slice(b"PK\x06\x06");
    bytes.extend_from_slice(&44u64.to_le_bytes());
    bytes.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for value in [records, records, size, offset] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(b"PK\x06\x07\0\0\0\0");
    bytes.extend_from_slice(&zip64_at.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend(end(u16::MAX, u32::MAX, u32::MAX));
    let classic = u32::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(&[0; 64]);
    bytes.extend(end(2, 100, classic));
    bytes
}

/// Plans `bytes` on a small stack within a time bound.
fn plan_quickly(bytes: Vec<u8>, what: &'static str) -> ImportProblem {
    refused(within(Duration::from_secs(5), what, move || {
        Store::in_memory().plan_import_bytes(&bytes, &Limits::DEFAULT)
    }))
}

#[test]
fn long_comments_behind_a_lying_size_are_refused() {
    let problem = plan_quickly(long_comments(200), "long comments");
    assert!(
        matches!(&problem, ImportProblem::InvalidArchive(m) if m.contains("cut short")),
        "{problem}"
    );
}

#[test]
fn many_end_records_are_not_tried_one_by_one() {
    let problem = plan_quickly(many_end_records(400), "many end records");
    assert!(
        matches!(problem, ImportProblem::UnexpectedEntry(_)),
        "{problem}"
    );
}

#[test]
fn an_earlier_zip64_end_record_is_not_read() {
    let problem = plan_quickly(hidden_zip64(100_000), "a hidden zip64 end record");
    assert_eq!(
        problem,
        ImportProblem::InvalidArchive("entries overlap or are out of place".to_owned())
    );
}

/// The records must fill the size the end record declares, no more.
#[test]
fn a_directory_larger_than_its_records_is_refused() {
    let (env, _, _) = sample_env();
    let mut bytes = env.export();
    // Two bytes after the records, inside the declared size.
    let n = bytes.len();
    bytes.splice(n - 22..n - 22, [0, 0]);
    let n = bytes.len();
    let size = u32::from_le_bytes(bytes[n - 10..n - 6].try_into().unwrap());
    bytes[n - 10..n - 6].copy_from_slice(&(size + 2).to_le_bytes());
    assert_eq!(
        refused(Env::new().plan(&bytes)),
        ImportProblem::InvalidArchive("the list of entries holds more than it declares".to_owned())
    );
}

/// A small valid `cards.json` with one deck.
const ONE_DECK: &[u8] = br#"{"format_version":2,"exported_at":1792067696789,"decks":[{"id":"01920000-0000-7000-8000-000000000000","name":"Deck","created_at":1792067696789,"updated_at":1792067696789,"deleted_at":null}],"cards":[],"tags":[],"card_tags":[],"review_events":[],"settings":{},"media":[]}"#;

/// How [`hand_made`] writes `cards.json`'s central record.
#[derive(Clone, Copy)]
struct Made {
    /// Its sizes and offset in a zip64 extra field, the 32-bit fields
    /// holding the marker.
    zip64: bool,
    /// The size it declares, if not the real one.
    size: Option<u32>,
}

/// An archive written by hand: `cards.json` deflated, then `media/`, then
/// their central records as `made` says.
fn hand_made(made: Made) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(ONE_DECK).unwrap();
    let data = encoder.finish().unwrap();
    let crc = crc32fast::hash(ONE_DECK);
    let size = made
        .size
        .unwrap_or_else(|| u32::try_from(ONE_DECK.len()).unwrap());
    let compressed = u32::try_from(data.len()).unwrap();
    let header = |name: &[u8], method: u16, crc: u32, compressed: u32, size: u32| {
        let mut out = b"PK\x03\x04".to_vec();
        out.extend_from_slice(&[20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0]);
        for value in [crc, compressed, size] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out
    };
    let mut bytes = header(b"cards.json", 8, crc, compressed, size);
    bytes.extend_from_slice(&data);
    let media_at = u32::try_from(bytes.len()).unwrap();
    bytes.extend(header(b"media/", 0, 0, 0, 0));
    let directory_at = u32::try_from(bytes.len()).unwrap();

    let record = |name: &[u8], method: u16, crc: u32, fields: [u32; 3], extra: &[u8]| {
        let mut out = b"PK\x01\x02".to_vec();
        out.extend_from_slice(&[45, 0, 45, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0]);
        for value in [crc, fields[0], fields[1]] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&u16::try_from(extra.len()).unwrap().to_le_bytes());
        // Comment length, disk, internal and external attributes.
        out.extend_from_slice(&[0; 10]);
        out.extend_from_slice(&fields[2].to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(extra);
        out
    };
    let mut directory = if made.zip64 {
        let mut extra = 1u16.to_le_bytes().to_vec();
        extra.extend_from_slice(&24u16.to_le_bytes());
        for value in [u64::from(size), u64::from(compressed), 0] {
            extra.extend_from_slice(&value.to_le_bytes());
        }
        record(b"cards.json", 8, crc, [u32::MAX; 3], &extra)
    } else {
        record(b"cards.json", 8, crc, [compressed, size, 0], &[])
    };
    directory.extend(record(b"media/", 0, 0, [0, 0, media_at], &[]));
    let directory_size = u32::try_from(directory.len()).unwrap();
    bytes.extend(directory);
    bytes.extend(end(2, directory_size, directory_at));
    bytes
}

/// Sizes and offset given in a zip64 extra field are read from there.
#[test]
fn sizes_in_a_zip64_extra_field_are_read() {
    let plain = hand_made(Made {
        zip64: false,
        size: None,
    });
    let zip64 = hand_made(Made {
        zip64: true,
        size: None,
    });
    let expected = Env::new().plan(&plain).unwrap().summary().clone();
    assert_eq!(expected.decks.new, 1);
    assert_eq!(Env::new().plan(&zip64).unwrap().summary(), &expected);
}

/// Data whose size is not the one its central record declares is refused,
/// even with the right CRC.
#[test]
fn data_of_another_size_than_declared_is_refused() {
    let lying = hand_made(Made {
        zip64: false,
        size: Some(u32::try_from(ONE_DECK.len()).unwrap() + 5),
    });
    assert_eq!(
        refused(Env::new().plan(&lying)),
        ImportProblem::InvalidArchive("cards.json is not the size its entry declares".to_owned())
    );
}

/// A central record whose local header is not where it says is refused.
#[test]
fn a_missing_local_header_is_refused() {
    let mut bytes = hand_made(Made {
        zip64: false,
        size: None,
    });
    bytes[..4].copy_from_slice(b"XXXX");
    assert_eq!(
        refused(Env::new().plan(&bytes)),
        ImportProblem::InvalidArchive("an entry's header is missing".to_owned())
    );
}

/// One entry of [`raw_archive`]: what its headers say, and its data.
struct Raw {
    name: Vec<u8>,
    method: u16,
    flags: u16,
    crc: u32,
    /// The size its headers declare, decompressed.
    size: u32,
    /// The bytes in the file (compressed with `method`).
    data: Vec<u8>,
}

/// A stored entry whose headers tell the truth.
fn raw(name: &[u8], data: &[u8]) -> Raw {
    Raw {
        name: name.to_vec(),
        method: 0,
        flags: 0,
        crc: crc32fast::hash(data),
        size: u32::try_from(data.len()).unwrap(),
        data: data.to_vec(),
    }
}

/// An archive written by hand, entry by entry, then the central
/// directory, then the end record.
fn raw_archive(entries: &[Raw]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut offsets = Vec::new();
    let name_len = |e: &Raw| u16::try_from(e.name.len()).unwrap().to_le_bytes();
    for e in entries {
        offsets.push(u32::try_from(bytes.len()).unwrap());
        bytes.extend_from_slice(b"PK\x03\x04\x14\0");
        bytes.extend_from_slice(&e.flags.to_le_bytes());
        bytes.extend_from_slice(&e.method.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0x21, 0]);
        bytes.extend_from_slice(&e.crc.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(e.data.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&e.size.to_le_bytes());
        bytes.extend_from_slice(&name_len(e));
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&e.name);
        bytes.extend_from_slice(&e.data);
    }
    let directory_at = u32::try_from(bytes.len()).unwrap();
    for (e, offset) in entries.iter().zip(offsets) {
        bytes.extend_from_slice(b"PK\x01\x02\x14\0\x14\0");
        bytes.extend_from_slice(&e.flags.to_le_bytes());
        bytes.extend_from_slice(&e.method.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0x21, 0]);
        bytes.extend_from_slice(&e.crc.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(e.data.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&e.size.to_le_bytes());
        bytes.extend_from_slice(&name_len(e));
        bytes.extend_from_slice(&[0; 12]);
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&e.name);
    }
    let size = u32::try_from(bytes.len()).unwrap() - directory_at;
    bytes.extend(end(
        u16::try_from(entries.len()).unwrap(),
        size,
        directory_at,
    ));
    bytes
}

/// `cards.json` with these media records, and the `media/` directory.
fn json_and_dir(records: &[Value]) -> [Raw; 2] {
    let doc = super::media::doc_with(records);
    [
        raw(b"cards.json", &serde_json::to_vec(&doc).unwrap()),
        raw(b"media/", b""),
    ]
}

/// A PNG of `n` and its reference.
fn png(n: u32) -> (crate::media::MediaRef, Vec<u8>) {
    let bytes = crate::test_support::png_bytes(n);
    (
        super::media::media_of(&bytes, crate::media::MediaKind::Png),
        bytes,
    )
}

/// Media entry names with paths, look-alikes and upper-case hex are not
/// media names: each refuses the file as an unexpected entry, from the
/// central directory, before anything is read.
#[test]
fn media_names_beyond_the_one_form_are_refused() {
    let (m, bytes) = png(1);
    let hex = m.hex();
    let names = [
        format!("media/../{hex}.png"),
        format!("media/../../etc/{hex}.png"),
        format!("media/x/{hex}.png"),
        format!("media//{hex}.png"),
        format!("media/{hex}.png/"),
        format!("media/{hex}.png/x"),
        format!("media/./{hex}.png"),
        format!("media\\{hex}.png"),
        format!("/media/{hex}.png"),
        format!("media/{}.png", hex.to_uppercase()),
        format!("media/{}.png", &hex[1..]),
        format!("media/{hex}0.png"),
        format!("media/{hex}.PNG"),
        format!("media/{hex}.svg"),
        format!("media/{hex}.jpeg"),
        format!("media/{hex}.png\0"),
        format!("media/{hex}"),
        format!("Media/{hex}.png"),
        format!("x/media/{hex}.png"),
    ];
    for name in names {
        let [json, dir] = json_and_dir(&[super::media::record(&m, &bytes, T0)]);
        let archive = raw_archive(&[json, dir, raw(name.as_bytes(), &bytes)]);
        let problem = plan_quickly(archive, "a media name");
        assert!(
            matches!(&problem, ImportProblem::UnexpectedEntry(_)),
            "{name:?}: {problem}"
        );
    }
}

/// The same media name twice, or one hash under two extensions: one file,
/// one entry.
#[test]
fn a_media_file_twice_is_refused() {
    let (m, bytes) = png(2);
    let name = format!("media/{}", m.file_name());
    let other_ext = format!("media/{}.gif", m.hex());
    for second in [name.clone(), other_ext] {
        let [json, dir] = json_and_dir(&[super::media::record(&m, &bytes, T0)]);
        let archive = raw_archive(&[
            json,
            dir,
            raw(name.as_bytes(), &bytes),
            raw(second.as_bytes(), &bytes),
        ]);
        assert_eq!(
            plan_quickly(archive, "a media file twice"),
            ImportProblem::UnexpectedEntry("(a name used twice)".to_owned()),
            "{second}"
        );
    }
}

/// A media entry that is empty, encrypted, compressed another way or
/// declares more than its kind's limit is refused from its central
/// record.
#[test]
fn media_entries_stored_wrongly_are_refused() {
    let (m, bytes) = png(3);
    let name = format!("media/{}", m.file_name());
    let record = || vec![super::media::record(&m, &bytes, T0)];
    let mut empty = raw(name.as_bytes(), b"");
    empty.size = 0;
    let mut encrypted = raw(name.as_bytes(), &bytes);
    encrypted.flags = 1;
    let mut bzip = raw(name.as_bytes(), &bytes);
    bzip.method = 12;
    let mut huge = raw(name.as_bytes(), &bytes);
    huge.size = 10_000_001;
    let cases = [
        (
            empty,
            ImportProblem::InvalidArchive("a media file is empty".to_owned()),
        ),
        (encrypted, ImportProblem::UnsupportedEntry("a media file")),
        (bzip, ImportProblem::UnsupportedEntry("a media file")),
        (
            huge,
            ImportProblem::EntryTooLarge {
                entry: "a media file",
                limit: 10_000_000,
            },
        ),
    ];
    for (entry, expected) in cases {
        let [json, dir] = json_and_dir(&record());
        let archive = raw_archive(&[json, dir, entry]);
        assert_eq!(plan_quickly(archive, "a media entry"), expected);
    }
}

/// A deflated media entry that declares 64 bytes (and its record says so)
/// but inflates to far more than an image may be: reading stops one byte
/// past the limit, so the bomb costs at most the limit.
#[test]
fn a_media_bomb_stops_at_its_limit() {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    let (m, _) = png(4);
    let mut inflated = crate::test_support::png_bytes(4);
    inflated.resize(12 << 20, 0);
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&inflated).unwrap();
    let data = encoder.finish().unwrap();
    drop(inflated);
    // Declaring 64 bytes, it stops past them (ADR 0063); declaring the
    // limit, one byte past the limit.
    for (declared, expected) in [
        (
            64,
            ImportProblem::InvalidArchive(
                "a media file is not the size its entry declares".to_owned(),
            ),
        ),
        (
            10_000_000,
            ImportProblem::EntryTooLarge {
                entry: "a media file",
                limit: 10_000_000,
            },
        ),
    ] {
        let mut record = super::media::record(&m, b"", T0);
        record["bytes"] = json!(declared);
        let [json, dir] = json_and_dir(&[record]);
        let bomb = Raw {
            name: format!("media/{}", m.file_name()).into_bytes(),
            method: 8,
            flags: 0,
            crc: 0,
            size: declared,
            data: data.clone(),
        };
        let archive = raw_archive(&[json, dir, bomb]);
        assert_eq!(
            plan_quickly(archive, "a media bomb"),
            expected,
            "{declared}"
        );
    }
}

/// A media entry whose name says one hash and whose content has another
/// is refused when its content is read, before anything is written.
#[test]
fn a_media_file_with_the_wrong_hash_is_refused() {
    let (m, _) = png(5);
    let (_, other) = png(6);
    let [json, dir] = json_and_dir(&[super::media::record(&m, &other, T0)]);
    let archive = raw_archive(&[
        json,
        dir,
        raw(format!("media/{}", m.file_name()).as_bytes(), &other),
    ]);
    assert_eq!(
        plan_quickly(archive, "a wrong hash"),
        ImportProblem::InvalidField {
            table: "media",
            index: 0,
            field: "hash",
            reason: FieldProblem::MediaFileDiffers,
        }
    );
}

/// Thousands of media entries: refused quickly when the list of media
/// does not have them (from the first one, in hash order), and planned
/// within a time bound when it does.
#[test]
fn thousands_of_media_entries_are_quick() {
    const N: u32 = 20_000;
    let files: Vec<_> = (0..N).map(png).collect();
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by_key(|(m, _)| *m);
    let entries = |with: &[Value]| {
        let [json, dir] = json_and_dir(with);
        let mut all = vec![json, dir];
        all.extend(
            files
                .iter()
                .map(|(m, b)| raw(format!("media/{}", m.file_name()).as_bytes(), b)),
        );
        all
    };
    let without = zip64_archive(&entries(&[]));
    assert_eq!(
        plan_quickly(without, "media entries without records"),
        ImportProblem::UnexpectedEntry(format!("\"media/{}\"", sorted[0].0.file_name()))
    );
    let records: Vec<Value> = sorted
        .iter()
        .map(|(m, b)| super::media::record(m, b, T0))
        .collect();
    let with = zip64_archive(&entries(&records));
    let plan = within(
        Duration::from_secs(60),
        "planning 20,000 media files",
        move || Store::in_memory().plan_import_bytes(&with, &Limits::DEFAULT),
    )
    .unwrap();
    assert_eq!(plan.summary().media.new, N as usize);
}

/// [`raw_archive`] for more entries than a classic end record counts: the
/// same entries through the zip crate's writer, which writes a zip64 end
/// record past 65,534.
fn zip64_archive(entries: &[Raw]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
    for e in entries {
        let name = String::from_utf8(e.name.clone()).unwrap();
        if name.ends_with('/') {
            zip.add_directory(name, SimpleFileOptions::DEFAULT).unwrap();
        } else {
            zip.start_file(name, stored).unwrap();
            zip.write_all(&e.data).unwrap();
        }
    }
    zip.finish().unwrap().into_inner()
}

// ------------------------------------------------- round 3 (ADR 0063)

/// `cards.json` deflated from more than the JSON limit (300 MiB of
/// spaces in about 1 MiB), declaring a small size or a huge one: the
/// inflating stops one byte past the limit, quickly, whatever is declared.
#[test]
fn a_deflate_bomb_stops_at_the_json_limit() {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    // Fast compression: making the bomb is the slow part in a debug build.
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    let block = vec![b' '; 1 << 20];
    encoder.write_all(b"{\"format_version\":2").unwrap();
    for _ in 0..300 {
        encoder.write_all(&block).unwrap();
    }
    let data = encoder.finish().unwrap();
    assert!(data.len() < 4 << 20, "{}", data.len());
    // Declaring 100 bytes it stops past them, at once (it read all of the
    // JSON limit, 256 MiB, before: 17 s of CPU in a debug build); declaring
    // more than the limit it is refused from its central record.
    for (declared, limit) in [
        (100, Duration::from_secs(2)),
        (u32::MAX - 1, Duration::from_secs(60)),
    ] {
        let bomb = raw_archive(&[
            Raw {
                name: b"cards.json".to_vec(),
                method: 8,
                flags: 0,
                crc: 0,
                size: declared,
                data: data.clone(),
            },
            raw(b"media/", b""),
        ]);
        let problem = refused(within(limit, "a deflate bomb", move || {
            Store::in_memory().plan_import_bytes(&bomb, &Limits::DEFAULT)
        }));
        if declared == 100 {
            assert_eq!(
                problem,
                ImportProblem::InvalidArchive(
                    "cards.json is not the size its entry declares".to_owned()
                )
            );
        } else {
            assert!(
                matches!(problem, ImportProblem::EntryTooLarge { .. }),
                "{problem}"
            );
        }
    }
}

/// zip64 extra fields at the extremes: sizes and offsets of `u64::MAX`,
/// a marker with no extra field, an extra field too short, one zip64
/// field among many others: each refused or read, never an overflow.
#[test]
fn zip64_fields_at_the_extremes_are_refused() {
    let plain = hand_made(Made {
        zip64: true,
        size: None,
    });
    let extra = plain
        .windows(4)
        .position(|w| w == b"\x01\x00\x18\x00")
        .expect("the zip64 extra field");
    for (at, value) in [(4, u64::MAX), (12, u64::MAX), (20, u64::MAX), (20, 1 << 40)] {
        let mut bytes = plain.clone();
        bytes[extra + at..extra + at + 8].copy_from_slice(&value.to_le_bytes());
        let problem = refused(Env::new().plan(&bytes));
        assert!(
            matches!(problem, ImportProblem::InvalidArchive(_)),
            "field {at} = {value}: {problem}"
        );
    }
    // The extra field's own length says 8 where 24 are needed.
    let mut short = plain.clone();
    short[extra + 2..extra + 4].copy_from_slice(&8_u16.to_le_bytes());
    assert_eq!(
        refused(Env::new().plan(&short)),
        ImportProblem::InvalidArchive("an entry's sizes cannot be read".to_owned())
    );
    // Another id where the zip64 field should be.
    let mut other = plain;
    other[extra..extra + 2].copy_from_slice(&0x5455_u16.to_le_bytes());
    assert_eq!(
        refused(Env::new().plan(&other)),
        ImportProblem::InvalidArchive("an entry's sizes cannot be read".to_owned())
    );
}

/// What a local header says beyond where its data starts (its sizes,
/// method, CRC, flags such as a data descriptor, even its name) is never
/// relied on: the central record decides, so a file whose local headers
/// disagree imports as its central records say, or is refused by them.
#[test]
fn local_headers_that_disagree_are_not_relied_on() {
    let plain = hand_made(Made {
        zip64: false,
        size: None,
    });
    let expected = Env::new().plan(&plain).unwrap().summary().clone();
    // cards.json's local header: flags (data descriptor), method, CRC and
    // sizes changed; then its name, the same length.
    let mut lying = plain.clone();
    lying[6..8].copy_from_slice(&8_u16.to_le_bytes());
    lying[8..10].copy_from_slice(&0_u16.to_le_bytes());
    lying[14..26].copy_from_slice(&[0; 12]);
    assert_eq!(Env::new().plan(&lying).unwrap().summary(), &expected);
    let renamed = {
        let mut b = plain.clone();
        b[30..40].copy_from_slice(b"evil.jsonx");
        b
    };
    assert_eq!(Env::new().plan(&renamed).unwrap().summary(), &expected);
    // A local name longer than the bytes left: its data would start past
    // the directory.
    let mut long = plain;
    long[26..28].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        refused(Env::new().plan(&long)),
        ImportProblem::InvalidArchive("entries overlap or are out of place".to_owned())
    );
}

/// Names a zip reader may stumble on: a backslash path, a NUL, a name of
/// 65,535 bytes, 10,000 empty `media/` entries, many empty media files.
#[test]
fn odd_names_and_empty_entries_are_refused_from_the_directory() {
    let (m, bytes) = png(1);
    let [json, _] = json_and_dir(&[super::media::record(&m, &bytes, T0)]);
    let long = vec![b'a'; usize::from(u16::MAX)];
    for name in [
        format!("media\\{}", m.file_name()).into_bytes(),
        format!("media/{}\0", m.file_name()).into_bytes(),
        long,
    ] {
        let archive = raw_archive(&[
            raw(&json.name, &json.data),
            raw(b"media/", b""),
            raw(&name, &bytes),
        ]);
        assert!(
            matches!(
                plan_quickly(archive, "an odd name"),
                ImportProblem::UnexpectedEntry(_)
            ),
            "{:?}",
            String::from_utf8_lossy(&name[..20.min(name.len())])
        );
    }
    let mut dirs = vec![raw(&json.name, &json.data)];
    dirs.extend((0..10_000).map(|_| raw(b"media/", b"")));
    assert_eq!(
        plan_quickly(raw_archive(&dirs), "10,000 media/ entries"),
        ImportProblem::UnexpectedEntry("(a name used twice)".to_owned())
    );
    let mut empty = vec![raw(&json.name, &json.data), raw(b"media/", b"")];
    empty.push(raw(format!("media/{}", m.file_name()).as_bytes(), b""));
    assert_eq!(
        plan_quickly(raw_archive(&empty), "an empty media file"),
        ImportProblem::InvalidArchive("a media file is empty".to_owned())
    );
}

/// A folder for files too large for `/tmp` (a RAM disk): `target/tmp`.
fn disk_dir() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

/// Archives at the 2 GiB limit, as sparse files (no disk space): exactly
/// 2 GiB with a valid archive at its end is planned (bytes before an
/// archive are not looked for); one byte more is refused by its size.
#[test]
fn an_archive_of_exactly_two_gib_is_read_and_one_byte_more_is_not() {
    let dir = disk_dir();
    let env = Env::new();
    let archive = env.export();
    let limit = Limits::DEFAULT.archive;
    for (extra, ok) in [(0, true), (1, false)] {
        let path = dir.path().join(format!("edge-{extra}.rappel"));
        let size = limit + extra;
        let base = size - u64::try_from(archive.len()).unwrap();
        // Offsets in the archive count from the file's start.
        let moved = shifted(&archive, base);
        let file = fs::File::create(&path).unwrap();
        file.set_len(base).unwrap();
        let mut file = file;
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(base)).unwrap();
        file.write_all(&moved).unwrap();
        drop(file);
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        let planned = env.store.plan_import(&path);
        if ok {
            assert!(planned.is_ok(), "{planned:?}");
        } else {
            assert_eq!(refused(planned), ImportProblem::FileTooLarge { limit });
        }
        fs::remove_file(&path).unwrap();
    }
}

/// `archive` with every offset its records hold moved by `base`: its
/// central records' local offsets and the end record's directory offset.
fn shifted(archive: &[u8], base: u64) -> Vec<u8> {
    let mut out = archive.to_vec();
    let add = |field: &mut [u8]| {
        let value = u64::from(u32::from_le_bytes(field[..4].try_into().unwrap())) + base;
        field[..4].copy_from_slice(&u32::try_from(value).unwrap().to_le_bytes());
    };
    let end = out.len() - 22;
    assert_eq!(&out[end..end + 4], b"PK\x05\x06");
    let directory = usize::try_from(u32::from_le_bytes(
        out[end + 16..end + 20].try_into().unwrap(),
    ))
    .unwrap();
    let mut pos = directory;
    while &out[pos..pos + 4] == b"PK\x01\x02" {
        let names = usize::from(u16::from_le_bytes([out[pos + 28], out[pos + 29]]))
            + usize::from(u16::from_le_bytes([out[pos + 30], out[pos + 31]]))
            + usize::from(u16::from_le_bytes([out[pos + 32], out[pos + 33]]));
        add(&mut out[pos + 42..pos + 46]);
        pos += 46 + names;
    }
    add(&mut out[end + 16..end + 20]);
    out
}

/// A file cut short, or replaced by a symbolic link to a pipe, between the
/// plan and the apply: refused as changed, nothing written, and never
/// waiting on the pipe.
#[test]
fn a_file_cut_or_swapped_for_a_pipe_after_the_plan_writes_nothing() {
    let (env, _, _) = sample_env();
    let archive = env.export();
    for swap in ["cut", "pipe"] {
        let mut target = Env::new();
        let path = target.tmp.path().join("file.rappel");
        fs::write(&path, &archive).unwrap();
        let plan = target.store.plan_import(&path).unwrap();
        fs::remove_file(&path).unwrap();
        if swap == "cut" {
            fs::write(&path, &archive[..archive.len() / 2]).unwrap();
        } else {
            let fifo = target.tmp.path().join("pipe");
            let made = std::process::Command::new("mkfifo").arg(&fifo).status();
            if !made.is_ok_and(|s| s.success()) {
                eprintln!("mkfifo is not available: the pipe case is skipped");
                continue;
            }
            std::os::unix::fs::symlink(&fifo, &path).unwrap();
        }
        let before = target.export();
        let store = std::mem::replace(&mut target.store, Store::in_memory());
        let (store, applied) = within(
            Duration::from_secs(10),
            "an apply on a swapped file",
            move || {
                let mut store = store;
                let applied = store.apply_import_at(&plan, T_EXPORT);
                (store, applied)
            },
        );
        target.store = store;
        assert!(
            matches!(
                applied,
                Err(Error::ImportFileChanged | Error::ImportRefused(ImportProblem::NotAFile))
            ),
            "{swap}: {applied:?}"
        );
        assert_eq!(target.export(), before, "{swap}");
    }
}
