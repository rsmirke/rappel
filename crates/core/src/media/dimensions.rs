//! An image's size, read from its header before anything decodes it
//! (ADR 0063). The decoder, glycin's loader, runs in a sandbox with a
//! memory limit where the system allows; where it does not (a container
//! without user namespaces, ADR 0055), it is a plain process without one,
//! and a small file whose header claims 100,000 × 100,000 pixels makes it
//! ask for 30 GB. So the app reads the size here first and sends no image
//! past its limits to the decoder at all.
//!
//! Each format's size is where its specification puts it: PNG's `IHDR`,
//! the first chunk; a JPEG's first frame header (`SOFn`, every marker
//! before it stepped over by its length); a WebP's first chunk (`VP8 `,
//! `VP8L` or the `VP8X` canvas); a GIF's logical screen and its first
//! image, whichever is larger. One pass, bounds checked on every read:
//! linear and never panics. A header that cannot be read is `None`: the
//! file is damaged, and is not decoded either.

use super::MediaKind;

fn u16_be(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

fn u16_le(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn u24_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(3)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], 0]))
}

fn u32_be(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The width and height an image of `kind` declares in `bytes` (its
/// whole file, or at least its header), or `None` if they cannot be read
/// or `kind` is not an image.
#[must_use]
pub fn image_size(kind: MediaKind, bytes: &[u8]) -> Option<(u32, u32)> {
    match kind {
        MediaKind::Png => png(bytes),
        MediaKind::Jpeg => jpeg(bytes),
        MediaKind::WebP => webp(bytes),
        MediaKind::Gif => gif(bytes),
        MediaKind::Ogg | MediaKind::Mp3 | MediaKind::M4a | MediaKind::Wav => None,
    }
}

/// `IHDR` must be the first chunk (PNG §11.2.2).
fn png(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(12..16)? == b"IHDR").then_some(())?;
    Some((u32_be(b, 16)?, u32_be(b, 20)?))
}

/// The first frame header (`SOF0` to `SOF15`, not `DHT`, `JPG` or `DAC`),
/// found by stepping over each marker segment by its length (ITU T.81
/// B.1.1); `None` at a scan (`SOS`) or the end before one.
fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    let mut pos = 2;
    loop {
        if *b.get(pos)? != 0xFF {
            return None;
        }
        // Fill bytes: any number of 0xFF before a marker.
        while *b.get(pos + 1)? == 0xFF {
            pos += 1;
        }
        let marker = *b.get(pos + 1)?;
        pos += 2;
        match marker {
            // Markers without a length.
            0x01 | 0xD0..=0xD7 => continue,
            // The end, or a scan, with no frame header before it.
            0xD8..=0xDA => return None,
            _ => {}
        }
        let len = usize::from(u16_be(b, pos)?);
        if len < 2 {
            return None;
        }
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16_be(b, pos + 3)?;
            let width = u16_be(b, pos + 5)?;
            return Some((u32::from(width), u32::from(height)));
        }
        pos += len;
    }
}

/// The first chunk after `RIFF` … `WEBP`: a lossy frame (`VP8 `, its key
/// frame's 14-bit sizes), a lossless one (`VP8L`, 14 bits each, less one)
/// or the extended format's canvas (`VP8X`, 24 bits each, less one).
fn webp(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(0..4)? == b"RIFF" && b.get(8..12)? == b"WEBP").then_some(())?;
    match b.get(12..16)? {
        b"VP8 " => {
            (b.get(23..26)? == [0x9D, 0x01, 0x2A]).then_some(())?;
            let width = u16_le(b, 26)? & 0x3FFF;
            let height = u16_le(b, 28)? & 0x3FFF;
            Some((u32::from(width), u32::from(height)))
        }
        b"VP8L" => {
            (*b.get(20)? == 0x2F).then_some(())?;
            let bits = u32_le(b, 21)?;
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => Some((u24_le(b, 24)? + 1, u24_le(b, 27)? + 1)),
        _ => None,
    }
}

/// The logical screen and the first image's size, the larger of each:
/// a decoder lays the frame out on the screen.
fn gif(b: &[u8]) -> Option<(u32, u32)> {
    let screen = (u32::from(u16_le(b, 6)?), u32::from(u16_le(b, 8)?));
    let packed = *b.get(10)?;
    let mut pos = 13;
    if packed & 0x80 != 0 {
        pos += 3 << ((packed & 0x07) + 1);
    }
    loop {
        match *b.get(pos)? {
            // An extension: its label, then sub-blocks to an empty one.
            0x21 => {
                pos += 2;
                loop {
                    let size = usize::from(*b.get(pos)?);
                    pos += 1 + size;
                    if size == 0 {
                        break;
                    }
                }
            }
            // An image descriptor: left, top, width, height.
            0x2C => {
                let left = u32::from(u16_le(b, pos + 1)?);
                let top = u32::from(u16_le(b, pos + 3)?);
                let width = u32::from(u16_le(b, pos + 5)?);
                let height = u32::from(u16_le(b, pos + 7)?);
                return Some((screen.0.max(left + width), screen.1.max(top + height)));
            }
            // The trailer: no image.
            0x3B => return Some(screen),
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests;
