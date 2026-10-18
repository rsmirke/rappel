//! Images in the editor (ADR 0055): Insert Image… on each side, Ctrl+V of
//! a picture or of image files, and dropping them on a side. Each file is
//! added to the media store on a worker thread (`Store::add_image`: its
//! kind from its first bytes, at most 10 MB), then its reference goes in
//! at the cursor, or where it was dropped, as one undoable step
//! ([`media::insertion`]). A refused file is a toast in the editor's own
//! overlay saying why ([`media::add_failed_text`]).
//!
//! Nothing is ever fetched: a file a drop or the clipboard names by a URL
//! (`https:`, `sftp:`) is refused unread (`AppState::add_image_on_worker`),
//! and when no offered file is local, the picture offered with them is
//! taken instead, or else their text as before ([`media::without_local_file`]). A picture is taken as the bytes
//! the other program offers in one of the four image types, never decoded
//! or encoded in Rappel's process: the store checks its first bytes, and
//! the review page decodes it in glycin's loader like any image.
//!
//! An added file stays in the media folder whether or not the card is then
//! saved: it is content-addressed and harmless, and the clean-up of unused
//! media (phase 2, slice 5) removes it.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use rappel_core::media::{IMAGE_MAX_BYTES, MediaRef};

use super::CardEditor;
use super::media::{self, Offered, WithoutLocal};
use crate::state::{AddImageError, AppState, local_path};
use crate::ui::worker;

/// Something to add as an image.
#[derive(Debug)]
pub enum Incoming {
    /// A file the chooser, the clipboard or a drop gave.
    File(gio::File),
    /// A picture's bytes from the clipboard or a drop.
    Bytes(Vec<u8>),
    /// A picture that could not be taken (too large, unreadable).
    Refused(AddImageError),
}

