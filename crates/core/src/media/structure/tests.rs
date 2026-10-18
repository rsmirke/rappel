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

/// Why `bytes` is refused as `kind`, and whether for holding more than
/// plain audio (not damage).
fn why(kind: MediaKind, bytes: &[u8]) -> (&'static str, bool) {
    let refused = check_structure(kind, bytes).expect_err("refused");
    (refused.why, refused.extra)
}

fn damaged(why: &'static str) -> (&'static str, bool) {
    (why, false)
}

fn more_than_audio(why: &'static str) -> (&'static str, bool) {
    (why, true)
}

/// `file` with `bytes` inserted at `at`, and the size of each box named
/// in `grown` (the first of that type, a 32-bit size) made that much
/// larger: the boxes that hold the insertion.
fn grow(file: &[u8], grown: &[&[u8; 4]], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut out = file.to_vec();
    for name in grown {
        let header = find(file, *name) - 4;
        let size = u32::from_be_bytes(out[header..header + 4].try_into().unwrap());
        let size = size + u32::try_from(bytes.len()).unwrap();
        out[header..header + 4].copy_from_slice(&size.to_be_bytes());
    }
    out.splice(at..at, bytes.iter().copied());
    out
}

/// A box of `kind` around `body`.
fn mp4_box(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(body.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(&kind);
    out.extend_from_slice(body);
    out
}

/// Where the fixture's `moov` ends (it is the last box of the file).
fn moov_end() -> usize {
    M4A.len()
}

/// An APE tag (version 2000) of `items` (key, value), its footer
/// declaring `count` items and `extra` more bytes than they fill, with a
/// header if `header`.
fn ape_tag(items: &[(&str, &str)], count: u32, extra: u32, header: bool) -> Vec<u8> {
    ape_tag_flagged(items, 0, count, extra, header)
}

/// [`ape_tag`] with `item_flags` on every item.
fn ape_tag_flagged(
    items: &[(&str, &str)],
    item_flags: u32,
    count: u32,
    extra: u32,
    header: bool,
) -> Vec<u8> {
    let mut body = Vec::new();
    for (key, value) in items {
        body.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&item_flags.to_le_bytes());
        body.extend_from_slice(key.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());
    }
    let size = u32::try_from(body.len()).unwrap() + 32 + extra;
    let part = |flags: u32| {
        let mut out = b"APETAGEX".to_vec();
        out.extend_from_slice(&2000_u32.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out
    };
    let mut out = Vec::new();
    if header {
        out.extend(part(0xE000_0000));
    }
    out.extend(body);
    out.extend(part(if header { 0x8000_0000 } else { 0 }));
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
        assert_eq!(
            why(MediaKind::Mp3, &file),
            damaged("something that is not an MP3 frame")
        );
    }
    // After real frames too.
    let mut tail = MP3_ID3.to_vec();
    tail.extend_from_slice(b"#EXTM3U\nhttp://example.invalid/\n");
    assert_eq!(
        why(MediaKind::Mp3, &tail),
        damaged("something that is not an MP3 frame")
    );
    // A cut last frame, and nothing but a tag.
    assert_eq!(
        why(MediaKind::Mp3, &MP3[..MP3.len() - 1]),
        damaged("a cut MP3 frame")
    );
    assert_eq!(why(MediaKind::Mp3, &id3_then(b"")), damaged("no MP3 frame"));
}

