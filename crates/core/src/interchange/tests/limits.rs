//! Every limit of the import at its boundary (ADR 0063): a file exactly at
//! a limit is planned and applied, one byte (or entry) past it is refused,
//! with nothing written.

use super::media::{doc_with, media_archive, media_of, record};
use super::*;
use crate::media::{MEDIA_DIR_NAME, MediaKind};
use crate::test_support::{png_bytes, wav_bytes};

/// A file with one deck (`Media`), one card (`a picture`), one tag (`t1`)
/// on it, an image and a sound.
fn file() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let png = png_bytes(7);
    let wav = wav_bytes(8_000);
    let (image, sound) = (
        media_of(&png, MediaKind::Png),
        media_of(&wav, MediaKind::Wav),
    );
    let mut records = vec![record(&image, &png, T0), record(&sound, &wav, T0)];
    records.sort_by_key(|r| r["hash"].as_str().unwrap().to_owned());
    let mut doc = doc_with(&records);
    doc["tags"] = json!([{"id": "01920000-0000-7000-8000-000000000003", "name": "t1"}]);
    doc["card_tags"] = json!([{
        "card_id": "01920000-0000-7000-8000-000000000002",
        "tag_id": "01920000-0000-7000-8000-000000000003"
    }]);
    let json = serde_json::to_vec_pretty(&doc).unwrap();
    let mut files = vec![
        (image.file_name(), png.clone()),
        (sound.file_name(), wav.clone()),
    ];
    files.sort();
    (media_archive(&doc, &files), json, png, wav)
}

/// The central directory's size, from the end record that ends `archive`.
fn directory_size(archive: &[u8]) -> u64 {
    let end = &archive[archive.len() - 22..];
    assert_eq!(&end[..4], b"PK\x05\x06");
    u64::from(u32::from_le_bytes(end[12..16].try_into().unwrap()))
}

#[test]
fn each_limit_holds_at_its_boundary_and_not_one_past() {
    let (archive, json, png, wav) = file();
    let len = |n: usize| u64::try_from(n).unwrap();
    let at = |set: &dyn Fn(&mut Limits)| {
        let mut limits = Limits::DEFAULT;
        set(&mut limits);
        limits
    };
    let cases: [(&str, Limits, Limits); 9] = [
        (
            "archive",
            at(&|l| l.archive = len(archive.len())),
            at(&|l| l.archive = len(archive.len()) - 1),
        ),
        // cards.json, media/ and the two files.
        ("entries", at(&|l| l.entries = 4), at(&|l| l.entries = 3)),
        (
            "central directory",
            at(&|l| l.central_directory = directory_size(&archive)),
            at(&|l| l.central_directory = directory_size(&archive) - 1),
        ),
        (
            "cards.json",
            at(&|l| l.json = len(json.len())),
            at(&|l| l.json = len(json.len()) - 1),
        ),
        (
            "an image",
            at(&|l| l.image = len(png.len())),
            at(&|l| l.image = len(png.len()) - 1),
        ),
        (
            "a sound",
            at(&|l| l.audio = len(wav.len())),
            at(&|l| l.audio = len(wav.len()) - 1),
        ),
        // "a picture"
        (
            "a card side",
            at(&|l| l.card_text = 9),
            at(&|l| l.card_text = 8),
        ),
        // "Media"
        (
            "a deck name",
            at(&|l| l.deck_name = 5),
            at(&|l| l.deck_name = 4),
        ),
        // "t1"
        (
            "a tag name",
            at(&|l| l.tag_name = 2),
            at(&|l| l.tag_name = 1),
        ),
    ];
    for (what, limit, past) in cases {
        let mut env = Env::new();
        let refusal = env.store.plan_import_bytes(&archive, &past);
        assert!(
            matches!(refusal, Err(Error::ImportRefused(_))),
            "{what} one past its limit: {refusal:?}"
        );
        let plan = env
            .store
            .plan_import_bytes(&archive, &limit)
            .unwrap_or_else(|err| panic!("{what} at its limit: {err}"));
        let report = env.store.apply_import_at(&plan, T_EXPORT).unwrap();
        assert_eq!(report.media, 2, "{what}");
        let folder = env.tmp.path().join("data").join(MEDIA_DIR_NAME);
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 2, "{what}");
    }
}
