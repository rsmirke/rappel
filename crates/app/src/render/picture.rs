//! A card image on screen (ADR 0055): [`CardImage`], decoded off the main
//! thread by glycin in a sandboxed process, cached, scaled down to the
//! card's width and a height cap, and opened at full size by a click,
//! Enter or Space. The decisions are in [`super::images`].
//!
//! The file is read only through [`super::images::read_image_file`]: from
//! the media folder, by the name the core builds from a checked hash, a
//! regular file of its kind. Those bytes, never the path, go to glycin,
//! whose own reading of the type must agree with the kind
//! ([`accepted_mime`]), so an SVG (which has a loader on most systems) is
//! never decoded. The header's size is checked against the limits
//! ([`within_limits`]) before the pixels are asked for, and a decode that
//! takes longer than [`DECODE_TIMEOUT`] is cancelled. Every failure is a
//! placeholder, never a panic.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use rappel_core::media::MediaRef;

use super::images::{
    CACHE_BYTES, Failure, PixelBudget, TextureCache, accepted_mime, image_name, placeholder_label,
    read_image_file, texture_cost, within_limits,
};
use crate::ui::dialog;

/// How long one decode may take before it is given up (and its loader
/// process told to stop). A 10 MB photo decodes in well under a second.
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the cache keeps per file: the texture, or a failure that would
/// happen again (`Failure::is_lasting`). Keyed by the file's path, which
/// is the media folder and the reference.
type Decoded = Result<gdk::Texture, Failure>;

thread_local! {
    /// Decoded images by path, bounded by their texture size. GTK runs on
    /// one thread, so a thread-local is the whole cache.
    static CACHE: RefCell<TextureCache<PathBuf, Decoded>> =
        RefCell::new(TextureCache::new(CACHE_BYTES));
    /// The images waiting for a decode already running, by path, so one
    /// file shown twice is decoded once.
    static WAITING: RefCell<HashMap<PathBuf, Vec<glib::WeakRef<CardImage>>>> =
        RefCell::new(HashMap::new());
}

/// The cost a cached failure is counted at: small, but not nothing.
const FAILURE_COST: usize = 64;

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gdk, glib};

    use super::super::images::display_size;

    #[derive(Debug, Default)]
    pub struct CardImage {
        pub child: RefCell<Option<gtk::Widget>>,
        /// The texture's size while it is shown, else `None`.
        pub natural: Cell<Option<(i32, i32)>>,
        pub max_height: Cell<i32>,
        pub alt: RefCell<String>,
        pub texture: RefCell<Option<gdk::Texture>>,
        /// The side's pixel budget (`images::PixelBudget`).
        pub pixels: RefCell<Option<std::rc::Rc<RefCell<super::super::images::PixelBudget>>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CardImage {
        const NAME: &'static str = "RappelCardImage";
        type Type = super::CardImage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("card-image");
        }
    }

    impl ObjectImpl for CardImage {
        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CardImage {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        /// A shown texture measures as [`display_size`] gives it: its
        /// natural width at most (it can shrink to nothing), and for a
        /// width the height that keeps its aspect within the cap. Anything
        /// else (a placeholder, nothing yet) measures as its child.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let cap = self.max_height.get();
            if let Some(natural) = self.natural.get() {
                if orientation == gtk::Orientation::Horizontal {
                    let (w, _) = display_size(natural, i32::MAX, cap);
                    return (0, w, -1, -1);
                }
                let width = if for_size < 0 { i32::MAX } else { for_size };
                let (_, h) = display_size(natural, width, cap);
                return (h, h, -1, -1);
            }
            match self.child.borrow().as_ref() {
                Some(child) if child.should_layout() => {
                    let (min, nat, _, _) = child.measure(orientation, for_size);
                    (min, nat, -1, -1)
                }
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            if !child.should_layout() {
                return;
            }
            match self.natural.get() {
                Some(natural) => {
                    let cap = self.max_height.get().min(height.max(1));
                    let (w, h) = display_size(natural, width, cap);
                    // At the start of the line, as the text is.
                    let x = if self.obj().direction() == gtk::TextDirection::Rtl {
                        width - w
                    } else {
                        0
                    };
                    child.size_allocate(&gtk::Allocation::new(x, 0, w, h), -1);
                }
                None => child.allocate(width, height, baseline, None),
            }
        }
    }
}

