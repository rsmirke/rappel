use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use rappel_core::media::MediaKind;

use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: HashMap<String, OsString> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
        .collect();
    move |name| map.get(name).cloned()
}

fn names(vars: &[(&'static str, OsString)]) -> Vec<&'static str> {
    vars.iter().map(|(name, _)| *name).collect()
}

#[test]
fn our_entries_come_last_and_once() {
    let ours = demoted();
    assert_eq!(with_demoted(None).as_deref(), Some(ours.as_str()));
    assert_eq!(with_demoted(Some("  ")).as_deref(), Some(ours.as_str()));
    // The user's value is kept, ours after it so ours win.
    let mine = with_demoted(Some("avdec_mp3:none")).unwrap();
    assert_eq!(mine, format!("avdec_mp3:none,{ours}"));
    // Already there: nothing to do, so no loop.
    assert_eq!(with_demoted(Some(&mine)), None);
    assert_eq!(with_demoted(Some(&ours)), None);
    // A user value that raises one of ours again, after ours: ours are
    // added again at the end.
    let raised = format!("{ours},hlsdemux2:primary");
    assert!(with_demoted(Some(&raised)).unwrap().ends_with(&ours));
    for name in [
        "hlsdemux2",
        "dashdemux2",
        "mssdemux2",
        "sdpdemux",
        "souphttpsrc",
        "curlhttpsrc",
        "rtspsrc",
    ] {
        assert!(ours.contains(&format!("{name}:none")), "{name}");
    }
    // GTK's own source for our memory stream is not touched.
    assert!(!ours.contains("giostreamsrc"));
}

/// The entries parse as `GStreamer` reads them: `name:rank`, separated by
/// commas, names of letters and digits.
#[test]
fn the_entries_are_well_formed() {
    for entry in demoted().split(',') {
        let (name, rank) = entry.split_once(':').expect("name:rank");
        assert_eq!(rank, "none");
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric()), "{name}");
    }
}

/// One entry of several names, so `GStreamer` takes each for a plugin's
/// name, never a source package's (a single name may be either, and
/// `gst-plugins-good` would allow all of it); no `:` (another entry),
/// `@` (a path) or space; no plugin of many formats.
#[test]
fn the_allow_list_is_one_entry_of_plugin_names() {
    let value = allowed();
    assert!(value.contains(','));
    for name in value.split(',') {
        assert!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()),
            "{name}"
        );
        assert!(!name.starts_with("gst"), "{name} looks like a package");
    }
    let mut sorted = ALLOWED.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ALLOWED.len(), "a name twice");
    for never in [
        "libav", "matroska", "flac", "hls", "dash", "soup", "playbin",
    ] {
        assert!(!ALLOWED.contains(&never), "{never}");
    }
    // What `playbin3` and GTK's stream need is there.
    for needed in ["playback", "coreelements", "typefindfunctions", "gio"] {
        assert!(ALLOWED.contains(&needed), "{needed}");
    }
}

#[test]
fn the_registry_is_rappel_s_own_unless_one_is_chosen() {
    let arch = std::env::consts::ARCH;
    let file = registry_file(&env(&[("HOME", "/home/u")])).unwrap();
    assert_eq!(
        file,
        PathBuf::from(format!(
            "/home/u/.cache/rappel/gstreamer-1.0/registry.{arch}.bin"
        ))
    );
    let file = registry_file(&env(&[("HOME", "/home/u"), ("XDG_CACHE_HOME", "/c")])).unwrap();
    assert_eq!(
        file,
        PathBuf::from(format!("/c/rappel/gstreamer-1.0/registry.{arch}.bin"))
    );
    // A relative cache folder is not one (XDG); nor a relative home.
    let file = registry_file(&env(&[("HOME", "/h"), ("XDG_CACHE_HOME", "c")])).unwrap();
    assert!(file.starts_with("/h/.cache"));
    assert_eq!(registry_file(&env(&[("HOME", "h")])), None);
    assert_eq!(registry_file(&env(&[])), None);

    // A registry the user chose is kept, under either name.
    for chosen in REGISTRY {
        let vars = wanted(&env(&[("HOME", "/h"), (chosen, "/mine.bin")]));
        assert!(!names(&vars).contains(&REGISTRY[0]), "{chosen}");
    }
    // An empty one is not a choice.
    let vars = wanted(&env(&[("HOME", "/h"), ("GST_REGISTRY", "")]));
    assert!(names(&vars).contains(&REGISTRY[0]));
}

#[test]
fn the_new_start_sets_what_is_missing_and_then_nothing() {
    let vars = wanted(&env(&[("HOME", "/h")]));
    assert_eq!(names(&vars), [FEATURE_RANK, ALLOW_LIST, REGISTRY[0]]);
    // The environment of the new start needs nothing more: no loop.
    let mut after: Vec<(&str, String)> = vec![("HOME", "/h".to_owned())];
    after.extend(
        vars.iter()
            .map(|(k, v)| (*k, v.to_string_lossy().into_owned())),
    );
    let after: Vec<(&str, &str)> = after.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(wanted(&env(&after)), Vec::new());
    // A user's allow-list is replaced by ours, never widened.
    let vars = wanted(&env(&[("HOME", "/h"), (ALLOW_LIST, "gst-libav")]));
    let list = vars.iter().find(|(k, _)| *k == ALLOW_LIST).unwrap();
    assert_eq!(list.1, OsString::from(allowed()));
}

