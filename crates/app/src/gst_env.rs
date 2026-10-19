//! What `GStreamer` may load and plug when GTK plays card audio (ADR 0056,
//! ADR 0062).
//!
//! GTK plays card audio through `GStreamer`'s `playbin3`, which chooses its
//! demuxers and decoders itself, by finding the format of the bytes, and
//! runs them in Rappel's process. The core lets only plain audio of the
//! four kinds through (`media::check_structure`); these are the next
//! lines, set in the environment before `GStreamer` starts:
//!
//! 1. **Only allow-listed plugins load at all** (`ALLOWED`, in
//!    `GST_PLUGIN_LOADING_WHITELIST`). `GStreamer` reads the variable when
//!    it starts (`_priv_gst_plugin_initialize`, `gst/gstplugin.c`): entries
//!    separated by `:`, each `names[@path]`; a single name matches a
//!    plugin's name or its source package's, several names separated by
//!    commas only plugins' names. A plugin not listed is never loaded:
//!    "Whitelist specified and plugin not in whitelist, not loading"
//!    (`_priv_gst_plugin_load_file_for_registry`), so none of its elements
//!    can be made, by `playbin3` or anyone. Rappel gives one entry of
//!    several names, so only plugin names count (never a package, which
//!    would allow everything in it), and no path (plugin folders differ
//!    between distributions; names do not). Checked with `GStreamer` 1.28
//!    (`only_the_allowed_plugins_play`, in a child process): the six
//!    fixtures play, and FLAC and Matroska, whose plugins are not listed,
//!    do not, even given to `gtk::MediaFile` directly.
//! 2. **A registry of Rappel's own** (`GST_REGISTRY_1_0`, under the user's
//!    cache folder), unless the user chose one: with an allow-list,
//!    `GStreamer` keeps the plugins it did not load as blacklisted in its
//!    registry cache, and the user's other programs share the default one;
//!    alternating would rescan every plugin each time (3 s here). Rappel's
//!    holds only the allowed plugins: built in about 0.1 s.
//! 3. **The elements that follow URIs or reach the network are demoted**
//!    (`DEMOTED`, rank `none` in `GST_PLUGIN_FEATURE_RANK`: exact feature
//!    names, later entries winning; autoplugging never picks a feature of
//!    rank `none`). Their plugins are not allowed anyway; this stays as a
//!    second line in case a distribution ships one of them in an allowed
//!    plugin. Checked: an HLS master playlist behind an ID3 tag no longer
//!    opens its variant or segments (`strace`).
//!
//! Rust 2024 makes changing a running process's environment `unsafe`
//! (`std::env::set_var`, `glib::setenv`), which the workspace forbids. So
//! when a variable lacks our value, `main` starts the program again in
//! place (`execv`, the same process) with them set, before GTK or
//! `GStreamer` are loaded or any thread exists. A guard variable stops it
//! from doing so twice; if the new start fails the app carries on with
//! only the core's check, and says so in the log.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

/// The variable of feature ranks.
pub const FEATURE_RANK: &str = "GST_PLUGIN_FEATURE_RANK";

/// The variable of the plugins that may load.
pub const ALLOW_LIST: &str = "GST_PLUGIN_LOADING_WHITELIST";

/// The variable of the registry cache's file, and the older name
/// `GStreamer` reads when it is unset (`gst/gstregistry.c`).
pub const REGISTRY: [&str; 2] = ["GST_REGISTRY_1_0", "GST_REGISTRY"];

/// Set on the new start to the process's id, so it never starts again;
/// `1` skips the restart for debugging (README).
const GUARD: &str = "RAPPEL_GST_ENVIRONMENT_SET";

