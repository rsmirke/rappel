use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::*;
use crate::Error;
use crate::storage::Store;
use crate::test_support::{T0, png_bytes, wav_bytes};

/// An Ogg page header starting a stream whose first packet is `packet`.
fn ogg(packet: &[u8]) -> Vec<u8> {
    let mut out = b"OggS\0\x02".to_vec();
    out.extend_from_slice(&[0; 20]);
    out.push(1);
    out.push(u8::try_from(packet.len()).unwrap());
    out.extend_from_slice(packet);
    out
}

/// An ISO base media `ftyp` box with these brands (the first is major).
fn ftyp(brands: &[&[u8; 4]]) -> Vec<u8> {
    let size = 16 + 4 * (brands.len() - 1);
    let mut out = u32::try_from(size).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(brands[0]);
    out.extend_from_slice(&[0, 0, 2, 0]);
    for brand in &brands[1..] {
        out.extend_from_slice(*brand);
    }
    out.extend_from_slice(b"\0\0\0\x08free");
    out
}

#[test]
fn each_kind_is_told_by_its_first_bytes() {
    let cases: [(Vec<u8>, MediaKind); 13] = [
        (png_bytes(1), MediaKind::Png),
        (b"\xff\xd8\xff\xe0\0\x10JFIF\0".to_vec(), MediaKind::Jpeg),
        (b"\xff\xd8\xff\xe1\0\x10Exif\0".to_vec(), MediaKind::Jpeg),
        (b"RIFF\x1a\0\0\0WEBPVP8 ".to_vec(), MediaKind::WebP),
        (b"GIF87a\x01\0\x01\0".to_vec(), MediaKind::Gif),
        (b"GIF89a\x01\0\x01\0".to_vec(), MediaKind::Gif),
        (ogg(b"OpusHead\x01\x02"), MediaKind::Ogg),
        (ogg(b"\x01vorbis\0\0\0\0"), MediaKind::Ogg),
        (b"ID3\x04\0\0\0\0\0\0".to_vec(), MediaKind::Mp3),
        (b"\xff\xfb\x90\x64".to_vec(), MediaKind::Mp3),
        (ftyp(&[b"M4A ", b"M4A ", b"mp42", b"isom"]), MediaKind::M4a),
        (ftyp(&[b"mp42", b"isom", b"M4A "]), MediaKind::M4a),
        (wav_bytes(8_000), MediaKind::Wav),
    ];
    for (bytes, kind) in cases {
        assert_eq!(MediaKind::detect(&bytes), Some(kind), "{bytes:?}");
    }
}

/// Text formats, video and look-alikes are refused, whatever they are
/// called; a real signature followed by anything is that kind (its bytes
/// are only ever handed to a decoder of that kind).
#[test]
fn only_the_signature_decides_and_text_is_refused() {
    let refused: [&[u8]; 18] = [
        b"",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
        b"<?xml version=\"1.0\"?><svg/>",
        b"\xef\xbb\xbf<svg/>",
        b"<!DOCTYPE html><html><body><img src=x onerror=alert(1)>",
        b"%PDF-1.7",
        b"RIFF\x1a\0\0\0AVI LIST",
        b"RIFF\x1a\0\0",
        b"OggS\0\x02\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x01\x07\x80theora",
        b"OggS\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x01\x08OpusHead",
        b"OggS",
        b"ID3",
        b"ID3\x09\0\0",
        b"\xff\xfe\x90\x64",
        b"\xff\xfb\xf0\x64",
        b"\x89PNG\r\n",
        b"GIF8",
        b"\xff\xd8",
    ];
    for bytes in refused {
        assert_eq!(MediaKind::detect(bytes), None, "{bytes:?}");
    }
    assert_eq!(
        MediaKind::detect(&ftyp(&[b"isom", b"iso2", b"avc1", b"mp41"])),
        None,
        "a video MP4"
    );
    // A PNG signature followed by HTML is a PNG, never HTML.
    let mut png_html = png_bytes(1);
    png_html.extend_from_slice(b"<html><script>alert(1)</script></html>");
    assert_eq!(MediaKind::detect(&png_html), Some(MediaKind::Png));
    // A GIF that is also a script (the classic polyglot) is a GIF.
    assert_eq!(
        MediaKind::detect(b"GIF89a/*\x01\0*/=1;alert(1)//"),
        Some(MediaKind::Gif)
    );
    // Only the first DETECT_BYTES count: a brand after them is not read.
    let mut late = ftyp(&[b"mp42"]);
    late[..4].copy_from_slice(&u32::MAX.to_be_bytes());
    late.resize(DETECT_BYTES, 0);
    late.extend_from_slice(b"M4A ");
    assert_eq!(MediaKind::detect(&late), None);
}