glib::wrapper! {
    /// One `media:` image of a card side: nothing while it decodes, then
    /// the picture (a flat button that opens it at full size) or a
    /// placeholder that says why it is not shown.
    pub struct CardImage(ObjectSubclass<imp::CardImage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CardImage {
    /// The image `media` of the media folder `folder` (`None`: there is
    /// none, so it is missing), with alt text `alt`, at most `max_height`
    /// pixels tall. A decoded image is shown at once from the cache;
    /// otherwise it is decoded on another thread and shown when done.
    /// `pixels` is the side's [`PixelBudget`]: a decoded image past it is
    /// not shown.
    pub fn new(
        folder: Option<&Path>,
        media: MediaRef,
        alt: &str,
        max_height: i32,
        pixels: Rc<RefCell<PixelBudget>>,
    ) -> Self {
        let image: Self = glib::Object::new();
        let imp = image.imp();
        imp.max_height.set(max_height);
        imp.pixels.replace(Some(pixels));
        imp.alt.replace(alt.to_owned());
        let Some(folder) = folder else {
            image.show_failure(Failure::Missing);
            return image;
        };
        let path = folder.join(media.file_name());
        let cached = CACHE.with_borrow_mut(|cache| cache.get(&path));
        match cached {
            Some(decoded) => image.show(decoded),
            None => image.load(path, media),
        }
        image
    }

    /// A placeholder for an image that is not read at all (past the
    /// side's share of images).
    pub fn not_read(alt: &str, failure: Failure) -> Self {
        let image: Self = glib::Object::new();
        image.imp().alt.replace(alt.to_owned());
        image.show_failure(failure);
        image
    }

    /// Whether a texture is shown (tests).
    #[cfg(test)]
    pub fn is_shown(&self) -> bool {
        self.imp().natural.get().is_some()
    }

    /// The placeholder's failure, if one is shown (tests): read back from
    /// its accessible label's text.
    #[cfg(test)]
    pub fn shown_failure(&self) -> Option<Failure> {
        let child = self.imp().child.borrow().clone()?;
        let label = child
            .first_child()?
            .next_sibling()?
            .downcast::<gtk::Label>()
            .ok()?;
        let text = label.text();
        [
            Failure::Missing,
            Failure::CannotShow,
            Failure::TooMany,
            Failure::OverBudget,
        ]
        .into_iter()
        .find(|f| text.starts_with(&super::images::placeholder_text(*f)))
    }

    /// Joins the decode of `path` already running, or starts it.
    fn load(&self, path: PathBuf, media: MediaRef) {
        let first = WAITING.with_borrow_mut(|waiting| {
            let list = waiting.entry(path.clone()).or_default();
            list.push(self.downgrade());
            list.len() == 1
        });
        if !first {
            return;
        }
        glib::spawn_future_local(async move {
            let decoded = decode(path.clone(), media, DECODE_TIMEOUT).await;
            let cost = match &decoded {
                Ok(texture) => Some(texture_cost(texture.width(), texture.height())),
                Err(failure) if failure.is_lasting() => Some(FAILURE_COST),
                Err(_) => None,
            };
            if let Some(cost) = cost {
                CACHE.with_borrow_mut(|cache| cache.insert(path.clone(), decoded.clone(), cost));
            }
            let waiting = WAITING.with_borrow_mut(|waiting| waiting.remove(&path));
            for image in waiting.into_iter().flatten() {
                if let Some(image) = image.upgrade() {
                    image.show(decoded.clone());
                }
            }
        });
    }

    fn show(&self, decoded: Decoded) {
        match decoded {
            Ok(texture) => {
                let fits = self.imp().pixels.borrow().as_ref().is_none_or(|pixels| {
                    pixels.borrow_mut().take(texture.width(), texture.height())
                });
                if fits {
                    self.show_texture(texture);
                } else {
                    self.show_failure(Failure::OverBudget);
                }
            }
            Err(failure) => self.show_failure(failure),
        }
    }

    fn set_child(&self, child: Option<gtk::Widget>) {
        let imp = self.imp();
        if let Some(old) = imp.child.take() {
            old.unparent();
        }
        if let Some(child) = &child {
            child.set_parent(self);
        }
        imp.child.replace(child);
        self.queue_resize();
    }

    /// The picture in a flat button, named by the alt text. The click does
    /// not take the focus (`focus-on-click`), so on the review page the
    /// card keeps it and its keys keep working; Tab reaches the button,
    /// whose own Enter and Space open the image before the page's keys
    /// (which run in the bubble phase) see them.
    fn show_texture(&self, texture: gdk::Texture) {
        let imp = self.imp();
        let name = image_name(&imp.alt.borrow());
        let picture = gtk::Picture::builder()
            .paintable(&texture)
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .alternative_text(&name)
            .build();
        let button = gtk::Button::builder()
            .child(&picture)
            .focus_on_click(false)
            .has_frame(false)
            .build();
        button.add_css_class("card-image");
        // Clips the picture to the button's rounded corners.
        button.set_overflow(gtk::Overflow::Hidden);
        button.update_property(&[
            gtk::accessible::Property::Label(&name),
            gtk::accessible::Property::Description(&super::images::open_description()),
        ]);
        let shown = texture.clone();
        button.connect_clicked(move |button| open_full_size(button, &shown, &name));
        imp.natural.set(Some((texture.width(), texture.height())));
        imp.texture.replace(Some(texture));
        self.set_child(Some(button.upcast()));
    }

    /// A dimmed line: an icon and why the image is not shown, with its alt
    /// text. One image for screen readers, named by the same words.
    fn show_failure(&self, failure: Failure) {
        let imp = self.imp();
        imp.natural.set(None);
        imp.texture.replace(None);
        let text = placeholder_label(failure, &imp.alt.borrow());
        let line = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .accessible_role(gtk::AccessibleRole::Img)
            .build();
        line.add_css_class("dimmed");
        line.update_property(&[gtk::accessible::Property::Label(&text)]);
        let icon = gtk::Image::builder()
            .icon_name("image-missing-symbolic")
            .valign(gtk::Align::Center)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        // Plain text (the alt text is card content), with breaks in long
        // runs like every label of a side.
        let label = gtk::Label::builder()
            .label(super::breaks::breakable(&text))
            .use_markup(false)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .hexpand(true)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        line.append(&icon);
        line.append(&label);
        self.set_child(Some(line.upcast()));
    }
}

/// Reads and decodes the file at `path`, checked, within the limits and
/// `timeout` ([`DECODE_TIMEOUT`]; a test gives a shorter one).
async fn decode(path: PathBuf, media: MediaRef, timeout: Duration) -> Decoded {
    // `gio::spawn_blocking` returns a panic in the closure as its error.
    let bytes = gio::spawn_blocking(move || read_image_file(&path, media))
        .await
        .map_err(|_| Failure::CannotShow)??;
    let cancellable = gio::Cancellable::new();
    let load = decode_bytes(bytes, media, cancellable.clone());
    if let Ok(decoded) = glib::future_with_timeout(timeout, load).await {
        decoded
    } else {
        cancellable.cancel();
        tracing::debug!("an image took too long to decode");
        Err(Failure::TimedOut)
    }
}

/// glycin's decode of `bytes`: its loader runs in a sandbox (bubblewrap
/// with a seccomp filter and a memory limit, or flatpak-spawn in a
/// Flatpak); where the system refuses to make one (a container without
/// user namespaces), glycin runs the loader as a plain separate process
/// with no seccomp filter, no namespaces and no memory limit
/// (`SandboxSelector::Auto`, glycin's `api_common.rs`), which is logged
/// once as a warning. `Auto` and not `Bwrap`: forcing bubblewrap would
/// break a Flatpak, where glycin uses flatpak-spawn.
async fn decode_bytes(bytes: Vec<u8>, media: MediaRef, cancellable: gio::Cancellable) -> Decoded {
    #[cfg(test)]
    LOADS.set(LOADS.get() + 1);
    let mut loader = glycin::Loader::new_vec(bytes);
    loader.cancellable(cancellable);
    let image = loader.load().await.map_err(|err| {
        tracing::debug!("an image could not be decoded: {err}");
        Failure::CannotShow
    })?;
    if matches!(
        image.active_sandbox_mechanism(),
        glycin::SandboxMechanism::NotSandboxed
    ) {
        warn_unsandboxed();
    }
    let mime = image.mime_type();
    if !accepted_mime(media.kind(), mime.as_str()) {
        tracing::debug!(
            "an image was refused: the decoder read it as {}",
            mime.as_str()
        );
        return Err(Failure::CannotShow);
    }
    let details = image.details();
    if !within_limits(details.width(), details.height()) {
        tracing::debug!(
            width = details.width(),
            height = details.height(),
            "an image is over the size limits"
        );
        return Err(Failure::CannotShow);
    }
    let frame = image.next_frame().await.map_err(|err| {
        tracing::debug!("an image's first frame could not be decoded: {err}");
        Failure::CannotShow
    })?;
    let (width, height) = (frame.width(), frame.height());
    if !within_limits(width, height) {
        return Err(Failure::CannotShow);
    }
    Ok(frame.texture())
}

#[cfg(test)]
thread_local! {
    /// How many images were sent to glycin on this thread (tests).
    static LOADS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Logs, once per run, that images are decoded without glycin's sandbox.
fn warn_unsandboxed() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing::warn!(
            "images are decoded without a sandbox: bubblewrap cannot create one here, \
             so glycin runs its loader as a plain process"
        );
    });
}