impl CardEditor {
    /// The Insert Image… buttons, the paste handlers and the drop targets
    /// of both sides.
    pub(super) fn setup_images(&self) {
        let imp = self.imp();
        let sides = [
            (
                &*imp.front_image_button,
                &*imp.front_view,
                &*imp.front_frame,
                "Front",
            ),
            (
                &*imp.back_image_button,
                &*imp.back_view,
                &*imp.back_frame,
                "Back",
            ),
        ];
        for (button, view, frame, name) in sides {
            button.update_property(&[gtk::accessible::Property::Label(&media::insert_label(name))]);
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = editor)]
                self,
                #[weak]
                view,
                move |_| editor.choose_images(&view)
            ));
            self.setup_paste(view);
            self.setup_drop(view, frame);
        }
    }

    /// Opens the file chooser, filtered to the image kinds, and adds the
    /// chosen files at the cursor of `view`.
    fn choose_images(&self, view: &sourceview5::View) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        for mime in media::IMAGE_MIME_TYPES {
            filter.add_mime_type(mime);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title("Insert Image")
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();
        let window = self.root().and_downcast::<gtk::Window>();
        let mark = cursor_mark(view);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = editor)]
            self,
            #[weak]
            view,
            async move {
                match chooser.open_multiple_future(window.as_ref()).await {
                    Ok(files) => {
                        let incoming = files
                            .iter::<gio::File>()
                            .filter_map(Result::ok)
                            .map(Incoming::File)
                            .collect();
                        editor.add_images(&view, mark, incoming).await;
                    }
                    // Cancelled, or no chooser: nothing to add.
                    Err(err) => {
                        view.buffer().delete_mark(&mark);
                        if !err.matches(gtk::DialogError::Dismissed) {
                            tracing::warn!("the image chooser failed: {err}");
                        }
                    }
                }
            }
        ));
    }

    /// Ctrl+V (and the menu's Paste) of image files or of a picture adds
    /// them; any other paste is the text view's own. `paste-clipboard` is
    /// a run-last action signal (gtktextview.c, GTK 4.24): this handler
    /// runs before the class handler, which stopping the emission skips.
    /// The clipboard is read asynchronously, so the window never waits.
    fn setup_paste(&self, view: &sourceview5::View) {
        view.connect_paste_clipboard(glib::clone!(
            #[weak(rename_to = editor)]
            self,
            move |view| {
                let clipboard = view.clipboard();
                let offer = Offer::Clipboard(clipboard);
                if offer.offered() == Offered::Text {
                    return;
                }
                view.stop_signal_emission_by_name("paste-clipboard");
                let mark = cursor_mark(view);
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    editor,
                    #[weak]
                    view,
                    async move {
                        let taken = offer.read().await;
                        editor.take(&view, mark, taken).await;
                    }
                ));
            }
        ));
    }

    /// Dropping image files or a picture on a side adds them where they
    /// were dropped. The target is on the side's frame in the capture
    /// phase, so it sees a drop before the text view's own target (which
    /// takes text, `G_TYPE_STRING`, gtktextview.c): drop events go through
    /// the capture and bubble phases like others (`gtk_propagate_event`,
    /// gtkmain.c), the first target to answer a motion sets the status
    /// (`gtk_drop_status`, gtkdrop.c), and a target that takes the drop
    /// stops it there. A drag of plain text offers none of its formats,
    /// so the target does not accept it (`gtk_drop_target_async_accept`,
    /// gtkdroptargetasync.c) and the text view takes it.
    ///
    /// An async target, not `GtkDropTarget`: that one loads a single type
    /// before its `drop` signal (the first of its types the drop offers,
    /// `gdk_content_formats_match_gtype`), so it could not fall back to the
    /// picture when the files turn out to be URLs. This one reads what it
    /// chooses and finishes the drop itself (`gdk_drop_finish`).
    ///
    /// Its formats are the file list's type and the image types, with the
    /// MIME types that deserialize to the file list (`text/uri-list`)
    /// added (`union_deserialize_mime_types`), as GDK does for a read of a
    /// type (`gdk_drop_read_value_internal`, gdkdrop.c), so a drag from
    /// another process, which offers only MIME types, matches.
    fn setup_drop(&self, view: &sourceview5::View, frame: &gtk::Frame) {
        let mut formats = gdk::ContentFormatsBuilder::new().add_type(gdk::FileList::static_type());
        for mime in media::IMAGE_MIME_TYPES {
            formats = formats.add_mime_type(mime);
        }
        let formats = formats.build().union_deserialize_mime_types();
        let target = gtk::DropTargetAsync::new(Some(formats), gdk::DragAction::COPY);
        target.set_propagation_phase(gtk::PropagationPhase::Capture);
        target.connect_drop(glib::clone!(
            #[weak(rename_to = editor)]
            self,
            #[weak]
            view,
            #[weak]
            frame,
            #[upgrade_or]
            false,
            move |_, drop, x, y| {
                let mark = drop_mark(&view, &frame, x, y);
                let drop = drop.clone();
                let editor = editor.downgrade();
                let view = view.downgrade();
                // The drop is held strongly and finished as soon as it is
                // read, before anything needs the editor: a dialog closed
                // meanwhile still finishes it, so the source is not left
                // waiting.
                glib::spawn_future_local(async move {
                    let taken = Offer::Drop(drop.clone()).read().await;
                    drop.finish(if matches!(taken, Taken::Nothing) {
                        gdk::DragAction::empty()
                    } else {
                        gdk::DragAction::COPY
                    });
                    if let (Some(editor), Some(view)) = (editor.upgrade(), view.upgrade()) {
                        editor.take(&view, mark, taken).await;
                    }
                });
                true
            }
        ));
        frame.add_controller(target);
    }

    /// Does what a paste or a drop gave: adds the images, inserts the text
    /// at `mark` (a drop) or pastes the clipboard's text at the cursor (a
    /// paste), or nothing.
    async fn take(&self, view: &sourceview5::View, mark: gtk::TextMark, taken: Taken) {
        let buffer = view.buffer();
        match taken {
            Taken::Images(incoming) => {
                self.add_images(view, mark, incoming).await;
                return;
            }
            Taken::Text(Some(text)) => {
                let mut at = buffer.iter_at_mark(&mark);
                buffer.begin_user_action();
                buffer.insert(&mut at, &text);
                buffer.end_user_action();
            }
            // The text view's own paste, which this handler stopped: the
            // same call its class handler makes (`gtk_text_view_paste_clipboard`).
            Taken::Text(None) => {
                buffer.paste_clipboard(&view.clipboard(), None, view.is_editable());
            }
            Taken::Nothing => {}
        }
        buffer.delete_mark(&mark);
    }

    /// Adds `incoming` one by one on a worker thread, then inserts the
    /// references of those added at `mark` in `view`, and removes the
    /// mark. The first refusal is a toast.
    pub(super) async fn add_images(
        &self,
        view: &sourceview5::View,
        mark: gtk::TextMark,
        incoming: Vec<Incoming>,
    ) {
        let imp = self.imp();
        let Some(data_dir) = imp.state.get().map(|state| state.data_dir().to_path_buf()) else {
            view.buffer().delete_mark(&mark);
            return;
        };
        let mut added: Vec<MediaRef> = Vec::new();
        let mut failure = None;
        for item in incoming {
            let data_dir = data_dir.clone();
            let result = match item {
                Incoming::File(file) => {
                    worker::run(self, move || {
                        AppState::add_image_on_worker(&data_dir, &file)
                    })
                    .await
                }
                Incoming::Bytes(bytes) => {
                    worker::run(self, move || {
                        AppState::add_image_bytes_on_worker(&data_dir, &bytes)
                    })
                    .await
                }
                Incoming::Refused(err) => Ok(Err(err)),
            };
            match result {
                Ok(Ok(media)) => added.push(media),
                Ok(Err(err)) => {
                    failure.get_or_insert_with(|| media::add_failed_text(&err));
                }
                Err(worker::Panicked) => {
                    failure.get_or_insert_with(|| "The image could not be added".to_owned());
                }
            }
        }
        if let Some(message) = failure {
            imp.image_errors
                .show(&*imp.toast_overlay, "adding an image", &message);
        }
        insert_references(view, &mark, &added);
        view.buffer().delete_mark(&mark);
    }
}

