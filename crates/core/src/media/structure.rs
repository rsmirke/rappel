//! The structure of an audio file, checked over the whole file (ADR 0056).
//!
//! The first bytes ([`MediaKind::detect`]) only say what a file claims to
//! be. `GStreamer`, which plays audio, finds the format itself from the
//! content: an "MP3" that is an ID3 tag followed by an HLS or DASH
//! playlist is played as that playlist, and the playlist's URIs are
//! fetched (`file:`, `http:`). So an audio file is accepted only if all
//! of it is plain audio of its kind:
//!
//! - **MP3**: `ID3v2` tags (their declared sizes), then MPEG audio layer III
//!   frames, each header valid and each frame whole, back to back to the
//!   end, which may hold an APE tag whose items fill exactly its declared
//!   size, and an `ID3v1` tag. Nothing else.
//! - **Ogg**: pages back to back to the end, each with its CRC, all of one
//!   logical stream (one serial number, the first page the only one that
//!   begins a stream, page numbers in order, nothing after the last page),
//!   whose first packet is an Opus or a Vorbis header.
//! - **WAV**: a RIFF `WAVE` chunk exactly as long as the file, its chunks
//!   within it, one `fmt ` of PCM or IEEE float (also inside
//!   `WAVE_FORMAT_EXTENSIBLE`) before a `data` chunk.
//! - **M4A**: boxes within their parents to the end of the file, at most
//!   16 deep: `ftyp` first, with an audio brand, one `moov` and its
//!   `mdat`. **An allow-list**: at the top level and inside `moov`, `trak`,
//!   `edts`, `mdia`, `minf`, `dinf`, `stbl`, `udta` and `meta` only the
//!   boxes real audio files have ([`allowed_inside`]), so no reference
//!   movie, redirect or compressed movie (`cmov`, which qtdemux
//!   decompresses and parses), no fragments, no other kind of track; every
//!   data reference self-contained (`url ` or `urn ` with flag 1), every
//!   sample description an audio one (`mp4a`, `alac`, `Opus`; an `esds`
//!   of an audio object type, without a URL), every track's handler
//!   `soun`; `udta` holds any box but the refused ones, not walked. `GstPlay` follows a redirect, so for M4A
//!   "never a URI" rests on this check.
//!
//! Images are not checked here: their decoder runs in glycin's sandbox
//! and never reads a URI (ADR 0055). Every check is one pass over the
//! bytes (linear), with bounds checked on every read: it never panics.

use super::MediaKind;

/// Why an audio file is refused: what was wrong, for the logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed {
    /// What was found.
    pub why: &'static str,
    /// Whether the file is whole but holds more than plain audio (another
    /// stream, a reference, a box or a format audio files do not have),
    /// rather than damaged (cut, out of bounds, bad checksums, junk).
    pub extra: bool,
}

type Check = Result<(), Malformed>;

/// Damage: cut, out of bounds, a bad checksum, junk where audio should be.
fn bad(why: &'static str) -> Check {
    Err(Malformed { why, extra: false })
}

/// A whole file that holds more than plain audio of its kind.
fn extra(why: &'static str) -> Check {
    Err(Malformed { why, extra: true })
}

/// Whether `bytes`, the whole file, is plain audio of `kind` (see the
/// module). Images pass: their structure is the sandboxed decoder's
/// business.
///
/// # Errors
/// [`Malformed`] with the first problem found.
pub fn check_structure(kind: MediaKind, bytes: &[u8]) -> Check {
    match kind {
        MediaKind::Png | MediaKind::Jpeg | MediaKind::WebP | MediaKind::Gif => Ok(()),
        MediaKind::Mp3 => mp3(bytes),
        MediaKind::Ogg => ogg(bytes),
        MediaKind::Wav => wav(bytes),
        MediaKind::M4a => m4a(bytes),
    }
}

