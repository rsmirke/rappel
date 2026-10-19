//! A card's audio on screen (ADR 0056): [`CardAudio`], a play button with
//! the link's words, and beside it the time played or why it cannot play.
//! The decisions are in [`super::audio`].
//!
//! Nothing plays until the user asks: a click on the button, its Enter or
//! Space, or R on the review page ([`CardAudio::play_from_start`]). Only
//! then is the file read, through [`read_audio_for_playing`]: from the
//! media folder, by the name the core builds from a checked hash, a
//! regular file of its kind and size, plain audio over its whole length,
//! read whole on a worker thread, and cut to its audio (an MP3's frames
//! without its tags, ADR 0062). Those bytes, never
//! the path or a URI, go to GTK's media stream
//! (`gtk_media_file_new_for_input_stream`), which GTK 4.24 plays with
//! `GStreamer`'s `GstPlay` (playbin3; GTK hands the source element our
//! stream in its `source-setup`, `gtkgstmediafile.c`).
//! So the file cannot change between the check and the play, and no
//! `file:` or other URI reaches playbin. `GStreamer` finds the demuxer and
//! decoder itself, in Rappel's process, among the plugins `gst_env` allows
//! (ADR 0062).
//!
//! One control plays at a time: starting one stops the one playing
//! ([`stop_all`]). A control stops when it leaves the screen (`unmap`): the
//! next card, an undo, leaving the page or closing the editor all remove
//! or hide it. When rendered, the file is only looked at
//! ([`check_media_file`], its first bytes), so a missing file shows at
//! once as "Audio not available".

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use rappel_core::media::MediaRef;

use super::audio::{State, accessible_label, button_text, time_text};
use super::media_file::{Unreadable, check_media_file, read_audio_for_playing};
use crate::ui::init_once;

/// How long `GStreamer` may take to get a stream ready to play once it has
/// the bytes. A damaged file is an error in milliseconds; past this the
/// play is given up and the control says it could not play.
pub const PREPARE_TIMEOUT: Duration = Duration::from_secs(10);

thread_local! {
    /// The control playing (or loading), if any. GTK runs on one thread.
    static CURRENT: RefCell<glib::WeakRef<CardAudio>> = RefCell::new(glib::WeakRef::new());
}