/// A mark at the cursor of `view`, which stays where it is while text is
/// typed after it (left gravity), so a slow add still lands there.
fn cursor_mark(view: &sourceview5::View) -> gtk::TextMark {
    let buffer = view.buffer();
    let at = buffer.iter_at_mark(&buffer.get_insert());
    buffer.create_mark(None, &at, true)
}

/// A mark where `x`, `y` (in `frame`'s coordinates) falls in `view`'s
/// text, or at the cursor if that is outside it.
fn drop_mark(view: &sourceview5::View, frame: &gtk::Frame, x: f64, y: f64) -> gtk::TextMark {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "widget coordinates fit an f32"
    )]
    let point = frame.compute_point(view, &gtk::graphene::Point::new(x as f32, y as f32));
    let iter = point.and_then(|point| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "widget coordinates fit an i32"
        )]
        let (bx, by) = view.window_to_buffer_coords(
            gtk::TextWindowType::Widget,
            point.x().round() as i32,
            point.y().round() as i32,
        );
        view.iter_at_location(bx, by)
    });
    match iter {
        Some(iter) => view.buffer().create_mark(None, &iter, true),
        None => cursor_mark(view),
    }
}

/// How long reading what a paste or a drop offers may take: the other
/// program sends it, and may never finish.
const OFFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What a paste or a drop gives, once read.
#[derive(Debug)]
enum Taken {
    /// Images (or files) to add.
    Images(Vec<Incoming>),
    /// Text instead: a drop's (read here), or the clipboard's (`None`:
    /// pasted by the text view's own code).
    Text(Option<String>),
    Nothing,
}