/// An APE tag (`ReplayGain` from `MP3Gain`) passes when its items fill
/// exactly what its footer declares, with or without a header, before an
/// `ID3v1` tag too; anything else is damage.
#[test]
fn an_ape_tag_must_fill_its_size() {
    let gain = [
        ("REPLAYGAIN_TRACK_GAIN", "-6.50 dB"),
        ("REPLAYGAIN_TRACK_PEAK", "0.912"),
    ];
    for header in [false, true] {
        let mut tagged = MP3.to_vec();
        tagged.extend(ape_tag(&gain, 2, 0, header));
        assert_eq!(check_structure(MediaKind::Mp3, &tagged), Ok(()), "{header}");
        tagged.extend_from_slice(b"TAG");
        tagged.extend_from_slice(&[b' '; 125]);
        assert_eq!(check_structure(MediaKind::Mp3, &tagged), Ok(()), "{header}");
    }
    let not_filled = damaged("an APE tag whose items do not fill it");
    for tag in [
        ape_tag(&gain, 3, 0, false),
        ape_tag(&gain, 1, 0, false),
        ape_tag(&gain, 2, 1, false),
        ape_tag(&[("K", "a key of one letter")], 1, 0, false),
        ape_tag(&[("KEY\u{7f}", "x")], 1, 0, false),
    ] {
        let mut tagged = MP3.to_vec();
        tagged.extend(tag);
        assert_eq!(why(MediaKind::Mp3, &tagged), not_filled);
    }
    // A locator item (type 2 in flag bits 1 and 2) links elsewhere; a
    // read-only text item (flag bit 0) is fine.
    let mut locator = MP3.to_vec();
    locator.extend(ape_tag_flagged(
        &[("Related", "https://example.invalid/")],
        0b100,
        1,
        0,
        false,
    ));
    assert_eq!(
        why(MediaKind::Mp3, &locator),
        more_than_audio("an APE item that links elsewhere")
    );
    let mut read_only = MP3.to_vec();
    read_only.extend(ape_tag_flagged(&gain, 1, 2, 0, false));
    assert_eq!(check_structure(MediaKind::Mp3, &read_only), Ok(()));
    // A declared size past the frames.
    let mut huge = MP3.to_vec();
    let mut tag = ape_tag(&gain, 2, 0, false);
    let at = tag.len() - 20;
    tag[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    huge.extend(tag);
    assert_eq!(why(MediaKind::Mp3, &huge), not_filled);
}

/// A player is given an MP3's frames only: no `ID3v2` tag before them,
/// no APE or `ID3v1` tag after (ADR 0062); the other kinds whole.
#[test]
fn a_player_is_given_the_frames_of_an_mp3_and_the_rest_whole() {
    let gain = [("REPLAYGAIN_TRACK_GAIN", "-6.50 dB")];
    let mut tagged = id3_then(&id3_then(MP3));
    let start = tagged.len() - MP3.len();
    tagged.extend(ape_tag(&gain, 1, 0, true));
    tagged.extend_from_slice(b"TAG");
    tagged.extend_from_slice(&[b' '; 125]);
    let frames = audio_payload(MediaKind::Mp3, &tagged).unwrap();
    assert_eq!(&tagged[frames.clone()], MP3);
    assert_eq!(frames.start, start);
    assert_eq!(check_structure(MediaKind::Mp3, &tagged[frames]), Ok(()));
    // The fixture made by GStreamer's `id3v2mux`: its frames start after
    // the tag, and are frames.
    let frames = audio_payload(MediaKind::Mp3, MP3_ID3).unwrap();
    assert!(frames.start > 10 && frames.end == MP3_ID3.len());
    let header = &MP3_ID3[frames.start..frames.start + 2];
    assert!(header[0] == 0xFF && header[1] & 0xE0 == 0xE0, "{header:?}");
    assert_eq!(audio_payload(MediaKind::Mp3, MP3), Ok(0..MP3.len()));
    for (kind, bytes) in fixtures().into_iter().filter(|(k, _)| *k != MediaKind::Mp3) {
        assert_eq!(audio_payload(kind, bytes), Ok(0..bytes.len()), "{kind:?}");
    }
    // What is refused is refused the same way.
    assert_eq!(
        audio_payload(MediaKind::Mp3, &id3_then(b"#EXTM3U\n")).unwrap_err(),
        check_structure(MediaKind::Mp3, &id3_then(b"#EXTM3U\n")).unwrap_err()
    );
}

#[test]
fn a_second_ogg_stream_or_bytes_after_the_last_page_are_refused() {
    // Chained: a second logical stream after the first.
    let mut chained = OPUS.to_vec();
    chained.extend_from_slice(VORBIS);
    assert_eq!(
        why(MediaKind::Ogg, &chained),
        more_than_audio("a second Ogg stream")
    );
    let mut trailing = OPUS.to_vec();
    trailing.extend_from_slice(b"#EXTM3U\n");
    assert_eq!(
        why(MediaKind::Ogg, &trailing),
        damaged("bytes after the last Ogg page")
    );
    // A flipped byte breaks a page's CRC.
    let mut flipped = VORBIS.to_vec();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert_eq!(
        why(MediaKind::Ogg, &flipped),
        damaged("an Ogg page with a bad CRC")
    );
    assert_eq!(
        why(MediaKind::Ogg, &OPUS[..OPUS.len() - 1]),
        damaged("a cut Ogg page")
    );
}

#[test]
fn m4a_references_redirects_and_other_tracks_are_refused() {
    // A reference movie box inside moov: rename the `udta` or `free` box,
    // keeping its size, to `rmra`.
    let mut redirect = M4A.to_vec();
    let at = find(&redirect, b"mvhd");
    redirect[at..at + 4].copy_from_slice(b"rmra");
    assert_eq!(MediaKind::detect(&redirect), Some(MediaKind::M4a));
    assert_eq!(
        why(MediaKind::M4a, &redirect),
        more_than_audio("a reference movie, redirect or compressed movie box")
    );
    // A data reference that is not self-contained (flag 1 off): the
    // samples would be read from the URL it names.
    let mut external = M4A.to_vec();
    let at = find(&external, b"url ");
    external[at + 7] &= !1;
    let elsewhere = more_than_audio("a data reference to another file");
    assert_eq!(why(MediaKind::M4a, &external), elsewhere);
    // A data reference of another kind (`alis`).
    let mut alias = M4A.to_vec();
    alias[at..at + 4].copy_from_slice(b"alis");
    assert_eq!(why(MediaKind::M4a, &alias), elsewhere);
    // A video track.
    let mut video = M4A.to_vec();
    let at = find(&video, b"soun");
    video[at..at + 4].copy_from_slice(b"vide");
    assert_eq!(
        why(MediaKind::M4a, &video),
        more_than_audio("a track that is not sound")
    );
    // A box longer than the file, trailing bytes, and no moov.
    let outside = damaged("a box outside its parent");
    assert_eq!(why(MediaKind::M4a, &M4A[..M4A.len() - 1]), outside);
    let mut trailing = M4A.to_vec();
    trailing.extend_from_slice(b"#EXTM3U");
    assert_eq!(why(MediaKind::M4a, &trailing), damaged("a cut box header"));
    trailing.extend_from_slice(b"\nhttps://example.invalid/\n");
    assert_eq!(why(MediaKind::M4a, &trailing), outside);
    // A compressed movie (qtdemux would decompress and parse it, with
    // whatever redirect it holds), a fragmented movie, and a box no audio
    // file has in the sample table.
    let last_udta = M4A.windows(4).rposition(|w| w == b"udta").unwrap();
    for (at, name, expected) in [
        (
            last_udta,
            b"cmov",
            more_than_audio("a reference movie, redirect or compressed movie box"),
        ),
        (
            last_udta,
            b"mvex",
            more_than_audio("a box an audio file does not have there"),
        ),
        (
            find(M4A, b"stco"),
            b"xyzw",
            more_than_audio("a box an audio file does not have there"),
        ),
        (
            find(M4A, b"esds"),
            b"wave",
            more_than_audio("a QuickTime wave box in a version 0 description"),
        ),
        (
            find(M4A, b"mp4a"),
            b"avc1",
            more_than_audio("a sample description that is not audio"),
        ),
    ] {
        let mut changed = M4A.to_vec();
        changed[at..at + 4].copy_from_slice(name);
        assert_eq!(why(MediaKind::M4a, &changed), expected);
    }
    // At the top level: a fragment, a compressed movie.
    let mut fragment = M4A.to_vec();
    fragment.extend_from_slice(b"\x00\x00\x00\x10moof\x00\x00\x00\x08mfhd");
    assert_eq!(
        why(MediaKind::M4a, &fragment),
        more_than_audio("a box an audio file does not have there")
    );
    let mut compressed = M4A.to_vec();
    compressed.extend_from_slice(b"\x00\x00\x00\x10cmov\x00\x00\x00\x08dcom");
    assert_eq!(
        why(MediaKind::M4a, &compressed),
        more_than_audio("a reference movie, redirect or compressed movie box")
    );
    // Inside `moov`: a compressed movie added, and one hidden in `udta`.
    let cmov = mp4_box(*b"cmov", &mp4_box(*b"dcom", b"zlib"));
    let in_moov = grow(M4A, &[b"moov"], moov_end(), &cmov);
    assert_eq!(
        why(MediaKind::M4a, &in_moov),
        more_than_audio("a reference movie, redirect or compressed movie box")
    );
    let in_udta = grow(M4A, &[b"moov"], moov_end(), &mp4_box(*b"udta", &cmov));
    assert_eq!(
        why(MediaKind::M4a, &in_udta),
        more_than_audio("a reference movie, redirect or compressed movie box")
    );
    let mut no_moov = M4A.to_vec();
    let at = find(&no_moov, b"moov");
    no_moov[at..at + 4].copy_from_slice(b"skip");
    assert_eq!(
        why(MediaKind::M4a, &no_moov),
        damaged("an M4A without one moov and its mdat")
    );
}

#[test]
fn a_wav_of_another_format_or_length_is_refused() {
    let at = find(WAV, b"fmt ") + 8;
    // MPEG layer III inside a WAV (tag 0x55): another decoder.
    let mut mp3_in_wav = WAV.to_vec();
    mp3_in_wav[at] = 0x55;
    assert_eq!(
        why(MediaKind::Wav, &mp3_in_wav),
        more_than_audio("a WAV that is not PCM or float")
    );
    // Float is fine; bad bits are not.
    let mut float = WAV.to_vec();
    float[at] = 3;
    float[at + 14] = 32;
    assert_eq!(check_structure(MediaKind::Wav, &float), Ok(()));
    let mut odd_bits = WAV.to_vec();
    odd_bits[at + 14] = 12;
    assert_eq!(
        why(MediaKind::Wav, &odd_bits),
        damaged("a WAV format with bad values")
    );
    let mut trailing = WAV.to_vec();
    trailing.extend_from_slice(b"#EXTM3U\n");
    let length = damaged("a RIFF chunk of another length than the file");
    assert_eq!(why(MediaKind::Wav, &trailing), length);
    assert_eq!(why(MediaKind::Wav, &WAV[..WAV.len() - 2]), length);
    let mut no_data = WAV.to_vec();
    let d = find(&no_data, b"data");
    no_data[d..d + 4].copy_from_slice(b"junk");
    assert_eq!(
        why(MediaKind::Wav, &no_data),
        damaged("a WAV without its data")
    );
}

/// What other encoders write passes: ffmpeg's and `iTunes`' tags with cover
/// art in `udta/meta/ilst`, `GPAC`'s `meta` directly in `moov`, any tag in
/// a track's `udta`, `uuid` and `pdin` boxes, and a `QuickTime`-style
/// sound description (version 1, its codec data in a `wave` box).
#[test]
fn m4a_files_of_other_encoders_pass() {
    let hdlr = mp4_box(*b"hdlr", &[&[0; 8][..], b"mdirappl", &[0; 10]].concat());
    let covr = mp4_box(
        *b"covr",
        &mp4_box(
            *b"data",
            &[&[0, 0, 0, 13, 0, 0, 0, 0][..], b"\xff\xd8\xff"].concat(),
        ),
    );
    let ilst = mp4_box(*b"ilst", &covr);
    let meta = mp4_box(*b"meta", &[&[0; 4][..], &hdlr, &ilst].concat());
    let udta = mp4_box(*b"udta", &[&meta[..], &mp4_box(*b"\xa9nam", b"x")].concat());
    let tagged = grow(M4A, &[b"moov"], moov_end(), &udta);
    assert_eq!(check_structure(MediaKind::M4a, &tagged), Ok(()));
    let gpac = grow(M4A, &[b"moov"], moov_end(), &meta);
    assert_eq!(check_structure(MediaKind::M4a, &gpac), Ok(()));
    // A QuickTime-style `meta` (Apple's `mdta` tags, as an iPhone
    // writes them): no version and flags, its handler first, then `keys`
    // and `ilst`.
    let quicktime = mp4_box(
        *b"meta",
        &[&hdlr[..], &mp4_box(*b"keys", &[0; 8]), &ilst].concat(),
    );
    let apple = grow(M4A, &[b"moov"], moov_end(), &quicktime);
    assert_eq!(check_structure(MediaKind::M4a, &apple), Ok(()));
    // Without the handler first, the first 4 bytes are taken as version
    // and flags, and what follows must still be allowed boxes.
    let unknown = mp4_box(
        *b"meta",
        &[&[0; 4][..], &mp4_box(*b"iinf", &[0; 8])].concat(),
    );
    let refused = grow(M4A, &[b"moov"], moov_end(), &unknown);
    assert_eq!(
        why(MediaKind::M4a, &refused),
        more_than_audio("a box an audio file does not have there")
    );
    let uuid = grow(M4A, &[b"moov"], moov_end(), &mp4_box(*b"uuid", &[7; 16]));
    assert_eq!(check_structure(MediaKind::M4a, &uuid), Ok(()));
    let mut pdin = M4A.to_vec();
    pdin.extend(mp4_box(*b"pdin", &[0; 12]));
    assert_eq!(check_structure(MediaKind::M4a, &pdin), Ok(()));
    // `moov/meta` holds only what tags need.
    let odd_meta = mp4_box(
        *b"meta",
        &[&[0; 4][..], &mp4_box(*b"iloc", &[0; 8])].concat(),
    );
    let odd = grow(M4A, &[b"moov"], moov_end(), &odd_meta);
    assert_eq!(
        why(MediaKind::M4a, &odd),
        more_than_audio("a box an audio file does not have there")
    );
}

/// QuickTime-style sound descriptions pass: version 1 with its codec data
/// in a `wave` box, and version 2.
#[test]
fn quicktime_sound_descriptions_pass() {
    // A version 1 sound description: 16 more bytes of fields, then a
    // `wave` box with the format, a small `mp4a` atom, the `esds` and
    // the terminator.
    let entry = find(M4A, b"mp4a") + 4;
    let mut v1 = grow(
        M4A,
        &[
            b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"mp4a",
        ],
        entry + 28,
        &[0; 16],
    );
    v1[entry + 9] = 1;
    let esds_at = find(&v1, b"esds") - 4;
    let esds_len = usize::try_from(u32::from_be_bytes(
        v1[esds_at..esds_at + 4].try_into().unwrap(),
    ))
    .unwrap();
    let esds = v1[esds_at..esds_at + esds_len].to_vec();
    let wave = mp4_box(
        *b"wave",
        &[
            &mp4_box(*b"frma", b"mp4a")[..],
            &mp4_box(*b"mp4a", &[0; 4]),
            &esds,
            &mp4_box([0; 4], b""),
        ]
        .concat(),
    );
    let entry_end = esds_at + esds_len;
    let with_wave = grow(
        &v1,
        &[
            b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"mp4a",
        ],
        entry_end,
        &wave,
    );
    assert_eq!(check_structure(MediaKind::M4a, &v1), Ok(()));
    // Version 2: 20 more bytes of fields than version 1, the size of the
    // structure (72) at +28, after always 3, 16, -2, 0 and 65536.
    let mut v2 = grow(
        &v1,
        &[
            b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"mp4a",
        ],
        entry + 44,
        &[0; 20],
    );
    v2[entry + 9] = 2;
    v2[entry + 16..entry + 32]
        .copy_from_slice(&[0, 3, 0, 16, 0xFF, 0xFE, 0, 0, 0, 1, 0, 0, 0, 0, 0, 72]);
    assert_eq!(check_structure(MediaKind::M4a, &v2), Ok(()));
    let mut v2_odd = v2.clone();
    v2_odd[entry + 31] = 64;
    assert_eq!(
        why(MediaKind::M4a, &v2_odd),
        more_than_audio("a sound description of an unknown version")
    );
    assert_eq!(check_structure(MediaKind::M4a, &with_wave), Ok(()));
    // Anything else in `wave` is refused.
    let mut odd_wave = with_wave.clone();
    let frma = find(&odd_wave, b"frma");
    odd_wave[frma..frma + 4].copy_from_slice(b"rmra");
    assert_eq!(
        why(MediaKind::M4a, &odd_wave),
        more_than_audio("a box a QuickTime wave box does not have")
    );
}

/// The elementary stream descriptor names audio, and no URL.
#[test]
fn an_esds_must_name_audio_in_this_file() {
    let es = find(M4A, b"esds") + 8;
    assert_eq!(M4A[es], 3, "the ES_Descriptor tag");
    let mut pos = es + 1;
    while M4A[pos] & 0x80 != 0 {
        pos += 1;
    }
    let flags = pos + 3;
    assert_eq!(M4A[flags], 0, "the fixture's stream has no optional fields");
    assert_eq!(M4A[flags + 1], 4, "the decoder configuration tag");
    let mut pos = flags + 2;
    while M4A[pos] & 0x80 != 0 {
        pos += 1;
    }
    let object = pos + 1;
    assert_eq!(M4A[object], 0x40, "MPEG-4 audio");
    let mut video = M4A.to_vec();
    video[object] = 0x20;
    assert_eq!(
        why(MediaKind::M4a, &video),
        more_than_audio("an elementary stream that is not audio")
    );
    let mut url = M4A.to_vec();
    url[flags] = 0x40;
    assert_eq!(
        why(MediaKind::M4a, &url),
        more_than_audio("an elementary stream named by a URL")
    );
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