/// Stops whatever card audio is playing (the review page calls it when it
/// opens the editor; leaving the screen stops a control by itself).
pub fn stop_all() {
    let current = CURRENT.with_borrow(glib::WeakRef::upgrade);
    if let Some(current) = current {
        current.stop();
    }
}

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};
    use std::path::PathBuf;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use rappel_core::media::MediaRef;

    use super::super::audio::State;

    #[derive(Debug)]
    pub struct CardAudio {
        pub button: OnceCell<gtk::Button>,
        pub icon: OnceCell<gtk::Image>,
        pub label: OnceCell<gtk::Label>,
        pub note: OnceCell<gtk::Label>,
        /// The link's words, on one line (`audio::link_text`).
        pub text: RefCell<String>,
        /// The file in the media folder, `None` without a folder.
        pub path: RefCell<Option<PathBuf>>,
        pub media: Cell<Option<MediaRef>>,
        pub state: Cell<State>,
        /// The stream of the last play, kept for a replay or a resume.
        pub stream: RefCell<Option<gtk::MediaFile>>,
        /// Counts the plays and stops, so a read or a timeout that ends
        /// after a newer one is ignored.
        pub generation: Cell<u64>,
        /// How many plays were asked for (tests: R must reach a control
        /// without real sound).
        #[cfg(test)]
        pub plays: Cell<u32>,
    }

    impl Default for CardAudio {
        fn default() -> Self {
            Self {
                button: OnceCell::new(),
                icon: OnceCell::new(),
                label: OnceCell::new(),
                note: OnceCell::new(),
                text: RefCell::default(),
                path: RefCell::default(),
                media: Cell::new(None),
                state: Cell::new(State::Ready),
                stream: RefCell::default(),
                generation: Cell::new(0),
                #[cfg(test)]
                plays: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CardAudio {
        const NAME: &'static str = "RappelCardAudio";
        type Type = super::CardAudio;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("card-audio");
            klass.set_layout_manager_type::<gtk::BoxLayout>();
        }
    }

    impl ObjectImpl for CardAudio {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }

        fn dispose(&self) {
            self.obj().stop();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CardAudio {
        /// Off screen, nothing plays: the next card, an undo, leaving the
        /// page and closing the editor all unmap the control (a widget
        /// removed from a mapped parent is unmapped first,
        /// `gtk_widget_unparent` → `gtk_widget_unrealize`, gtkwidget.c).
        fn unmap(&self) {
            self.obj().stop();
            self.parent_unmap();
        }
    }
}

glib::wrapper! {
    /// One `media:` audio link of a card side: a button that plays and
    /// pauses it, and the time played or why it cannot play.
    pub struct CardAudio(ObjectSubclass<imp::CardAudio>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CardAudio {
    /// The audio `media` of the media folder `folder` (`None`: there is
    /// none, so it is not available), named by the link's words `text`.
    /// Its file is looked at on a worker thread, not read.
    pub fn new(folder: Option<&Path>, media: MediaRef, text: &str) -> Self {
        let audio: Self = glib::Object::new();
        let imp = audio.imp();
        imp.text.replace(text.to_owned());
        imp.media.set(Some(media));
        let Some(folder) = folder else {
            audio.set_state(State::Unavailable);
            return audio;
        };
        // Without GStreamer's playbin, GstPlay would abort the process
        // (`g_error`) when the stream is made: nothing is read or played.
        if !super::playback::available() {
            audio.set_state(State::NoPlayback);
            return audio;
        }
        let path = folder.join(media.file_name());
        imp.path.replace(Some(path.clone()));
        audio.set_state(State::Ready);
        let weak = audio.downgrade();
        glib::spawn_future_local(async move {
            // `gio::spawn_blocking` returns a panic in the closure as its
            // error.
            let checked = gio::spawn_blocking(move || check_media_file(&path, media))
                .await
                .unwrap_or(Err(Unreadable::Refused));
            if let (Err(_), Some(audio)) = (checked, weak.upgrade())
                && audio.imp().state.get() == State::Ready
                && audio.imp().stream.borrow().is_none()
            {
                audio.set_state(State::Unavailable);
            }
        });
        audio
    }

    /// A control for an audio link past the side's share
    /// (`audio::MAX_AUDIO_PER_SIDE`): disabled, its file never read.
    pub fn not_read(text: &str) -> Self {
        let audio: Self = glib::Object::new();
        audio.imp().text.replace(text.to_owned());
        audio.set_state(State::TooMany);
        audio
    }

    /// Whether it can be played (its file is not known to be missing or
    /// refused).
    pub fn is_available(&self) -> bool {
        self.imp().state.get().enabled()
    }

    /// Where it is (tests).
    #[cfg(test)]
    pub fn state(&self) -> State {
        self.imp().state.get()
    }

    /// How many plays were asked for (tests).
    #[cfg(test)]
    pub fn plays(&self) -> u32 {
        self.imp().plays.get()
    }

    /// The button (tests).
    #[cfg(test)]
    pub fn button(&self) -> gtk::Button {
        self.imp()
            .button
            .get()
            .expect("built in constructed")
            .clone()
    }

    /// The children: the button (an icon and the link's words) and the
    /// dimmed note beside it.
    fn build(&self) {
        let imp = self.imp();
        if let Some(layout) = self.layout_manager().and_downcast::<gtk::BoxLayout>() {
            layout.set_spacing(12);
        }
        let icon = gtk::Image::builder()
            .icon_name(State::Ready.icon())
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        // Plain text (the link's words are card content), wrapping, with
        // breaks in long runs like every label of a side.
        let label = gtk::Label::builder()
            .use_markup(false)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        content.append(&icon);
        content.append(&label);
        // The click does not take the focus (`focus-on-click`), so on the
        // review page the card keeps it and its keys keep working; Tab
        // reaches the button, whose own Enter and Space play before the
        // page's keys (which run in the bubble phase) see them, as for an
        // image (ADR 0055).
        let button = gtk::Button::builder()
            .child(&content)
            .focus_on_click(false)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = audio)]
            self,
            move |_| audio.toggle()
        ));
        let note = gtk::Label::builder()
            .visible(false)
            .wrap(true)
            .xalign(0.0)
            .hexpand(true)
            .valign(gtk::Align::Center)
            // The button's description says the same to screen readers;
            // the time ticking is not read.
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        note.add_css_class("dimmed");
        note.add_css_class("caption");
        note.add_css_class("numeric");
        button.set_parent(self);
        note.set_parent(self);
        init_once(&imp.icon, icon);
        init_once(&imp.label, label);
        init_once(&imp.note, note);
        init_once(&imp.button, button);
    }

    /// Shows `state`: the button's icon, words, tooltip, accessible label
    /// and description, and the note.
    fn set_state(&self, state: State) {
        let imp = self.imp();
        imp.state.set(state);
        let text = imp.text.borrow().clone();
        let (Some(button), Some(icon), Some(label), Some(note)) = (
            imp.button.get(),
            imp.icon.get(),
            imp.label.get(),
            imp.note.get(),
        ) else {
            return;
        };
        icon.set_icon_name(Some(state.icon()));
        label.set_label(&super::breaks::breakable(&button_text(&text)));
        button.set_sensitive(state.enabled());
        button.set_tooltip_text(Some(&state.tooltip()));
        button.update_property(&[gtk::accessible::Property::Label(&accessible_label(
            &text, state,
        ))]);
        if let Some(words) = state.note() {
            button.update_property(&[gtk::accessible::Property::Description(&words)]);
            note.set_label(&words);
            note.set_visible(true);
        } else {
            button.reset_property(gtk::AccessibleProperty::Description);
            self.show_time();
        }
    }

    /// The time played and the length beside the button, once a play has
    /// told them.
    fn show_time(&self) {
        let imp = self.imp();
        let Some(note) = imp.note.get() else {
            return;
        };
        if imp.state.get().note().is_some() {
            return;
        }
        let shown = imp.stream.borrow().as_ref().map(|stream| {
            (
                stream.is_prepared(),
                // The timestamp reads 0 once the stream has ended
                // (seen with GTK 4.24 and GStreamer 1.28); a sound played
                // to its end shows its whole length.
                if stream.is_ended() {
                    time_text(stream.duration(), stream.duration())
                } else {
                    time_text(stream.timestamp(), stream.duration())
                },
            )
        });
        match shown {
            Some((true, text)) => {
                note.set_label(&text);
                note.set_visible(true);
            }
            _ => note.set_visible(false),
        }
    }

    /// A click (or Enter, Space on the focused button): pauses while
    /// playing; else resumes a paused play, or plays from the start.
    fn toggle(&self) {
        let imp = self.imp();
        match imp.state.get() {
            State::Playing | State::Loading => self.pause(),
            State::Ready => {
                let resumable = imp
                    .stream
                    .borrow()
                    .as_ref()
                    .filter(|stream| stream.error().is_none() && !stream.is_ended())
                    .cloned();
                match resumable {
                    Some(stream) => {
                        self.make_current();
                        stream.play();
                        self.set_state(State::Playing);
                    }
                    None => self.play_from_start(),
                }
            }
            State::Failed => self.play_from_start(),
            State::Unavailable | State::TooMany | State::NoPlayback => {}
        }
    }

    /// Plays from the start (R on the review page, a first click, a click
    /// after the end or a failure). Stops any other control first. Does
    /// nothing if the file is not available.
    pub fn play_from_start(&self) {
        let imp = self.imp();
        if !imp.state.get().enabled() {
            return;
        }
        #[cfg(test)]
        imp.plays.set(imp.plays.get() + 1);
        self.make_current();
        let kept = imp
            .stream
            .borrow()
            .as_ref()
            .filter(|stream| stream.error().is_none() && stream.is_prepared())
            .cloned();
        if let Some(stream) = kept {
            stream.seek(0);
            stream.play();
            self.set_state(State::Playing);
        } else {
            self.load_and_play();
        }
    }

    /// Pauses (or forgets a play still being read).
    fn pause(&self) {
        let imp = self.imp();
        if imp.state.get() == State::Loading {
            imp.generation.set(imp.generation.get() + 1);
        }
        if let Some(stream) = imp.stream.borrow().as_ref() {
            stream.pause();
        }
        self.set_state(State::Ready);
    }

    /// Stops and lets go of the stream (and its bytes). A control that
    /// could not play keeps saying so.
    pub fn stop(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        if let Some(stream) = imp.stream.take() {
            retire(stream);
        }
        if imp.state.get().playing() {
            self.set_state(State::Ready);
        } else {
            self.show_time();
        }
    }

    /// Makes this the one control playing: the one before stops.
    fn make_current(&self) {
        let before = CURRENT.with_borrow(glib::WeakRef::upgrade);
        if let Some(before) = before.filter(|before| before != self) {
            before.stop();
        }
        CURRENT.with_borrow_mut(|current| current.set(Some(self)));
    }

    /// Reads the file on a worker thread, checked, then plays its bytes.
    fn load_and_play(&self) {
        let imp = self.imp();
        if let Some(stream) = imp.stream.take() {
            retire(stream);
        }
        let (Some(path), Some(media)) = (imp.path.borrow().clone(), imp.media.get()) else {
            self.set_state(State::Unavailable);
            return;
        };
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        self.set_state(State::Loading);
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let read = read_on_worker(path, media).await;
            let Some(audio) = weak.upgrade() else {
                return;
            };
            if audio.imp().generation.get() != generation {
                return;
            }
            match read {
                Ok(bytes) => audio.start(bytes, generation),
                Err(_) => audio.set_state(State::Unavailable),
            }
        });
    }

    /// Plays `bytes` through GTK's media stream, unless the control has
    /// left the screen meanwhile.
    fn start(&self, bytes: Vec<u8>, generation: u64) {
        let imp = self.imp();
        if !super::playback::available() {
            self.set_state(State::NoPlayback);
            return;
        }
        if !self.is_mapped() {
            self.set_state(State::Ready);
            return;
        }
        let input = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
        let stream = gtk::MediaFile::for_input_stream(&input);
        // Tests make no sound, wherever they run.
        #[cfg(test)]
        stream.set_muted(true);
        self.watch(&stream);
        imp.stream.replace(Some(stream.clone()));
        stream.play();
        self.set_state(State::Playing);
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            glib::timeout_future(PREPARE_TIMEOUT).await;
            let Some(audio) = weak.upgrade() else {
                return;
            };
            let stuck = audio.imp().generation.get() == generation
                && audio
                    .imp()
                    .stream
                    .borrow()
                    .as_ref()
                    .is_some_and(|stream| !stream.is_prepared());
            if stuck {
                tracing::debug!("an audio file took too long to start");
                audio.fail();
            }
        });
    }

    /// Follows `stream`: its errors, its end, a video track (refused), and
    /// the time for the note.
    fn watch(&self, stream: &gtk::MediaFile) {
        // Each handler acts only while `stream` is still this control's:
        // a stream let go of (stopped, replaced, or kept by `retire` until
        // it is prepared) must not touch the state of a newer play.
        let current = |audio: &Self, stream: &gtk::MediaFile| {
            audio.imp().stream.borrow().as_ref() == Some(stream)
        };
        stream.connect_error_notify(glib::clone!(
            #[weak(rename_to = audio)]
            self,
            move |stream| {
                if !current(&audio, stream) {
                    return;
                }
                if let Some(err) = stream.error() {
                    tracing::debug!("an audio file could not be played: {err}");
                    audio.fail();
                }
            }
        ));
        stream.connect_prepared_notify(glib::clone!(
            #[weak(rename_to = audio)]
            self,
            move |stream| {
                if !current(&audio, stream) {
                    return;
                }
                // An audio kind by its structure, yet GStreamer found a
                // picture track in it: not what a card's audio is.
                if stream.is_prepared() && stream.has_video() {
                    tracing::debug!("an audio file has a video track and is not played");
                    audio.fail();
                } else {
                    audio.show_time();
                }
            }
        ));
        stream.connect_ended_notify(glib::clone!(
            #[weak(rename_to = audio)]
            self,
            move |stream| {
                if current(&audio, stream)
                    && stream.is_ended()
                    && audio.imp().state.get() == State::Playing
                {
                    audio.set_state(State::Ready);
                }
            }
        ));
        stream.connect_playing_notify(glib::clone!(
            #[weak(rename_to = audio)]
            self,
            move |stream| {
                if current(&audio, stream)
                    && !stream.is_playing()
                    && audio.imp().state.get() == State::Playing
                {
                    audio.set_state(State::Ready);
                }
            }
        ));
        stream.connect_timestamp_notify(glib::clone!(
            #[weak(rename_to = audio)]
            self,
            move |stream| {
                if current(&audio, stream) {
                    audio.show_time();
                }
            }
        ));
        stream.connect_duration_notify(glib::clone!(
            #[weak(rename_to = audio)]
            self,
            move |stream| {
                if current(&audio, stream) {
                    audio.show_time();
                }
            }
        ));
    }

    /// Gives up the play: the stream is let go, and the control says it
    /// could not play (pressing it tries again).
    fn fail(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        if let Some(stream) = imp.stream.take() {
            retire(stream);
        }
        self.set_state(State::Failed);
    }
}