fn u32_be(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u16_le(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn u64_be(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(8)?)?;
    let mut a = [0; 8];
    a.copy_from_slice(s);
    Some(u64::from_be_bytes(a))
}

fn to_usize(n: u64) -> Option<usize> {
    usize::try_from(n).ok()
}

// ---------------------------------------------------------------- MP3

/// Layer III bit rates in kbit/s, by index: MPEG-1, then MPEG-2 and 2.5.
const V1: [usize; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const V2: [usize; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
/// MPEG-1 sample rates by index (halved for MPEG-2, quartered for 2.5).
const RATES: [usize; 3] = [44_100, 48_000, 32_000];

/// The length of the MPEG audio layer III frame whose header is `h`, or
/// `None` if the header is not one (free format, reserved values).
fn mp3_frame_len(h: [u8; 4]) -> Option<usize> {
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 0b11; // 0: 2.5, 2: 2, 3: 1
    let layer = (h[1] >> 1) & 0b11; // 1: layer III
    if version == 1 || layer != 1 {
        return None;
    }
    let bitrate_index = usize::from(h[2] >> 4);
    let rate_index = usize::from((h[2] >> 2) & 0b11);
    let padding = usize::from((h[2] >> 1) & 1);
    if bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    let (kbps, rate, factor) = match version {
        3 => (V1[bitrate_index], RATES[rate_index], 144_000),
        2 => (V2[bitrate_index], RATES[rate_index] / 2, 72_000),
        _ => (V2[bitrate_index], RATES[rate_index] / 4, 72_000),
    };
    Some(factor * kbps / rate + padding)
}

fn mp3(b: &[u8]) -> Check {
    let mut pos = 0;
    // ID3v2 tags, each skipped by its declared size (syncsafe).
    while b.get(pos..pos + 3) == Some(b"ID3") {
        let Some(h) = b.get(pos..pos + 10) else {
            return bad("a cut ID3 tag");
        };
        if !(2..=4).contains(&h[3]) || h[6..10].iter().any(|&x| x & 0x80 != 0) {
            return bad("an ID3 tag with a bad header");
        }
        let size = h[6..10]
            .iter()
            .fold(0_usize, |n, &x| (n << 7) | usize::from(x));
        let footer = if h[3] == 4 && h[5] & 0x10 != 0 { 10 } else { 0 };
        pos += 10 + size + footer;
        if pos > b.len() {
            return bad("an ID3 tag longer than the file");
        }
    }
    let mut end = b.len();
    // An ID3v1 tag: the last 128 bytes.
    if end - pos >= 128 && b.get(end - 128..end - 125) == Some(b"TAG") {
        end -= 128;
    }
    // An APE tag (ReplayGain from MP3Gain, for one): its items must fill
    // exactly the size its footer declares; no item is acted on.
    if end - pos >= 32 && b.get(end - 32..end - 24) == Some(b"APETAGEX") {
        end = ape_start(b, pos, end)?;
    }
    if pos >= end {
        return bad("no MP3 frame");
    }
    while pos < end {
        let Some(h) = b.get(pos..pos + 4) else {
            return bad("a cut MP3 frame header");
        };
        let Some(len) = mp3_frame_len([h[0], h[1], h[2], h[3]]) else {
            return bad("something that is not an MP3 frame");
        };
        if len < 4 || pos + len > end {
            return bad("a cut MP3 frame");
        }
        pos += len;
    }
    Ok(())
}

/// Where the APE tag whose 32-byte footer ends `b[..end]` begins (its
/// header included), checked: its items (a value size, flags, a key of 2
/// to 255 printable ASCII characters ended by a NUL, the value) fill
/// exactly the size the footer declares, as many as it says; a header,
/// if the flags announce one, is there; all of it after `floor`.
fn ape_start(b: &[u8], floor: usize, end: usize) -> Result<usize, Malformed> {
    let damaged = Malformed {
        why: "an APE tag whose items do not fill it",
        extra: false,
    };
    let (Some(size), Some(count), Some(flags)) = (
        u32_le(b, end - 20),
        u32_le(b, end - 16),
        u32_le(b, end - 12),
    ) else {
        return Err(damaged);
    };
    let size = to_usize(u64::from(size)).ok_or(damaged)?;
    if size < 32 || size > end - floor {
        return Err(damaged);
    }
    let start = end - size;
    let items_end = end - 32;
    let mut p = start;
    let mut seen = 0_u32;
    while p < items_end {
        let value = u32_le(b, p)
            .and_then(|n| to_usize(u64::from(n)))
            .ok_or(damaged)?;
        // Bits 1 and 2 of an item's flags give its type; 2 is a locator,
        // a link to something elsewhere.
        if u32_le(b, p + 4).is_some_and(|flags| (flags >> 1) & 0b11 == 2) {
            return Err(Malformed {
                why: "an APE item that links elsewhere",
                extra: true,
            });
        }
        let key_start = p + 8;
        let key_len = b
            .get(key_start..items_end)
            .and_then(|rest| rest.iter().take(256).position(|&c| c == 0))
            .ok_or(damaged)?;
        let key = &b[key_start..key_start + key_len];
        if !(2..=255).contains(&key_len) || !key.iter().all(|c| (0x20..=0x7E).contains(c)) {
            return Err(damaged);
        }
        let value_start = key_start + key_len + 1;
        p = value_start.checked_add(value).ok_or(damaged)?;
        if p > items_end {
            return Err(damaged);
        }
        seen = seen.saturating_add(1);
    }
    if seen != count {
        return Err(damaged);
    }
    if flags & 0x8000_0000 == 0 {
        return Ok(start);
    }
    if start - floor < 32 || b.get(start - 32..start - 24) != Some(b"APETAGEX") {
        return Err(damaged);
    }
    Ok(start - 32)
}

// ---------------------------------------------------------------- Ogg

/// The CRC of an Ogg page (polynomial 0x04C11DB7, not reflected, starting
/// at 0), with its CRC field taken as zeros.
fn ogg_crc(page: &[u8]) -> u32 {
    const fn table() -> [u32; 256] {
        let mut t = [0; 256];
        let mut i = 0;
        while i < 256 {
            #[expect(clippy::cast_possible_truncation, reason = "i is below 256")]
            let mut r = (i as u32) << 24;
            let mut k = 0;
            while k < 8 {
                r = if r & 0x8000_0000 != 0 {
                    (r << 1) ^ 0x04C1_1DB7
                } else {
                    r << 1
                };
                k += 1;
            }
            t[i] = r;
            i += 1;
        }
        t
    }
    const TABLE: [u32; 256] = table();
    let mut crc = 0_u32;
    for (i, &byte) in page.iter().enumerate() {
        let byte = if (22..26).contains(&i) { 0 } else { byte };
        crc = (crc << 8) ^ TABLE[usize::from(crc.to_be_bytes()[0] ^ byte)];
    }
    crc
}

fn ogg(b: &[u8]) -> Check {
    let mut pos = 0;
    let mut serial = None;
    let mut expected_seq = 0_u32;
    let mut ended = false;
    while pos < b.len() {
        if ended {
            // Another stream chained after the last page, or junk.
            return if b.get(pos..pos + 4) == Some(b"OggS") {
                extra("a second Ogg stream")
            } else {
                bad("bytes after the last Ogg page")
            };
        }
        let Some(h) = b.get(pos..pos + 27) else {
            return bad("a cut Ogg page header");
        };
        if &h[0..4] != b"OggS" || h[4] != 0 {
            return bad("something that is not an Ogg page");
        }
        let flags = h[5];
        if flags & !0b111 != 0 {
            return bad("an Ogg page with unknown flags");
        }
        let page_serial = u32_le(h, 14).unwrap_or(0);
        let seq = u32_le(h, 18).unwrap_or(0);
        let crc = u32_le(h, 22).unwrap_or(0);
        let segments = usize::from(h[26]);
        let Some(table) = b.get(pos + 27..pos + 27 + segments) else {
            return bad("a cut Ogg segment table");
        };
        let body: usize = table.iter().map(|&n| usize::from(n)).sum();
        let page_len = 27 + segments + body;
        let Some(page) = b.get(pos..pos + page_len) else {
            return bad("a cut Ogg page");
        };
        if ogg_crc(page) != crc {
            return bad("an Ogg page with a bad CRC");
        }
        let begins = flags & 0b010 != 0;
        match serial {
            None => {
                if !begins {
                    return bad("an Ogg stream that does not begin");
                }
                let first = &page[27 + segments..];
                if !(first.starts_with(b"OpusHead") || first.starts_with(b"\x01vorbis")) {
                    return extra("an Ogg stream that is not Opus or Vorbis");
                }
                serial = Some(page_serial);
            }
            Some(s) => {
                if begins || s != page_serial {
                    return extra("a second Ogg stream");
                }
            }
        }
        if seq != expected_seq {
            return bad("Ogg pages out of order");
        }
        expected_seq = expected_seq.wrapping_add(1);
        ended = flags & 0b100 != 0;
        pos += page_len;
    }
    if serial.is_none() {
        return bad("no Ogg page");
    }
    Ok(())
}

// ---------------------------------------------------------------- WAV

/// The GUID of `WAVE_FORMAT_EXTENSIBLE`'s sub-formats after their first
/// two bytes (`KSDATAFORMAT_SUBTYPE_PCM` and `_IEEE_FLOAT`).
const SUBTYPE_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

fn wav_format(fmt: &[u8]) -> Check {
    let (Some(tag), Some(channels), Some(rate), Some(bits)) = (
        u16_le(fmt, 0),
        u16_le(fmt, 2),
        u32_le(fmt, 4),
        u16_le(fmt, 14),
    ) else {
        return bad("a cut WAV format");
    };
    let tag = if tag == 0xFFFE {
        // WAVE_FORMAT_EXTENSIBLE: the real tag is the sub-format's first
        // two bytes, with the rest of a known GUID.
        let (Some(extension), Some(sub)) = (u16_le(fmt, 16), u16_le(fmt, 24)) else {
            return bad("a cut extensible WAV format");
        };
        if extension < 22 || fmt.get(26..40) != Some(&SUBTYPE_TAIL[..]) {
            return extra("an extensible WAV format of an unknown kind");
        }
        sub
    } else {
        tag
    };
    let bits_ok = match tag {
        1 => matches!(bits, 8 | 16 | 24 | 32),
        3 => matches!(bits, 32 | 64),
        _ => return extra("a WAV that is not PCM or float"),
    };
    if !bits_ok || channels == 0 || rate == 0 {
        return bad("a WAV format with bad values");
    }
    Ok(())
}

fn wav(b: &[u8]) -> Check {
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return bad("not a RIFF WAVE file");
    }
    let Some(size) = u32_le(b, 4).and_then(|n| to_usize(u64::from(n))) else {
        return bad("a cut RIFF header");
    };
    if size.checked_add(8) != Some(b.len()) {
        return bad("a RIFF chunk of another length than the file");
    }
    let mut pos = 12;
    let mut format = false;
    let mut data = false;
    while pos < b.len() {
        let (Some(id), Some(len)) = (b.get(pos..pos + 4), u32_le(b, pos + 4)) else {
            return bad("a cut WAV chunk header");
        };
        let Some(len) = to_usize(u64::from(len)) else {
            return bad("a WAV chunk too long");
        };
        let start = pos + 8;
        let Some(body) = start.checked_add(len).and_then(|e| b.get(start..e)) else {
            return bad("a WAV chunk longer than the file");
        };
        match id {
            b"fmt " => {
                if format || data {
                    return bad("a misplaced WAV format");
                }
                wav_format(body)?;
                format = true;
            }
            b"data" => {
                if !format || data {
                    return bad("WAV data without a format, or twice");
                }
                data = true;
            }
            _ => {}
        }
        // A chunk of odd length is followed by a pad byte.
        pos = start + len + (len & 1);
    }
    if pos != b.len() || !data {
        return bad("a WAV without its data");
    }
    Ok(())
}

// ---------------------------------------------------------------- M4A

/// The deepest boxes are looked at.
const MAX_BOX_DEPTH: usize = 16;

/// Boxes that point a player elsewhere or hide boxes from this walk:
/// reference movies, redirects, and the compressed movie (`cmov`, whose
/// `cmvd` qtdemux decompresses and parses as a movie). Refused with their
/// own reason; the allow-lists below would refuse them too.
const REFUSED: [&[u8; 4]; 11] = [
    b"rmra", b"rdrf", b"rmda", b"rmdr", b"rmvc", b"rmcd", b"rmqu", b"rmcs", b"cmov", b"dcom",
    b"cmvd",
];

/// What may be inside a box of a given type.
enum Inside {
    /// Only these boxes, each walked by its own rule.
    Only(&'static [&'static [u8; 4]]),
    /// Any box but the [`REFUSED`] ones, none walked into: `udta`, whose
    /// tags qtdemux reads by their known names only (it looks for
    /// redirects and compressed movies only directly in `moov`).
    AnyLeaf,
}

/// What may be inside a box of type `parent`, as real encoders write it
/// (FDK AAC with `mp4mux`, ffmpeg, `GPAC`, Apple's tools, `iTunes` tags);
/// `None` for a box whose inside is not walked (a leaf: `mdat`, `ilst`,
/// `stts`, …). Any other box in an `Only` list refuses the file: a
/// fragmented movie (`moov/mvex`, `moof`), track references (so no
/// chapter track), compressed movies, unknown boxes.
fn allowed_inside(parent: [u8; 4]) -> Option<Inside> {
    Some(Inside::Only(match &parent {
        b"file" => &[
            b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"pdin",
        ],
        b"moov" => &[
            b"mvhd", b"trak", b"udta", b"meta", b"iods", b"uuid", b"free", b"skip",
        ],
        b"trak" => &[b"tkhd", b"edts", b"mdia", b"udta", b"free", b"skip"],
        b"edts" => &[b"elst"],
        b"mdia" => &[b"mdhd", b"hdlr", b"minf", b"free", b"skip"],
        // A QuickTime-style file has its data handler's `hdlr` here.
        b"minf" => &[b"smhd", b"hdlr", b"dinf", b"stbl", b"free", b"skip"],
        b"dinf" => &[b"dref"],
        b"stbl" => &[
            b"stsd", b"stts", b"stsc", b"stsz", b"stz2", b"stco", b"co64", b"stss", b"sgpd",
            b"sbgp", b"free", b"skip",
        ],
        b"udta" => return Some(Inside::AnyLeaf),
        // `moov/meta` (GPAC): the handler, the tags (not walked), the keys.
        b"meta" => &[b"hdlr", b"ilst", b"keys", b"free", b"skip"],
        _ => return None,
    }))
}

/// The sample descriptions an audio track may have, and the boxes inside
/// one.
const SAMPLE_ENTRIES: [&[u8; 4]; 3] = [b"mp4a", b"alac", b"Opus"];
const SAMPLE_ENTRY_BOXES: [&[u8; 4]; 7] = [
    b"esds", b"alac", b"dOps", b"btrt", b"chan", b"free", b"wave",
];

/// Inside a `QuickTime` `wave` box (a version 1 or 2 sound description's
/// codec data): the original format, a small `mp4a` atom, the elementary
/// stream descriptor, the channel layout, and the terminator.
const WAVE_BOXES: [&[u8; 4]; 5] = [b"frma", b"mp4a", b"esds", b"chan", &[0; 4]];

/// The MPEG-4 object types of audio an `esds` may name: MPEG-4 audio
/// (AAC and kin), MPEG-2 AAC (main, LC, SSR) and MPEG-1 audio.
const AUDIO_OBJECT_TYPES: [u8; 5] = [0x40, 0x66, 0x67, 0x68, 0x6B];

/// One box of `b` at `pos` within `end`: its type, and where its body
/// starts and ends.
fn next_box(b: &[u8], pos: usize, end: usize) -> Result<([u8; 4], usize, usize), Malformed> {
    let (Some(size), Some(kind)) = (u32_be(b, pos), b.get(pos + 4..pos + 8)) else {
        return Err(Malformed {
            why: "a cut box header",
            extra: false,
        });
    };
    let kind = [kind[0], kind[1], kind[2], kind[3]];
    let (header, total) = match size {
        0 => (8, end - pos),
        1 => {
            let Some(large) = u64_be(b, pos + 8).and_then(to_usize) else {
                return Err(Malformed {
                    why: "a cut large box header",
                    extra: false,
                });
            };
            (16, large)
        }
        n => (8, to_usize(u64::from(n)).unwrap_or(usize::MAX)),
    };
    if total < header || pos.checked_add(total).is_none_or(|e| e > end) {
        return Err(Malformed {
            why: "a box outside its parent",
            extra: false,
        });
    }
    Ok((kind, pos + header, pos + total))
}

/// Walks the boxes of `b[start..end]`, `depth` deep, inside a box of type
/// `parent`: each must be one [`allowed_inside`] it, and is checked.
fn walk_boxes(b: &[u8], start: usize, end: usize, depth: usize, parent: [u8; 4]) -> Check {
    if depth > MAX_BOX_DEPTH {
        return bad("boxes nested too deep");
    }
    let allowed = match allowed_inside(parent) {
        None => return Ok(()),
        Some(Inside::AnyLeaf) => {
            return each_box(b, start, end, |kind| {
                if REFUSED.contains(&&kind) {
                    extra("a reference movie, redirect or compressed movie box")
                } else {
                    Ok(())
                }
            });
        }
        Some(Inside::Only(allowed)) => allowed,
    };
    let mut pos = start;
    let mut handler = false;
    while pos < end {
        let (kind, body, next) = next_box(b, pos, end)?;
        if REFUSED.contains(&&kind) {
            return extra("a reference movie, redirect or compressed movie box");
        }
        if !allowed.contains(&&kind) {
            return extra("a box an audio file does not have there");
        }
        match &kind {
            b"hdlr" if &parent == b"mdia" => {
                if b.get(body + 8..body + 12) != Some(b"soun") {
                    return extra("a track that is not sound");
                }
                handler = true;
            }
            b"dref" => dref(b, body, next)?,
            b"stsd" => stsd(b, body, next)?,
            // An ISO full box: its children start after version and
            // flags; a QuickTime one (Apple's `mdta` tags) has neither, so
            // its first child, the handler, starts at once.
            b"meta" => {
                let children = if b.get(body + 4..body + 8) == Some(b"hdlr") {
                    body
                } else {
                    body + 4
                };
                walk_boxes(b, children, next, depth + 1, kind)?;
            }
            _ => walk_boxes(b, body, next, depth + 1, kind)?,
        }
        pos = next;
    }
    if &parent == b"mdia" && !handler {
        return bad("a track without a handler");
    }
    Ok(())
}

/// Calls `check` with the type of each box of `b[start..end]`.
fn each_box(b: &[u8], start: usize, end: usize, mut check: impl FnMut([u8; 4]) -> Check) -> Check {
    let mut pos = start;
    while pos < end {
        let (kind, _, next) = next_box(b, pos, end)?;
        check(kind)?;
        pos = next;
    }
    Ok(())
}

/// A data reference box: every entry a self-contained `url ` or `urn `.
fn dref(b: &[u8], body: usize, end: usize) -> Check {
    let Some(count) = u32_be(b, body + 4) else {
        return bad("a cut data reference");
    };
    let mut pos = body + 8;
    let mut seen = 0_u32;
    while pos < end {
        let (kind, entry, next) = next_box(b, pos, end)?;
        let flags = u32_be(b, entry).unwrap_or(0) & 0x00FF_FFFF;
        if !matches!(&kind, b"url " | b"urn ") || flags & 1 == 0 {
            return extra("a data reference to another file");
        }
        seen = seen.saturating_add(1);
        pos = next;
    }
    if seen != count || seen == 0 {
        return bad("a data reference of a bad count");
    }
    Ok(())
}

/// The sample descriptions: each an audio one of [`SAMPLE_ENTRIES`]
/// (sound description version 0 or 1), with only
/// [`SAMPLE_ENTRY_BOXES`] after its fields.
fn stsd(b: &[u8], body: usize, end: usize) -> Check {
    let Some(count) = u32_be(b, body + 4) else {
        return bad("a cut sample description");
    };
    let mut pos = body + 8;
    let mut seen = 0_u32;
    while pos < end {
        let (kind, entry, next) = next_box(b, pos, end)?;
        if !SAMPLE_ENTRIES.contains(&&kind) {
            return extra("a sample description that is not audio");
        }
        // 6 reserved bytes and a data reference index, then the sound
        // description (QuickTime File Format): version 0 has 20 bytes of
        // fields, version 1 36, version 2 56 (its size of the structure,
        // header included, at +28 after the always-3, 16, -2, 0 and
        // 65536 fields, then 72).
        let fields = match b.get(entry + 8..entry + 10) {
            Some([0, 0]) => 28,
            Some([0, 1]) => 44,
            Some([0, 2]) if u32_be(b, entry + 28) == Some(72) => 64,
            _ => return extra("a sound description of an unknown version"),
        };
        let mut inner = entry + fields;
        if inner > next {
            return bad("a cut sound description");
        }
        while inner < next {
            let (child, body, after) = next_box(b, inner, next)?;
            if !SAMPLE_ENTRY_BOXES.contains(&&child) {
                return extra("a box an audio sample description does not have");
            }
            match &child {
                b"esds" => esds(b, body, after)?,
                b"wave" if fields > 28 => wave(b, body, after)?,
                b"wave" => return extra("a QuickTime wave box in a version 0 description"),
                _ => {}
            }
            inner = after;
        }
        seen = seen.saturating_add(1);
        pos = next;
    }
    if seen != count || seen == 0 {
        return bad("a sample description of a bad count");
    }
    Ok(())
}

/// A `QuickTime` `wave` box: only [`WAVE_BOXES`], its `esds` checked.
fn wave(b: &[u8], body: usize, end: usize) -> Check {
    let mut pos = body;
    while pos < end {
        let (kind, inner, next) = next_box(b, pos, end)?;
        if !WAVE_BOXES.contains(&&kind) {
            return extra("a box a QuickTime wave box does not have");
        }
        if &kind == b"esds" {
            esds(b, inner, next)?;
        }
        pos = next;
    }
    Ok(())
}

/// The length of an MPEG-4 descriptor at `pos` (1 to 4 bytes, 7 bits
/// each, the top bit saying more follow) and where its body starts.
fn descriptor_len(b: &[u8], mut pos: usize) -> Option<(usize, usize)> {
    let mut len = 0_usize;
    for _ in 0..4 {
        let byte = *b.get(pos)?;
        pos += 1;
        len = (len << 7) | usize::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Some((len, pos));
        }
    }
    None
}

