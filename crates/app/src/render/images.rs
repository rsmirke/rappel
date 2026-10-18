//! The decisions about card images that need no GTK (ADR 0055): which
//! inlines become pictures, how large one is drawn, the limits a decoded
//! image must keep, the texts and accessible names, the per-side budget,
//! the bounded texture cache, and the checked read of a media file. Pure
//! (the read aside) and unit-tested.
//!
//! Only an image whose target is a `media:` reference
//! ([`ImageTarget::Media`]) is ever read, and only from the media folder,
//! by the path the core builds from its checked hash. Any other target
//! (`http:`, `file:`, a path) stays text: [`pieces`] never returns one, so
//! nothing here can open or fetch it.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use rappel_core::content::{ImageTarget, Inline};
use rappel_core::media::{IMAGE_MAX_BYTES, MediaKind, MediaRef};

/// At most this many images are drawn per side; later ones are
/// placeholders, never read. Each decoded image costs memory and a
/// sandboxed decode, and a card with more is not a flashcard.
pub const MAX_IMAGES_PER_SIDE: usize = 20;

/// The longest side a decoded image may have, in pixels.
pub const MAX_SIDE: u32 = 16_384;

/// The most pixels a decoded image may have: 64 MP, 256 MiB as a texture.
/// A 45 MP camera photo passes; a decompression bomb claiming 100,000 ×
/// 100,000 does not, and is refused from its header, before its pixels.
pub const MAX_PIXELS: u64 = 64_000_000;

/// How many bytes of decoded textures [`TextureCache`] keeps.
pub const CACHE_BYTES: usize = 256 << 20;

/// How many entries the cache keeps at most, whatever their size, so its
/// linear lookups stay cheap.
pub const CACHE_ENTRIES: usize = 256;

/// The share of the view's height an image may take on the review page
/// and in the preview, so a tall image leaves room for the text around it.
const MAX_HEIGHT_SHARE: f64 = 0.6;

/// The smallest cap on an image's height, in pixels, however short the
/// view: an image is still recognisable at this size.
const MIN_MAX_HEIGHT: i32 = 120;

/// What a paragraph is made of: runs of text, each one label, and the
/// images drawn between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a [Inline]),
    Image { alt: &'a str, media: MediaRef },
}

/// The pieces of a paragraph's `inlines`: each `media:` image at the top
/// level becomes an image, and the runs between them text. A run of only
/// spaces and line breaks between or around images is dropped. Without a
/// media image, the whole paragraph is one text piece. An image inside
/// emphasis or a link, and every other image target, stays in the text.
pub fn pieces(inlines: &[Inline]) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut any_image = false;
    for (i, inline) in inlines.iter().enumerate() {
        if let Inline::Image {
            alt,
            target: ImageTarget::Media(media),
        } = inline
        {
            push_text(&mut out, &inlines[start..i]);
            out.push(Piece::Image { alt, media: *media });
            start = i + 1;
            any_image = true;
        }
    }
    if !any_image {
        return vec![Piece::Text(inlines)];
    }
    push_text(&mut out, &inlines[start..]);
    out
}

fn push_text<'a>(out: &mut Vec<Piece<'a>>, run: &'a [Inline]) {
    let blank = run.iter().all(|inline| match inline {
        Inline::Text(text) => text.trim().is_empty(),
        Inline::LineBreak => true,
        _ => false,
    });
    if !blank {
        out.push(Piece::Text(run));
    }
}

/// How many `media:` images at the top level of `inlines` [`pieces`]
/// draws as pictures (before the per-side cap).
pub fn image_count(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .filter(|inline| {
            matches!(
                inline,
                Inline::Image {
                    target: ImageTarget::Media(_),
                    ..
                }
            )
        })
        .count()
}

