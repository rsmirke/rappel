//! An image's embedded colour profile, taken out before it is decoded
//! (ADR 0063).
//!
//! glycin's loader unpacks an image's ICC profile in its sandbox and sends
//! it to the app's process, where glycin parses it with lcms2 and applies
//! it to the pixels (`glycin` 3.1, `dbus.rs`, whatever the loader's
//! options). So a profile is parsed outside the sandbox, and PNG's `iCCP`
//! is compressed: 64 KiB of it grew Rappel's process by about 900 MiB in
//! the tests, and the 10 MB a file may have would unpack to gigabytes.
//! Rappel therefore sends the decoder the image without its profile: PNG's
//! `iCCP` chunk, a JPEG's `ICC_PROFILE` segments (APP2), a WebP's `ICCP`
//! chunk (and the flag that announces it). The pixels are then shown as
//! sRGB, which most images are; a wide-gamut photo's colours are a little
//! less saturated. GIF's ICC application extension is not read by glycin's
//! GIF loader, so a GIF is sent as it is.
//!
//! Each walk is one pass over the file's chunks or segments with bounds
//! checked on every read; a structure that does not hold is `None` (the
//! file is damaged and is not decoded).

use super::MediaKind;

fn u16_be(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(usize::from(u16::from_be_bytes([s[0], s[1]])))
}

fn u32_be(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_be_bytes([s[0], s[1], s[2], s[3]])).ok()
}

fn u32_le(b: &[u8], at: usize) -> Option<usize> {
    let s = b.get(at..at.checked_add(4)?)?;
    usize::try_from(u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok()
}

/// `bytes`, an image of `kind`, without its colour profile; `None` if its
/// structure does not hold. The same bytes when it has none.
#[must_use]
pub fn without_color_profile(kind: MediaKind, bytes: &[u8]) -> Option<Vec<u8>> {
    match kind {
        MediaKind::Png => png(bytes),
        MediaKind::Jpeg => jpeg(bytes),
        MediaKind::WebP => webp(bytes),
        MediaKind::Gif => Some(bytes.to_vec()),
        MediaKind::Ogg | MediaKind::Mp3 | MediaKind::M4a | MediaKind::Wav => None,
    }
}

/// Every chunk (length, type, data, CRC) copied but `iCCP`, up to and with
/// `IEND`; what follows `IEND` is dropped, as decoders ignore it.
fn png(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(b.get(..8)?);
    let mut pos = 8;
    loop {
        let len = u32_be(b, pos)?;
        let end = pos.checked_add(12)?.checked_add(len)?;
        let chunk = b.get(pos..end)?;
        let kind = &chunk[4..8];
        if kind != b"iCCP" {
            out.extend_from_slice(chunk);
        }
        if kind == b"IEND" {
            return Some(out);
        }
        pos = end;
    }
}

/// Every segment copied but APP2 segments holding `ICC_PROFILE`, up to the
/// first scan, then the rest as it is (entropy-coded data has no
/// profile).
fn jpeg(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(b.get(..2)?);
    let mut pos = 2;
    loop {
        if *b.get(pos)? != 0xFF {
            return None;
        }
        let marker = *b.get(pos + 1)?;
        if marker == 0xFF {
            // A fill byte.
            out.push(0xFF);
            pos += 1;
            continue;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            out.extend_from_slice(&b[pos..pos + 2]);
            pos += 2;
            continue;
        }
        if matches!(marker, 0xD9 | 0xDA) {
            // The end, or the first scan: the rest as it is.
            out.extend_from_slice(&b[pos..]);
            return Some(out);
        }
        let len = u16_be(b, pos + 2)?;
        if len < 2 {
            return None;
        }
        let end = pos.checked_add(2 + len)?;
        let segment = b.get(pos..end)?;
        let icc = marker == 0xE2 && segment.get(4..16) == Some(b"ICC_PROFILE\0");
        if !icc {
            out.extend_from_slice(segment);
        }
        pos = end;
    }
}

/// Every RIFF chunk (padded to an even length) copied but `ICCP`; the
/// `VP8X` flag that announces a profile cleared and the RIFF size made
/// that of what is left.
fn webp(b: &[u8]) -> Option<Vec<u8>> {
    (b.get(0..4)? == b"RIFF" && b.get(8..12)? == b"WEBP").then_some(())?;
    let riff_end = u32_le(b, 4)?.checked_add(8)?.min(b.len());
    let mut out = b[..12].to_vec();
    let mut pos = 12;
    while pos < riff_end {
        let len = u32_le(b, pos + 4)?;
        let padded = len.checked_add(len % 2)?;
        let end = pos.checked_add(8)?.checked_add(padded)?;
        let chunk = b.get(pos..end.min(riff_end))?;
        let kind = chunk.get(..4)?;
        if kind == b"ICCP" {
            pos = end;
            continue;
        }
        let at = out.len();
        out.extend_from_slice(chunk);
        if kind == b"VP8X" {
            // Flag bit 5: an ICC profile follows.
            *out.get_mut(at + 8)? &= !0x20;
        }
        pos = end;
    }
    let size = u32::try_from(out.len() - 8).ok()?;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Some(out)
}

#[cfg(test)]
mod tests;
