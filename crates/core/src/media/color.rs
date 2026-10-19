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
//! chunk (and the flag that announces it), a GIF's `ICCRGBG1012`
//! application extension (which the `gif` crate used by glycin's loader
//! returns as the image's profile). The pixels are then shown as sRGB,
//! which most images are; a wide-gamut photo's colours are a little less
//! saturated.
//!
//! Each walk is one pass over the file's chunks or segments with bounds
//! checked on every read; a structure that does not hold is `None` (the
//! file is damaged and is not decoded).

use super::{MediaKind, bytes};

fn u16_be(b: &[u8], at: usize) -> Option<usize> {
    bytes::u16_be(b, at).map(usize::from)
}

fn u32_be(b: &[u8], at: usize) -> Option<usize> {
    bytes::u32_be(b, at).and_then(|n| usize::try_from(n).ok())
}

fn u32_le(b: &[u8], at: usize) -> Option<usize> {
    bytes::u32_le(b, at).and_then(|n| usize::try_from(n).ok())
}

/// `bytes`, an image of `kind`, without its colour profile; `None` if its
/// structure does not hold. The same bytes when it has none.
#[must_use]
pub fn without_color_profile(kind: MediaKind, bytes: &[u8]) -> Option<Vec<u8>> {
    match kind {
        MediaKind::Png => png(bytes),
        MediaKind::Jpeg => jpeg(bytes),
        MediaKind::WebP => webp(bytes),
        MediaKind::Gif => gif(bytes),
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

/// The sub-blocks starting at `pos` (each a size byte and that many
/// bytes, ended by an empty one): where they end.
fn sub_blocks_end(b: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let size = usize::from(*b.get(pos)?);
        pos = pos.checked_add(1 + size)?;
        if size == 0 {
            return (pos <= b.len()).then_some(pos);
        }
    }
}

/// Every block copied but an application extension named `ICCRGBG1012`
/// (an ICC profile, `GIF89a`'s convention), up to the trailer. The file's
/// end, or a byte that starts no block, where a block should start is
/// taken for the trailer, as decoders do: the blocks before it are kept,
/// the stray bytes dropped (never passed on unread), and a trailer added.
/// Such GIFs are common, and showed their frames before the walk.
fn gif(b: &[u8]) -> Option<Vec<u8>> {
    let packed = *b.get(10)?;
    let mut pos = 13;
    if packed & 0x80 != 0 {
        pos += 3 << ((packed & 0x07) + 1);
    }
    let mut out = b.get(..pos)?.to_vec();
    loop {
        match b.get(pos).copied() {
            Some(0x21) => {
                let label = *b.get(pos + 1)?;
                let end = sub_blocks_end(b, pos + 2)?;
                let icc = label == 0xFF
                    && b.get(pos + 2) == Some(&11)
                    && b.get(pos + 3..pos + 14) == Some(b"ICCRGBG1012");
                if !icc {
                    out.extend_from_slice(&b[pos..end]);
                }
                pos = end;
            }
            Some(0x2C) => {
                let flags = *b.get(pos + 9)?;
                let mut data = pos + 10;
                if flags & 0x80 != 0 {
                    data += 3 << ((flags & 0x07) + 1);
                }
                // The LZW minimum code size, then the image's sub-blocks.
                let end = sub_blocks_end(b, data.checked_add(1)?)?;
                out.extend_from_slice(b.get(pos..end)?);
                pos = end;
            }
            // The trailer, the file's end or stray bytes: the end.
            Some(_) | None => {
                out.push(0x3B);
                return Some(out);
            }
        }
    }
}

#[cfg(test)]
mod tests;
