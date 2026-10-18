//! Whether `GStreamer` can play at all here (ADR 0056).
//!
//! GTK plays card audio with `GstPlay`, which makes a `playbin3` element
//! when the first stream is made, and calls `g_error` if there is none
//! ("'playbin3' element not found"): that aborts the whole process. So
//! before any `GtkMediaFile` is made, Rappel looks for the plugin that
//! provides `playbin3`, `libgstplayback.so` (`GStreamer`'s base plugins),
//! where `GStreamer` itself looks for plugins. Without it, every audio
//! control says "Audio playback is not available on this system" and
//! nothing is played; the reason is logged once.
//!
//! `GStreamer`'s search (`scan_and_update_registry`, `gst/gstregistry.c`,
//! `GStreamer` 1.28): the directories of `GST_PLUGIN_PATH_1_0` (or, if that
//! is unset, `GST_PLUGIN_PATH`), then those of
//! `GST_PLUGIN_SYSTEM_PATH_1_0` (or `GST_PLUGIN_SYSTEM_PATH`), which,
//! when set, replace the defaults: the user's
//! `$XDG_DATA_HOME/gstreamer-1.0/plugins`, then the library's own folder
//! (`<dir of libgstreamer>/gstreamer-1.0`, where relocation finds it) or
//! the compiled-in `PLUGINDIR`. Rappel cannot read `PLUGINDIR`; it takes
//! the folder of the `libgstreamer-1.0` its process has loaded (GTK links
//! it), from `/proc/self/maps`, which is that same folder on every
//! distribution and in a Flatpak runtime, and then the usual places as a
//! fallback.
//!
//! A plugin file that is there but cannot be loaded (damaged, of another
//! `GStreamer`) is not caught: that is a broken installation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The plugin that provides `playbin3`.
pub const PLUGIN: &str = "libgstplayback.so";

/// The folders tried when `GStreamer`'s own library folder is not known.
const FALLBACK_DIRS: [&str; 6] = [
    "/usr/lib64/gstreamer-1.0",
    "/usr/lib/gstreamer-1.0",
    "/usr/local/lib64/gstreamer-1.0",
    "/usr/local/lib/gstreamer-1.0",
    // A Flatpak's own extensions.
    "/app/lib/gstreamer-1.0",
    "/app/lib64/gstreamer-1.0",
];

/// What `GStreamer`'s search depends on.
#[derive(Debug, Clone, Default)]
pub struct SearchEnv {
    /// `GST_PLUGIN_PATH_1_0`, else `GST_PLUGIN_PATH`.
    pub plugin_path: Option<OsString>,
    /// `GST_PLUGIN_SYSTEM_PATH_1_0`, else `GST_PLUGIN_SYSTEM_PATH`.
    pub system_path: Option<OsString>,
    /// `g_get_user_data_dir()`.
    pub user_data_dir: Option<PathBuf>,
    /// The folder of the loaded `libgstreamer-1.0`.
    pub library_dir: Option<PathBuf>,
}

/// The folders `GStreamer` looks for plugins in, in its order, then the
/// fallbacks for its compiled-in folder.
pub fn plugin_dirs(env: &SearchEnv) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env
        .plugin_path
        .as_deref()
        .map(|p| entries(p).collect())
        .unwrap_or_default();
    if let Some(system) = env.system_path.as_deref() {
        dirs.extend(entries(system));
        return dirs;
    }
    if let Some(data) = &env.user_data_dir {
        dirs.push(data.join("gstreamer-1.0").join("plugins"));
    }
    if let Some(lib) = &env.library_dir {
        dirs.push(lib.join("gstreamer-1.0"));
    }
    // Debian and Ubuntu's multiarch folder for this machine.
    dirs.push(PathBuf::from(format!(
        "/usr/lib/{}-linux-gnu/gstreamer-1.0",
        std::env::consts::ARCH
    )));
    dirs.extend(FALLBACK_DIRS.iter().map(PathBuf::from));
    dirs
}

