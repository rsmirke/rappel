//! Real files (made with `GStreamer` from a test tone; the M4A's major brand
//! set to `M4A `, as Apple's tools write it, since `mp4mux` writes
//! `mp42`) and hostile ones.

use std::time::Duration;

use proptest::prelude::*;

use super::*;
use crate::test_support::within;

const MP3: &[u8] = include_bytes!("../fixtures/tone.mp3");
const MP3_ID3: &[u8] = include_bytes!("../fixtures/tone-id3.mp3");
const OPUS: &[u8] = include_bytes!("../fixtures/tone-opus.ogg");
const VORBIS: &[u8] = include_bytes!("../fixtures/tone-vorbis.ogg");
const M4A: &[u8] = include_bytes!("../fixtures/tone.m4a");
const WAV: &[u8] = include_bytes!("../fixtures/tone.wav");

fn fixtures() -> [(MediaKind, &'static [u8]); 6] {
    [
        (MediaKind::Mp3, MP3),
        (MediaKind::Mp3, MP3_ID3),
        (MediaKind::Ogg, OPUS),
        (MediaKind::Ogg, VORBIS),
        (MediaKind::M4a, M4A),
        (MediaKind::Wav, WAV),
    ]
}

/// An `ID3v2.4` tag of `body.len()` bytes of padding, then `rest`.
fn id3_then(rest: &[u8]) -> Vec<u8> {
    let pad = 16_usize;
    let mut out = b"ID3\x04\x00\x00".to_vec();
    out.extend_from_slice(&[0, 0, 0, u8::try_from(pad).unwrap()]);
    out.extend_from_slice(&vec![0; pad]);
    out.extend_from_slice(rest);
    out
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("{:?} not found", String::from_utf8_lossy(needle)))
}

#[test]
fn real_audio_files_are_accepted() {
    for (kind, bytes) in fixtures() {
        assert_eq!(MediaKind::detect(bytes), Some(kind));
        assert_eq!(check_structure(kind, bytes), Ok(()), "{kind:?}");
    }
    // An ID3v1 tag at the end too.
    let mut tagged = MP3.to_vec();
    tagged.extend_from_slice(b"TAG");
    tagged.extend_from_slice(&[b' '; 125]);
    assert_eq!(check_structure(MediaKind::Mp3, &tagged), Ok(()));
    // Images are not this module's business.
    assert_eq!(check_structure(MediaKind::Png, b"anything"), Ok(()));
}

/// The reviewer's reproduction: an ID3 tag makes it an "MP3" by its first
/// bytes, and `GStreamer` would play what follows as a playlist or a
/// session, fetching what it names.
#[test]
fn playlists_and_sessions_behind_an_id3_tag_are_refused() {
    for rest in [
        &b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nfile:///tmp/variant.m3u8\n"[..],
        &b"#EXTM3U\n#EXTINF:1,\nhttps://example.invalid/a.mp3\n"[..],
        &b"<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"><BaseURL>http://example.invalid/</BaseURL></MPD>"[..],
        &b"v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=x\r\nc=IN IP4 192.0.2.1\r\nm=audio 5004 RTP/AVP 0\r\n"[..],
        &b"<SmoothStreamingMedia MajorVersion=\"2\"/>"[..],
    ] {
        let file = id3_then(rest);
        assert_eq!(MediaKind::detect(&file), Some(MediaKind::Mp3));
        assert!(check_structure(MediaKind::Mp3, &file).is_err());
    }
    // After real frames too.
    let mut tail = MP3_ID3.to_vec();
    tail.extend_from_slice(b"#EXTM3U\nhttp://example.invalid/\n");
    assert!(check_structure(MediaKind::Mp3, &tail).is_err());
    // A cut last frame, and nothing but a tag.
    assert!(check_structure(MediaKind::Mp3, &MP3[..MP3.len() - 1]).is_err());
    assert!(check_structure(MediaKind::Mp3, &id3_then(b"")).is_err());
}

#[test]
fn a_second_ogg_stream_or_bytes_after_the_last_page_are_refused() {
    // Chained: a second logical stream after the first.
    let mut chained = OPUS.to_vec();
    chained.extend_from_slice(VORBIS);
    assert!(check_structure(MediaKind::Ogg, &chained).is_err());
    let mut trailing = OPUS.to_vec();
    trailing.extend_from_slice(b"#EXTM3U\n");
    assert!(check_structure(MediaKind::Ogg, &trailing).is_err());
    // A flipped byte breaks a page's CRC.
    let mut flipped = VORBIS.to_vec();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert!(check_structure(MediaKind::Ogg, &flipped).is_err());
    assert!(check_structure(MediaKind::Ogg, &OPUS[..OPUS.len() - 1]).is_err());
}