/// The plugins that may load, by name (`gst-inspect-1.0 <name>`): what
/// GTK's media stream and the four kinds need, and nothing else.
///
/// - Core and playback: `coreelements` (queues, `typefind`, …),
///   `typefindfunctions`, `playback` (`playbin3`, which `GstPlay` makes),
///   `gio` (`giostreamsrc`, through which GTK hands over our memory
///   stream), `audioconvert`, `audioresample`, `volume`.
/// - Output: `autodetect` (`autoaudiosink`) and the sinks it may choose:
///   `pulseaudio`, `pipewire`, `alsa`.
/// - Ogg Opus and Vorbis: `ogg`, `opus`, `opusparse`, `vorbis`.
/// - WAV: `wavparse`. MP3: `audioparsers` (`mpegaudioparse`, `aacparse`),
///   `mpg123`.
/// - M4A: `isomp4` (`qtdemux`) and an AAC decoder, `fdkaac` or `faad`.
///
/// Not listed: `libav` (`FFmpeg`), one plugin of hundreds of demuxers and
/// decoders that an allow-list of plugins cannot cut down, and whose
/// features' names depend on the `FFmpeg` it was built with; without it an
/// M4A of Apple Lossless (ALAC, which only `FFmpeg` decodes in
/// `GStreamer`) or of AAC on a system with no other AAC decoder "could not
/// be played" (ADR 0062). Nor `id3demux` and `apetag`: an MP3 is given
/// without its tags (`media::audio_payload`; `GStreamer` 1.28 aborts the
/// process on an `ID3v2` tag in GTK's stream), so an ID3 tag that reaches
/// `GStreamer` anyway is a format it cannot play. Nor `app` and `pbtypes`,
/// which playback does not need.
pub const ALLOWED: [&str; 21] = [
    "coreelements",
    "typefindfunctions",
    "playback",
    "gio",
    "audioconvert",
    "audioresample",
    "volume",
    "autodetect",
    "pulseaudio",
    "pipewire",
    "alsa",
    "ogg",
    "opus",
    "opusparse",
    "vorbis",
    "wavparse",
    "audioparsers",
    "mpg123",
    "isomp4",
    "fdkaac",
    "faad",
];

/// The features given rank `none`: the adaptive-streaming and session
/// demuxers, which open the URIs their input names, and the network
/// sources. None of them is needed to play a local Ogg, MP3, M4A or WAV.
pub const DEMOTED: [&str; 19] = [
    "hlsdemux",
    "hlsdemux2",
    "dashdemux",
    "dashdemux2",
    "mssdemux",
    "mssdemux2",
    "sdpdemux",
    "souphttpsrc",
    "curlhttpsrc",
    "neonhttpsrc",
    "rtspsrc",
    "rtmpsrc",
    "rtmp2src",
    "srtsrc",
    "ristsrc",
    "rtpsrc",
    "udpsrc",
    "tcpclientsrc",
    "webrtcbin",
];