/// The image `texture` at full size in a dialog over `parent`, titled
/// `name`: scaled down to fit the dialog, never up.
fn open_full_size(parent: &impl IsA<gtk::Widget>, texture: &gdk::Texture, name: &str) {
    let Ok((full, title, picture)) = full_size_layout() else {
        return;
    };
    // Plain text: AdwWindowTitle does not parse markup.
    title.set_title(name);
    full.set_title(name);
    picture.set_paintable(Some(texture));
    picture.set_alternative_text(Some(name));
    // No content size is set: the dialog takes the picture's natural size
    // (the image's, with its margins and the header bar) and libadwaita
    // keeps it inside the window, scaling the picture down to fit.
    full.present(Some(parent));
}

/// The full-size dialog of `data/image_dialog.blp`: the dialog, its
/// title and its picture, or `Err` with the id of one that is missing.
pub(crate) fn full_size_layout()
-> Result<(adw::Dialog, adw::WindowTitle, gtk::Picture), &'static str> {
    const FILE: &str = "image_dialog.ui";
    let builder = dialog::builder(FILE);
    Ok((
        dialog::object(&builder, FILE, "image_dialog")?,
        dialog::object(&builder, FILE, "image_title")?,
        dialog::object(&builder, FILE, "image_picture")?,
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use super::*;
    use crate::state::{Accept, AppState};

    fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(data);
        let mut crc = crc32fast::Hasher::new();
        crc.update(&kind);
        crc.update(data);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }

    /// A PNG whose header says `w` × `h` (8-bit RGB) and whose one IDAT
    /// holds `raw` compressed: valid CRCs, so only its size or data lie.
    fn hand_png(w: u32, h: u32, raw: &[u8]) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, *b"IHDR", &ihdr);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        zlib.write_all(raw).unwrap();
        chunk(&mut out, *b"IDAT", &zlib.finish().unwrap());
        chunk(&mut out, *b"IEND", b"");
        out
    }

    /// A GIF of `frames` frames of one pixel each.
    fn many_frame_gif(frames: usize) -> Vec<u8> {
        let mut out = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff".to_vec();
        for _ in 0..frames {
            // A graphic control extension (10 ms), then the one pixel.
            out.extend_from_slice(b"\x21\xf9\x04\x00\x01\x00\x00\x00");
            out.extend_from_slice(b"\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00");
        }
        out.push(0x3b);
        out
    }

    /// Puts `bytes` into the media folder of `data_dir` under the name a
    /// reference of `ext` with a hash made from `seed` has, as a damaged or
    /// hostile file would be (the store itself would name it by its hash).
    fn plant(data_dir: &Path, seed: u8, ext: &str, bytes: &[u8]) -> MediaRef {
        let media = MediaRef::parse(&format!("media:{}.{ext}", format!("{seed:02x}").repeat(32)))
            .expect("a valid reference");
        let folder = data_dir.join("media");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(media.file_name()), bytes).unwrap();
        media
    }

    /// Shows `media` and waits for its decode to end.
    fn settle(folder: &Path, media: MediaRef) -> CardImage {
        let image = CardImage::new(Some(folder), media, "An image", 400, Rc::default());
        let done = crate::testing::settle_until(
            || image.is_shown() || image.shown_failure().is_some(),
            Duration::from_secs(60),
        );
        assert!(done, "the decode of {} did not end", media.file_name());
        image
    }

    #[test]
    fn a_media_image_is_decoded_and_scaled_to_the_card() {
        crate::testing::with_widgets(|| {
            let (tmp, state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let media = AppState::add_media_bytes_on_worker(
                &data,
                &crate::testing::png_bytes(300, 1200),
                Accept::Image,
            )
            .unwrap();
            let folder = state.media_folder().expect("a media folder");
            let image = settle(&folder, media);
            assert!(image.is_shown(), "{:?}", image.shown_failure());
            // Within the cap and the width, aspect kept, never upscaled.
            let (_, natural, _, _) = image.measure(gtk::Orientation::Vertical, 700);
            assert_eq!(natural, 400);
            let (_, natural, _, _) = image.measure(gtk::Orientation::Horizontal, -1);
            assert_eq!(natural, 100);
            let (_, natural, _, _) = image.measure(gtk::Orientation::Vertical, 50);
            assert_eq!(natural, 200);
            // A second one comes from the cache at once.
            let again = CardImage::new(Some(&folder), media, "", 400, Rc::default());
            assert!(again.is_shown());
            // The button is named by the alt text, the picture is an image.
            let button = again
                .first_child()
                .and_downcast::<gtk::Button>()
                .expect("a button");
            assert_eq!(button.accessible_role(), gtk::AccessibleRole::Button);
            let picture = button.child().and_downcast::<gtk::Picture>().unwrap();
            assert_eq!(picture.accessible_role(), gtk::AccessibleRole::Img);
            assert_eq!(picture.alternative_text().as_deref(), Some("Image"));
            assert!(!button.gets_focus_on_click());
        });
    }

    #[test]
    fn a_missing_file_or_folder_is_a_placeholder() {
        crate::testing::with_widgets(|| {
            let (tmp, _state) = crate::testing::temporary_state();
            let media = MediaRef::parse(&format!("media:{}.png", "cd".repeat(32))).unwrap();
            let image = settle(&tmp.path().join("data/media"), media);
            assert_eq!(image.shown_failure(), Some(Failure::Missing));
            let none = CardImage::new(None, media, "x", 400, Rc::default());
            assert_eq!(none.shown_failure(), Some(Failure::Missing));
            let many = CardImage::not_read("x", Failure::TooMany);
            assert_eq!(many.shown_failure(), Some(Failure::TooMany));
            let child = many.first_child().unwrap();
            assert_eq!(child.accessible_role(), gtk::AccessibleRole::Img);
        });
    }

    /// Hostile files under media names: each is a placeholder, decoded (or
    /// refused) within a time bound, and the window's thread never decodes.
    #[test]
    fn hostile_images_are_placeholders_in_time() {
        crate::testing::with_widgets(|| {
            let (tmp, _state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let folder = data.join("media");
            // 100,000 × 100,000 claimed, a few bytes of data.
            let huge = hand_png(100_000, 100_000, &[0; 64]);
            // A bomb: 16,384 × 16,384 of zeros (768 MiB raw) in under 1 MB.
            let row = vec![0_u8; 1 + 16_384 * 3];
            let mut raw = Vec::with_capacity(row.len() * 2_048);
            for _ in 0..2_048 {
                raw.extend_from_slice(&row);
            }
            let bomb = hand_png(16_384, 16_384, &raw);
            // Valid size, data cut short.
            let short = hand_png(4_000, 4_000, &[0; 100]);
            let jpeg =
                b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00\xff\xdb"
                    .to_vec();
            let webp = b"RIFF\x20\x00\x00\x00WEBPVP8 \x10\x00\x00\x00\x00\x00\x00broken!!".to_vec();
            let svg_as_png =
                b"\x89PNG\r\n\x1a\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
            let cases = [
                ("png", huge, "a header claiming 100,000 × 100,000"),
                ("png", bomb, "a decompression bomb"),
                ("png", short, "a truncated PNG"),
                ("jpg", jpeg, "a truncated JPEG"),
                ("webp", webp, "a WebP with a broken header"),
                ("png", svg_as_png, "SVG behind a PNG signature"),
            ];
            for (seed, (ext, bytes, what)) in (1_u8..).zip(cases) {
                let media = plant(&data, seed, ext, &bytes);
                let started = std::time::Instant::now();
                let image = settle(&folder, media);
                crate::testing::print_timing(what, started.elapsed());
                assert_eq!(image.shown_failure(), Some(Failure::CannotShow), "{what}");
            }
        });
    }

    /// A JPEG of `w` × `h` by its frame header, and nothing to decode.
    fn jpeg_of(w: u16, h: u16) -> Vec<u8> {
        let mut out =
            b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00".to_vec();
        out.extend_from_slice(b"\xff\xc0\x00\x0b\x08");
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(b"\x01\x01\x11\x00\xff\xd9");
        out
    }

    /// A WebP whose `VP8X` canvas is `w` × `h`.
    fn webp_of(w: u32, h: u32) -> Vec<u8> {
        let mut out = b"RIFF\x16\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00\x00\x00\x00\x00".to_vec();
        out.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        out.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        out
    }

    /// A GIF whose screen is `w` × `h`, with one frame of that size.
    fn gif_of(w: u16, h: u16) -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(b"\x00\x00\x00\x2c\x00\x00\x00\x00");
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(b"\x00\x02\x02\x44\x01\x00\x3b");
        out
    }

    /// An image whose header claims more than the limits (16,384 px a
    /// side, 64 MP) is refused by Rappel's own reading of the header,
    /// before its bytes are sent to glycin: without a sandbox (a container,
    /// ADR 0055) the loader has no memory limit (ADR 0063).
    #[test]
    fn images_too_large_by_their_header_never_reach_the_decoder() {
        crate::testing::with_widgets(|| {
            let (tmp, _state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let folder = data.join("media");
            let cases = [
                (
                    "png",
                    hand_png(100_000, 100_000, &[0; 64]),
                    "a PNG of 100,000 × 100,000",
                ),
                ("png", hand_png(16_385, 1, &[0; 64]), "a PNG 16,385 px wide"),
                ("png", hand_png(9_000, 9_000, &[0; 64]), "a PNG of 81 MP"),
                ("jpg", jpeg_of(60_000, 60_000), "a JPEG of 60,000 × 60,000"),
                (
                    "webp",
                    webp_of(1 << 24, 1 << 24),
                    "a WebP canvas of 2^24 × 2^24",
                ),
                ("gif", gif_of(65_535, 65_535), "a GIF of 65,535 × 65,535"),
            ];
            for (seed, (ext, bytes, what)) in (40_u8..).zip(cases) {
                let media = plant(&data, seed, ext, &bytes);
                let before = LOADS.get();
                let image = settle(&folder, media);
                assert_eq!(image.shown_failure(), Some(Failure::CannotShow), "{what}");
                assert_eq!(LOADS.get(), before, "{what} reached the decoder");
            }
            // Within the limits, the decoder is asked.
            let media = plant(&data, 60, "png", &crate::testing::png_bytes(20, 10));
            let before = LOADS.get();
            assert!(settle(&folder, media).is_shown());
            assert_eq!(LOADS.get(), before + 1);
        });
    }

    /// The peak memory of this process so far (`VmHWM`), in bytes.
    fn peak_memory() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmHWM:"))
            .and_then(|kb| kb.trim().trim_end_matches(" kB").trim().parse::<u64>().ok())
            .map_or(0, |kb| kb * 1024)
    }

    /// `n` bytes of `byte`, compressed with zlib: a few hundred KiB for
    /// hundreds of MiB.
    fn zlib_of(byte: u8, n: usize) -> Vec<u8> {
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        let block = vec![byte; 1 << 20];
        for _ in 0..n >> 20 {
            zlib.write_all(&block).unwrap();
        }
        zlib.finish().unwrap()
    }

    /// An 8 × 8 RGB PNG with `extra` chunks before its pixels.
    fn png_with(extra: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = 8_u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&8_u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, *b"IHDR", &ihdr);
        for (kind, data) in extra {
            chunk(&mut out, *kind, data);
        }
        let raw = vec![0_u8; 8 * (1 + 8 * 3)];
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        zlib.write_all(&raw).unwrap();
        chunk(&mut out, *b"IDAT", &zlib.finish().unwrap());
        chunk(&mut out, *b"IEND", b"");
        out
    }

    /// An animated PNG of `frames` frames of 8 × 8 (`acTL`, `fcTL`,
    /// `fdAT`), the first the default image.
    fn many_frame_png(frames: u32) -> Vec<u8> {
        let mut actl = frames.to_be_bytes().to_vec();
        actl.extend_from_slice(&0_u32.to_be_bytes());
        let fctl = |seq: u32| {
            let mut c = seq.to_be_bytes().to_vec();
            for v in [8_u32, 8, 0, 0] {
                c.extend_from_slice(&v.to_be_bytes());
            }
            c.extend_from_slice(&[0, 1, 0, 100, 0, 0]);
            c
        };
        let mut out = png_with(&[(*b"acTL", actl), (*b"fcTL", fctl(0))]);
        // Before IEND: the other frames.
        let iend = out.split_off(out.len() - 12);
        let raw = vec![0_u8; 8 * (1 + 8 * 3)];
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        zlib.write_all(&raw).unwrap();
        let pixels = zlib.finish().unwrap();
        let mut seq = 1;
        for _ in 1..frames {
            chunk(&mut out, *b"fcTL", &fctl(seq));
            let mut data = (seq + 1).to_be_bytes().to_vec();
            data.extend_from_slice(&pixels);
            chunk(&mut out, *b"fdAT", &data);
            seq += 2;
        }
        out.extend_from_slice(&iend);
        out
    }

    /// Metadata bombs (ICC profiles and text compressed from hundreds of
    /// MiB, megabytes of EXIF and of ICC segments) and an animated PNG of
    /// 10,000 frames: each is shown or a placeholder within the decode's
    /// timeout, and Rappel's own process does not grow with what the
    /// loader unpacks (ADR 0063).
    #[test]
    fn metadata_bombs_and_many_frames_stay_in_the_loader() {
        crate::testing::with_widgets(|| {
            let (tmp, _state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let folder = data.join("media");
            let bomb = zlib_of(0, 64 << 20);
            let text = zlib_of(b'a', 64 << 20);
            let mut iccp = b"icc\0\0".to_vec();
            iccp.extend_from_slice(&bomb);
            let mut ztxt = b"Comment\0\0".to_vec();
            ztxt.extend_from_slice(&text);
            let mut itxt = b"XML:com.adobe.xmp\0\x01\x00\0\0".to_vec();
            itxt.extend_from_slice(&text);
            let mut exif = b"MM\0\x2a\0\0\0\x08".to_vec();
            exif.resize(9 << 20, 0xAB);
            // A JPEG with 150 ICC segments of 64 KiB before its frame.
            let jpeg = {
                let base = include_bytes!("../../../core/src/media/fixtures/images/3x2.jpg");
                let mut out = base[..2].to_vec();
                for i in 0..150_u8 {
                    out.extend_from_slice(b"\xff\xe2\xff\xff");
                    out.extend_from_slice(b"ICC_PROFILE\0");
                    out.push(i + 1);
                    out.push(150);
                    out.resize(out.len() + 0xffff - 2 - 14, 0x5A);
                }
                out.extend_from_slice(&base[2..]);
                out
            };
            let cases = [
                (
                    "png",
                    png_with(&[(*b"zTXt", ztxt)]),
                    "a text of 64 MiB compressed",
                ),
                (
                    "png",
                    png_with(&[(*b"iTXt", itxt)]),
                    "XMP of 64 MiB compressed",
                ),
                ("png", png_with(&[(*b"eXIf", exif)]), "9 MiB of EXIF"),
                ("jpg", jpeg, "150 ICC segments of 64 KiB"),
                (
                    "png",
                    many_frame_png(10_000),
                    "an animated PNG of 10,000 frames",
                ),
                (
                    "png",
                    png_with(&[(*b"iCCP", iccp)]),
                    "an ICC profile of 64 MiB compressed",
                ),
            ];
            let before = peak_memory();
            for (seed, (ext, bytes, what)) in (70_u8..).zip(cases) {
                assert!(bytes.len() <= 10_000_000, "{what}: {} bytes", bytes.len());
                let media = plant(&data, seed, ext, &bytes);
                let started = std::time::Instant::now();
                let image = settle(&folder, media);
                crate::testing::print_timing(what, started.elapsed());
                assert!(
                    image.is_shown() || image.shown_failure() == Some(Failure::CannotShow),
                    "{what}: {:?}",
                    image.shown_failure()
                );
            }
            // An image with a profile is still shown, as sRGB.
            let icc = include_bytes!("../../../core/src/media/fixtures/images/4x3-icc.png");
            let media = plant(&data, 90, "png", icc);
            assert!(settle(&folder, media).is_shown());
            let grown = peak_memory().saturating_sub(before);
            assert!(
                grown < 128 << 20,
                "Rappel's process grew by {} MiB",
                grown >> 20
            );
        });
    }

    #[test]
    fn a_gif_with_many_frames_shows_its_first() {
        crate::testing::with_widgets(|| {
            let (tmp, _state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let media = plant(&data, 9, "gif", &many_frame_gif(20_000));
            let started = std::time::Instant::now();
            let image = settle(&data.join("media"), media);
            crate::testing::print_timing("a GIF of 20,000 frames", started.elapsed());
            assert!(image.is_shown(), "{:?}", image.shown_failure());
        });
    }

    #[test]
    fn a_side_s_images_share_their_pixels() {
        crate::testing::with_widgets(|| {
            let (tmp, state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let media = AppState::add_media_bytes_on_worker(
                &data,
                &crate::testing::png_bytes(30, 20),
                Accept::Image,
            )
            .unwrap();
            let folder = state.media_folder().unwrap();
            // Room for one 600-pixel image, not two.
            let pixels = Rc::new(RefCell::new(PixelBudget::new(1_000)));
            let first = CardImage::new(Some(&folder), media, "", 400, Rc::clone(&pixels));
            assert!(crate::testing::settle_until(
                || first.is_shown(),
                Duration::from_secs(60)
            ));
            let second = CardImage::new(Some(&folder), media, "", 400, pixels);
            assert_eq!(second.shown_failure(), Some(Failure::OverBudget));
        });
    }

    /// A decode past its timeout is cancelled and is a placeholder that is
    /// not cached (`Failure::is_lasting`).
    #[test]
    fn a_decode_past_its_timeout_gives_up() {
        crate::testing::with_widgets(|| {
            let (tmp, _state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let media = plant(&data, 7, "png", &crate::testing::png_bytes(8, 8));
            let path = data.join("media").join(media.file_name());
            let decoded =
                glib::MainContext::default().block_on(decode(path.clone(), media, Duration::ZERO));
            assert_eq!(decoded.err(), Some(Failure::TimedOut));
            assert!(!Failure::TimedOut.is_lasting());
            // With time, the same file decodes.
            let decoded =
                glib::MainContext::default().block_on(decode(path, media, DECODE_TIMEOUT));
            assert!(decoded.is_ok());
        });
    }
}
