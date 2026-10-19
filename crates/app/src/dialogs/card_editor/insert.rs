//! Images and audio in the editor (ADR 0055, 0056): Insert Image… and
//! Insert Audio… on each side, Ctrl+V of a picture or of media files, and
//! dropping them on a side. Each file is added to the media store on a
//! worker thread (`Store::add_image`, `add_audio`, or for a paste or a drop
//! of files `add_media`, which takes either: the kind from the first
//! bytes, at most 10 MB for an image and 20 MB for audio), then its
//! reference goes in at the cursor, or where it was dropped, as one
//! undoable step ([`media::insertion`]: `![](…)` for an image, `[](…)` for
//! audio). A refused file is a toast in the editor's own overlay saying
//! why ([`media::add_failed_text`]).
//!
//! Nothing is ever fetched: a file a drop or the clipboard names by a URL
//! (`https:`, `sftp:`) is refused unread (`AppState::add_media_on_worker`),
//! and when no offered file is local, the picture offered with them is
//! taken instead, or else their text as before
//! ([`media::without_local_file`]). A picture is taken as the bytes the
//! other program offers in one of the four image types, never decoded or
//! encoded in Rappel's process: the store checks its first bytes, and the
//! review page decodes it in glycin's loader like any image. Audio comes
//! only as files: no program offers sound on the clipboard in the kinds
//! kept, so the drop target takes no audio MIME type.
//!
//! An added file stays in the media folder whether or not the card is then
//! saved: it is content-addressed and harmless, and Clean Up Media in
//! Preferences removes it (ADR 0057).

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use rappel_core::media::{IMAGE_MAX_BYTES, MediaRef};

use super::CardEditor;
use super::media::{self, Offered, WithoutLocal};
use crate::i18n::gettext;
use crate::state::{Accept, AddMediaError, AppState, local_path};
use crate::ui::worker;
use rappel_core::domain::CardSide;

/// Something to add as media.
#[derive(Debug)]
pub enum Incoming {
    /// A file the chooser, the clipboard or a drop gave.
    File(gio::File),
    /// A picture's bytes from the clipboard or a drop: always added as an
    /// image.
    Bytes(Vec<u8>),
    /// A picture that could not be taken (too large, unreadable).
    Refused(AddMediaError),
}

impl CardEditor {
    /// The Insert Image… and Insert Audio… buttons, the paste handlers
    /// and the drop targets of both sides.
    pub(super) fn setup_media(&self) {
        let imp = self.imp();
        let sides = [
            (
                &*imp.front_image_button,
                &*imp.front_audio_button,
                &*imp.front_view,
                &*imp.front_frame,
                CardSide::Front,
            ),
            (
                &*imp.back_image_button,
                &*imp.back_audio_button,
                &*imp.back_view,
                &*imp.back_frame,
                CardSide::Back,
            ),
        ];
        for (image_button, audio_button, view, frame, name) in sides {
            image_button
                .update_property(&[gtk::accessible::Property::Label(&media::insert_label(name))]);
            audio_button.update_property(&[gtk::accessible::Property::Label(
                &media::insert_audio_label(name),
            )]);
            for (button, accept) in [(image_button, Accept::Image), (audio_button, Accept::Audio)] {
                button.connect_clicked(glib::clone!(
                    #[weak(rename_to = editor)]
                    self,
                    #[weak]
                    view,
                    move |_| editor.choose_files(&view, accept)
                ));
            }
            self.setup_paste(view);
            self.setup_drop(view, frame);
        }
    }