/// Where pasted or dropped images come from: the same questions of
/// either.
enum Offer {
    Clipboard(gdk::Clipboard),
    Drop(gdk::Drop),
}

impl Offer {
    /// The offered formats with the types they deserialize to. GDK adds
    /// these itself when a clipboard or a drop gets its formats
    /// (`gdk_clipboard_claim`, gdkclipboard.c; `gdk_drop_add_formats`,
    /// gdkdrop.c: `gdk_content_formats_union_deserialize_gtypes`), so a
    /// `text/uri-list` from another process is a file list here; done
    /// again so the check does not rest on that.
    fn formats(&self) -> gdk::ContentFormats {
        match self {
            Self::Clipboard(clipboard) => clipboard.formats(),
            Self::Drop(drop) => drop.formats(),
        }
        .union_deserialize_types()
    }

    /// Whether text is offered too (the addresses of the files, as text).
    fn has_text(&self) -> bool {
        self.formats().contains_type(glib::Type::STRING)
    }

    /// Whether a picture is offered in one of the image types.
    fn has_picture(&self) -> bool {
        let formats = self.formats();
        media::IMAGE_MIME_TYPES
            .iter()
            .any(|mime| formats.contain_mime_type(mime))
    }

    fn offered(&self) -> Offered {
        media::offered(
            self.formats().contains_type(gdk::FileList::static_type()),
            self.has_picture(),
        )
    }

    /// What to take: the files if one is on this computer; else the
    /// picture, or the text, or the files to be refused
    /// ([`media::without_local_file`]). A failed or slow read is logged and
    /// gives nothing.
    async fn read(&self) -> Taken {
        match self.offered() {
            Offered::Files => {
                let files = self.read_files().await;
                let local = files.iter().filter(|f| local_path(f).is_some()).count();
                if local > 0 {
                    return Taken::Images(files.into_iter().map(Incoming::File).collect());
                }
                match media::without_local_file(self.has_picture(), self.has_text()) {
                    WithoutLocal::Picture => Taken::Images(vec![self.read_picture().await]),
                    WithoutLocal::Text => match self {
                        Self::Clipboard(_) => Taken::Text(None),
                        Self::Drop(_) => self
                            .read_text()
                            .await
                            .map_or(Taken::Nothing, |text| Taken::Text(Some(text))),
                    },
                    WithoutLocal::Refuse if files.is_empty() => Taken::Nothing,
                    WithoutLocal::Refuse => {
                        Taken::Images(files.into_iter().map(Incoming::File).collect())
                    }
                }
            }
            Offered::Picture => Taken::Images(vec![self.read_picture().await]),
            Offered::Text => Taken::Nothing,
        }
    }

    /// The value of type `ty` offered, within [`OFFER_TIMEOUT`].
    async fn read_value(&self, ty: glib::Type) -> Option<glib::Value> {
        let read = async {
            match self {
                Self::Clipboard(clipboard) => {
                    clipboard
                        .read_value_future(ty, glib::Priority::DEFAULT)
                        .await
                }
                Self::Drop(drop) => drop.read_value_future(ty, glib::Priority::DEFAULT).await,
            }
        };
        match glib::future_with_timeout(OFFER_TIMEOUT, read).await {
            Ok(Ok(value)) => Some(value),
            Ok(Err(err)) => {
                tracing::warn!("what was offered could not be read: {err}");
                None
            }
            Err(_) => {
                tracing::warn!("what was offered took too long to read");
                None
            }
        }
    }

    async fn read_files(&self) -> Vec<gio::File> {
        self.read_value(gdk::FileList::static_type())
            .await
            .and_then(|value| value.get::<gdk::FileList>().ok())
            .map(|files| files.files())
            .unwrap_or_default()
    }

    async fn read_text(&self) -> Option<String> {
        self.read_value(glib::Type::STRING)
            .await
            .and_then(|value| value.get::<String>().ok())
    }