thread_local! {
    /// Streams let go of before `GStreamer` had them ready, kept until it
    /// has (or has failed).
    static RETIRING: RefCell<Vec<gtk::MediaFile>> = const { RefCell::new(Vec::new()) };
}

/// Pauses `stream` and lets go of it, never while `GStreamer` is still
/// getting it ready: closing a `GtkGstMediaFile` before it is prepared
/// (when another stream was made before it in the process) makes GTK 4.24
/// unref an object that is not there (a `g_object_unref` critical, seen
/// in this module's tests). Such a stream is kept, paused, until it is
/// prepared or fails, or [`PREPARE_TIMEOUT`] passes. The drop always comes
/// from an idle callback, after the signal that may have brought us here
/// (the stream's own notify handlers call [`CardAudio::fail`]) has
/// returned, so `GstPlay` is never freed during its own emission.
fn retire(stream: gtk::MediaFile) {
    stream.pause();
    if stream.is_prepared() || stream.error().is_some() {
        glib::idle_add_local_once(move || drop(stream));
        return;
    }
    let release = |stream: &gtk::MediaFile| {
        let stream = stream.clone();
        glib::idle_add_local_once(move || {
            RETIRING.with_borrow_mut(|retiring| retiring.retain(|kept| *kept != stream));
        });
    };
    stream.connect_prepared_notify(release);
    stream.connect_error_notify(release);
    let late = stream.downgrade();
    glib::timeout_add_local_once(PREPARE_TIMEOUT, move || {
        if let Some(stream) = late.upgrade() {
            release(&stream);
        }
    });
    RETIRING.with_borrow_mut(|retiring| retiring.push(stream));
}