/// The size an image of `natural` pixels is drawn at, given `width` pixels
/// of room and at most `max_height`: scaled down, aspect kept, to fit
/// both, and never scaled up. Each side is at least 1 pixel for an image
/// that has pixels.
pub fn display_size(natural: (i32, i32), width: i32, max_height: i32) -> (i32, i32) {
    let (w, h) = natural;
    if w <= 0 || h <= 0 {
        return (0, 0);
    }
    let scale = (f64::from(width.max(1)) / f64::from(w))
        .min(f64::from(max_height.max(1)) / f64::from(h))
        .min(1.0);
    let side = |n: i32| {
        // At most `n` (scale ≤ 1), so the cast cannot overflow.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the value is between 1 and an i32 side"
        )]
        let scaled = (f64::from(n) * scale).round() as i32;
        scaled.clamp(1, n)
    };
    (side(w), side(h))
}

/// The cap on an image's height in a view `view_height` pixels tall: 60 %
/// of it, at least [`MIN_MAX_HEIGHT`]. A view not yet laid out (0) counts
/// as a 720-pixel one.
pub fn max_height(view_height: i32) -> i32 {
    let view = if view_height > 0 { view_height } else { 720 };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "60 % of an i32 fits an i32"
    )]
    let share = (f64::from(view) * MAX_HEIGHT_SHARE).round() as i32;
    share.max(MIN_MAX_HEIGHT)
}

/// Whether an image of `width` × `height` may be decoded: no side over
/// [`MAX_SIDE`], no more than [`MAX_PIXELS`], and not empty. Checked on the
/// header, before any pixel is decoded.
pub fn within_limits(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_SIDE
        && height <= MAX_SIDE
        && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

/// Whether the decoder's own reading of a file's type, `mime`, agrees with
/// the kind its first bytes gave. An animated PNG is still a PNG (its
/// first frame is drawn); anything else, SVG above all, is refused even
/// if a loader for it is installed.
pub fn accepted_mime(kind: MediaKind, mime: &str) -> bool {
    match kind {
        MediaKind::Png => matches!(mime, "image/png" | "image/apng"),
        MediaKind::Jpeg => mime == "image/jpeg",
        MediaKind::WebP => mime == "image/webp",
        MediaKind::Gif => mime == "image/gif",
        _ => false,
    }
}

/// Why an image is not drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// No file (never added, lost, or no media folder).
    Missing,
    /// The file is there but cannot be drawn: not of its kind, too large,
    /// damaged, refused by the decoder or over the limits.
    CannotShow,
    /// Past [`MAX_IMAGES_PER_SIDE`]: not read at all.
    TooMany,
}

impl Failure {
    /// Whether a later try could succeed, so the failure is not cached: a
    /// missing file can come with an import.
    pub fn is_lasting(self) -> bool {
        self == Self::CannotShow
    }
}

/// What an image is called: its alt text, or "Image" without one. The
/// full-size dialog's title and the picture's accessible name.
pub fn image_name(alt: &str) -> String {
    let alt = alt.split_whitespace().collect::<Vec<_>>().join(" ");
    if alt.is_empty() {
        "Image".to_owned()
    } else {
        alt
    }
}

/// The placeholder's text for `failure`.
pub fn placeholder_text(failure: Failure) -> &'static str {
    match failure {
        Failure::Missing => "Image not available",
        Failure::CannotShow => "Image could not be shown",
        Failure::TooMany => "Image not shown: too many images on this side",
    }
}

/// What a screen reader says for a placeholder: its text, then the alt
/// text if there is one.
pub fn placeholder_label(failure: Failure, alt: &str) -> String {
    let text = placeholder_text(failure);
    let alt = alt.split_whitespace().collect::<Vec<_>>().join(" ");
    if alt.is_empty() {
        text.to_owned()
    } else {
        format!("{text}: {alt}")
    }
}

/// The accessible description of an image that opens at full size.
pub const OPEN_DESCRIPTION: &str = "Opens the image at full size";

/// The per-side count of images drawn, in document order, like
/// `math::Budget` for formulas.
#[derive(Debug, Default)]
pub struct ImageBudget {
    used: usize,
}

impl ImageBudget {
    /// Whether the next image may be drawn; counts it.
    pub fn take(&mut self) -> bool {
        self.used += 1;
        self.used <= MAX_IMAGES_PER_SIDE
    }
}