    /// Opens the file chooser, filtered to the image or the audio kinds
    /// (`accept`), and adds the chosen files at the cursor of `view`.
    fn choose_files(&self, view: &sourceview5::View, accept: Accept) {
        let filter = gtk::FileFilter::new();
        let title = if accept == Accept::Audio {
            filter.set_name(Some(&gettext("Audio")));
            for mime in media::AUDIO_MIME_TYPES {
                filter.add_mime_type(mime);
            }
            for suffix in media::AUDIO_SUFFIXES {
                filter.add_suffix(suffix);
            }
            gettext("Insert Audio")
        } else {
            filter.set_name(Some(&gettext("Images")));
            for mime in media::IMAGE_MIME_TYPES {
                filter.add_mime_type(mime);
            }
            gettext("Insert Image")
        };
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title(title.as_str())
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
                        editor.add_media(&view, mark, incoming, accept).await;
                    }
                    // Cancelled, or no chooser: nothing to add.
                    Err(err) => {
                        view.buffer().delete_mark(&mark);
                        if !err.matches(gtk::DialogError::Dismissed) {
                            tracing::warn!("the file chooser failed: {err}");
                        }
                    }
                }
            }
        ));
    }

    /// Ctrl+V (and the menu's Paste) of image or audio files or of a
    /// picture adds them; any other paste is the text view's own. `paste-clipboard` is
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

    /// Dropping image or audio files or a picture on a side adds them
    /// where they were dropped. The target is on the side's frame in the capture
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

    /// Does what a paste or a drop gave: adds the media, inserts the text
    /// at `mark` (a drop) or pastes the clipboard's text at the cursor (a
    /// paste), or nothing.
    async fn take(&self, view: &sourceview5::View, mark: gtk::TextMark, taken: Taken) {
        let buffer = view.buffer();
        match taken {
            Taken::Media(incoming, accept, instead) => {
                let added = self
                    .add_media_or(view, &mark, incoming, accept, instead.is_some())
                    .await;
                match (added, instead) {
                    (Added::NotMedia, Some(Instead::Text(text))) => {
                        insert_text(&buffer, &mark, &text);
                    }
                    // At the mark: where the paste was asked, even if the
                    // cursor moved while the files were read.
                    (Added::NotMedia, Some(Instead::Clipboard)) => {
                        let at = buffer.iter_at_mark(&mark);
                        buffer.paste_clipboard(&view.clipboard(), Some(&at), view.is_editable());
                    }
                    _ => {}
                }
            }
            Taken::Text(Some(text)) => insert_text(&buffer, &mark, &text),
            // The text view's own paste, which this handler stopped: the
            // same call its class handler makes (`gtk_text_view_paste_clipboard`).
            Taken::Text(None) => {
                buffer.paste_clipboard(&view.clipboard(), None, view.is_editable());
            }
            Taken::Nothing => {}
        }
        buffer.delete_mark(&mark);
    }

    /// Adds `incoming` one by one on a worker thread, files as `accept`
    /// takes them and a picture's bytes as an image, then inserts the
    /// references of those added at `mark` in `view`, and removes the
    /// mark. The first refusal is a toast.
    pub(super) async fn add_media(
        &self,
        view: &sourceview5::View,
        mark: gtk::TextMark,
        incoming: Vec<Incoming>,
        accept: Accept,
    ) {
        self.add_media_or(view, &mark, incoming, accept, false)
            .await;
        view.buffer().delete_mark(&mark);
    }

    /// [`Self::add_media`], leaving the mark; with `text_offered`, a paste
    /// or drop whose files are none of them media says nothing and returns
    /// [`Added::NotMedia`], for its text to go in instead.
    async fn add_media_or(
        &self,
        view: &sourceview5::View,
        mark: &gtk::TextMark,
        incoming: Vec<Incoming>,
        accept: Accept,
        text_offered: bool,
    ) -> Added {
        let imp = self.imp();
        let Some(data_dir) = imp.state.get().map(|state| state.data_dir().to_path_buf()) else {
            return Added::Done;
        };
        let mut added: Vec<MediaRef> = Vec::new();
        let mut refusals: Vec<AddMediaError> = Vec::new();
        let mut failure = None;
        for item in incoming {
            let data_dir = data_dir.clone();
            let result = match item {
                Incoming::File(file) => {
                    worker::run(self, move || {
                        AppState::add_media_on_worker(&data_dir, &file, accept)
                    })
                    .await
                }
                Incoming::Bytes(bytes) => {
                    worker::run(self, move || {
                        AppState::add_media_bytes_on_worker(&data_dir, &bytes, Accept::Image)
                    })
                    .await
                }
                Incoming::Refused(err) => Ok(Err(err)),
            };
            match result {
                Ok(Ok(media)) => added.push(media),
                Ok(Err(err)) => {
                    failure.get_or_insert_with(|| media::add_failed_text(&err, accept));
                    refusals.push(err);
                }
                Err(worker::Panicked) => {
                    failure.get_or_insert_with(|| media::could_not_add(accept));
                }
            }
        }
        let refused: Vec<&AddMediaError> = refusals.iter().collect();
        if media::text_instead(added.len(), &refused, text_offered) {
            return Added::NotMedia;
        }
        if let Some(message) = failure {
            imp.media_errors
                .show(&*imp.toast_overlay, "adding media", &message);
        }
        insert_references(view, mark, &added);
        Added::Done
    }
}

