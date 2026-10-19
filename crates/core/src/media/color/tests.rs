//! Images with an ICC profile, made by Pillow (`fixtures/images/README.md`).

use proptest::prelude::*;

use super::*;
use crate::media::image_size;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../fixtures/images/", $name)).as_slice()
    };
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn the_profile_is_taken_out_and_the_rest_kept() {
    for (kind, bytes, marker) in [
        (MediaKind::Png, fixture!("4x3-icc.png"), b"iCCP".as_slice()),
        (MediaKind::Jpeg, fixture!("4x3-icc.jpg"), b"ICC_PROFILE\0"),
        (MediaKind::WebP, fixture!("4x3-icc.webp"), b"ICCP"),
    ] {
        assert!(has(bytes, marker), "{kind:?}: the fixture has a profile");
        let out = without_color_profile(kind, bytes).unwrap();
        assert!(!has(&out, marker), "{kind:?}");
        assert!(
            out.len() + 200 < bytes.len(),
            "{kind:?}: the profile is gone"
        );
        assert_eq!(MediaKind::detect(&out), Some(kind));
        assert_eq!(image_size(kind, &out), Some((4, 3)), "{kind:?}");
        // Taking it out again changes nothing.
        assert_eq!(
            without_color_profile(kind, &out).as_deref(),
            Some(out.as_slice())
        );
    }
    // The WebP's flag and size follow.
    let out = without_color_profile(MediaKind::WebP, fixture!("4x3-icc.webp")).unwrap();
    assert_eq!(&out[12..16], b"VP8X");
    assert_eq!(out[20] & 0x20, 0);
    assert_eq!(
        usize::try_from(u32::from_le_bytes(out[4..8].try_into().unwrap())).unwrap(),
        out.len() - 8
    );
}

/// Where the blocks of a GIF start: after its header, screen descriptor
/// and global colour table.
fn gif_blocks_at(gif: &[u8]) -> usize {
    let packed = gif[10];
    let table = if packed & 0x80 != 0 {
        3 << ((packed & 0x07) + 1)
    } else {
        0
    };
    13 + table
}

/// `GIF89a`'s ICC application extension: `ICCRGBG1012`, then a made-up
/// profile of 588 bytes in sub-blocks.
fn icc_extension() -> Vec<u8> {
    let mut ext = b"\x21\xff\x0bICCRGBG1012".to_vec();
    for chunk in [vec![0x41_u8; 255], vec![0x42; 255], vec![0x43; 78]] {
        ext.push(u8::try_from(chunk.len()).unwrap());
        ext.extend_from_slice(&chunk);
    }
    ext.push(0);
    ext
}

const NETSCAPE: &[u8] = b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00";

/// The real GIF with an ICC application extension put after its colour
/// table, as `GIF89a`'s convention writes it, and a Netscape loop extension
/// beside it.
fn gif_with_icc() -> Vec<u8> {
    let base = fixture!("11x7.gif");
    let at = gif_blocks_at(base);
    let mut gif = base[..at].to_vec();
    gif.extend_from_slice(&icc_extension());
    gif.extend_from_slice(NETSCAPE);
    gif.extend_from_slice(&base[at..]);
    gif
}

/// The ICC extension is taken out, the Netscape extension and the image
/// kept byte for byte.
#[test]
fn a_gif_s_icc_extension_is_taken_out() {
    let base = fixture!("11x7.gif");
    let at = gif_blocks_at(base);
    let gif = gif_with_icc();
    assert!(has(&gif, b"ICCRGBG1012"), "the fixture has a profile");
    let out = without_color_profile(MediaKind::Gif, &gif).unwrap();
    assert!(!has(&out, b"ICCRGBG1012"));
    let mut expected = base[..at].to_vec();
    expected.extend_from_slice(NETSCAPE);
    expected.extend_from_slice(&base[at..]);
    assert_eq!(out, expected);
    assert_eq!(image_size(MediaKind::Gif, &out), Some((11, 7)));
    // The animated one keeps its frames.
    let animated = fixture!("9x6-animated.gif");
    assert_eq!(
        without_color_profile(MediaKind::Gif, animated).as_deref(),
        Some(animated)
    );
}