/// A small least-recently-used cache bounded by a total cost and by
/// [`CACHE_ENTRIES`]. A value costing more than the whole budget is not
/// kept.
#[derive(Debug)]
pub struct TextureCache<K, V> {
    budget: usize,
    used: usize,
    /// Oldest first.
    entries: VecDeque<(K, V, usize)>,
}

impl<K: PartialEq, V: Clone> TextureCache<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            entries: VecDeque::new(),
        }
    }

    /// The value of `key`, now the most recently used.
    pub fn get(&mut self, key: &K) -> Option<V> {
        let i = self.entries.iter().position(|(k, _, _)| k == key)?;
        let entry = self.entries.remove(i)?;
        let value = entry.1.clone();
        self.entries.push_back(entry);
        Some(value)
    }

    /// Keeps `value` for `key` at `cost`, dropping the least recently used
    /// entries to make room.
    pub fn insert(&mut self, key: K, value: V, cost: usize) {
        if let Some(i) = self.entries.iter().position(|(k, _, _)| *k == key)
            && let Some((_, _, old)) = self.entries.remove(i)
        {
            self.used -= old;
        }
        if cost > self.budget {
            return;
        }
        while self.used + cost > self.budget || self.entries.len() >= CACHE_ENTRIES {
            match self.entries.pop_front() {
                Some((_, _, old)) => self.used -= old,
                None => break,
            }
        }
        self.used += cost;
        self.entries.push_back((key, value, cost));
    }

    /// The total cost kept.
    #[cfg(test)]
    pub fn used(&self) -> usize {
        self.used
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// What a texture of `width` × `height` costs in the cache: 4 bytes a
/// pixel, the most a decoded frame takes for these formats.
pub fn texture_cost(width: i32, height: i32) -> usize {
    let side = |n: i32| usize::try_from(n).unwrap_or(0);
    side(width).saturating_mul(side(height)).saturating_mul(4)
}

/// Reads the media file at `path` for decoding, checked: a regular file
/// (never through a symbolic link), at most [`IMAGE_MAX_BYTES`], whose
/// first bytes are of `media`'s kind. The bytes read are the ones decoded,
/// so the file cannot change between the check and the decode.
pub fn read_image_file(path: &Path, media: MediaRef) -> Result<Vec<u8>, Failure> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(Failure::Missing),
        Err(_) => return Err(Failure::CannotShow),
    };
    if !meta.file_type().is_file() || meta.len() > IMAGE_MAX_BYTES || meta.len() == 0 {
        return Err(Failure::CannotShow);
    }
    let file = fs::File::open(path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => Failure::Missing,
        _ => Failure::CannotShow,
    })?;
    let mut bytes = Vec::new();
    // One byte past the limit is enough to know the file grew.
    file.take(IMAGE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::CannotShow)?;
    let too_large = u64::try_from(bytes.len()).map_or(true, |n| n > IMAGE_MAX_BYTES);
    if too_large || MediaKind::detect(&bytes) != Some(media.kind()) || !media.kind().is_image() {
        return Err(Failure::CannotShow);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use rappel_core::content::{Block, parse};

    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn paragraph(md: &str) -> Vec<Inline> {
        match parse(md).blocks.into_iter().next() {
            Some(Block::Paragraph(inlines)) => inlines,
            other => panic!("not a paragraph: {other:?}"),
        }
    }

    fn kinds(pieces: &[Piece<'_>]) -> Vec<String> {
        pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text(run) => format!("text {}", run.len()),
                Piece::Image { alt, media } => format!("image {alt} {}", media.file_name()),
            })
            .collect()
    }

    #[test]
    fn media_images_are_pulled_out_of_their_paragraph() {
        let md = format!("Before ![a cell](media:{HASH}.png) after");
        let inlines = paragraph(&md);
        let got = pieces(&inlines);
        assert_eq!(
            kinds(&got),
            ["text 1", &format!("image a cell {HASH}.png"), "text 1"]
        );
        assert_eq!(image_count(&inlines), 1);
    }

    #[test]
    fn blank_runs_between_images_are_dropped() {
        let md = format!("![](media:{HASH}.png)\n![](media:{HASH}.gif)  \n");
        let inlines = paragraph(&md);
        let got = pieces(&inlines);
        assert_eq!(
            kinds(&got),
            [format!("image  {HASH}.png"), format!("image  {HASH}.gif")]
        );
    }

    #[test]
    fn a_paragraph_without_media_images_is_one_text() {
        let inlines = paragraph("Just *text*");
        assert_eq!(pieces(&inlines), [Piece::Text(&inlines)]);
    }

    /// No IO can follow from these: they are never an image piece, so the
    /// renderer never asks for a file (they stay the Markdown typed).
    #[test]
    fn other_image_targets_are_never_read() {
        for url in [
            "https://example.org/x.png",
            "http://example.org/x.png",
            "file:///etc/passwd",
            "/home/user/x.png",
            "../x.png",
            &format!("media:{}.png", HASH.to_uppercase()),
            &format!("media:{HASH}.svg"),
            &format!("media:{HASH}.wav"),
            &format!("media:{HASH}.png?x"),
        ] {
            let md = format!("![alt]({url})");
            let inlines = paragraph(&md);
            assert_eq!(pieces(&inlines), [Piece::Text(&inlines)], "{url}");
            assert_eq!(image_count(&inlines), 0, "{url}");
        }
    }

    #[test]
    fn images_nested_in_other_inlines_stay_in_the_text() {
        let md = format!("*![a](media:{HASH}.png)* [![b](media:{HASH}.png)](x)");
        let inlines = paragraph(&md);
        assert_eq!(pieces(&inlines), [Piece::Text(&inlines)]);
    }

    #[test]
    fn images_are_scaled_down_never_up() {
        // Small: natural size.
        assert_eq!(display_size((40, 30), 700, 400), (40, 30));
        // Wide: the width.
        assert_eq!(display_size((2000, 1000), 700, 400), (700, 350));
        // Tall: the height cap.
        assert_eq!(display_size((300, 3000), 700, 400), (40, 400));
        // Both.
        assert_eq!(display_size((1400, 1200), 350, 200), (233, 200));
        // Never below a pixel, never empty for an image with pixels.
        assert_eq!(display_size((10_000, 1), 100, 400), (100, 1));
        assert_eq!(display_size((40, 30), 0, 0), (1, 1));
        assert_eq!(display_size((0, 30), 700, 400), (0, 0));
    }

    #[test]
    fn the_height_cap_follows_the_view() {
        assert_eq!(max_height(720), 432);
        assert_eq!(max_height(1000), 600);
        // A phone held sideways: 60 % of the card's room.
        assert_eq!(max_height(300), 180);
        assert_eq!(max_height(100), MIN_MAX_HEIGHT);
        assert_eq!(max_height(0), 432);
        assert_eq!(max_height(-5), 432);
    }

    #[test]
    fn decoded_sizes_are_limited() {
        assert!(within_limits(1, 1));
        assert!(within_limits(8192, 5464));
        assert!(within_limits(MAX_SIDE, 3906));
        assert!(!within_limits(MAX_SIDE + 1, 1));
        assert!(!within_limits(1, MAX_SIDE + 1));
        assert!(!within_limits(MAX_SIDE, MAX_SIDE));
        assert!(!within_limits(100_000, 100_000));
        assert!(!within_limits(0, 10));
        assert!(!within_limits(u32::MAX, u32::MAX));
    }

    #[test]
    fn the_decoder_must_agree_with_the_first_bytes() {
        assert!(accepted_mime(MediaKind::Png, "image/png"));
        assert!(accepted_mime(MediaKind::Png, "image/apng"));
        assert!(accepted_mime(MediaKind::Jpeg, "image/jpeg"));
        assert!(accepted_mime(MediaKind::WebP, "image/webp"));
        assert!(accepted_mime(MediaKind::Gif, "image/gif"));
        for mime in ["image/svg+xml", "image/jpeg", "text/html", "image/tiff", ""] {
            assert!(!accepted_mime(MediaKind::Png, mime), "{mime}");
        }
        assert!(!accepted_mime(MediaKind::Gif, "image/svg+xml"));
        assert!(!accepted_mime(MediaKind::Wav, "audio/wav"));
    }

    #[test]
    fn names_and_placeholders() {
        assert_eq!(image_name(""), "Image");
        assert_eq!(image_name("  \n "), "Image");
        assert_eq!(image_name(" A  cell\nwall "), "A cell wall");
        assert_eq!(
            placeholder_label(Failure::Missing, ""),
            "Image not available"
        );
        assert_eq!(
            placeholder_label(Failure::CannotShow, "Mitochondrion"),
            "Image could not be shown: Mitochondrion"
        );
        assert_eq!(
            placeholder_text(Failure::TooMany),
            "Image not shown: too many images on this side"
        );
        assert!(Failure::CannotShow.is_lasting());
        assert!(!Failure::Missing.is_lasting());
        assert!(!Failure::TooMany.is_lasting());
    }

    #[test]
    fn a_side_draws_at_most_its_share_of_images() {
        let mut budget = ImageBudget::default();
        let drawn = (0..MAX_IMAGES_PER_SIDE + 5)
            .filter(|_| budget.take())
            .count();
        assert_eq!(drawn, MAX_IMAGES_PER_SIDE);
    }

    #[test]
    fn the_cache_keeps_the_most_recent_within_its_budget() {
        let mut cache = TextureCache::new(100);
        cache.insert(1, "a", 40);
        cache.insert(2, "b", 40);
        assert_eq!(cache.get(&1), Some("a"));
        // 1 was used last, so 2 goes.
        cache.insert(3, "c", 40);
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&1), Some("a"));
        assert_eq!(cache.get(&3), Some("c"));
        assert_eq!(cache.used(), 80);
        // Too large for the whole budget: not kept, nothing dropped.
        cache.insert(4, "d", 101);
        assert_eq!(cache.get(&4), None);
        assert_eq!(cache.len(), 2);
        // The same key again replaces its entry and its cost.
        cache.insert(1, "e", 10);
        assert_eq!(cache.used(), 50);
        assert_eq!(cache.get(&1), Some("e"));
        // The entry count is bounded too.
        let mut many = TextureCache::new(usize::MAX);
        for i in 0..CACHE_ENTRIES + 10 {
            many.insert(i, (), 0);
        }
        assert_eq!(many.len(), CACHE_ENTRIES);
        assert_eq!(many.get(&0), None);
        assert_eq!(texture_cost(10, 20), 800);
        assert_eq!(texture_cost(-1, 20), 0);
    }

    #[test]
    fn a_media_file_is_read_only_when_it_is_what_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        let media = MediaRef::parse(&format!("media:{HASH}.png")).unwrap();
        let path = dir.path().join(media.file_name());
        assert_eq!(read_image_file(&path, media), Err(Failure::Missing));
        fs::write(&path, &png).unwrap();
        assert_eq!(read_image_file(&path, media), Ok(png.clone()));
        // Another kind under a PNG's name.
        let gif = MediaRef::parse(&format!("media:{HASH}.gif")).unwrap();
        fs::write(dir.path().join(gif.file_name()), &png).unwrap();
        assert_eq!(
            read_image_file(&dir.path().join(gif.file_name()), gif),
            Err(Failure::CannotShow)
        );
        // SVG text under a PNG's name.
        fs::write(&path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
        assert_eq!(read_image_file(&path, media), Err(Failure::CannotShow));
        // Empty, too large, a folder, a link.
        fs::write(&path, b"").unwrap();
        assert_eq!(read_image_file(&path, media), Err(Failure::CannotShow));
        let mut big = png.clone();
        big.resize(usize::try_from(IMAGE_MAX_BYTES).unwrap() + 1, 0);
        fs::write(&path, &big).unwrap();
        assert_eq!(read_image_file(&path, media), Err(Failure::CannotShow));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(read_image_file(&path, media), Err(Failure::CannotShow));
        fs::remove_dir(&path).unwrap();
        let real = dir.path().join("real.png");
        fs::write(&real, &png).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert_eq!(read_image_file(&path, media), Err(Failure::CannotShow));
    }
}