    /// The picture's bytes, at most an image's limit: read as a stream,
    /// stopping one chunk past it, within [`OFFER_TIMEOUT`].
    async fn read_picture(&self) -> Incoming {
        glib::future_with_timeout(OFFER_TIMEOUT, self.read_picture_bytes())
            .await
            .unwrap_or_else(|_| {
                Incoming::Refused(AddImageError::Unreadable(std::io::Error::other(
                    "the picture took too long to read",
                )))
            })
    }

    async fn read_picture_bytes(&self) -> Incoming {
        let read = match self {
            Self::Clipboard(clipboard) => {
                clipboard
                    .read_future(&media::IMAGE_MIME_TYPES, glib::Priority::DEFAULT)
                    .await
            }
            Self::Drop(drop) => {
                drop.read_future(&media::IMAGE_MIME_TYPES, glib::Priority::DEFAULT)
                    .await
            }
        };
        let stream = match read {
            Ok((stream, _mime)) => stream,
            Err(err) => {
                return Incoming::Refused(AddImageError::Unreadable(std::io::Error::other(
                    err.to_string(),
                )));
            }
        };
        let limit = usize::try_from(IMAGE_MAX_BYTES).unwrap_or(usize::MAX);
        let mut bytes = Vec::new();
        loop {
            match stream
                .read_bytes_future(64 << 10, glib::Priority::DEFAULT)
                .await
            {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => {
                    bytes.extend_from_slice(&chunk);
                    if bytes.len() > limit {
                        return Incoming::Refused(AddImageError::TooLarge);
                    }
                }
                Err(err) => {
                    return Incoming::Refused(AddImageError::Unreadable(std::io::Error::other(
                        err.to_string(),
                    )));
                }
            }
        }
        Incoming::Bytes(bytes)
    }
}

