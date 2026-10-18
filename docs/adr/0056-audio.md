# 56. Audio: played by GTK's GStreamer media stream, never on its own, added in the editor

- Status: Accepted
- Date: 2026-10-18
- Amends: ADR 0054 (the `[audio: …]` placeholder is gone; `Store::add_audio`, `MediaProblem::NotAudio`), ADR 0055 (the editor's Insert Image… path, paste and drop take audio files too; one checked read of a media file for images and audio), ADR 0025 and ADR 0050 (rows 46 to 48 of the untrusted-input table), ADR 0039 (R on the review page, in the Keyboard Shortcuts dialog)
- Refers to: SPEC §4.9, §4.3, principle 5; `docs/briefs/phase-2.md` (slice 4)

## Context

Slice 2 (ADR 0054) gave the media store and `Inline::Audio`, a link `[words](media:<sha256>.<ext>)` to an Ogg (Opus or Vorbis), MP3, M4A or WAV file of at most 20 MB; the app showed `[audio: …]`. This slice plays it: a play control on the card, never autoplay, R on the review page plays the card's audio again, and Insert Audio… and drag and drop in the editor. No recording. SPEC §4.9 says playback is by GStreamer, opens only a local file built from the hash, never a URI from card text, with an allow-list of formats and decoders, and asks the ADR to justify the GStreamer binding as a new dependency.

Audio demuxers and decoders are an attack surface like image decoders, and the files come from imports and drops.

## Decision

### 1. Playback: `GtkMediaFile`, no new dependency

GTK 4.24 has GStreamer playback built in: `libgtk-4.so.1` links `libgstplay-1.0`, `libgstreamer-1.0` and `libgstaudio-1.0`, and its `GtkMediaFile` implementation for GStreamer (`GtkGstMediaFile`) plays a stream with `GstPlay` (playbin3). **No GStreamer Rust binding is added**: `gtk::MediaFile` and the `MediaStream` interface of gtk4-rs (already a dependency) are enough, so SPEC §4.9's "justify the binding" is answered by not needing one.

**How a file reaches GStreamer** (`render/media_file.rs`, `render/audio_player.rs`):

1. Only an `Inline::Audio` is ever played: a checked hash and an audio kind; the path is the media folder joined with the name the core builds (`MediaRef::file_name`). A link to anything else (`https:`, `file:`, a path, a bad hash) is an ordinary link, never a control (a unit test lists them).
2. **Nothing is read until the user asks.** When a side is rendered, the file is only looked at, on a worker thread (`check_media_file`: opened with `O_NOFOLLOW | O_NONBLOCK`, a regular file on the handle, not empty, within 20 MB, its first bytes of the reference's kind), so a missing or wrong file shows at once as "Audio not available". No byte goes to GStreamer.
3. On play (a click, Enter or Space on the focused button, or R), `read_media_file` reads the whole file on a worker thread with the same checks on the open handle, read capped at the limit plus one byte, and its first bytes of its kind again. The function is shared with images (`images::read_image_file` is it plus "an image kind"), the one way the renderer reads a media file.
4. Those bytes go to `gtk::MediaFile::for_input_stream` over a `GMemoryInputStream`. GTK hands GStreamer the stream through a source element it sets up itself (`source-setup`; GTK 4.24's library names `giostreamsrc`, and GStreamer's error messages show the stream entering through `urisourcebin`), not a file URI: **no `file:` or other URI of ours ever reaches playbin**, and the file cannot change between the check and the play.
5. A stream GStreamer finds a picture track in (`has_video` once prepared) is stopped and the control says "Could not be played": an audio kind by its first bytes that GStreamer takes for a video is not card audio.
6. A stream not prepared within `PREPARE_TIMEOUT` (10 s) is given up the same way. An error (`GtkMediaStream:error`) is logged at debug level, never with card text, and the control shows its failed state; pressing it again tries again.

**What cannot be restricted, honestly.** GStreamer chooses the demuxer and decoder itself, by typefinding the bytes and by plugin rank, and runs them **in Rappel's process**, in GstPlay's streaming threads: on a system with `gstreamer1.0-libav`, that can be FFmpeg's decoders. Through `GtkMediaFile` there is no way to name the elements allowed. Two ways were looked at and **not taken**:

- **A GStreamer binding** (`gstreamer-rs`, MIT/Apache-2.0, licences already allowed) would let Rappel build its own pipeline (`giostreamsrc ! typefind ! <allowed demuxer> ! <allowed decoder> ! autoaudiosink`) or veto elements in playbin's `autoplug-select`. It needs the GStreamer development packages (`libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`), which are not installed in this environment and need administrator rights: not feasible here. It is the way to a real allow-list later (a phase 2 hardening item).
- **Environment variables read by `gst_init`**: `GST_PLUGIN_FEATURE_RANK` takes exact feature names only (checked: `avdec_*:none` is ignored, `avdec_mp3:none` works), so it cannot shut out the hundreds of libav features; `GST_PLUGIN_SYSTEM_PATH` pointing at a folder of allowed plugins would need the system's plugin folder (distribution- and Flatpak-specific), and setting the environment of a running process is `unsafe` in Rust 2024 (`unsafe_code` is forbidden). Fragile and partial; not done.

**What limits the risk**: only files of the four audio kinds by their first bytes, at most 20 MB, from the media folder by a checked name, opened without following links and checked on the handle; nothing plays, and no byte reaches GStreamer, until the user presses play or R on that card; one stream at a time; no network (the source is a memory stream; GStreamer is never given a URI); a picture track refused; every failure a state of the control, never a crash or a hang of the window. A demuxer or decoder exploit in a file the user then chooses to play runs as the user, inside Rappel. This is recorded in row 46 below.

**What it needs at run time**: GStreamer's `base` plugins (playbin3, Ogg, Vorbis, Opus, typefinding, audio conversion), `good` plugins (WAV, MP4/M4A, MP3 through mpg123, an audio sink) and an AAC decoder for M4A (`fdkaac`, `faad` or `libav`). GNOME systems have them; the README lists them. Without them, a control says "Could not be played"; nothing else changes. The tests do not need them (below).

**Measured**: a truncated WAV, an Ogg whose first page announces Opus followed by garbage pages, 20 MB of MP3 frame headers with noise between them, an M4A whose boxes lie, and HTML under a `.wav` name each end in about 5 to 80 ms here (an error, a refusal before GStreamer, or for the noise a stream that plays, muted in the test, until stopped); the window's thread does not wait on any of them. **Main-thread time**: making the stream and starting it runs on the main thread (GTK objects live there): about 40 ms the first time in a process (GStreamer's init), and **about 1.7 s once per user and GStreamer update**, when GStreamer builds its plugin registry cache (`~/.cache/gstreamer-1.0`; seen with a fresh home folder, as the screenshot harness and CI have). An accepted cost: it happens on the first play ever, by a user's action.

**A GTK quirk worked around**: closing or dropping a `GtkGstMediaFile` that is not prepared yet, when another stream was made earlier in the process, makes GTK 4.24 call `g_object_unref` on something that is not an object (a GLib critical, found by the tests under `G_DEBUG=fatal-criticals`). A stream let go of before it is prepared is therefore kept, paused, until it is prepared or fails (or 10 s pass), then dropped from an idle callback (`retire`).

### 2. The control

- **Where.** A `media:` audio link at the top level of a paragraph is a control of its own between the paragraph's runs of text (`images::pieces`, now images and audio), as an image is. Inside emphasis or a link, in a heading or a table cell, it is its words in italics, or "Audio" for a link without words; nothing plays from there (such cards are rare, and a button inside a line of text would need a layout of its own).
- **Looks** (`CardAudio`): a normal button, at the start of the line, with a `media-playback-start-symbolic` icon and the link's words (plain text, wrapping, with the long-run breaks of ADR 0025), "Audio" without words; `media-playback-pause-symbolic` while it plays. Beside it a dimmed caption: "0:03 / 0:12" once GStreamer knows the length (the whole length after the end), "Audio not available" (disabled button, `audio-x-generic-symbolic`), "Audio not available: too many on this side", or "Could not be played" (`dialog-warning-symbolic`, still pressable). A raised button, not flat: it must look pressable in the text. Libadwaita style classes only (`dimmed`, `caption`, `numeric`).
- **Names.** Tooltip "Play" / "Pause" / "Audio not available" / "Audio could not be played". Accessible label "Play audio: <words>" ("Play audio" without words), "Pause audio: …" while playing; the caption's words are the button's accessible description; the caption itself is a Presentation (the ticking time is not read). The card area's own label (`plain_text`) already holds the words. The card lists (`plain_summary`) show the words, or nothing.
- **Clicking**: plays from the start, pauses while playing, resumes a paused play; after the end, from the start again.
- **One at a time**: starting a control stops the one playing and lets go of its stream (and its bytes, up to 20 MB): only one file is in memory.
- **Stops** when the control leaves the screen (`unmap`: the next card, an undo, a suspend or delete, leaving the page, the summary, closing the editor or a preview refresh), and when the review page opens the editor (`render::stop_audio`). It keeps playing when the answer is revealed, and while Card Information is open.
- **Budget**: at most 20 controls per side (`MAX_AUDIO_PER_SIDE`); later ones are disabled and never read. Each costs six widgets in the side's 2,000 (`style::widget_cost`).

### 3. The review page: R

- **R** (and Shift+R) plays the card's audio from its start (`Act::Replay`, never delayed: it grades nothing). Which (`replay_target`, pure): once the answer is shown and the back has audio controls, the first one on the back that can play; otherwise the first one on the front that can play. A side whose controls all cannot play gives nothing: R never plays something other than what is under the eyes. R was free (E, I, S, Delete, 1, 2, Space, Enter, Ctrl+Z are taken).
- **Focus**, as for images (ADR 0055): the button does not take the focus on click (`focus-on-click: false`), so Space and Enter stay the page's; Tab reaches it, and then its own Space and Enter press it (the page's keys run in the bubble phase); 1, 2, E, I, S, R, Delete and Ctrl+Z keep working with it focused.
- The Keyboard Shortcuts dialog lists **Play Audio** (R, "From the start"); its test checks the key table.

