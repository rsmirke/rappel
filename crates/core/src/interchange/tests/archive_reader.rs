//! The archive reader (ADR 0053): entries listed without reading them,
//! an entry streamed with a cap and its hash, the zip64 end record, and
//! limits refused from the end record.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};

use super::super::archive::{Archive, CopyError, Role, Window};
use super::*;

fn open<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Archive<Cursor<&'a [u8]>>, ImportProblem> {
    Archive::open(Cursor::new(bytes), bytes.len() as u64, limits)
}

/// `bytes` (an archive without a comment) with a zip64 end record and its
/// locator before the end record, whose count, size and offset then say
/// "see zip64". `entries` replaces the zip64 record's count.
fn as_zip64(bytes: &[u8], entries: Option<u64>) -> Vec<u8> {
    let start = bytes.len() - 22;
    let eocd = &bytes[start..];
    let total = u64::from(u16::from_le_bytes([eocd[10], eocd[11]]));
    let size = u64::from(u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]));
    let offset = u64::from(u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]));
    let entries = entries.unwrap_or(total);
    let mut out = bytes[..start].to_vec();
    let record_at = out.len() as u64;
    out.extend_from_slice(b"PK\x06\x06");
    out.extend_from_slice(&44u64.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&45u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(b"PK\x06\x07");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&record_at.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&u16::MAX.to_le_bytes());
    out.extend_from_slice(&u16::MAX.to_le_bytes());
    out.extend_from_slice(&u32::MAX.to_le_bytes());
    out.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// A writer that always fails.
struct Full;

impl Write for Full {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn entries_are_listed_without_reading_them() {
    let (env, _, _) = sample_env();
    let bytes = env.export();
    let archive = open(&bytes, &Limits::DEFAULT).unwrap();
    let roles: Vec<Role> = archive.entries().iter().map(|e| e.role).collect();
    assert_eq!(roles, [Role::Json, Role::MediaDir]);
    let json = read_cards_json(&bytes, &Limits::DEFAULT).unwrap();
    assert_eq!(archive.entries()[0].size, json.len() as u64);
    assert_eq!(archive.entries()[1].size, 0);
}

#[test]
fn an_entry_streams_to_a_writer_with_its_hash_and_a_hard_cap() {
    let (env, _, _) = sample_env();
    let bytes = env.export();
    let json = read_cards_json(&bytes, &Limits::DEFAULT).unwrap();
    let mut archive = open(&bytes, &Limits::DEFAULT).unwrap();
    let entry = archive.json_entry();

    let mut out = Vec::new();
    let copied = archive
        .copy_entry(&entry, json.len() as u64, &mut out)
        .unwrap();
    assert_eq!(out, json);
    assert_eq!(copied.bytes, json.len() as u64);
    let expected: [u8; 32] = Sha256::digest(&json).into();
    assert_eq!(copied.sha256, expected);

    // One byte under: refused, whatever the entry declares.
    let mut out = Vec::new();
    match archive.copy_entry(&entry, json.len() as u64 - 1, &mut out) {
        Err(CopyError::Archive(ImportProblem::EntryTooLarge { limit, .. })) => {
            assert_eq!(limit, json.len() as u64 - 1);
        }
        other => panic!("{other:?}"),
    }
    assert!(out.len() < json.len());

    // A writer that fails.
    assert!(matches!(
        archive.copy_entry(&entry, u64::MAX, &mut Full),
        Err(CopyError::Write(err)) if err.to_string() == "full"
    ));
}

#[test]
fn a_zip64_end_record_is_read() {
    let (env, _, _) = sample_env();
    let bytes = env.export();
    let expected = Env::new().plan(&bytes).unwrap().summary().clone();
    let zip64 = as_zip64(&bytes, None);
    assert_eq!(Env::new().plan(&zip64).unwrap().summary(), &expected);

    // Misplaced, or with a count other than the classic record's.
    let mut moved = zip64.clone();
    let n = moved.len();
    moved[n - 22 - 12..n - 22 - 4].copy_from_slice(&0u64.to_le_bytes());
    assert!(matches!(
        refused(Env::new().plan(&moved)),
        ImportProblem::InvalidArchive(_)
    ));
    let mut counted = zip64;
    let n = counted.len();
    counted[n - 12..n - 10].copy_from_slice(&1u16.to_le_bytes());
    counted[n - 14..n - 12].copy_from_slice(&1u16.to_le_bytes());
    assert!(matches!(
        refused(Env::new().plan(&counted)),
        ImportProblem::InvalidArchive(_)
    ));
}

/// The entry count and the size of the list of entries are refused from
/// the end record, before the list is read.
#[test]
fn too_many_entries_or_too_large_a_list_is_refused_from_the_end_record() {
    let (env, _, _) = sample_env();
    let bytes = env.export();
    assert_eq!(
        refused(Env::new().plan(&as_zip64(&bytes, Some(1_000_000_000)))),
        ImportProblem::TooManyEntries { limit: 100_000 }
    );
    let one = Limits {
        entries: 1,
        ..Limits::DEFAULT
    };
    assert_eq!(
        refused(Env::new().store.plan_import_bytes(&bytes, &one)),
        ImportProblem::TooManyEntries { limit: 1 }
    );
    let mut large = bytes.clone();
    let n = large.len();
    large[n - 10..n - 6].copy_from_slice(&(20u32 << 20).to_le_bytes());
    assert!(matches!(
        refused(Env::new().plan(&large)),
        ImportProblem::InvalidArchive(message) if message.contains("list of entries")
    ));
}

/// A file over the archive limit (2 GiB) is refused by its size, without
/// reading it: here a sparse file, which takes no space.
#[test]
fn a_file_over_two_gib_is_refused_by_its_size() {
    let env = Env::new();
    let path = env.tmp.path().join("huge.rappel");
    let file = File::create(&path).unwrap();
    file.set_len(Limits::DEFAULT.archive + 1).unwrap();
    drop(file);
    assert_eq!(
        refused(env.store.plan_import(&path)),
        ImportProblem::FileTooLarge {
            limit: Limits::DEFAULT.archive
        }
    );
}

#[test]
fn a_window_ends_where_it_is_told() {
    let mut window = Window::new(Cursor::new(b"abcdef".to_vec()), 4).unwrap();
    let mut all = Vec::new();
    window.read_to_end(&mut all).unwrap();
    assert_eq!(all, b"abcd");
    assert_eq!(window.seek(SeekFrom::End(-1)).unwrap(), 3);
    let mut one = [0];
    window.read_exact(&mut one).unwrap();
    assert_eq!(&one, b"d");
    assert!(window.seek(SeekFrom::Current(-5)).is_err());
}
