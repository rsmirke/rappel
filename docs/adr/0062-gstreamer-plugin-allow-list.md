# 62. Audio: an allow-list of GStreamer's plugins, a registry of Rappel's own, and an MP3's frames only

- Status: Accepted
- Date: 2026-10-19
- Amends: ADR 0056 (§1a layer 2 and "What is guaranteed": decoders are allow-listed by plugin; the player is given an MP3 without its tags; the first play no longer waits for `GStreamer`'s whole registry)
- Answers: `docs/briefs/QUESTIONS.md`, Phase 2, question 2 (done without `gstreamer-rs`)
- Refers to: SPEC §4.9, principle 5; `docs/briefs/phase-2.md` (slice 10)

## Context

SPEC §4.9 asks for an allow-list of formats **and decoders**. ADR 0056 met the first (the core's structure check) but not the second: `playbin3` picks its demuxer and decoder itself and runs them in Rappel's process, `FFmpeg`'s where `gstreamer1.0-libav` is installed. A real veto seemed to need `gstreamer-rs` and the `GStreamer` development packages (QUESTIONS.md, Phase 2, 2). `GStreamer` has a variable for this, though: `GST_PLUGIN_LOADING_WHITELIST`, in `libgstreamer-1.0.so.0` 1.28 (its strings: "plugins whitelist entry: %s", "Whitelist specified and plugin not in whitelist, not loading: name=%s, package=%s, file=%s").

## Decision

### 1. Only allow-listed plugins load

`gst_env.rs` sets, with the feature ranks of ADR 0056, `GST_PLUGIN_LOADING_WHITELIST` to one entry of 21 plugin names. The syntax (`gst/gstplugin.c`, `gst_plugin_desc_matches_whitelist_entry`): entries separated by `:`, each `names[@path]`; a single name is compared with a plugin's name **or its source package's**, several names separated by commas only with plugins' names. So Rappel writes one entry of several names (a single `gst-plugins-good` would allow all of it) and no path (plugin folders differ between distributions and Flatpak runtimes; names do not). A plugin not listed is never loaded: none of its elements can be made, by `playbin3` or anyone, and the registry keeps it as blacklisted.

| Need | Plugins |
|---|---|
| Core and playback | `coreelements`, `typefindfunctions`, `playback` (`playbin3`), `gio` (`giostreamsrc`, GTK's source for our memory stream), `audioconvert`, `audioresample`, `volume` |
| Output | `autodetect`, `pulseaudio`, `pipewire`, `alsa` |
| Ogg Opus and Vorbis | `ogg`, `opus`, `opusparse`, `vorbis` |
| WAV, MP3 | `wavparse`; `audioparsers` (`mpegaudioparse`, `aacparse`), `mpg123` |
| M4A | `isomp4` (`qtdemux`), and AAC: `fdkaac` or `faad` |

Found by trying (`gst-launch-1.0 playbin3` and the test below): nothing else is needed; `app` and `pbtypes`, which the brief listed, are not.

**`libav` is not allowed.** It is one plugin holding hundreds of `FFmpeg` demuxers and decoders. The allow-list works per plugin, so allowing it allows all of them; demoting all but the AAC decoder would need every feature's exact name (`GST_PLUGIN_FEATURE_RANK` takes exact names), which depend on the `FFmpeg` it was built with, and a feature added by an update would be plugged. AAC has two dedicated decoders: `faad` (in Ubuntu's `gstreamer1.0-plugins-bad`) and `fdkaac` (`gstreamer1.0-fdkaac`). Which of them a Flatpak runtime ships was not checked here; the Flatpak manifest (phase 3) must make sure one is there. **Cost, accepted:** an M4A of Apple Lossless (ALAC: only `FFmpeg` decodes it in `GStreamer`), or of AAC on a system whose only AAC decoder is `FFmpeg`'s, shows "Could not be played". Nothing crashes: the stream fails, or is given up after `PREPARE_TIMEOUT`.

`playback.rs`'s check (ADR 0056: `libgstplayback.so` must be there, else `GstPlay` aborts the process) is unchanged and enough: `playback` is on the list, and a missing listed plugin only makes a format fail ("Could not be played"), which the tests check for formats outside the list.

### 2. A registry of Rappel's own

With an allow-list, `GStreamer` writes the plugins it did not load into its registry cache as blacklisted. The default cache (`~/.cache/gstreamer-1.0/registry.<arch>.bin`) is shared by every program of the user, and alternating Rappel with another would rescan every plugin each time (3.1 s here, 274 plugins). So Rappel also sets `GST_REGISTRY_1_0` to `<cache>/rappel/gstreamer-1.0/registry.<arch>.bin` (`XDG_CACHE_HOME` if it is an absolute path, else `$HOME/.cache`; a Flatpak's own cache folder), unless the user set `GST_REGISTRY_1_0` or `GST_REGISTRY`. That registry holds only the allowed plugins: built in 0.08 s instead of 3.1 s (`gst-inspect-1.0`, a fresh file).

**The first play no longer holds the window for seconds** (ADR 0056's open item, slice 10 item C5). Making and starting the first stream, which starts `GStreamer` and builds or reads its registry on the main thread, measured in a child process of the test binary (debug build, `RAPPEL_TIMINGS=1`): with a fresh registry **0.13 to 0.34 s** with the allow-list against **1.9 s** without; with the registry cached, 37 ms against 105 ms. Warming `GStreamer` up earlier was not done: without a binding, only making a `GtkMediaFile` starts it, which runs on the main thread just the same, and doing it at every start for a card that may never play audio would cost every start what only a play needs.

### 3. An MP3 is given without its tags

**Found by the test of §1, and reproduced every time:** `GStreamer` 1.28.7 **aborts the whole process** on an MP3 with an `ID3v2` tag given through GTK's stream: "ERROR:../gst/playback/gstdecodebin3.c:3381:mq_slot_handle_stream_start: assertion failed: (collection)", with or without the allow-list, for the slice 4 fixture and for a 15 KB file made by `id3v2mux` (12 runs of 12; with `GST_DEBUG` logging on it sometimes plays: a race). `gst-launch-1.0 playbin3` from a file does not abort; GTK's path goes through `urisourcebin` with its own `parsebin` and `multiqueue`. Most MP3s have an `ID3v2` tag, so **pressing Play on an ordinary MP3 closed Rappel** since slice 4; the slice 4 tests played WAVs.

**Fixed:** the core's check now returns the part a player is given (`media::audio_payload`): for an MP3 its frames only, without the `ID3v2` tags before them and the APE and `ID3v1` tags after; for the other kinds the whole file. `render::media_file::read_audio_for_playing` cuts the bytes it read to that part on the worker thread, and `CardAudio` plays them. Rappel shows no tags, so nothing visible is lost, and `id3demux` and `apetag` leave the allow-list: an ID3 tag that reached `GStreamer` anyway would be a format it cannot play (in the test it is neither prepared nor failed, so the app's 10 s timeout gives it up; it does not abort).

### Tests

- `gst_env::tests::only_the_allowed_plugins_play` runs `play_each_in_a_child` twice, each in a new process of the test binary (`testing::run_alone`; the variables count only when `GStreamer` starts) with a registry of its own in a temporary folder: with Rappel's environment and without its allow-list. The child plays each file through `gtk::MediaFile::for_input_stream`, muted, directly (past the structure check), and prints whether it was prepared. With the allow-list each slice 4 fixture plays exactly as without it (M4A: if `fdkaac` or `faad` is installed); FLAC (`flacdec`, plugin `flac`: its parser `flacparse` is in the allowed `audioparsers`, so this tests a decoder) and Matroska with Vorbis (`matroskademux`, plugin `matroska`: a demuxer, with a decoder that is allowed) never play with it, and do play without it where their plugins are installed. The raw ID3 MP3 runs only with the allow-list (without it, it aborts the child). **Checked with the allow-list removed from the environment** (a scratch copy): FLAC and Matroska played, and the test failed.
- `render::audio_player::tests::an_mp3_with_an_id3_tag_plays_without_aborting` plays the ID3 fixture through `CardAudio`, as the app does, in a process of its own (without the allow-list, so it isolates the fix of §3). **Checked with the cut reverted** (a scratch copy): the child aborted with the assertion above, and the test failed.
- `media::structure::tests::a_player_is_given_the_frames_of_an_mp3_and_the_rest_whole`; the pure parts of `gst_env` (`the_allow_list_is_one_entry_of_plugin_names`, `the_registry_is_rappel_s_own_unless_one_is_chosen`, `the_new_start_sets_what_is_missing_and_then_nothing`).

CI installs `gstreamer1.0-plugins-bad` too, for `faad`, so M4A plays under the allow-list there; `flac` and `matroska` come with `gstreamer1.0-plugins-good`.

## Consequences

- SPEC §4.9's allow-list of decoders is met by plugin, without `gstreamer-rs` and without new packages: `GStreamer` can only use the 21 plugins above in Rappel's process. Within an allowed plugin it still chooses the element by rank (`audioparsers` holds parsers for formats the structure check never lets through, which typefinding then never asks for).
- ALAC M4A files, and AAC M4A files on a system with only `FFmpeg`'s AAC decoder, do not play.
- A user's own `GST_PLUGIN_LOADING_WHITELIST` is replaced by Rappel's, never widened. `GST_REGISTRY_1_0` or `GST_REGISTRY` set by the user is kept.
- The restart now sets up to three variables; the guard is `RAPPEL_GST_ENVIRONMENT_SET` (was `RAPPEL_GST_FEATURES_DEMOTED`, README), set to the process's id, which `exec` keeps, so a guard inherited from another process is ignored; `1` stays the debugging escape hatch (with a warning when the environment lacks Rappel's values). If the restart fails, or the variables are still missing after it, audio is turned off (`gst_env::restricted`, which `playback::available` asks) rather than played without the allow-list (`gst_env::tests::the_guard_counts_only_for_this_process`). **Programs Rappel starts inherit them**, as they did the feature ranks: a file manager opened by Open Backup Folder or a browser opened by a link in About Rappel, when it is started by Rappel itself (not already running, not D-Bus activated), plays media only through these plugins and uses Rappel's registry. Rust 2024 makes removing a variable from a running process `unsafe`, and `GStreamer` reads them only when the first stream is made, so they cannot be removed after use; GNOME's own file manager and most browsers are D-Bus activated or already running, so this is rare.
- `id3demux` and `apetag` are not loaded; tags inside the Ogg, M4A and WAV containers are still read by their demuxers.