#[test]
fn the_guard_counts_only_for_this_process() {
    let pid = 4_242;
    let ours = OsString::from("pid:4242");
    let bare = OsString::from("4242");
    let other = OsString::from("17");
    let debug = OsString::from("1");
    assert_eq!(start(None, pid, false), Start::Restart);
    assert_eq!(start(None, pid, true), Start::Ready);
    // The new start: the environment is ready, or still lacks Rappel's
    // values (audio then off).
    assert_eq!(start(Some(&ours), pid, true), Start::Ready);
    assert_eq!(start(Some(&ours), pid, false), Start::Unrestricted);
    // Inherited from another process: not ours, so it restarts.
    assert_eq!(start(Some(&other), pid, false), Start::Restart);
    assert_eq!(start(Some(&other), pid, true), Start::Ready);
    // A bare id is not the restart's value.
    assert_eq!(start(Some(&bare), pid, false), Start::Restart);
    // In process 1 the restart's value is still not the escape hatch.
    assert_eq!(
        start(Some(&OsString::from("pid:1")), 1, false),
        Start::Unrestricted
    );
    assert_eq!(start(Some(&OsString::from("pid:1")), 1, true), Start::Ready);
    // The debugging escape hatch.
    assert_eq!(start(Some(&debug), pid, false), Start::Debugging);
    assert_eq!(start(Some(&debug), pid, true), Start::Debugging);
    // Not set by this process: restricted.
    assert!(restricted());
}

/// Set for the child processes of `only_the_allowed_plugins_play`: `1`
/// plays with Rappel's environment, `0` without its allow-list.
const CHILD: &str = "RAPPEL_TEST_GST_CHILD";

/// Lists the names of [`FILES`] a child leaves out, separated by commas.
const SKIP: &str = "RAPPEL_TEST_GST_SKIP";

/// Each file of the test, by name: the six audio fixtures of the core
/// (each as the app gives it, so the ID3 one without its tag:
/// `audio_payload`), the ID3 one whole (`mp3-id3-raw`: `GStreamer` 1.28
/// aborts on it without the allow-list, ADR 0062), and two of formats
/// whose plugins are not allowed (`render/fixtures`).
const FILES: [(&str, &[u8]); 9] = [
    (
        "opus",
        include_bytes!("../../../core/src/media/fixtures/tone-opus.ogg"),
    ),
    (
        "vorbis",
        include_bytes!("../../../core/src/media/fixtures/tone-vorbis.ogg"),
    ),
    (
        "mp3",
        include_bytes!("../../../core/src/media/fixtures/tone.mp3"),
    ),
    ("mp3-id3", ID3_MP3),
    ("mp3-id3-raw", ID3_MP3),
    (
        "wav",
        include_bytes!("../../../core/src/media/fixtures/tone.wav"),
    ),
    (
        "m4a",
        include_bytes!("../../../core/src/media/fixtures/tone.m4a"),
    ),
    ("flac", include_bytes!("../render/fixtures/tone.flac")),
    (
        "matroska",
        include_bytes!("../render/fixtures/tone-vorbis.mka"),
    ),
];

const ID3_MP3: &[u8] = include_bytes!("../../../core/src/media/fixtures/tone-id3.mp3");

/// What became of each file in a child, from its output lines
/// `gst-outcome <name> <played|failed|stuck>`.
fn outcomes(stdout: &str) -> HashMap<String, String> {
    stdout
        .lines()
        // libtest prints the test's name before its output, on that line.
        .filter_map(|line| line.split_once("gst-outcome ").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_once(' '))
        .map(|(name, outcome)| (name.to_owned(), outcome.trim().to_owned()))
        .collect()
}

/// Runs `play_each_in_a_child` in a new process of this test binary,
/// with Rappel's environment (`restricted`) or without its allow-list,
/// and a registry of its own in `dir`.
fn run_child(dir: &Path, restricted: bool) -> HashMap<String, String> {
    let registry = dir
        .join(if restricted { "a" } else { "b" })
        .join("registry.bin");
    let registry_text = registry.to_string_lossy().into_owned();
    let get = |name: &str| {
        if name == REGISTRY[0] {
            Some(OsString::from(&registry_text))
        } else {
            std::env::var_os(name)
        }
    };
    let mut vars = wanted(&get);
    if !restricted {
        vars.retain(|(name, _)| *name != ALLOW_LIST);
    }
    let stdout = crate::testing::run_alone("gst_env::tests::play_each_in_a_child", |command| {
        command
            .envs(vars)
            .env(REGISTRY[0], &registry)
            .env(CHILD, if restricted { "1" } else { "0" });
        if !restricted {
            // It would abort the child (ADR 0062).
            command.env_remove(ALLOW_LIST).env(SKIP, "mp3-id3-raw");
        }
    });
    if restricted {
        assert!(registry.is_file(), "GStreamer wrote no registry of its own");
    }
    outcomes(&stdout)
}

