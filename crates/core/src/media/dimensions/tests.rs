//! Real images made by Pillow (`fixtures/images/README.md`) and hostile
//! headers.

use std::time::Duration;

use proptest::prelude::*;

use super::*;
use crate::test_support::within;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../fixtures/images/", $name)).as_slice()
    };
}

fn fixtures() -> [(MediaKind, &'static [u8], (u32, u32)); 9] {
    [
        (MediaKind::Png, fixture!("13x9.png"), (13, 9)),
        (MediaKind::Jpeg, fixture!("3x2.jpg"), (3, 2)),
        (MediaKind::Jpeg, fixture!("5x4-progressive.jpg"), (5, 4)),
        // An EXIF segment before the frame header (its own thumbnail,
        // if any, is inside it and stepped over).
        (MediaKind::Jpeg, fixture!("6x3-exif.jpg"), (6, 3)),
        (MediaKind::WebP, fixture!("7x5-lossy.webp"), (7, 5)),
        (MediaKind::WebP, fixture!("7x5-lossless.webp"), (7, 5)),
        (MediaKind::WebP, fixture!("9x6-animated.webp"), (9, 6)),
        (MediaKind::Gif, fixture!("11x7.gif"), (11, 7)),
        (MediaKind::Gif, fixture!("9x6-animated.gif"), (9, 6)),
    ]
}

#[test]
fn real_images_give_their_size() {
    for (kind, bytes, size) in fixtures() {
        assert_eq!(MediaKind::detect(bytes), Some(kind));
        assert_eq!(image_size(kind, bytes), Some(size), "{kind:?} {size:?}");
    }
    // Not an image.
    assert_eq!(image_size(MediaKind::Wav, fixture!("13x9.png")), None);
}

/// What a header claims is what is read, however large: the app compares
/// it with its limits.
#[test]
fn huge_claims_are_read_as_they_are() {
    let mut png = fixture!("13x9.png").to_vec();
    png[16..20].copy_from_slice(&100_000_u32.to_be_bytes());
    png[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(image_size(MediaKind::Png, &png), Some((100_000, u32::MAX)));
    let mut gif = fixture!("11x7.gif").to_vec();
    gif[6..10].copy_from_slice(&[0xFF; 4]);
    assert_eq!(image_size(MediaKind::Gif, &gif), Some((65_535, 65_535)));
    let webp = b"RIFF\x16\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0\xff\xff\xff\xff\xff\xff";
    assert_eq!(image_size(MediaKind::WebP, webp), Some((1 << 24, 1 << 24)));
    // A GIF frame placed beyond its screen counts where it ends.
    let mut far = b"GIF89a\x01\0\x01\0\0\0\0\x2c".to_vec();
    far.extend_from_slice(&[0xFF, 0xFF, 0, 0, 0x10, 0, 1, 0, 0, 2, 2, 0x44, 1, 0, 0x3B]);
    assert_eq!(image_size(MediaKind::Gif, &far), Some((65_535 + 16, 1)));
}

/// A header that is not where it belongs is no size: the file is
/// damaged, and is not decoded.
#[test]
fn headers_out_of_place_give_no_size() {
    // A PNG whose first chunk is not IHDR.
    let mut png = fixture!("13x9.png").to_vec();
    png[12..16].copy_from_slice(b"tEXt");
    assert_eq!(image_size(MediaKind::Png, &png), None);
    // A JPEG scan before any frame header, and one that just ends.
    assert_eq!(image_size(MediaKind::Jpeg, b"\xff\xd8\xff\xda\0\x02"), None);
    assert_eq!(image_size(MediaKind::Jpeg, b"\xff\xd8\xff\xd9"), None);
    assert_eq!(image_size(MediaKind::Jpeg, b"\xff\xd8\xff\xe0\0\x01"), None);
    // A WebP of another first chunk.
    assert_eq!(
        image_size(MediaKind::WebP, b"RIFF\x10\0\0\0WEBPALPH\0\0\0\0"),
        None
    );
    // A GIF with something unknown where a block belongs.
    assert_eq!(
        image_size(MediaKind::Gif, b"GIF89a\x01\0\x01\0\0\0\0\x99"),
        None
    );
}

/// Every cut of every fixture: no panic, and never a size the whole file
/// does not have (a cut before the size is none).
#[test]
fn cut_headers_never_panic() {
    for (kind, bytes, size) in fixtures() {
        for cut in 0..bytes.len() {
            let got = image_size(kind, &bytes[..cut]);
            assert!(
                got.is_none() || got == Some(size),
                "{kind:?} cut at {cut}: {got:?}"
            );
        }
    }
}

/// Many segments or blocks before the size are stepped over in one pass.
#[test]
fn the_reading_is_linear() {
    within(Duration::from_secs(5), "reading 10 MB headers", || {
        // 10 MB of empty APP1 segments before the frame header.
        let mut jpeg = b"\xff\xd8".to_vec();
        while jpeg.len() < 10_000_000 {
            jpeg.extend_from_slice(b"\xff\xe1\x00\x02");
        }
        jpeg.extend_from_slice(&fixture!("3x2.jpg")[2..]);
        assert_eq!(image_size(MediaKind::Jpeg, &jpeg), Some((3, 2)));
        // 10 MB of fill bytes.
        let mut fill = b"\xff\xd8".to_vec();
        fill.resize(10_000_000, 0xFF);
        assert_eq!(image_size(MediaKind::Jpeg, &fill), None);
        // 10 MB of GIF comment sub-blocks.
        let mut gif = b"GIF89a\x01\0\x01\0\0\0\0\x21\xfe".to_vec();
        while gif.len() < 10_000_000 {
            gif.push(255);
            gif.extend_from_slice(&[b'x'; 255]);
        }
        gif.push(0);
        gif.extend_from_slice(b"\x2c\0\0\0\0\x02\0\x03\0\0\x02\x02\x44\x01\0\x3b");
        assert_eq!(image_size(MediaKind::Gif, &gif), Some((2, 3)));
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// Damaged fixtures and noise never make the reading panic.
    #[test]
    fn damaged_headers_never_panic(
        which in 0_usize..9,
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
        noise in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        let (kind, bytes, _) = fixtures()[which];
        let mut file = bytes.to_vec();
        for (at, value) in flips {
            let i = at.index(file.len());
            file[i] = value;
        }
        let _ = image_size(kind, &file);
        let mut prefixed = bytes[..bytes.len().min(16)].to_vec();
        prefixed.extend_from_slice(&noise);
        let _ = image_size(kind, &prefixed);
    }
}
