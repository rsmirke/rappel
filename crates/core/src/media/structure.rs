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
//!   end, which may hold an `ID3v1` tag. Nothing else (an APE tag is
//!   refused).
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
//!   sample description an audio one (`mp4a`, `alac`, `Opus`), every
//!   track's handler `soun`. `GstPlay` follows a redirect, so for M4A
//!   "never a URI" rests on this check.
//!
//! Images are not checked here: their decoder runs in glycin's sandbox
//! and never reads a URI (ADR 0055). Every check is one pass over the
//! bytes (linear), with bounds checked on every read: it never panics.

use super::MediaKind;

/// Why an audio file is refused: what was wrong, for the logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

type Check = Result<(), Malformed>;

fn bad(why: &'static str) -> Check {
    Err(Malformed(why))
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
    // No APE tag: its items are not checked here, so it is refused (it is
    // not a frame).
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
            return bad("bytes after the last Ogg page");
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
                    return bad("an Ogg stream that is not Opus or Vorbis");
                }
                serial = Some(page_serial);
            }
            Some(s) => {
                if begins || s != page_serial {
                    return bad("a second Ogg stream");
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
        let (Some(extra), Some(sub)) = (u16_le(fmt, 16), u16_le(fmt, 24)) else {
            return bad("a cut extensible WAV format");
        };
        if extra < 22 || fmt.get(26..40) != Some(&SUBTYPE_TAIL[..]) {
            return bad("an extensible WAV format of an unknown kind");
        }
        sub
    } else {
        tag
    };
    let bits_ok = match tag {
        1 => matches!(bits, 8 | 16 | 24 | 32),
        3 => matches!(bits, 32 | 64),
        _ => return bad("a WAV that is not PCM or float"),
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

/// The boxes an audio file may have inside a box of type `parent`, as
/// real encoders write them (FDK AAC with `mp4mux`, Apple's and ffmpeg's
/// M4A, iTunes tags); `None` for a box whose inside is not walked. Any
/// other box there refuses the file: a fragmented movie (`moov/mvex`,
/// `moof`), track references, compressed movies, unknown boxes.
fn allowed_inside(parent: [u8; 4]) -> Option<&'static [&'static [u8; 4]]> {
    Some(match &parent {
        b"file" => &[b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide"],
        b"moov" => &[b"mvhd", b"trak", b"udta", b"iods", b"free", b"skip"],
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
        b"udta" => &[b"meta", b"free", b"skip"],
        // iTunes tags: the handler and the item list (not walked: tags).
        b"meta" => &[b"hdlr", b"ilst", b"free", b"skip"],
        _ => return None,
    })
}

/// The sample descriptions an audio track may have, and the boxes inside
/// one.
const SAMPLE_ENTRIES: [&[u8; 4]; 3] = [b"mp4a", b"alac", b"Opus"];
const SAMPLE_ENTRY_BOXES: [&[u8; 4]; 6] = [b"esds", b"alac", b"dOps", b"btrt", b"chan", b"free"];

/// One box of `b` at `pos` within `end`: its type, and where its body
/// starts and ends.
fn next_box(b: &[u8], pos: usize, end: usize) -> Result<([u8; 4], usize, usize), Malformed> {
    let (Some(size), Some(kind)) = (u32_be(b, pos), b.get(pos + 4..pos + 8)) else {
        return Err(Malformed("a cut box header"));
    };
    let kind = [kind[0], kind[1], kind[2], kind[3]];
    let (header, total) = match size {
        0 => (8, end - pos),
        1 => {
            let Some(large) = u64_be(b, pos + 8).and_then(to_usize) else {
                return Err(Malformed("a cut large box header"));
            };
            (16, large)
        }
        n => (8, to_usize(u64::from(n)).unwrap_or(usize::MAX)),
    };
    if total < header || pos.checked_add(total).is_none_or(|e| e > end) {
        return Err(Malformed("a box outside its parent"));
    }
    Ok((kind, pos + header, pos + total))
}

/// Walks the boxes of `b[start..end]`, `depth` deep, inside a box of type
/// `parent`: each must be one [`allowed_inside`] it, and is checked.
fn walk_boxes(b: &[u8], start: usize, end: usize, depth: usize, parent: [u8; 4]) -> Check {
    if depth > MAX_BOX_DEPTH {
        return bad("boxes nested too deep");
    }
    let Some(allowed) = allowed_inside(parent) else {
        return Ok(());
    };
    let mut pos = start;
    let mut handler = false;
    while pos < end {
        let (kind, body, next) = next_box(b, pos, end)?;
        if REFUSED.contains(&&kind) {
            return bad("a reference movie, redirect or compressed movie box");
        }
        if !allowed.contains(&&kind) {
            return bad("a box an audio file does not have there");
        }
        match &kind {
            b"hdlr" if &parent == b"mdia" => {
                if b.get(body + 8..body + 12) != Some(b"soun") {
                    return bad("a track that is not sound");
                }
                handler = true;
            }
            b"dref" => dref(b, body, next)?,
            b"stsd" => stsd(b, body, next)?,
            // A full box: its children start after version and flags.
            b"meta" => walk_boxes(b, body + 4, next, depth + 1, kind)?,
            _ => walk_boxes(b, body, next, depth + 1, kind)?,
        }
        pos = next;
    }
    if &parent == b"mdia" && !handler {
        return bad("a track without a handler");
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
            return bad("a data reference to another file");
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
            return bad("a sample description that is not audio");
        }
        // 6 reserved bytes and a data reference index, then the sound
        // description: version 0 has 20 bytes of fields, version 1 36.
        let fields = match b.get(entry + 8..entry + 10) {
            Some([0, 0]) => 28,
            Some([0, 1]) => 44,
            _ => return bad("a sound description of an unknown version"),
        };
        let mut inner = entry + fields;
        if inner > next {
            return bad("a cut sound description");
        }
        while inner < next {
            let (child, _, after) = next_box(b, inner, next)?;
            if !SAMPLE_ENTRY_BOXES.contains(&&child) {
                return bad("a box an audio sample description does not have");
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

fn m4a(b: &[u8]) -> Check {
    let (first, _, _) = next_box(b, 0, b.len())?;
    if &first != b"ftyp" {
        return bad("an M4A that does not begin with ftyp");
    }
    if MediaKind::detect(b) != Some(MediaKind::M4a) {
        return bad("an MP4 without an audio brand");
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