#[test]
fn each_kind_has_one_extension_and_one_type() {
    for kind in MediaKind::ALL {
        assert_eq!(MediaKind::from_extension(kind.extension()), Some(kind));
        assert_eq!(MediaKind::from_mime(kind.mime()), Some(kind));
        let image = kind.mime().starts_with("image/");
        assert_eq!(kind.is_image(), image);
        assert_eq!(
            kind.max_bytes(),
            if image {
                IMAGE_MAX_BYTES
            } else {
                AUDIO_MAX_BYTES
            }
        );
    }
    for other in ["PNG", "jpeg", "svg", "svgz", "html", "", ".png", "png "] {
        assert_eq!(MediaKind::from_extension(other), None, "{other}");
    }
    for other in ["image/svg+xml", "text/html", "image/PNG", "audio/x-wav", ""] {
        assert_eq!(MediaKind::from_mime(other), None, "{other}");
    }
}

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn references_are_exact() {
    let media = MediaRef::parse(&format!("media:{HASH}.png")).unwrap();
    assert_eq!(media.kind(), MediaKind::Png);
    assert_eq!(media.hex(), HASH);
    assert_eq!(media.file_name(), format!("{HASH}.png"));
    assert_eq!(media.reference(), format!("media:{HASH}.png"));
    assert_eq!(MediaRef::parse(&media.reference()), Some(media));
    assert_eq!(format!("{media:?}"), format!("MediaRef({HASH}.png)"));
    let upper = HASH.to_uppercase();
    let short = &HASH[1..];
    for url in [
        format!("media:{upper}.png"),
        format!("media:{short}.png"),
        format!("media:{HASH}0.png"),
        format!("media:{HASH}.PNG"),
        format!("media:{HASH}.jpeg"),
        format!("media:{HASH}.svg"),
        format!("media:{HASH}"),
        format!("media:{HASH}."),
        format!("media:{HASH}.png/"),
        format!("media:{HASH}.png?x"),
        format!("media:../{HASH}.png"),
        format!("media:/{HASH}.png"),
        format!("media:{HASH}.png.png"),
        format!("Media:{HASH}.png"),
        format!(" media:{HASH}.png"),
        format!("{HASH}.png"),
        format!("https://x.org/{HASH}.png"),
        format!("file:///home/me/{HASH}.png"),
        "media:".to_owned(),
        format!("media:{}.png", "g".repeat(64)),
        format!("media:{}.png", "é".repeat(32)),
    ] {
        assert_eq!(MediaRef::parse(&url), None, "{url}");
    }
}

#[test]
fn hex_round_trips() {
    let bytes: [u8; 32] = std::array::from_fn(|i| u8::try_from(i * 8).unwrap());
    assert_eq!(parse_hex(hex(&bytes).as_bytes()), Some(bytes));
    assert_eq!(
        parse_hex(HASH.as_bytes()).map(|b| hex(&b)),
        Some(HASH.to_owned())
    );
}

/// A store in a temporary data directory.
fn store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("data")).unwrap();
    (tmp, store)
}

fn media_dir(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    tmp.path().join("data").join(MEDIA_DIR_NAME)
}

/// The names in the media folder, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn rows(store: &Store) -> Vec<(String, String, i64, i64)> {
    store
        .conn
        .prepare("SELECT hash, mime, bytes, created_at FROM media ORDER BY hash")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_file_is_stored_under_its_hash_and_recorded_once() {
    let (tmp, store) = store();
    let bytes = png_bytes(7);
    let media = store.add_media_at(&mut bytes.as_slice(), T0).unwrap();
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    assert_eq!(media, MediaRef::new(hash, MediaKind::Png));
    let path = store.media_path(&media).unwrap();
    assert_eq!(path, media_dir(&tmp).join(media.file_name()));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&media_dir(&tmp)), 0o700);
    let row = (
        media.hex(),
        "image/png".to_owned(),
        i64::try_from(bytes.len()).unwrap(),
        T0,
    );
    assert_eq!(rows(&store), std::slice::from_ref(&row));

    // Again, later: the same reference, the first time kept, one file.
    let again = store.add_media_at(&mut bytes.as_slice(), T0 + 5).unwrap();
    assert_eq!(again, media);
    assert_eq!(rows(&store), [row]);
    assert_eq!(names(&media_dir(&tmp)), [media.file_name()]);
    assert_eq!(store.add_media_bytes(&bytes).unwrap(), media);
}

/// The kind comes from the content: a JPEG is stored as `.jpg`, whatever
/// the file it came from was called.
#[test]
fn the_extension_comes_from_the_content() {
    let (tmp, store) = store();
    let jpeg = b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01".to_vec();
    let media = store.add_media_bytes(&jpeg).unwrap();
    assert_eq!(media.kind(), MediaKind::Jpeg);
    assert_eq!(names(&media_dir(&tmp)), [format!("{}.jpg", media.hex())]);
    let wav = store.add_media_bytes(&wav_bytes(1)).unwrap();
    assert_eq!(wav.kind(), MediaKind::Wav);
    assert_eq!(
        rows(&store)[..]
            .iter()
            .filter(|r| r.1 == "audio/wav")
            .count(),
        1
    );
}