/// What [`CardEditor::add_media_or`] did.
enum Added {
    /// Added what it could and said what it could not.
    Done,
    /// None of the files was media: nothing done, the text goes in.
    NotMedia,
}

/// `text` inserted at `mark` as one user action (one Ctrl+Z).
fn insert_text(buffer: &gtk::TextBuffer, mark: &gtk::TextMark, text: &str) {
    let mut at = buffer.iter_at_mark(mark);
    buffer.begin_user_action();
    buffer.insert(&mut at, text);
    buffer.end_user_action();
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
    /// Media to add, files as the [`Accept`] takes them, and the text the
    /// offer has, given instead if no file is media ([`media::text_instead`]).
    Media(Vec<Incoming>, Accept, Option<Instead>),
    /// Text instead: a drop's (read here), or the clipboard's (`None`:
    /// pasted by the text view's own code).
    Text(Option<String>),
    Nothing,
}

/// The text a paste or a drop of files gives instead of them.
#[derive(Debug)]
enum Instead {
    /// The clipboard's, pasted by the text view's own code.
    Clipboard,
    /// A drop's, read with the files (a drop cannot be read once
    /// finished).
    Text(String),
}

/// Where pasted or dropped media come from: the same questions of
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
                    let instead = if !self.has_text() {
                        None
                    } else if let Self::Drop(_) = self {
                        self.read_text().await.map(Instead::Text)
                    } else {
                        Some(Instead::Clipboard)
                    };
                    return Taken::Media(
                        files.into_iter().map(Incoming::File).collect(),
                        Accept::Any,
                        instead,
                    );
                }
                match media::without_local_file(self.has_picture(), self.has_text()) {
                    WithoutLocal::Picture => {
                        Taken::Media(vec![self.read_picture().await], Accept::Image, None)
                    }
                    WithoutLocal::Text => match self {
                        Self::Clipboard(_) => Taken::Text(None),
                        Self::Drop(_) => self
                            .read_text()
                            .await
                            .map_or(Taken::Nothing, |text| Taken::Text(Some(text))),
                    },
                    WithoutLocal::Refuse if files.is_empty() => Taken::Nothing,
                    WithoutLocal::Refuse => Taken::Media(
                        files.into_iter().map(Incoming::File).collect(),
                        Accept::Any,
                        None,
                    ),
                }
            }
            Offered::Picture => Taken::Media(vec![self.read_picture().await], Accept::Image, None),
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
                Incoming::Refused(AddMediaError::Unreadable(std::io::Error::other(
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
                return Incoming::Refused(AddMediaError::Unreadable(std::io::Error::other(
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
                        return Incoming::Refused(AddMediaError::TooLarge);
                    }
                }
                Err(err) => {
                    return Incoming::Refused(AddMediaError::Unreadable(std::io::Error::other(
                        err.to_string(),
                    )));
                }
            }
        }
        Incoming::Bytes(bytes)
    }
}