/// With Rappel's environment, `GStreamer` loads only the allowed plugins:
/// each audio fixture plays through `gtk::MediaFile` as it does without
/// the allow-list, and FLAC (`flacdec`) and Matroska (`matroskademux`)
/// files do not, even given to GTK's stream directly, past the core's
/// structure check, while they do play without the allow-list where their
/// plugins are installed. Each run is a new process of this binary: the
/// variables count only when `GStreamer` starts.
#[test]
fn only_the_allowed_plugins_play() {
    if std::env::var_os(CHILD).is_some() {
        return;
    }
    if !crate::render::playback::available() {
        eprintln!("GStreamer's base plugins are missing: nothing can play here");
        return;
    }
    let dir = tempfile::tempdir().expect("a temporary folder");
    let open = run_child(dir.path(), false);
    let restricted = run_child(dir.path(), true);
    eprintln!("without the allow-list: {open:?}\nwith it: {restricted:?}");
    let played = |outcomes: &HashMap<String, String>, name: &str| {
        outcomes.get(name).map(String::as_str) == Some("played")
    };
    for (name, _) in FILES {
        assert!(
            open.contains_key(name) || name == "mp3-id3-raw",
            "{name} has no outcome"
        );
        assert!(restricted.contains_key(name), "{name} has no outcome");
    }
    // An ID3 tag given to `GStreamer` as it is: its demuxer is not allowed,
    // so the stream fails (and does not abort the process).
    assert!(!played(&restricted, "mp3-id3-raw"));
    // Something plays here at all (base and good plugins, a sink or the
    // fake one autoaudiosink falls back to): else this proves nothing.
    assert!(played(&open, "wav"), "a WAV does not play here: {open:?}");
    for name in ["opus", "vorbis", "mp3", "mp3-id3", "wav"] {
        assert_eq!(
            played(&restricted, name),
            played(&open, name),
            "{name} plays differently with the allow-list"
        );
    }
    // M4A needs an allowed AAC decoder (not FFmpeg's).
    let aac = ["fdkaac", "faad"]
        .iter()
        .any(|name| crate::render::playback::plugin_installed(name));
    assert_eq!(
        played(&restricted, "m4a"),
        aac && played(&open, "m4a"),
        "M4A with the allow-list"
    );
    // Formats outside the list never play with it, and do without it
    // where their plugins are there: the list is what stops them.
    for (name, plugin) in [("flac", "flac"), ("matroska", "matroska")] {
        assert!(
            !played(&restricted, name),
            "{name} played with the allow-list"
        );
        if crate::render::playback::plugin_installed(plugin) {
            assert!(
                played(&open, name),
                "{name} does not play even without the allow-list"
            );
        }
    }
}

/// The child of `only_the_allowed_plugins_play`: plays each file through
/// `gtk::MediaFile` from memory, muted, and prints what became of it.
/// Does nothing in a normal run.
#[test]
fn play_each_in_a_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    crate::testing::with_widgets(|| {
        use gtk::prelude::*;
        let mut kept = Vec::new();
        let skip = std::env::var(SKIP).unwrap_or_default();
        for (name, bytes) in FILES {
            if skip.split(',').any(|skipped| skipped == name) {
                continue;
            }
            let bytes = if name == "mp3-id3" {
                let frames = rappel_core::media::audio_payload(MediaKind::Mp3, bytes)
                    .expect("the fixture is plain MP3");
                &bytes[frames]
            } else {
                bytes
            };
            let input =
                gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_static(bytes));
            // The first stream starts `GStreamer`, which builds its
            // registry on this (the main) thread: `RAPPEL_TIMINGS=1`.
            let started = std::time::Instant::now();
            let stream = gtk::MediaFile::for_input_stream(&input);
            stream.set_muted(true);
            stream.play();
            crate::testing::print_timing(&format!("making and starting {name}"), started.elapsed());
            // `GStreamer` neither prepares nor fails the raw ID3 one: the
            // app's own timeout would give it up.
            let wait = if name == "mp3-id3-raw" { 3 } else { 20 };
            crate::testing::settle_until(
                || stream.is_prepared() || stream.error().is_some(),
                Duration::from_secs(wait),
            );
            let outcome = if stream.error().is_some() {
                "failed"
            } else if stream.is_prepared() && !stream.has_video() {
                "played"
            } else {
                "stuck"
            };
            println!("gst-outcome {name} {outcome}");
            stream.pause();
            // Never dropped while not prepared (`audio_player::retire`):
            // kept until the process ends.
            kept.push(stream);
        }
        std::mem::forget(kept);
    });
}