#[test]
fn m4a_references_redirects_and_other_tracks_are_refused() {
    // A reference movie box inside moov: rename the `udta` or `free` box,
    // keeping its size, to `rmra`.
    let mut redirect = M4A.to_vec();
    let at = find(&redirect, b"mvhd");
    redirect[at..at + 4].copy_from_slice(b"rmra");
    assert_eq!(MediaKind::detect(&redirect), Some(MediaKind::M4a));
    assert!(check_structure(MediaKind::M4a, &redirect).is_err());
    // A data reference that is not self-contained (flag 1 off): the
    // samples would be read from the URL it names.
    let mut external = M4A.to_vec();
    let at = find(&external, b"url ");
    external[at + 7] &= !1;
    assert!(check_structure(MediaKind::M4a, &external).is_err());
    // A data reference of another kind (`alis`).
    let mut alias = M4A.to_vec();
    alias[at..at + 4].copy_from_slice(b"alis");
    assert!(check_structure(MediaKind::M4a, &alias).is_err());
    // A video track.
    let mut video = M4A.to_vec();
    let at = find(&video, b"soun");
    video[at..at + 4].copy_from_slice(b"vide");
    assert!(check_structure(MediaKind::M4a, &video).is_err());
    // A box longer than the file, trailing bytes, and no moov.
    assert!(check_structure(MediaKind::M4a, &M4A[..M4A.len() - 1]).is_err());
    let mut trailing = M4A.to_vec();
    trailing.extend_from_slice(b"#EXTM3U");
    assert!(check_structure(MediaKind::M4a, &trailing).is_err());
    let mut no_moov = M4A.to_vec();
    let at = find(&no_moov, b"moov");
    no_moov[at..at + 4].copy_from_slice(b"skip");
    assert!(check_structure(MediaKind::M4a, &no_moov).is_err());
}

#[test]
fn a_wav_of_another_format_or_length_is_refused() {
    let at = find(WAV, b"fmt ") + 8;
    // MPEG layer III inside a WAV (tag 0x55): another decoder.
    let mut mp3_in_wav = WAV.to_vec();
    mp3_in_wav[at] = 0x55;
    assert!(check_structure(MediaKind::Wav, &mp3_in_wav).is_err());
    // Float is fine; bad bits are not.
    let mut float = WAV.to_vec();
    float[at] = 3;
    float[at + 14] = 32;
    assert_eq!(check_structure(MediaKind::Wav, &float), Ok(()));
    let mut odd_bits = WAV.to_vec();
    odd_bits[at + 14] = 12;
    assert!(check_structure(MediaKind::Wav, &odd_bits).is_err());
    let mut trailing = WAV.to_vec();
    trailing.extend_from_slice(b"#EXTM3U\n");
    assert!(check_structure(MediaKind::Wav, &trailing).is_err());
    assert!(check_structure(MediaKind::Wav, &WAV[..WAV.len() - 2]).is_err());
    let mut no_data = WAV.to_vec();
    let d = find(&no_data, b"data");
    no_data[d..d + 4].copy_from_slice(b"junk");
    assert!(check_structure(MediaKind::Wav, &no_data).is_err());
}

/// The largest files are checked in a time bound: one pass.
#[test]
fn the_check_is_linear() {
    within(Duration::from_secs(10), "checking 20 MB files", || {
        // 20 MB of minimal MPEG-1 frames (128 kbps, 44.1 kHz: 417 bytes).
        let frame = [&[0xFF_u8, 0xFB, 0x90, 0x00][..], &[0; 413]].concat();
        let mp3 = frame.repeat(20_000_000 / frame.len());
        assert_eq!(check_structure(MediaKind::Mp3, &mp3), Ok(()));
        // 20 MB of tiny boxes.
        let mut m4a = M4A.to_vec();
        m4a.extend(b"\x00\x00\x00\x08free".repeat(2_400_000));
        assert_eq!(check_structure(MediaKind::M4a, &m4a), Ok(()));
        // 20 MB of tiny WAV chunks.
        let mut wav = WAV.to_vec();
        wav.extend(b"junk\x00\x00\x00\x00".repeat(2_400_000));
        let size = u32::try_from(wav.len() - 8).unwrap().to_le_bytes();
        wav[4..8].copy_from_slice(&size);
        assert_eq!(check_structure(MediaKind::Wav, &wav), Ok(()));
        // Boxes nested past the cap.
        let mut deep = Vec::new();
        for depth in 0..40_u32 {
            let size = 8 * (40 - depth);
            deep.extend_from_slice(&size.to_be_bytes());
            deep.extend_from_slice(b"moov");
        }
        assert!(walk_boxes(&deep, 0, deep.len(), 0, *b"file").is_err());
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Damaged files never make the check panic.
    #[test]
    fn damaged_files_never_panic(
        which in 0_usize..6,
        cut in any::<prop::sample::Index>(),
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
    ) {
        let (kind, bytes) = fixtures()[which];
        let mut file = bytes.to_vec();
        for (at, value) in flips {
            let i = at.index(file.len());
            file[i] = value;
        }
        file.truncate(cut.index(file.len() + 1));
        let _ = check_structure(kind, &file);
    }

    #[test]
    fn noise_never_panics(kind in 0_usize..4, bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let kind = [MediaKind::Mp3, MediaKind::Ogg, MediaKind::Wav, MediaKind::M4a][kind];
        let _ = check_structure(kind, &bytes);
        let _ = check_structure(kind, &id3_then(&bytes));
    }
}