/// An elementary stream descriptor box: an `ES_Descriptor` without a URL
/// (which would name the stream elsewhere), whose decoder configuration
/// names an audio object type ([`AUDIO_OBJECT_TYPES`]).
fn esds(b: &[u8], body: usize, end: usize) -> Check {
    let field = b.get(..end).unwrap_or_default();
    // Version and flags, then the ES_Descriptor (tag 3).
    let mut pos = body + 4;
    if field.get(pos) != Some(&3) {
        return bad("an elementary stream descriptor without its descriptor");
    }
    let Some((_, start)) = descriptor_len(field, pos + 1) else {
        return bad("a cut elementary stream descriptor");
    };
    // ES_ID, then flags: stream dependence (2 more bytes), URL (a length
    // and the URL), OCR stream (2 more bytes).
    let Some(&flags) = field.get(start + 2) else {
        return bad("a cut elementary stream descriptor");
    };
    if flags & 0x40 != 0 {
        return extra("an elementary stream named by a URL");
    }
    pos = start + 3;
    if flags & 0x80 != 0 {
        pos += 2;
    }
    if flags & 0x20 != 0 {
        pos += 2;
    }
    // The decoder configuration (tag 4), its first byte the object type.
    if field.get(pos) != Some(&4) {
        return bad("an elementary stream without its decoder configuration");
    }
    let Some((_, config)) = descriptor_len(field, pos + 1) else {
        return bad("a cut decoder configuration");
    };
    match field.get(config) {
        Some(object) if AUDIO_OBJECT_TYPES.contains(object) => Ok(()),
        Some(_) => extra("an elementary stream that is not audio"),
        None => bad("a cut decoder configuration"),
    }
}

fn m4a(b: &[u8]) -> Check {
    let (first, _, _) = next_box(b, 0, b.len())?;
    if &first != b"ftyp" {
        return bad("an M4A that does not begin with ftyp");
    }
    if MediaKind::detect(b) != Some(MediaKind::M4a) {
        return extra("an MP4 without an audio brand");
    }
    let (mut moov, mut mdat) = (0, 0);
    let mut pos = 0;
    while pos < b.len() {
        let (kind, _, next) = next_box(b, pos, b.len())?;
        match &kind {
            b"moov" => moov += 1,
            b"mdat" => mdat += 1,
            _ => {}
        }
        pos = next;
    }
    if moov != 1 || mdat == 0 {
        return bad("an M4A without one moov and its mdat");
    }
    walk_boxes(b, 0, b.len(), 0, *b"file")
}

#[cfg(test)]
mod tests;