/// Our feature ranks, as `GStreamer` reads them.
fn demoted() -> String {
    DEMOTED
        .iter()
        .map(|name| format!("{name}:none"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The allow-list's value: one entry of several names, so `GStreamer`
/// compares each with plugins' names only.
pub fn allowed() -> String {
    ALLOWED.join(",")
}

/// The value [`FEATURE_RANK`] needs, given the one it has (`None`: unset,
/// or not text), or `None` if it ends with our entries already. The user's
/// own entries are kept, before ours, so ours win.
pub fn with_demoted(existing: Option<&str>) -> Option<String> {
    let ours = demoted();
    match existing.map(str::trim) {
        Some(value) if value.ends_with(&ours) => None,
        Some(value) if !value.is_empty() => Some(format!("{value},{ours}")),
        _ => Some(ours),
    }
}

/// Rappel's registry file, in the user's cache folder (`XDG_CACHE_HOME`
/// if it is an absolute path, else `$HOME/.cache`), named as `GStreamer`
/// names its own (`registry.<arch>.bin`). `None` without either.
pub fn registry_file(get: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| {
        get(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let cache =
        absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cache")))?;
    Some(
        cache
            .join("rappel")
            .join("gstreamer-1.0")
            .join(format!("registry.{}.bin", std::env::consts::ARCH)),
    )
}

/// The variables to set, with their values, given how `get` reads the
/// environment: empty when every one is as Rappel needs it already.
pub fn wanted(get: &dyn Fn(&str) -> Option<OsString>) -> Vec<(&'static str, OsString)> {
    let mut out = Vec::new();
    let ranks = get(FEATURE_RANK);
    let text = ranks.as_deref().and_then(OsStr::to_str);
    if ranks.is_some() && text.is_none() {
        tracing::warn!(
            "{FEATURE_RANK} is not text; GStreamer's network elements are demoted anyway"
        );
    }
    if let Some(value) = with_demoted(text) {
        out.push((FEATURE_RANK, OsString::from(value)));
    }
    let allowed = allowed();
    if get(ALLOW_LIST).as_deref() != Some(OsStr::new(&allowed)) {
        out.push((ALLOW_LIST, OsString::from(allowed)));
    }
    let chosen = REGISTRY
        .iter()
        .any(|name| get(name).is_some_and(|v| !v.is_empty()));
    if !chosen && let Some(file) = registry_file(get) {
        out.push((REGISTRY[0], file.into_os_string()));
    }
    out
}

/// What [`ensure_restricted`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The environment is as Rappel needs it: carry on.
    Ready,
    /// Start again in place with the variables set.
    Restart,
    /// `RAPPEL_GST_ENVIRONMENT_SET=1`, the debugging escape hatch (README):
    /// carry on as the environment is, warning if it lacks Rappel's values.
    Debugging,
    /// This is the new start, and the environment still lacks them: carry
    /// on with audio off rather than unrestricted.
    Unrestricted,
}

/// The guard's value on the new start: the process's id, which `exec`
/// keeps, so a guard inherited from another process (a Rappel that
/// started this one) is not taken for ours.
fn guard_value(pid: u32) -> String {
    pid.to_string()
}

/// What to do, given the guard's value, this process's id and whether
/// [`wanted`] is empty. Pure.
pub fn start(guard: Option<&OsStr>, pid: u32, nothing_wanted: bool) -> Start {
    let guard = guard.and_then(OsStr::to_str);
    match guard {
        Some("1") => Start::Debugging,
        Some(value) if value == guard_value(pid) => {
            if nothing_wanted {
                Start::Ready
            } else {
                Start::Unrestricted
            }
        }
        // Unset, or another process's.
        _ if nothing_wanted => Start::Ready,
        _ => Start::Restart,
    }
}

/// Set when `GStreamer` could not be restricted: [`restricted`] is then
/// false and audio is not played (`playback::available`).
static UNRESTRICTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Whether `GStreamer` runs with Rappel's restrictions, or was left as it
/// is on purpose (the debugging escape hatch). False after a failed
/// restart: the app then plays no audio rather than playing it with
/// every plugin `GStreamer` has.
pub fn restricted() -> bool {
    UNRESTRICTED.get().is_none()
}

/// Turns audio off ([`restricted`]); the caller has logged why.
fn unrestricted() {
    tracing::warn!("audio is turned off rather than played with every GStreamer plugin");
    let _ = UNRESTRICTED.set(());
}

/// Starts the program again with the variables of [`wanted`] set, unless
/// they are already (or this is the new start). Returns only if no new
/// start happens; called first thing in `main`. If the new start cannot be
/// made, audio is turned off ([`restricted`]).
pub fn ensure_restricted() {
    let vars = wanted(&|name| std::env::var_os(name));
    match start(
        std::env::var_os(GUARD).as_deref(),
        std::process::id(),
        vars.is_empty(),
    ) {
        Start::Ready => return,
        Start::Debugging => {
            if !vars.is_empty() {
                tracing::warn!(
                    "{GUARD}=1: GStreamer is not restricted to Rappel's plugins (debugging only)"
                );
            }
            return;
        }
        Start::Unrestricted => {
            tracing::warn!("GStreamer's variables are still missing after the restart");
            unrestricted();
            return;
        }
        Start::Restart => {}
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!("cannot find the program to restrict GStreamer's plugins: {err}");
            unrestricted();
            return;
        }
    };
    let mut args = std::env::args_os();
    let program_name = args.next().unwrap_or_else(|| OsString::from("rappel"));
    // `exec` returns only on failure; it keeps the process's id.
    let err = std::process::Command::new(exe)
        .arg0(program_name)
        .args(args)
        .envs(vars)
        .env(GUARD, guard_value(std::process::id()))
        .exec();
    tracing::warn!("cannot restart to restrict GStreamer's plugins: {err}");
    unrestricted();
}

#[cfg(test)]
mod tests;