### 4. The editor

- **Insert Audio…** (`audio-x-generic-symbolic`, accessible label "Insert Audio in Front" / "in Back") beside Insert Image… in each side's header. A `GtkFileDialog` filtered to the audio MIME types (with the names shared-mime-info gives them: `audio/x-vorbis+ogg`, `audio/x-opus+ogg`, `audio/x-m4a`, `audio/vnd.wave`, …) and the endings `ogg oga opus mp3 m4a wav`; the file's first bytes decide (`Store::add_audio`, new in core: an image is `MediaProblem::NotAudio` before anything is written).
- **Paste and drop** go through slice 3's one path (`dialogs/card_editor/insert.rs`, renamed from `images.rs`): files are added with `Store::add_media`, which takes an image or audio as their first bytes say, so a dropped WAV called `x.png` goes in as audio. Nothing else changes: the drop target's formats are the same (a file list and the four image types; no program offers sound on the clipboard in our kinds), text drops and link pastes behave as before, only local files are read (`AppState::add_media_on_worker`, the `local_path` and regular-file checks of ADR 0055 row 45, for audio too), every read of an offer within 10 s.
- **What is inserted** (`media::insertion`): `[](media:<sha256>.<ext>)` for audio, `![](…)` for an image, each as its kind when several are dropped, one user action, with the cursor between `[` and `]` for the words, consistent with images.
- **Refusals** in words, per what was asked: "This file is not an Ogg, MP3, M4A or WAV audio file", "This audio file is larger than 20 MB", "This file is not an image or audio file of a supported kind" (a drop), and those of ADR 0055.
- The preview plays like the review page; a refresh while typing stops it.

