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
