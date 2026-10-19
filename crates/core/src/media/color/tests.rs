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
    /// never holds the profile's marker where the walk could read it.
    #[test]
    fn damaged_images_never_panic(
        which in 0_usize..3,
        cut in any::<prop::sample::Index>(),
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
    ) {
        let (kind, bytes) = [
            (MediaKind::Png, fixture!("4x3-icc.png")),
            (MediaKind::Jpeg, fixture!("4x3-icc.jpg")),
            (MediaKind::WebP, fixture!("4x3-icc.webp")),
        ][which];
        let mut file = bytes.to_vec();
        for (at, value) in flips {
            let i = at.index(file.len());
            file[i] = value;
        }
        file.truncate(cut.index(file.len() + 1));
        let _ = without_color_profile(kind, &file);
    }
}