### Untrusted input (continues ADR 0055)

| # | Where it comes in | Risk | Status | Test |
|---|---|---|---|---|
| 46 | **Playing a media audio file** (review page, preview). | A demuxer or decoder exploit; a file that hangs or floods GStreamer; a URI or path reaching playbin (network, other files); a file swapped under its name; a video or another format smuggled behind an audio signature; autoplay of a hostile file. | **New.** Played only on the user's press or R, one at a time; only the media folder's regular files by a checked name (`O_NOFOLLOW | O_NONBLOCK`, checked on the handle), at most 20 MB, of their kind by their first bytes, read whole and given to GTK as a memory stream (never a URI); a picture track refused; not prepared in 10 s or an error: given up, shown, logged at debug. **Residual, accepted**: GStreamer picks its demuxer and decoder by its own typefinding and runs them in Rappel's process (FFmpeg's where `gstreamer1.0-libav` is installed); no allow-list of elements is possible through `GtkMediaFile`, and a GStreamer binding to build one is not feasible here (see Decision §1). | `render::media_file::tests::an_audio_file_is_read_only_when_it_is_what_it_names`, `render::audio_player::tests::hostile_audio_fails_in_time`, `a_side_with_an_audio_link_renders_a_named_control`, `without_a_folder_or_past_the_share_nothing_is_read`, `one_control_plays_at_a_time_and_stops_off_screen` |
| 47 | **Audio links in card text.** | A URL or path played or fetched. | **New.** Only `media:` + a checked hash + an audio extension is a control; every other link stays text with its URL as a tooltip, never opened. | `render::images::tests::audio_links_are_pulled_out_of_their_paragraph`, `render::audio_player::tests::without_a_folder_or_past_the_share_nothing_is_read` |
| 48 | **Audio files chosen, pasted or dropped in the editor.** | An image, SVG or HTML stored as audio; a huge file; a URL read through GVfs; a pipe that blocks. | **New.** `Store::add_audio` (Insert Audio…) or `add_media` (paste, drop) with row 39's checks; only local files (`local_path`), opened with `O_NONBLOCK`, regular files only; refusals in words. | `media::tests::add_audio_takes_only_audio`, `dialogs::card_editor::insert::tests::an_added_audio_file_is_a_link_with_the_cursor_in_its_words`, `dropped_files_go_in_as_their_kinds`, `a_url_is_refused_unread_and_a_pipe_is_not_waited_on`, `dialogs::card_editor::media::tests::*` |

## Consequences

- No new dependency, no migration, no change to the export format.
- Playback quality and codecs are the system's GStreamer: on a system without an AAC decoder, M4A files "could not be played".
- The first play ever (per user and GStreamer update) holds the window for about 1 to 2 s while GStreamer builds its registry.
- The tests mute every stream they make (`set_muted` in test builds) and accept "could not be played" where GStreamer, its plugins or a sound device are missing, so CI needs no new package; that a control reaches GStreamer is checked through a play counter (`CardAudio::plays`, test builds only), not by sound.
- A real allow-list of GStreamer elements needs `gstreamer-rs` and its development packages: left for the hardening slice of phase 2 (§4.10), to be weighed against the build dependency.