#[test]
fn refused_files_leave_nothing() {
    let (tmp, store) = store();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
    let mut big = png_bytes(1);
    big.resize(usize::try_from(IMAGE_MAX_BYTES).unwrap() + 1, 0);
    let mut big_audio = wav_bytes(1);
    big_audio.resize(usize::try_from(AUDIO_MAX_BYTES).unwrap() + 1, 0);
    let cases = [
        (Vec::new(), MediaProblem::Empty),
        (svg, MediaProblem::NotMedia),
        (
            big,
            MediaProblem::TooLarge {
                kind: MediaKind::Png,
                limit: IMAGE_MAX_BYTES,
            },
        ),
        (
            big_audio,
            MediaProblem::TooLarge {
                kind: MediaKind::Wav,
                limit: AUDIO_MAX_BYTES,
            },
        ),
    ];
    for (bytes, problem) in cases {
        match store.add_media_bytes(&bytes) {
            Err(Error::MediaRefused(p)) => assert_eq!(p, problem),
            other => panic!("{problem:?}: {other:?}"),
        }
    }
    assert_eq!(rows(&store), []);
    assert_eq!(names(&media_dir(&tmp)), Vec::<String>::new());
    // At the limit exactly, it is taken.
    let mut at_limit = png_bytes(1);
    at_limit.resize(usize::try_from(IMAGE_MAX_BYTES).unwrap(), 0);
    store.add_media_bytes(&at_limit).unwrap();
}

/// A reader that gives some bytes, then fails.
struct Failing(Vec<u8>);

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.0.is_empty() {
            return Err(io::Error::other("unplugged"));
        }
        let n = buf.len().min(self.0.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0.drain(..n);
        Ok(n)
    }
}

#[test]
fn a_failing_reader_leaves_nothing() {
    let (tmp, store) = store();
    let mut bytes = png_bytes(1);
    bytes.resize(200_000, 1);
    assert!(matches!(
        store.add_media(Failing(bytes)),
        Err(Error::MediaRead(_))
    ));
    assert_eq!(rows(&store), []);
    assert_eq!(names(&media_dir(&tmp)), Vec::<String>::new());
}

/// A file already under the name with the same size is the same content
/// (same hash) and is never replaced; one of another size is damaged and
/// is kept under a second name while the good copy takes the name.
#[test]
fn an_existing_file_is_never_overwritten() {
    let (tmp, store) = store();
    let bytes = png_bytes(3);
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    let media = MediaRef::new(hash, MediaKind::Png);
    let dir = media_dir(&tmp);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(media.file_name());

    // The same size: kept as it is.
    let same_size = vec![b'x'; bytes.len()];
    fs::write(&path, &same_size).unwrap();
    assert_eq!(store.add_media_bytes(&bytes).unwrap(), media);
    assert_eq!(fs::read(&path).unwrap(), same_size);
    assert_eq!(names(&dir), [media.file_name()]);

    // Another size: set aside, then replaced by the good copy.
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"short").unwrap();
    assert_eq!(store.add_media_bytes(&bytes).unwrap(), media);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let all = names(&dir);
    assert_eq!(all.len(), 2, "{all:?}");
    let aside = all.iter().find(|n| n.contains(".damaged-")).unwrap();
    assert_eq!(fs::read(dir.join(aside)).unwrap(), b"short");
}

#[test]
fn something_else_under_the_name_refuses_the_file() {
    let (tmp, store) = store();
    let bytes = png_bytes(4);
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    let media = MediaRef::new(hash, MediaKind::Png);
    let dir = media_dir(&tmp);
    fs::create_dir_all(dir.join(media.file_name())).unwrap();
    assert!(matches!(
        store.add_media_bytes(&bytes),
        Err(Error::Io { .. })
    ));
    assert_eq!(rows(&store), []);
    assert_eq!(names(&dir), [media.file_name()]);
}

#[test]
fn a_store_without_a_data_directory_has_no_media() {
    let store = Store::in_memory();
    let media = MediaRef::new([0; 32], MediaKind::Png);
    assert_eq!(store.media_path(&media), None);
    assert!(matches!(
        store.add_media_bytes(&png_bytes(1)),
        Err(Error::NoMediaDir)
    ));
}

/// The schema keeps the rules too: a bad hash, an unknown type or an
/// empty size cannot be stored, whatever writes it.
#[test]
fn the_table_refuses_bad_rows() {
    let (_tmp, store) = store();
    let insert = |hash: &str, mime: &str, bytes: i64| {
        store.conn.execute(
            "INSERT INTO media (hash, mime, bytes, created_at) VALUES (?1, ?2, ?3, 0)",
            rusqlite::params![hash, mime, bytes],
        )
    };
    insert(HASH, "image/png", 1).unwrap();
    for (hash, mime, bytes) in [
        (HASH.to_uppercase(), "image/png", 1),
        (HASH[1..].to_owned(), "image/png", 1),
        (format!("{}g", &HASH[1..]), "image/png", 1),
        ("f".repeat(64), "image/svg+xml", 1),
        ("f".repeat(64), "text/html", 1),
        ("f".repeat(64), "image/png", 0),
        ("f".repeat(64), "image/png", -1),
    ] {
        assert!(insert(&hash, mime, bytes).is_err(), "{hash} {mime} {bytes}");
    }
}