/// The folders of a path variable, without its empty entries: an empty
/// entry (`a::b`, a trailing colon, an empty variable) would otherwise be
/// the current folder, which Rappel must not take for a plugin folder.
fn entries(value: &std::ffi::OsStr) -> impl Iterator<Item = PathBuf> + '_ {
    std::env::split_paths(value).filter(|dir| !dir.as_os_str().is_empty())
}

/// Whether one of `dirs` has [`PLUGIN`], by `exists`.
pub fn playback_found(dirs: &[PathBuf], exists: impl Fn(&Path) -> bool) -> bool {
    dirs.iter().any(|dir| exists(&dir.join(PLUGIN)))
}

/// Whether the plugin `name` (`libgst<name>.so`, as `GStreamer` names the
/// file of a plugin) is in one of the folders `GStreamer` looks in here
/// (tests: which formats can play at all).
#[cfg(test)]
pub fn plugin_installed(name: &str) -> bool {
    plugin_dirs(&this_env())
        .iter()
        .any(|dir| dir.join(format!("libgst{name}.so")).is_file())
}

/// The folder of `libgstreamer-1.0` in the text of `/proc/self/maps`.
pub fn library_dir_from_maps(maps: &str) -> Option<PathBuf> {
    maps.lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .find(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("libgstreamer-1.0.so"))
        })
        .and_then(|path| Path::new(path).parent().map(Path::to_path_buf))
}

fn var(primary: &str, fallback: &str) -> Option<OsString> {
    std::env::var_os(primary).or_else(|| std::env::var_os(fallback))
}

/// This process's [`SearchEnv`].
fn this_env() -> SearchEnv {
    SearchEnv {
        plugin_path: var("GST_PLUGIN_PATH_1_0", "GST_PLUGIN_PATH"),
        system_path: var("GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"),
        user_data_dir: Some(gtk::glib::user_data_dir()),
        library_dir: std::fs::read_to_string("/proc/self/maps")
            .ok()
            .as_deref()
            .and_then(library_dir_from_maps),
    }
}