/// A GIF without its trailer, or with stray bytes after its last block,
/// is still decoded: what comes out ends with a trailer, holds no stray
/// byte, and still has a size.
#[test]
fn a_gif_without_a_trailer_or_with_stray_bytes_is_kept() {
    let base = fixture!("11x7.gif");
    assert_eq!(base.last(), Some(&0x3B));
    let body = &base[..base.len() - 1];
    let no_trailer = gif_with_icc()[..gif_with_icc().len() - 1].to_vec();
    let mut stray = gif_with_icc();
    stray.pop();
    stray.extend_from_slice(b"\x00\x00junk\x3b");
    let mut expected = body.to_vec();
    expected.push(0x3B);
    // The same without its trailer, the ICC extension gone.
    assert_eq!(
        without_color_profile(MediaKind::Gif, body).as_deref(),
        Some(base)
    );
    for (what, gif) in [("no trailer", no_trailer), ("stray bytes", stray)] {
        let out = without_color_profile(MediaKind::Gif, &gif).unwrap();
        assert!(!has(&out, b"ICCRGBG1012"), "{what}");
        assert!(!has(&out, b"junk"), "{what}");
        assert_eq!(out.last(), Some(&0x3B), "{what}");
        assert_eq!(image_size(MediaKind::Gif, &out), Some((11, 7)), "{what}");
        let mut kept = out.clone();
        kept.pop();
        let at = gif_blocks_at(base);
        assert_eq!(&kept[..at], &base[..at], "{what}");
        assert!(kept.ends_with(&body[at..]), "{what}: the image is kept");
    }
}

#[test]
fn images_without_a_profile_are_kept_as_they_are() {
    for (kind, bytes) in [
        (MediaKind::Png, fixture!("13x9.png")),
        (MediaKind::Jpeg, fixture!("3x2.jpg")),
        (MediaKind::Jpeg, fixture!("6x3-exif.jpg")),
        (MediaKind::WebP, fixture!("7x5-lossy.webp")),
        (MediaKind::WebP, fixture!("9x6-animated.webp")),
        (MediaKind::Gif, fixture!("11x7.gif")),
    ] {
        assert_eq!(
            without_color_profile(kind, bytes).as_deref(),
            Some(bytes),
            "{kind:?}"
        );
    }
}

#[test]
fn broken_structures_are_refused() {
    let png = fixture!("4x3-icc.png");
    // A chunk longer than the file, a JPEG segment of length 1, junk.
    let mut long = png[..8].to_vec();
    long.extend_from_slice(&[0x7F, 0xFF, 0xFF, 0xFF]);
    long.extend_from_slice(b"IHDR");
    assert_eq!(without_color_profile(MediaKind::Png, &long), None);
    assert_eq!(
        without_color_profile(MediaKind::Jpeg, b"\xff\xd8\xff\xe2\x00\x01"),
        None
    );
    assert_eq!(
        without_color_profile(MediaKind::Jpeg, b"\xff\xd8junk"),
        None
    );
    assert_eq!(
        without_color_profile(MediaKind::WebP, b"RIFF\x10\0\0\0WEBPVP8X\xff\xff\xff\x7f"),
        None
    );
    assert_eq!(without_color_profile(MediaKind::Wav, png), None);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// Damaged and cut images never make the walk panic, and what comes out
    /// holds no profile the walk can read: taking it out again changes
    /// nothing.
    #[test]
    fn damaged_images_never_panic(
        which in 0_usize..4,
        cut in any::<prop::sample::Index>(),
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
    ) {
        let gif = gif_with_icc();
        let (kind, bytes) = [
            (MediaKind::Png, fixture!("4x3-icc.png")),
            (MediaKind::Jpeg, fixture!("4x3-icc.jpg")),
            (MediaKind::WebP, fixture!("4x3-icc.webp")),
            (MediaKind::Gif, gif.as_slice()),
        ][which];
        let mut file = bytes.to_vec();
        for (at, value) in flips {
            let i = at.index(file.len());
            file[i] = value;
        }
        file.truncate(cut.index(file.len() + 1));
        // What comes out is itself free of a profile the walk can find:
        // taking it out again changes nothing.
        if let Some(out) = without_color_profile(kind, &file) {
            prop_assert_eq!(without_color_profile(kind, &out), Some(out.clone()));
        }
    }
}
