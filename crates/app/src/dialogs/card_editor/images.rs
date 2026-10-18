//! Images in the editor (ADR 0055): Insert Image… on each side, Ctrl+V of
//! a picture or of image files, and dropping them on a side. Each file is
//! added to the media store on a worker thread (`Store::add_image`: its
//! kind from its first bytes, at most 10 MB), then its reference goes in
//! at the cursor, or where it was dropped, as one undoable step
//! ([`media::insertion`]). A refused file is a toast in the editor's own
//! overlay saying why ([`media::add_failed_text`]).
//!
//! An added file stays in the media folder whether or not the card is then
//! saved: it is content-addressed and harmless, and the clean-up of unused
//! media (phase 2, slice 5) removes it.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use rappel_core::media::MediaRef;

use super::CardEditor;
use super::media::{self, Offered};
use crate::state::AppState;
use crate::ui::worker;

/// Something to add as an image.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// A file the chooser, the clipboard or a drop gave.
    File(gio::File),
    /// A picture from the clipboard or a drop, encoded as PNG.
    Png(glib::Bytes),
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
                let formats = clipboard.formats();
                let kind = media::offered(
                    formats.contains_type(gdk::FileList::static_type()),
                    formats.contains_type(gdk::Texture::static_type()),
                );
                if kind == Offered::Text {
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
                        let incoming = read_clipboard(&clipboard, kind).await;
                        editor.add_images(&view, mark, incoming).await;
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
    /// stops it there (`gtk_drop_target_handle_event`, gtkdroptarget.c). A
    /// drag of plain text offers neither type, so this target never takes
    /// it and the text view's does.
    fn setup_drop(&self, view: &sourceview5::View, frame: &gtk::Frame) {
        let target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        target.set_types(&[gdk::FileList::static_type(), gdk::Texture::static_type()]);
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
            move |_, value, x, y| {
                let incoming = if let Ok(files) = value.get::<gdk::FileList>() {
                    files.files().into_iter().map(Incoming::File).collect()
                } else if let Ok(texture) = value.get::<gdk::Texture>() {
                    vec![Incoming::Png(texture.save_to_png_bytes())]
                } else {
                    return false;
                };
                let mark = drop_mark(&view, &frame, x, y);
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    editor,
                    #[weak]
                    view,
                    async move { editor.add_images(&view, mark, incoming).await }
                ));
                true
            }
        ));
        frame.add_controller(target);
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
            let result = worker::run(self, move || match item {
                Incoming::File(file) => AppState::add_image_on_worker(&data_dir, &file),
                Incoming::Png(bytes) => AppState::add_image_bytes_on_worker(&data_dir, &bytes),
            })
            .await;
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

/// What the clipboard holds as images, read as `kind`. A failed read is
/// logged and adds nothing.
async fn read_clipboard(clipboard: &gdk::Clipboard, kind: Offered) -> Vec<Incoming> {
    match kind {
        Offered::Files => {
            match clipboard
                .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
                .await
            {
                Ok(value) => value
                    .get::<gdk::FileList>()
                    .map(|files| files.files().into_iter().map(Incoming::File).collect())
                    .unwrap_or_default(),
                Err(err) => {
                    tracing::warn!("the clipboard's files could not be read: {err}");
                    Vec::new()
                }
            }
        }
        Offered::Texture => match clipboard.read_texture_future().await {
            Ok(Some(texture)) => vec![Incoming::Png(texture.save_to_png_bytes())],
            Ok(None) => Vec::new(),
            Err(err) => {
                tracing::warn!("the clipboard's picture could not be read: {err}");
                Vec::new()
            }
        },
        Offered::Text => Vec::new(),
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
            let png = glib::Bytes::from_owned(crate::testing::png_bytes(4, 3));
            block_on(editor.add_images(view, cursor_mark(view), vec![Incoming::Png(png)]));
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
}