/// Inserts the references of `refs` at `mark` as one user action (one
/// Ctrl+Z takes it back), puts the cursor in the first one's words (an
/// image's alt text, an audio link's text) and the focus in the view.
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
            block_on(editor.add_media(
                view,
                cursor_mark(view),
                vec![Incoming::Bytes(png)],
                Accept::Image,
            ));
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false).to_string();
            let reference = text
                .strip_prefix("a ![](")
                .and_then(|rest| rest.strip_suffix(") b"))
                .expect("the reference between the letters");
            assert!(MediaRef::parse(reference).is_some(), "{reference}");
            let cursor = buffer.iter_at_mark(&buffer.get_insert()).offset();
            assert_eq!(cursor, 4, "between [ and ]");
            assert!(imp.media_errors.is_empty());
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
                block_on(editor.add_media(
                    view,
                    cursor_mark(view),
                    vec![Incoming::File(file)],
                    Accept::Image,
                ));
                assert_eq!(imp.media_errors.shown_text().as_deref(), Some(text));
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
                for accept in [Accept::Image, Accept::Audio, Accept::Any] {
                    block_on(editor.add_media(
                        view,
                        cursor_mark(view),
                        vec![Incoming::File(file.clone())],
                        accept,
                    ));
                    assert_eq!(
                        imp.media_errors.shown_text().as_deref(),
                        Some("Only files on this computer can be added")
                    );
                }
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
                move || {
                    AppState::add_media_on_worker(&data, &gio::File::for_path(&fifo), Accept::Any)
                },
            );
            assert!(
                matches!(refused, Err(AddMediaError::NotAFile)),
                "{refused:?}"
            );
        });
    }

    #[test]
    fn an_added_audio_file_is_a_link_with_the_cursor_in_its_words() {
        crate::testing::with_widgets(|| {
            let (tmp, _state, _deck, editor, _saved) = super::super::tests::editor_over(None);
            let imp = editor.imp();
            let buffer = &*imp.back_buffer;
            buffer.set_text("Hear:");
            buffer.place_cursor(&buffer.end_iter());
            let view = &*imp.back_view;
            let wav = tmp.path().join("tone.wav");
            std::fs::write(&wav, crate::testing::wav_bytes(8_000)).unwrap();
            // Insert Audio…: only audio.
            block_on(editor.add_media(
                view,
                cursor_mark(view),
                vec![Incoming::File(gio::File::for_path(&wav))],
                Accept::Audio,
            ));
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false).to_string();
            let reference = text
                .strip_prefix("Hear: [](")
                .and_then(|rest| rest.strip_suffix(')'))
                .expect("an audio link after the words");
            let media = MediaRef::parse(reference).expect("a media reference");
            assert!(!media.kind().is_image());
            let cursor = buffer.iter_at_mark(&buffer.get_insert()).offset();
            assert_eq!(cursor, 7, "between [ and ]");
            assert!(imp.media_errors.is_empty());
            buffer.undo();
            assert_eq!(buffer.char_count(), 5);
            // An image chosen for Insert Audio… is refused, in words.
            let png = tmp.path().join("x.png");
            std::fs::write(&png, crate::testing::png_bytes(2, 2)).unwrap();
            block_on(editor.add_media(
                view,
                cursor_mark(view),
                vec![Incoming::File(gio::File::for_path(&png))],
                Accept::Audio,
            ));
            assert_eq!(
                imp.media_errors.shown_text().as_deref(),
                Some("This file is not an Ogg, MP3, M4A or WAV audio file")
            );
            assert_eq!(buffer.char_count(), 5);
            // An audio file chosen for Insert Image… likewise.
            block_on(editor.add_media(
                view,
                cursor_mark(view),
                vec![Incoming::File(gio::File::for_path(&wav))],
                Accept::Image,
            ));
            assert_eq!(
                imp.media_errors.shown_text().as_deref(),
                Some("This file is not a PNG, JPEG, WebP or GIF image")
            );
            // Too large.
            let big = tmp.path().join("big.wav");
            let mut bytes = crate::testing::wav_bytes(8_000);
            bytes.resize(20_000_001, 128);
            std::fs::write(&big, &bytes).unwrap();
            block_on(editor.add_media(
                view,
                cursor_mark(view),
                vec![Incoming::File(gio::File::for_path(&big))],
                Accept::Audio,
            ));
            assert_eq!(
                imp.media_errors.shown_text().as_deref(),
                Some("This audio file is larger than 20 MB")
            );
            assert_eq!(buffer.char_count(), 5);
        });
    }

    /// Files pasted or dropped together go in each as its kind, whatever
    /// their names say.
    #[test]
    fn dropped_files_go_in_as_their_kinds() {
        crate::testing::with_widgets(|| {
            let (tmp, _state, _deck, editor, _saved) = super::super::tests::editor_over(None);
            let imp = editor.imp();
            let view = &*imp.front_view;
            // A WAV called .png and a PNG called .wav.
            let sound = tmp.path().join("sound.png");
            std::fs::write(&sound, crate::testing::wav_bytes(8_000)).unwrap();
            let picture = tmp.path().join("picture.wav");
            std::fs::write(&picture, crate::testing::png_bytes(3, 3)).unwrap();
            let files = [&picture, &sound]
                .map(|path| Incoming::File(gio::File::for_path(path)))
                .into_iter()
                .collect();
            block_on(editor.take(
                view,
                cursor_mark(view),
                Taken::Media(files, Accept::Any, None),
            ));
            let buffer = &*imp.front_buffer;
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false).to_string();
            let parts: Vec<_> = text.split("\n\n").collect();
            assert_eq!(parts.len(), 2, "{text}");
            assert!(parts[0].starts_with("![](media:") && parts[0].ends_with(".png)"));
            assert!(parts[1].starts_with("[](media:") && parts[1].ends_with(".wav)"));
            assert!(imp.media_errors.is_empty());
        });
    }

    /// A copied or dropped file that is neither an image nor audio (a text
    /// file, a folder) goes in as the text the offer has, its path, as a
    /// text editor pastes it; with no text offered it is refused as
    /// before.
    #[test]
    fn files_that_are_not_media_go_in_as_their_text() {
        crate::testing::with_widgets(|| {
            let (tmp, _state, _deck, editor, _saved) = super::super::tests::editor_over(None);
            let imp = editor.imp();
            let view = &*imp.front_view;
            let notes = tmp.path().join("notes.txt");
            std::fs::write(&notes, "plain text").unwrap();
            let path = notes.to_string_lossy().into_owned();
            let files = || vec![Incoming::File(gio::File::for_path(&notes))];
            // A drop: the text read with the files.
            block_on(editor.take(
                view,
                cursor_mark(view),
                Taken::Media(files(), Accept::Any, Some(Instead::Text(path.clone()))),
            ));
            let buffer = &*imp.front_buffer;
            let text = |b: &sourceview5::Buffer| {
                let (start, end) = b.bounds();
                b.text(&start, &end, false).to_string()
            };
            assert_eq!(text(buffer), path);
            assert!(imp.media_errors.is_empty());
            buffer.undo();
            assert_eq!(buffer.char_count(), 0);
            // A paste: no refusal; the text view's own paste puts the
            // clipboard's text in (`gtk_text_buffer_paste_clipboard`, the
            // path the editor takes for a plain text paste too).
            block_on(editor.take(
                view,
                cursor_mark(view),
                Taken::Media(files(), Accept::Any, Some(Instead::Clipboard)),
            ));
            assert!(imp.media_errors.is_empty());
            // No text offered: refused, as before.
            let back = &*imp.back_view;
            block_on(editor.take(
                back,
                cursor_mark(back),
                Taken::Media(files(), Accept::Any, None),
            ));
            assert_eq!(
                imp.media_errors.shown_text().as_deref(),
                Some("This file is not an image or audio file of a supported kind")
            );
            assert_eq!(imp.back_buffer.char_count(), 0);
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
            assert!(imp.media_errors.is_empty());
            // One step to undo.
            buffer.undo();
            assert_eq!(buffer.char_count(), 0);
            // Nothing taken: nothing changes, no mark is left.
            block_on(editor.take(view, cursor_mark(view), Taken::Nothing));
            assert_eq!(buffer.char_count(), 0);
        });
    }
}