/// Inserts the references of `refs` at `mark` as one user action (one
/// Ctrl+Z takes it back), puts the cursor in the first one's alt text and
/// the focus in the view.
pub(super) fn insert_references(view: &sourceview5::View, mark: &gtk::TextMark, refs: &[MediaRef]) {
    if refs.is_empty() {
        return;
    }
    let buffer = view.buffer();
    let mut at = buffer.iter_at_mark(mark);
    let before = {
        let mut back = at;
        back.backward_char().then(|| back.char())
    };
    let after = (!at.is_end()).then(|| at.char());
    let insertion = media::insertion(refs, before, after);
    let start = at.offset();
    buffer.begin_user_action();
    buffer.insert(&mut at, &insertion.text);
    buffer.end_user_action();
    let cursor = start.saturating_add(i32::try_from(insertion.cursor).unwrap_or(i32::MAX));
    buffer.place_cursor(&buffer.iter_at_offset(cursor));
    view.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        glib::MainContext::default().block_on(f)
    }

    #[test]
    fn an_added_image_is_one_undoable_insert_with_the_cursor_in_its_alt_text() {
        crate::testing::with_widgets(|| {
            let (_tmp, _state, _deck, editor, _saved) = super::super::tests::editor_over(None);
            let imp = editor.imp();
            let buffer = &*imp.front_buffer;
            buffer.set_text("ab");
            buffer.place_cursor(&buffer.iter_at_offset(1));
            let view = &*imp.front_view;
            let png = crate::testing::png_bytes(4, 3);
            block_on(editor.add_images(view, cursor_mark(view), vec![Incoming::Bytes(png)]));
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false).to_string();
            let reference = text
                .strip_prefix("a ![](")
                .and_then(|rest| rest.strip_suffix(") b"))
                .expect("the reference between the letters");
            assert!(MediaRef::parse(reference).is_some(), "{reference}");
            let cursor = buffer.iter_at_mark(&buffer.get_insert()).offset();
            assert_eq!(cursor, 4, "between [ and ]");
            assert!(imp.image_errors.is_empty());
            // One Ctrl+Z takes the whole reference back.
            buffer.undo();
            let (start, end) = buffer.bounds();
            assert_eq!(buffer.text(&start, &end, false), "ab");
        });
    }

    #[test]
    fn a_refused_file_says_why_and_inserts_nothing() {
        crate::testing::with_widgets(|| {
            let (tmp, _state, _deck, editor, _saved) = super::super::tests::editor_over(None);
            let imp = editor.imp();
            let view = &*imp.back_view;
            let svg = tmp.path().join("picture.png");
            std::fs::write(&svg, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
            let big = tmp.path().join("big.png");
            let mut bytes = crate::testing::png_bytes(2, 2);
            bytes.resize(10_000_001, 0);
            std::fs::write(&big, &bytes).unwrap();
            for (path, text) in [
                (&svg, "This file is not a PNG, JPEG, WebP or GIF image"),
                (&big, "This image is larger than 10 MB"),
            ] {
                let file = gio::File::for_path(path);
                block_on(editor.add_images(view, cursor_mark(view), vec![Incoming::File(file)]));
                assert_eq!(imp.image_errors.shown_text().as_deref(), Some(text));
                assert_eq!(imp.back_buffer.char_count(), 0);
            }
            // Nothing was kept for them.
            let media = tmp.path().join("data/media");
            let kept = std::fs::read_dir(&media).map_or(0, |dir| {
                dir.filter_map(Result::ok)
                    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    .count()
            });
            assert_eq!(kept, 0);
        });
    }

    #[test]
    fn a_url_is_refused_unread_and_a_pipe_is_not_waited_on() {
        crate::testing::with_widgets(|| {
            let (tmp, _state, _deck, editor, _saved) = super::super::tests::editor_over(None);
            let imp = editor.imp();
            let view = &*imp.front_view;
            // GVfs would fetch these; nothing may read them.
            for uri in [
                "https://example.invalid/x.png",
                "sftp://example.invalid/x.png",
                "smb://example.invalid/share/x.png",
            ] {
                let file = gio::File::for_uri(uri);
                assert_eq!(local_path(&file), None, "{uri}");
                block_on(editor.add_images(view, cursor_mark(view), vec![Incoming::File(file)]));
                assert_eq!(
                    imp.image_errors.shown_text().as_deref(),
                    Some("Only files on this computer can be added")
                );
            }
            assert_eq!(imp.front_buffer.char_count(), 0);
            // A local file is a path.
            let png = tmp.path().join("x.png");
            assert_eq!(local_path(&gio::File::for_path(&png)), Some(png));
            // A named pipe would block an open for reading until a writer
            // comes: it is opened without blocking and refused.
            let fifo = tmp.path().join("pipe.png");
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("mkfifo runs");
            assert!(made.success());
            let data = tmp.path().join("data");
            let refused = crate::testing::within(
                std::time::Duration::from_secs(2),
                "adding a pipe",
                move || AppState::add_image_on_worker(&data, &gio::File::for_path(&fifo)),
            );
            assert!(
                matches!(refused, Err(AddImageError::NotAFile)),
                "{refused:?}"
            );
        });
    }

    #[test]
    fn a_dropped_link_without_a_picture_is_text_as_before() {
        crate::testing::with_widgets(|| {
            let (_tmp, _state, _deck, editor, _saved) = super::super::tests::editor_over(None);
            let imp = editor.imp();
            let view = &*imp.back_view;
            let link = "https://example.invalid/x.png";
            block_on(editor.take(view, cursor_mark(view), Taken::Text(Some(link.to_owned()))));
            let buffer = &*imp.back_buffer;
            let (start, end) = buffer.bounds();
            assert_eq!(buffer.text(&start, &end, false), link);
            assert!(imp.image_errors.is_empty());
            // One step to undo.
            buffer.undo();
            assert_eq!(buffer.char_count(), 0);
            // Nothing taken: nothing changes, no mark is left.
            block_on(editor.take(view, cursor_mark(view), Taken::Nothing));
            assert_eq!(buffer.char_count(), 0);
        });
    }
}
