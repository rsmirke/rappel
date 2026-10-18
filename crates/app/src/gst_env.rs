//! `GStreamer`'s elements that follow URIs or reach the network, demoted so
//! `playbin` never chooses them (ADR 0056).
//!
//! GTK plays card audio through `GStreamer`'s `playbin`, which chooses its
//! demuxers by finding the format of the bytes. A playlist or a streaming
//! manifest (HLS, DASH, Smooth Streaming, an SDP session) makes it plug a
//! demuxer that opens the URIs inside: local files and the network. The
//! core refuses such content behind an audio signature
//! (`media::check_structure`); this is the second line. `GStreamer` reads
//! `GST_PLUGIN_FEATURE_RANK` when it starts (`gst_init`, exact feature
//! names, later entries winning), and autoplugging never picks a feature
//! of rank `none`. Checked: with these entries, an HLS master playlist
//! behind an ID3 tag no longer opens its variant or segments (`strace`,
//! `GStreamer` 1.28), and Ogg, MP3, M4A and WAV still play.
//!
//! Rust 2024 makes changing a running process's environment `unsafe`
//! (`std::env::set_var`, `glib::setenv`), which the workspace forbids. So
//! when the variable lacks our entries, `main` starts the program again in
//! place (`execv`, the same process) with them added after any value the
//! user set, before GTK or `GStreamer` are loaded or any thread exists. A
//! second variable guards against doing it twice; if the new start fails
//! the app carries on, with only the core's check, and says so in the log.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;

/// The variable `GStreamer` reads.
pub const VARIABLE: &str = "GST_PLUGIN_FEATURE_RANK";

/// Set on the new start, so it never starts again.
const GUARD: &str = "RAPPEL_GST_FEATURES_DEMOTED";

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

/// Our entries, as `GStreamer` reads them.
fn ours() -> String {
    DEMOTED
        .iter()
        .map(|name| format!("{name}:none"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The value the variable needs, given the one it has (`None`: unset, or
/// not text), or `None` if it ends with our entries already. The user's
/// own entries are kept, before ours, so ours win.
pub fn with_demoted(existing: Option<&str>) -> Option<String> {
    let ours = ours();
    match existing.map(str::trim) {
        Some(value) if value.ends_with(&ours) => None,
        Some(value) if !value.is_empty() => Some(format!("{value},{ours}")),
        _ => Some(ours),
    }
}

/// Starts the program again with [`VARIABLE`] extended, unless it is
/// already (or this is the new start). Returns only if no new start
/// happens; called first thing in `main`.
pub fn ensure_demoted() {
    if std::env::var_os(GUARD).is_some() {
        return;
    }
    let existing = std::env::var_os(VARIABLE);
    let text = existing.as_ref().and_then(|v| v.to_str());
    if existing.is_some() && text.is_none() {
        tracing::warn!("{VARIABLE} is not text; GStreamer's network elements are demoted anyway");
    }
    let Some(value) = with_demoted(text) else {
        return;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!("cannot find the program to demote GStreamer's network elements: {err}");
            return;
        }
    };
    let mut args = std::env::args_os();
    let program_name = args.next().unwrap_or_else(|| OsString::from("rappel"));
    // `exec` returns only on failure.
    let err = std::process::Command::new(exe)
        .arg0(program_name)
        .args(args)
        .env(VARIABLE, value)
        .env(GUARD, "1")
        .exec();
    tracing::warn!("cannot restart to demote GStreamer's network elements: {err}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_entries_come_last_and_once() {
        let ours = ours();
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
        for entry in ours().split(',') {
            let (name, rank) = entry.split_once(':').expect("name:rank");
            assert_eq!(rank, "none");
            assert!(name.chars().all(|c| c.is_ascii_alphanumeric()), "{name}");
        }
    }
}
