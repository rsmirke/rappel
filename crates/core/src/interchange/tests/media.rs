//! Media in the export and the import (ADR 0054): the files go into the
//! archive as `media/<sha256>.<ext>`, streamed from the media folder, with
//! one record each in `cards.json`; an import checks each record and each
//! entry, reads every file once before writing anything, and writes the
//! new ones into the media folder before its transaction commits.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;

use sha2::{Digest, Sha256};

use super::super::archive::ArchiveWriter;
use super::*;
use crate::media::{MediaKind, MediaRef};
use crate::test_support::{png_bytes, wav_bytes};

/// An archive of `json` with these media entries (`media/<name>`, stored),
/// in this order.
pub(super) fn media_archive(json: &Value, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let json = serde_json::to_vec_pretty(json).unwrap();
    let mut archive = ArchiveWriter::new(Cursor::new(Vec::new()), &json).unwrap();
    for (name, bytes) in files {
        archive.start_media(name).unwrap().write_all(bytes).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

/// The record of a media file, as an export writes it.
pub(super) fn record(media: &MediaRef, bytes: &[u8], created_at: i64) -> Value {
    json!({
        "hash": media.hex(),
        "mime": media.kind().mime(),
        "bytes": bytes.len(),
        "created_at": created_at,
    })
}

/// The reference of `bytes` as `kind`.
pub(super) fn media_of(bytes: &[u8], kind: MediaKind) -> MediaRef {
    MediaRef::new(Sha256::digest(bytes).into(), kind)
}

/// A document with one deck, one card naming `media`, and the records
/// given.
pub(super) fn doc_with(records: &[Value]) -> Value {
    let mut doc = json_of(&Env::new().export());
    doc["decks"] = json!([{
        "id": "01920000-0000-7000-8000-000000000001",
        "name": "Media",
        "created_at": T0,
        "updated_at": T0,
        "deleted_at": null
    }]);
    doc["cards"] = json!([{
        "id": "01920000-0000-7000-8000-000000000002",
        "deck_id": "01920000-0000-7000-8000-000000000001",
        "front_md": "a picture",
        "back_md": "",
        "created_at": T0,
        "updated_at": T0,
        "suspended": false,
        "deleted_at": null
    }]);
    doc["media"] = json!(records);
    doc
}

/// The sample, plus an image and a sound that two new cards show.
fn media_env() -> (Env, MediaRef, MediaRef) {
    let (mut env, deck, _) = sample_env();
    let png = env
        .store
        .add_media_at(&mut png_bytes(9).as_slice(), T0)
        .unwrap();
    let wav = env
        .store
        .add_media_at(&mut wav_bytes(16_000).as_slice(), T0 + 1)
        .unwrap();
    env.store
        .add_card(&deck, &format!("![a dot]({})", png.reference()), "dot")
        .unwrap();
    env.store
        .add_card(&deck, "hear it", &format!("[]({})", wav.reference()))
        .unwrap();
    (env, png, wav)
}

fn media_folder(env: &Env) -> PathBuf {
    env.tmp.path().join("data").join("media")
}

/// The files of the media folder, by name, with their bytes.
fn folder(env: &Env) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(media_folder(env))
        .map(|entries| {
            entries
                .map(|e| {
                    let e = e.unwrap();
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        fs::read(e.path()).unwrap(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn media_rows(env: &Env) -> Vec<(String, String, i64, i64)> {
    env.store
        .conn
        .prepare("SELECT hash, mime, bytes, created_at FROM media ORDER BY hash")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The entry names of an archive, in order.
fn names_of(bytes: &[u8]) -> Vec<String> {
    let zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    zip.file_names().map(|n| n.unwrap().into_owned()).collect()
}

#[test]
fn an_export_holds_each_media_file_after_cards_json() {
    let (env, png, wav) = media_env();
    let (bytes, summary) = env.store.export_bytes_at(T_EXPORT).unwrap();
    assert_eq!((summary.media, summary.missing_media), (2, 0));
    let mut expected = vec![
        format!("media/{}", png.file_name()),
        format!("media/{}", wav.file_name()),
    ];
    expected.sort();
    assert_eq!(
        names_of(&bytes),
        [
            &["cards.json".to_owned(), "media/".to_owned()][..],
            &expected
        ]
        .concat()
    );
    let mut zip = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let mut entry = zip.by_name(&format!("media/{}", png.file_name())).unwrap();
    assert_eq!(entry.compression(), CompressionMethod::Stored);
    assert_eq!(entry.unix_mode().map(|m| m & 0o777), Some(0o600));
    let mut data = Vec::new();
    entry.read_to_end(&mut data).unwrap();
    assert_eq!(data, png_bytes(9));
    drop(entry);
    // The records, sorted by hash, fields in file order.
    let doc = json_of(&bytes);
    let mut records = vec![
        record(&png, &png_bytes(9), T0),
        record(&wav, &wav_bytes(16_000), T0 + 1),
    ];
    records.sort_by(|a, b| a["hash"].as_str().cmp(&b["hash"].as_str()));
    assert_eq!(doc["media"], json!(records));
    let text = String::from_utf8(read_cards_json(&bytes, &Limits::DEFAULT).unwrap()).unwrap();
    let at = |key: &str| text.rfind(&format!("\"{key}\"")).unwrap();
    assert!(at("hash") < at("mime") && at("mime") < at("bytes") && at("bytes") < at("created_at"));
}

/// Export, import into an empty database, export again: the same bytes,
/// the same files in the new media folder, the same rows.
#[test]
fn media_round_trip_byte_for_byte() {
    let (env, png, wav) = media_env();
    let first = env.export();
    let mut target = Env::new();
    let plan = target.plan(&first).unwrap();
    assert_eq!(
        plan.summary().media,
        TableCounts {
            new: 2,
            present: 0,
            revived: 0
        }
    );
    let report = target.import(&first).unwrap();
    assert_eq!(report.media, 2);
    assert_eq!(target.export(), first);
    assert_eq!(folder(&target), folder(&env));
    assert_eq!(media_rows(&target), media_rows(&env));
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&media_folder(&target)), 0o700);
    for media in [png, wav] {
        let path = target.store.media_path(&media).unwrap();
        assert_eq!(mode(&path), 0o600);
    }
    // Again: nothing to do, nothing written.
    let again = target.plan(&first).unwrap();
    assert!(again.summary().is_noop(), "{:?}", again.summary());
    assert_eq!(again.summary().media.present, 2);
    assert_eq!(target.import(&first).unwrap().backup, None);
}

/// Media files alone are something to import: a database that has every
/// card already gets the files.
#[test]
fn media_alone_are_imported() {
    let (env, png, _) = media_env();
    let bytes = env.export();
    let mut target = Env::new();
    target.import(&bytes).unwrap();
    // The target loses its files' rows (as if made before they existed).
    target.store.conn.execute("DELETE FROM media", []).unwrap();
    let plan = target.plan(&bytes).unwrap();
    assert!(!plan.summary().is_noop());
    assert_eq!(plan.summary().media.new, 2);
    assert_eq!(plan.summary().cards.new, 0);
    let report = target.import(&bytes).unwrap();
    assert_eq!(report.media, 2);
    assert!(report.backup.is_some());
    assert_eq!(media_rows(&target), media_rows(&env));
    assert!(target.store.media_path(&png).unwrap().is_file());
}

/// A file missing from the media folder, or damaged, is left out with its
/// record, and counted; the export stays importable.
#[test]
fn missing_or_damaged_files_are_left_out_and_counted() {
    let (env, png, wav) = media_env();
    fs::remove_file(env.store.media_path(&png).unwrap()).unwrap();
    let (bytes, summary) = env.store.export_bytes_at(T_EXPORT).unwrap();
    assert_eq!((summary.media, summary.missing_media), (1, 1));
    assert_eq!(json_of(&bytes)["media"].as_array().unwrap().len(), 1);
    assert_eq!(names_of(&bytes).len(), 3);
    let mut target = Env::new();
    target.import(&bytes).unwrap();
    assert_eq!(media_rows(&target).len(), 1);

    // The same size, other bytes: damaged.
    let path = env.store.media_path(&wav).unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&path, bytes).unwrap();
    let (bytes, summary) = env.store.export_bytes_at(T_EXPORT).unwrap();
    assert_eq!((summary.media, summary.missing_media), (0, 2));
    assert_eq!(json_of(&bytes)["media"], json!([]));
    Env::new().import(&bytes).unwrap();
}

/// `export_to` streams the files into the file on disk too.
#[test]
fn export_to_writes_the_media_files() {
    let (env, _, _) = media_env();
    let path = env.tmp.path().join("all.rappel");
    let summary = env.store.export_to(&path).unwrap();
    assert_eq!(summary.media, 2);
    assert_eq!(names_of(&fs::read(&path).unwrap()).len(), 4);
    let mut target = Env::new();
    let plan = target.store.plan_import(&path).unwrap();
    assert_eq!(plan.summary().media.new, 2);
    target.store.apply_import(&plan).unwrap();
    assert_eq!(folder(&target), folder(&env));
}

/// The database has the hash with another type or size: a conflict, and
/// nothing is imported.
#[test]
fn a_media_file_recorded_differently_is_a_conflict() {
    let (env, png, _) = media_env();
    let bytes = env.export();
    let mut target = Env::new();
    target
        .store
        .conn
        .execute(
            "INSERT INTO media (hash, mime, bytes, created_at) VALUES (?1, 'image/png', 1, 0)",
            [png.hex()],
        )
        .unwrap();
    let plan = target.plan(&bytes).unwrap();
    assert_eq!(
        plan.summary().conflicts,
        [Conflict::MediaDiffers(png.hex())]
    );
    assert!(matches!(
        target.import(&bytes),
        Err(Error::ImportConflicts(1))
    ));
    assert_eq!(folder(&target), []);
}

/// A database with the same file added at another time already has it.
#[test]
fn the_same_file_added_at_another_time_is_present() {
    let (env, png, _) = media_env();
    let bytes = env.export();
    let mut target = Env::new();
    target
        .store
        .add_media_at(&mut png_bytes(9).as_slice(), T0 + 99)
        .unwrap();
    let plan = target.plan(&bytes).unwrap();
    assert_eq!(plan.summary().media.present, 1);
    assert_eq!(plan.summary().media.new, 1);
    target.import(&bytes).unwrap();
    let rows = media_rows(&target);
    let kept = rows.iter().find(|r| r.0 == png.hex()).unwrap();
    assert_eq!(kept.3, T0 + 99, "the database keeps its own time");
}

fn png_file(n: u32) -> (MediaRef, Vec<u8>) {
    let bytes = png_bytes(n);
    (media_of(&bytes, MediaKind::Png), bytes)
}

/// A case of [`refuses`]: the media records, the media entries, and the
/// problem expected.
type Case = (Vec<Value>, Vec<(String, Vec<u8>)>, ImportProblem);

/// Each case is refused before anything is written.
fn refuses(cases: Vec<Case>) {
    for (i, (records, files, expected)) in cases.into_iter().enumerate() {
        let bytes = media_archive(&doc_with(&records), &files);
        let mut env = Env::new();
        assert_eq!(refused(env.plan(&bytes)), expected, "case {i}");
        assert!(env.import(&bytes).is_err());
        assert_eq!(folder(&env), [], "case {i}");
        assert_eq!(media_rows(&env), [], "case {i}");
    }
}

fn media_problem(index: usize, field: &'static str, reason: FieldProblem) -> ImportProblem {
    ImportProblem::InvalidField {
        table: "media",
        index,
        field,
        reason,
    }
}

/// Two PNGs, the one with the smaller hash first, with their bytes.
fn two_pngs() -> [(MediaRef, Vec<u8>); 2] {
    let mut both = [png_file(1), png_file(2)];
    both.sort();
    both
}

/// Records without files, files without records, records out of order or
/// twice, records and entries of other sizes or extensions.
#[test]
fn records_and_entries_must_pair_up() {
    let [(first, first_bytes), (second, second_bytes)] = two_pngs();
    let entry = |m: &MediaRef, b: &[u8]| (m.file_name(), b.to_vec());
    let rec1 = record(&first, &first_bytes, T0);
    let rec2 = record(&second, &second_bytes, T0);
    let (e1, e2) = (entry(&first, &first_bytes), entry(&second, &second_bytes));
    let unexpected =
        |m: &MediaRef| ImportProblem::UnexpectedEntry(format!("\"media/{}\"", m.file_name()));
    let mut small = rec1.clone();
    small["bytes"] = json!(3);
    let mut gif = rec1.clone();
    gif["mime"] = json!("image/gif");
    refuses(vec![
        (
            vec![rec1.clone()],
            vec![],
            media_problem(0, "hash", FieldProblem::NoMediaFile),
        ),
        (vec![], vec![e1.clone()], unexpected(&first)),
        (
            vec![rec1.clone(), rec2.clone()],
            vec![e2.clone()],
            media_problem(0, "hash", FieldProblem::NoMediaFile),
        ),
        (
            vec![rec2.clone()],
            vec![e1.clone(), e2.clone()],
            unexpected(&first),
        ),
        (
            vec![rec2.clone(), rec1.clone()],
            vec![e1.clone(), e2.clone()],
            media_problem(1, "hash", FieldProblem::OutOfOrder),
        ),
        (
            vec![rec1.clone(), rec1.clone()],
            vec![e1.clone()],
            media_problem(1, "hash", FieldProblem::DuplicateId),
        ),
        // The size the record says is not the entry's.
        (
            vec![small],
            vec![e1.clone()],
            media_problem(0, "bytes", FieldProblem::SizeDiffers),
        ),
        // The same hash under another extension is not the record's name.
        (
            vec![gif],
            vec![e1],
            media_problem(0, "hash", FieldProblem::NoMediaFile),
        ),
    ]);
}

/// Each field of a record, then content that is not what the record and
/// the name say.
#[test]
fn broken_media_records_and_contents_are_refused() {
    let [(first, bytes), _] = two_pngs();
    let entry = || vec![(first.file_name(), bytes.clone())];
    let with = |key: &str, value: Value| {
        let mut r = record(&first, &bytes, T0);
        r[key] = value;
        vec![r]
    };
    let upper = first.hex().to_uppercase();
    let mut damaged = bytes.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;
    let jpeg = b"\xff\xd8\xff\xe0\0\x10JFIF\0".to_vec();
    let jpeg_as_png = media_of(&jpeg, MediaKind::Png);
    // An "MP3" by its first bytes (an ID3 tag) that is an HLS playlist:
    // GStreamer would fetch what it names (ADR 0056).
    let mut playlist = b"ID3\x04\0\0\0\0\0\0".to_vec();
    playlist.extend_from_slice(b"#EXTM3U\n#EXTINF:1,\nhttps://example.invalid/a.mp3\n");
    let playlist_ref = media_of(&playlist, MediaKind::Mp3);
    refuses(vec![
        (
            vec![record(&playlist_ref, &playlist, T0)],
            vec![(playlist_ref.file_name(), playlist.clone())],
            media_problem(0, "mime", FieldProblem::NotOfItsType),
        ),
        (
            with("hash", json!(upper)),
            entry(),
            media_problem(0, "hash", FieldProblem::NotHash),
        ),
        (
            with("hash", json!("../cards.json")),
            entry(),
            media_problem(0, "hash", FieldProblem::NotHash),
        ),
        (
            with("mime", json!("image/svg+xml")),
            entry(),
            media_problem(0, "mime", FieldProblem::UnknownMediaType),
        ),
        (
            with("bytes", json!(0)),
            entry(),
            media_problem(0, "bytes", FieldProblem::Empty),
        ),
        (
            with("bytes", json!(-5)),
            entry(),
            media_problem(0, "bytes", FieldProblem::Negative),
        ),
        (
            with("bytes", json!(10_000_001)),
            entry(),
            media_problem(0, "bytes", FieldProblem::TooLarge),
        ),
        (
            with("created_at", json!(-1)),
            entry(),
            media_problem(0, "created_at", FieldProblem::TimestampOutOfRange),
        ),
        // A file whose content is not its name's hash.
        (
            vec![record(&first, &bytes, T0)],
            vec![(first.file_name(), damaged)],
            media_problem(0, "hash", FieldProblem::MediaFileDiffers),
        ),
        // A JPEG under a PNG name and record, with the right hash.
        (
            vec![record(&jpeg_as_png, &jpeg, T0)],
            vec![(jpeg_as_png.file_name(), jpeg.clone())],
            media_problem(0, "mime", FieldProblem::NotOfItsType),
        ),
    ]);
}

/// A record of an unknown field is a JSON error, like every record.
#[test]
fn media_records_deny_unknown_fields() {
    let (m, bytes) = png_file(1);
    let mut r = record(&m, &bytes, T0);
    r["path"] = json!("/etc/passwd");
    let archive = media_archive(&doc_with(&[r]), &[(m.file_name(), bytes)]);
    assert!(matches!(
        refused(Env::new().plan(&archive)),
        ImportProblem::Json(_)
    ));
}

/// A file whose media entry changes after the plan (same size and time):
/// the apply's own hash finds it, and nothing is recorded.
#[test]
fn a_media_file_changed_after_the_plan_writes_nothing() {
    let (m, bytes) = png_file(5);
    let archive = media_archive(
        &doc_with(&[record(&m, &bytes, T0)]),
        &[(m.file_name(), bytes.clone())],
    );
    let mut env = Env::new();
    let path = env.tmp.path().join("in.rappel");
    fs::write(&path, &archive).unwrap();
    let plan = env.store.plan_import(&path).unwrap();
    assert_eq!(plan.summary().media.new, 1);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    // The last byte of the PNG, stored as it is: the CRC no longer
    // matches, as any change a copy would make.
    let mut changed = archive.clone();
    let at = changed
        .windows(bytes.len())
        .position(|w| w == bytes.as_slice())
        .unwrap()
        + bytes.len()
        - 1;
    changed[at] ^= 1;
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    std::io::Write::write_all(&mut &file, &changed).unwrap();
    file.set_modified(modified).unwrap();
    drop(file);
    assert!(matches!(
        env.store.apply_import(&plan),
        Err(Error::ImportFileChanged)
    ));
    assert_eq!(media_rows(&env), []);
    assert_eq!(env.store.list_decks(T_EXPORT, T_EXPORT).unwrap(), []);
    // The file under the plan's name was never written.
    assert!(!env.store.media_path(&m).unwrap().exists());
}

/// A card may name a file that is in neither the archive nor the
/// database (lost before the export): the import takes it.
#[test]
fn a_reference_to_a_file_nowhere_is_accepted() {
    let (m, _) = png_file(6);
    let mut doc = doc_with(&[]);
    doc["cards"][0]["front_md"] = json!(format!("![]({})", m.reference()));
    let mut env = Env::new();
    env.import(&archive_of(&doc)).unwrap();
    assert_eq!(media_rows(&env), []);
}

/// Planning reads every media file through its cap: a file whose entry
/// declares a small size but inflates past its kind's limit is refused at
/// the limit.
#[test]
fn a_media_file_is_read_through_its_cap() {
    let (m, bytes) = png_file(7);
    let archive = media_archive(
        &doc_with(&[record(&m, &bytes, T0)]),
        &[(m.file_name(), bytes.clone())],
    );
    let limits = Limits {
        image: 10,
        ..Limits::DEFAULT
    };
    // The entry declares more than 10 bytes: refused from its record.
    let problem = refused(Env::new().store.plan_import_bytes(&archive, &limits));
    assert_eq!(
        problem,
        ImportProblem::EntryTooLarge {
            entry: "a media file",
            limit: 10
        }
    );
}

/// The app exports and imports on a worker thread, on a second connection
/// (`Store::open_concurrent`): media go out and come in that way too.
#[test]
fn media_export_and_import_through_a_second_connection() {
    let (env, png, wav) = media_env();
    let path = env.tmp.path().join("worker.rappel");
    let summary = env.concurrent().export_to(&path).unwrap();
    assert_eq!((summary.media, summary.missing_media), (2, 0));

    let target = Env::new();
    let mut worker = target.concurrent();
    let plan = worker.plan_import(&path).unwrap();
    assert_eq!(plan.summary().media.new, 2);
    assert_eq!(worker.apply_import(&plan).unwrap().media, 2);
    // The first connection sees the rows, and the files are in the folder.
    assert_eq!(media_rows(&target), media_rows(&env));
    assert_eq!(folder(&target), folder(&env));
    for media in [png, wav] {
        assert!(target.store.media_path(&media).unwrap().is_file());
    }
}

/// A row the schema should never let in (a damaged database) cannot name a
/// file: it is counted as missing, and the export stays importable.
#[test]
fn a_media_row_rappel_cannot_name_counts_as_missing() {
    let (env, _, _) = media_env();
    env.store
        .conn
        .pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    env.store
        .conn
        .execute(
            "INSERT INTO media (hash, mime, bytes, created_at) VALUES ('NOT A HASH', 'image/svg+xml', 3, 0)",
            [],
        )
        .unwrap();
    let (bytes, summary) = env.store.export_bytes_at(T_EXPORT).unwrap();
    assert_eq!((summary.media, summary.missing_media), (2, 1));
    Env::new().import(&bytes).unwrap();
}