/// [`read_audio_for_playing`] on a worker thread.
async fn read_on_worker(path: PathBuf, media: MediaRef) -> Result<Vec<u8>, Unreadable> {
    // `gio::spawn_blocking` returns a panic in the closure as its error.
    gio::spawn_blocking(move || read_audio_for_playing(&path, media))
        .await
        .unwrap_or(Err(Unreadable::Refused))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::render::{Images, audio_controls, render};
    use crate::state::{Accept, AppState};

    /// Puts `bytes` into the media folder of `data_dir` under the name a
    /// reference of `ext` with a hash made from `seed` has, as a damaged or
    /// hostile file would be (the store itself names a file by its hash).
    fn plant(data_dir: &Path, seed: u8, ext: &str, bytes: &[u8]) -> MediaRef {
        let media = MediaRef::parse(&format!("media:{}.{ext}", format!("{seed:02x}").repeat(32)))
            .expect("a valid reference");
        let folder = data_dir.join("media");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(media.file_name()), bytes).unwrap();
        media
    }

    /// `widget` in a presented window, once it is mapped: a control plays
    /// only on screen.
    fn on_screen(widget: &impl IsA<gtk::Widget>) -> gtk::Window {
        let window = gtk::Window::builder()
            .default_width(600)
            .default_height(400)
            .child(widget)
            .build();
        window.present();
        assert!(
            crate::testing::settle_until(|| widget.is_mapped(), Duration::from_secs(5)),
            "the window did not show"
        );
        window
    }

    /// An `ID3v2.4` tag with no frames, then `rest`: an MP3 by its first
    /// bytes.
    fn playlist(rest: &[u8]) -> Vec<u8> {
        let mut out = b"ID3\x04\0\0\0\0\0\0".to_vec();
        out.extend_from_slice(rest);
        out
    }

    /// Waits until `audio` is no longer loading, or `limit` passes.
    fn settle_loaded(audio: &CardAudio, limit: Duration) -> State {
        crate::testing::settle_until(|| audio.state() != State::Loading, limit);
        audio.state()
    }

    #[test]
    fn a_side_with_an_audio_link_renders_a_named_control() {
        crate::testing::with_widgets(|| {
            let (tmp, state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let media = AppState::add_media_bytes_on_worker(
                &data,
                &crate::testing::wav_bytes(800),
                Accept::Audio,
            )
            .unwrap();
            let md = format!(
                "Listen: [the *tone*]({}) then [](media:{}.mp3)",
                media.reference(),
                "ee".repeat(32)
            );
            // Nothing is played here, so playback may be taken as there
            // whatever is installed.
            let forced = super::super::playback::force(true);
            let images = Images::new(state.media_folder(), 720);
            let side = render(&rappel_core::content::parse(&md), &images);
            drop(forced);
            let controls = audio_controls(&side);
            assert_eq!(controls.len(), 2);
            let (tone, missing) = (&controls[0], &controls[1]);
            // Looked for on a worker thread; nothing is read or played.
            assert!(crate::testing::settle_until(
                || missing.state() == State::Unavailable,
                Duration::from_secs(30)
            ));
            assert_eq!(tone.state(), State::Ready);
            assert!(tone.is_available());
            assert_eq!(tone.plays(), 0);
            assert!(tone.imp().stream.borrow().is_none(), "never autoplays");
            let button = tone.button();
            assert_eq!(button.accessible_role(), gtk::AccessibleRole::Button);
            assert!(!button.gets_focus_on_click());
            assert!(button.is_sensitive());
            assert_eq!(button.tooltip_text().as_deref(), Some("Play"));
            assert!(gtk::test_accessible_has_property(
                &button,
                gtk::AccessibleProperty::Label
            ));
            let label = tone.imp().label.get().unwrap();
            assert_eq!(label.text(), "the tone");
            assert_eq!(
                tone.imp().icon.get().unwrap().icon_name().as_deref(),
                Some("media-playback-start-symbolic")
            );
            // The file that is not there: disabled, and says so.
            let button = missing.button();
            assert!(!button.is_sensitive());
            assert_eq!(missing.imp().label.get().unwrap().text(), "Audio");
            let note = missing.imp().note.get().unwrap();
            assert!(note.is_visible());
            assert_eq!(note.text(), "Audio not available");
            assert!(gtk::test_accessible_has_property(
                &button,
                gtk::AccessibleProperty::Description
            ));
            // R (or a click) on it does nothing.
            missing.play_from_start();
            assert_eq!(missing.plays(), 0);
        });
    }

    /// Without `GStreamer`'s base plugins (forced here), no control is made
    /// playable, no stream is made (`GstPlay` would abort the process), and
    /// R or a click does nothing.
    #[test]
    fn without_gstreamer_s_playbin_nothing_is_played() {
        crate::testing::with_widgets(|| {
            let (tmp, state) = crate::testing::temporary_state();
            let media = AppState::add_media_bytes_on_worker(
                &tmp.path().join("data"),
                &crate::testing::wav_bytes(800),
                Accept::Audio,
            )
            .unwrap();
            let _forced = super::super::playback::force(false);
            let folder = state.media_folder().unwrap();
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let audio = CardAudio::new(Some(&folder), media, "tone");
            column.append(&audio);
            let window = on_screen(&column);
            assert_eq!(audio.state(), State::NoPlayback);
            assert!(!audio.is_available());
            assert!(!audio.button().is_sensitive());
            let note = audio.imp().note.get().unwrap();
            assert_eq!(
                note.text(),
                "Audio playback is not available on this system"
            );
            audio.play_from_start();
            audio.button().emit_clicked();
            crate::testing::settle_until(|| false, Duration::from_millis(200));
            assert_eq!(audio.plays(), 0);
            assert!(audio.imp().stream.borrow().is_none());
            assert_eq!(audio.state(), State::NoPlayback);
            window.close();
        });
    }

    #[test]
    fn without_a_folder_or_past_the_share_nothing_is_read() {
        crate::testing::with_widgets(|| {
            let media = MediaRef::parse(&format!("media:{}.ogg", "cd".repeat(32))).unwrap();
            let none = CardAudio::new(None, media, "x");
            assert_eq!(none.state(), State::Unavailable);
            let (_tmp, state) = crate::testing::temporary_state();
            let many = (0..super::super::audio::MAX_AUDIO_PER_SIDE + 3)
                .map(|n| format!("[{n}](media:{n:064x}.wav)"))
                .collect::<Vec<_>>()
                .join("\n\n");
            let side = render(
                &rappel_core::content::parse(&many),
                &Images::new(state.media_folder(), 720),
            );
            let controls = audio_controls(&side);
            assert_eq!(controls.len(), super::super::audio::MAX_AUDIO_PER_SIDE + 3);
            let too_many = controls
                .iter()
                .filter(|c| c.state() == State::TooMany)
                .count();
            assert_eq!(too_many, 3);
            // Other targets are links, never controls: nothing to read.
            let side = render(
                &rappel_core::content::parse(
                    "[a](https://e.org/a.mp3) [b](file:///etc/hosts) [c](media:x.wav)",
                ),
                &Images::new(state.media_folder(), 720),
            );
            assert_eq!(audio_controls(&side).len(), 0);
        });
    }

    /// A good file plays (or, where `GStreamer` or a sound device is
    /// missing, fails) and ends without anyone waiting; starting another
    /// control stops it; leaving the screen stops it.
    #[test]
    fn one_control_plays_at_a_time_and_stops_off_screen() {
        crate::testing::with_widgets(|| {
            let (tmp, state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let media = AppState::add_media_bytes_on_worker(
                &data,
                &crate::testing::wav_bytes(1_600),
                Accept::Audio,
            )
            .unwrap();
            let folder = state.media_folder().unwrap();
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let first = CardAudio::new(Some(&folder), media, "first");
            let second = CardAudio::new(Some(&folder), media, "second");
            column.append(&first);
            column.append(&second);
            let window = on_screen(&column);
            if !super::super::playback::available() {
                // No GStreamer playbin here: nothing can play (see
                // `without_gstreamer_s_playbin_nothing_is_played`).
                assert_eq!(first.state(), State::NoPlayback);
                window.close();
                return;
            }
            first.play_from_start();
            assert_eq!(first.state(), State::Loading);
            let played = settle_loaded(&first, Duration::from_secs(20));
            assert!(
                matches!(played, State::Playing | State::Ready | State::Failed),
                "{played:?}"
            );
            // The other one starts: the first stops and lets go of its
            // stream.
            second.play_from_start();
            assert_eq!(first.state(), State::Ready);
            assert!(first.imp().stream.borrow().is_none());
            assert!(second.state().playing());
            // A 0.2 s file ends by itself (or fails) without a hang.
            settle_loaded(&second, Duration::from_secs(20));
            let ended = crate::testing::settle_until(
                || matches!(second.state(), State::Ready | State::Failed),
                Duration::from_secs(20),
            );
            assert!(ended, "{:?}", second.state());
            // Played again, then taken off the screen: it stops.
            second.play_from_start();
            assert!(second.state().playing());
            column.remove(&second);
            assert_eq!(second.state(), State::Ready);
            assert!(second.imp().stream.borrow().is_none());
            // `stop_all` stops whatever plays.
            first.play_from_start();
            stop_all();
            assert_eq!(first.state(), State::Ready);
            window.close();
        });
    }

    /// Set for the child of `an_mp3_with_an_id3_tag_plays_without_aborting`.
    const ALONE: &str = "RAPPEL_TEST_AUDIO_ALONE";

    /// An MP3 with an `ID3v2` tag, as most are, plays: `GStreamer` 1.28
    /// aborted the whole process on the tag in GTK's stream
    /// (`gstdecodebin3.c`, "assertion failed: (collection)"), so the
    /// player is given the frames only (ADR 0062). In a process of its
    /// own, since the failure is an abort.
    #[test]
    fn an_mp3_with_an_id3_tag_plays_without_aborting() {
        if std::env::var_os(ALONE).is_some() || !super::super::playback::available() {
            return;
        }
        let registry = tempfile::tempdir().unwrap();
        let out = crate::testing::run_alone(
            "render::audio_player::tests::an_id3_mp3_in_a_process_of_its_own",
            |command| {
                command
                    .env(ALONE, "1")
                    .env("GST_REGISTRY_1_0", registry.path().join("registry.bin"));
            },
        );
        assert!(out.contains("audio-played"), "{out}");
    }

    /// The child of `an_mp3_with_an_id3_tag_plays_without_aborting`. Does
    /// nothing in a normal run.
    #[test]
    fn an_id3_mp3_in_a_process_of_its_own() {
        if std::env::var_os(ALONE).is_none() {
            return;
        }
        crate::testing::with_widgets(|| {
            let (tmp, state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let bytes = include_bytes!("../../../core/src/media/fixtures/tone-id3.mp3");
            let media = AppState::add_media_bytes_on_worker(&data, bytes, Accept::Audio).unwrap();
            let audio = CardAudio::new(state.media_folder().as_deref(), media, "tagged");
            let window = on_screen(&audio);
            audio.play_from_start();
            let prepared = || {
                audio
                    .imp()
                    .stream
                    .borrow()
                    .as_ref()
                    .is_some_and(gtk::prelude::MediaStreamExt::is_prepared)
            };
            let done = crate::testing::settle_until(
                || prepared() || matches!(audio.state(), State::Failed | State::Unavailable),
                Duration::from_secs(20),
            );
            assert!(done && prepared(), "{:?}", audio.state());
            println!("audio-played");
            audio.stop();
            window.close();
        });
    }

    /// Hostile files under audio names: each is refused before `GStreamer`
    /// sees it, or `GStreamer` fails or plays it, within a time bound, and
    /// the window's thread never waits on it.
    #[test]
    fn hostile_audio_fails_in_time() {
        crate::testing::with_widgets(|| {
            let (tmp, _state) = crate::testing::temporary_state();
            let data = tmp.path().join("data");
            let folder = data.join("media");
            let mut truncated = crate::testing::wav_bytes(8_000);
            truncated.truncate(60);
            // A first Ogg page announcing Opus, then garbage pages.
            let mut ogg = b"OggS\x00\x02".to_vec();
            ogg.extend_from_slice(&[0; 20]);
            ogg.extend_from_slice(b"\x01\x13OpusHead\x01\x01\x38\x01\x80\xbb\x00\x00\x00\x00\x00");
            for i in 0..2_000_u32 {
                ogg.extend_from_slice(b"OggS\x00\x00");
                ogg.extend_from_slice(&i.wrapping_mul(2_654_435_761).to_le_bytes().repeat(25));
            }
            // 20 MB of MP3 frame headers with noise between them.
            let mut noise = Vec::with_capacity(20_000_000);
            let mut x = 0x2545_f491_u32;
            while noise.len() + 417 <= 20_000_000 {
                noise.extend_from_slice(b"\xff\xfb\x90\x64");
                for _ in 0..413 {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    noise.push(x.to_le_bytes()[0]);
                }
            }
            let mut m4a = b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00M4A isom".to_vec();
            m4a.extend_from_slice(&[0xff; 4_096]);
            let cases = [
                ("wav", truncated, "a truncated WAV"),
                ("ogg", ogg, "an Ogg with garbage pages"),
                ("mp3", noise, "20 MB of MP3 noise"),
                ("m4a", m4a, "an M4A whose boxes lie"),
                (
                    "wav",
                    b"<html><script></script></html>".to_vec(),
                    "HTML as WAV",
                ),
                (
                    "mp3",
                    playlist(b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nfile:///tmp/v.m3u8\n"),
                    "an HLS playlist behind an ID3 tag",
                ),
                (
                    "mp3",
                    playlist(
                        b"<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>",
                    ),
                    "a DASH manifest behind an ID3 tag",
                ),
                (
                    "mp3",
                    playlist(b"v=0\r\nc=IN IP4 192.0.2.1\r\nm=audio 5004 RTP/AVP 0\r\n"),
                    "an SDP session behind an ID3 tag",
                ),
            ];
            // Refused before GStreamer sees a byte: only the noise (whole
            // MP3 frames) reaches it.
            let refused_first = |what: &str| what != "20 MB of MP3 noise";
            if !super::super::playback::available() {
                // No GStreamer playbin here: nothing reaches it at all.
                eprintln!("GStreamer's base plugins are missing: hostile audio not played");
                return;
            }
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let window = on_screen(&column);
            for (seed, (ext, bytes, what)) in (1_u8..).zip(cases) {
                let media = plant(&data, seed, ext, &bytes);
                let audio = CardAudio::new(Some(&folder), media, what);
                column.append(&audio);
                assert!(crate::testing::settle_until(
                    || audio.is_mapped(),
                    Duration::from_secs(5)
                ));
                let started = Instant::now();
                audio.play_from_start();
                // Read, then started or refused: never stuck loading.
                let state = settle_loaded(&audio, Duration::from_secs(30));
                assert_ne!(state, State::Loading, "{what}");
                if refused_first(what) {
                    assert_eq!(state, State::Unavailable, "{what}");
                    assert!(audio.imp().stream.borrow().is_none(), "{what}");
                }
                // A stream GStreamer cannot use fails within the timeout;
                // noise may play (it is muted here) until stopped.
                let settled = crate::testing::settle_until(
                    || {
                        audio.state() != State::Playing
                            || audio
                                .imp()
                                .stream
                                .borrow()
                                .as_ref()
                                .is_some_and(gtk::prelude::MediaStreamExt::is_prepared)
                    },
                    PREPARE_TIMEOUT + Duration::from_secs(10),
                );
                crate::testing::print_timing(what, started.elapsed());
                assert!(settled, "{what}: {:?}", audio.state());
                audio.stop();
                column.remove(&audio);
            }
            window.close();
        });
    }
}