#[cfg(test)]
thread_local! {
    /// Tests can make playback missing (or there) whatever is installed.
    static FORCED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Makes [`available`] answer `value` on this thread until the returned
/// guard is dropped (tests), even if the test panics meanwhile.
#[cfg(test)]
#[must_use = "the forced value lasts as long as the guard"]
pub fn force(value: bool) -> Forced {
    let before = FORCED.with(|forced| forced.replace(Some(value)));
    Forced { before }
}

/// Puts [`available`] back as it was when dropped.
#[cfg(test)]
pub struct Forced {
    before: Option<bool>,
}

#[cfg(test)]
impl Drop for Forced {
    fn drop(&mut self) {
        FORCED.with(|forced| forced.set(self.before));
    }
}

/// Whether [`PLUGIN`] was found, once per run.
static AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Whether audio can be played here: [`PLUGIN`] is where `GStreamer` looks.
/// Found once per run (a few file checks); the first `false` is logged.
pub fn available() -> bool {
    #[cfg(test)]
    if let Some(value) = FORCED.with(std::cell::Cell::get) {
        return value;
    }
    *AVAILABLE.get_or_init(|| {
        let dirs = plugin_dirs(&this_env());
        let found = playback_found(&dirs, Path::is_file);
        if !found {
            tracing::warn!(
                "audio cannot be played: GStreamer's base plugins ({PLUGIN}, which provides \
                 playbin3) are not installed"
            );
        }
        found
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_path_replaces_the_defaults() {
        let env = SearchEnv {
            plugin_path: Some(OsString::from("/a:/b")),
            system_path: Some(OsString::from("/sys")),
            user_data_dir: Some(PathBuf::from("/home/u/.local/share")),
            library_dir: Some(PathBuf::from("/usr/lib/x")),
        };
        assert_eq!(
            plugin_dirs(&env),
            ["/a", "/b", "/sys"].map(PathBuf::from).to_vec()
        );
        let env = SearchEnv {
            system_path: None,
            ..env
        };
        let dirs = plugin_dirs(&env);
        assert_eq!(
            dirs[..5],
            [
                "/a",
                "/b",
                "/home/u/.local/share/gstreamer-1.0/plugins",
                "/usr/lib/x/gstreamer-1.0",
                &format!(
                    "/usr/lib/{}-linux-gnu/gstreamer-1.0",
                    std::env::consts::ARCH
                ),
            ]
            .map(PathBuf::from)
        );
        assert!(dirs.contains(&PathBuf::from("/app/lib/gstreamer-1.0")));
    }

    #[test]
    fn empty_entries_are_not_the_current_folder() {
        let env = SearchEnv {
            plugin_path: Some(OsString::new()),
            system_path: Some(OsString::from("/a::/b:")),
            ..SearchEnv::default()
        };
        assert_eq!(plugin_dirs(&env), ["/a", "/b"].map(PathBuf::from).to_vec());
        let env = SearchEnv {
            system_path: Some(OsString::from(":")),
            ..SearchEnv::default()
        };
        assert_eq!(plugin_dirs(&env), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_forced_answer_ends_with_its_guard_even_on_a_panic() {
        let truth = available();
        {
            let _forced = force(!truth);
            assert_eq!(available(), !truth);
            {
                let _inner = force(truth);
                assert_eq!(available(), truth);
            }
            assert_eq!(available(), !truth);
        }
        assert_eq!(available(), truth);
        let panicked = std::panic::catch_unwind(|| {
            let _forced = force(!truth);
            panic!("a failing test");
        });
        assert!(panicked.is_err());
        assert_eq!(available(), truth);
    }

    #[test]
    fn the_plugin_is_found_only_where_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let system = tmp.path().join("system");
        let user = tmp.path().join("user");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        let env = SearchEnv {
            plugin_path: Some(user.clone().into_os_string()),
            system_path: Some(system.clone().into_os_string()),
            ..SearchEnv::default()
        };
        let dirs = plugin_dirs(&env);
        assert!(!playback_found(&dirs, Path::is_file));
        // Another plugin is not it.
        std::fs::write(system.join("libgstcoreelements.so"), b"").unwrap();
        assert!(!playback_found(&dirs, Path::is_file));
        std::fs::write(user.join(PLUGIN), b"").unwrap();
        assert!(playback_found(&dirs, Path::is_file));
        std::fs::remove_file(user.join(PLUGIN)).unwrap();
        std::fs::write(system.join(PLUGIN), b"").unwrap();
        assert!(playback_found(&dirs, Path::is_file));
        // A folder under the name is not a plugin.
        std::fs::remove_file(system.join(PLUGIN)).unwrap();
        std::fs::create_dir(system.join(PLUGIN)).unwrap();
        assert!(!playback_found(&dirs, Path::is_file));
    }

    #[test]
    fn the_library_folder_comes_from_the_maps() {
        let maps = "\
7f00-7f01 r--p 00000000 08:01 1 /usr/lib/x86_64-linux-gnu/libgtk-4.so.1.2400.1
7f02-7f03 r-xp 00001000 08:01 2 /usr/lib/x86_64-linux-gnu/libgstreamer-1.0.so.0.2807.0
7f04-7f05 rw-p 00000000 00:00 0
7f06-7f07 r--p 00000000 08:01 3 /usr/lib/x86_64-linux-gnu/libgstreamer-1.0-extra.so
";
        assert_eq!(
            library_dir_from_maps(maps),
            Some(PathBuf::from("/usr/lib/x86_64-linux-gnu"))
        );
        assert_eq!(library_dir_from_maps("7f04-7f05 rw-p 0 00:00 0\n"), None);
        assert_eq!(library_dir_from_maps(""), None);
    }
}
