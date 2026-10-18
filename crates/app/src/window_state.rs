//! The window's size, its maximised state and the deck that was open, kept
//! across restarts in `<data dir>/window-state.ini` (ADR 0039).
//!
//! A key file in the data directory and not `GSettings`: it follows
//! `RAPPEL_DATA_DIR` and the debug data directory, so a development build
//! never touches the real app's state; it needs no installed schema; and it
//! stays out of the database, whose settings are exported and backed up.
//! This state belongs to one device. A missing or broken file gives the
//! defaults, and a failed save is only logged: neither may stop the app.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use gtk::glib;

/// The file name in the data directory.
const FILE_NAME: &str = "window-state.ini";
const GROUP: &str = "window";
const WIDTH: &str = "width";
const HEIGHT: &str = "height";
const MAXIMIZED: &str = "maximized";
const DECK: &str = "deck";
/// The file's mode, like the database's: only the user reads it.
const FILE_MODE_BITS: u32 = 0o600;
const FILE_MODE: i32 = 0o600;

/// The window's default size, as in `data/window.blp`.
pub const DEFAULT_WIDTH: i32 = 640;
pub const DEFAULT_HEIGHT: i32 = 560;
/// The smallest size the window allows (`width-request`, `height-request`
/// in `data/window.blp`).
const MIN_WIDTH: i32 = 360;
const MIN_HEIGHT: i32 = 294;
/// No screen is this large; a bigger stored value is not a real size.
const MAX_SIDE: i32 = 16_384;
/// Deck ids are 36-character `UUIDv7` strings; anything much longer was
/// not written by us.
const MAX_DECK_ID_BYTES: usize = 64;
/// The file Rappel writes is about 100 bytes; a larger one was not written
/// by it, and is not read (ADR 0050, row 33).
const MAX_FILE_BYTES: u64 = 4 * 1024;

/// What is remembered between runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    /// The deck whose page was open when the window closed, if any.
    pub deck_id: Option<String>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            maximized: false,
            deck_id: None,
        }
    }
}

/// A stored side if it is a real window size, else `default`. Values below
/// the minimum or above any screen are not sizes the window can have had.
fn side(value: Option<i32>, min: i32, default: i32) -> i32 {
    value
        .filter(|v| (min..=MAX_SIDE).contains(v))
        .unwrap_or(default)
}

/// A stored deck id, if it can be one.
fn deck_id(value: Option<String>) -> Option<String> {
    value.filter(|id| !id.is_empty() && id.len() <= MAX_DECK_ID_BYTES)
}

/// Reads the state from a loaded key file. Every missing or bad value
/// takes its default; this never fails.
fn from_key_file(file: &glib::KeyFile) -> WindowState {
    WindowState {
        width: side(file.integer(GROUP, WIDTH).ok(), MIN_WIDTH, DEFAULT_WIDTH),
        height: side(file.integer(GROUP, HEIGHT).ok(), MIN_HEIGHT, DEFAULT_HEIGHT),
        maximized: file.boolean(GROUP, MAXIMIZED).unwrap_or(false),
        deck_id: deck_id(file.string(GROUP, DECK).ok().map(String::from)),
    }
}

/// The key file holding `state`. No deck means no `deck` key.
fn to_key_file(state: &WindowState) -> glib::KeyFile {
    let file = glib::KeyFile::new();
    file.set_integer(GROUP, WIDTH, state.width);
    file.set_integer(GROUP, HEIGHT, state.height);
    file.set_boolean(GROUP, MAXIMIZED, state.maximized);
    if let Some(id) = &state.deck_id {
        file.set_string(GROUP, DECK, id);
    }
    file
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Why the file was not read.
#[derive(Debug)]
enum Unread {
    /// There is no file (the first run).
    Missing,
    /// Something else: logged at debug level.
    Refused(String),
}

/// The file's bytes, if it is a regular file of at most
/// [`MAX_FILE_BYTES`]. The file is untrusted (ADR 0050, row 33): a symbolic
/// link is not followed, and a pipe, a device or a folder is never opened,
/// so reading can neither block nor run on without end. The checks are made
/// before opening, on the name, and again on the open file, with its read
/// cut one byte past the limit. A file swapped in between the two can only
/// be another regular file of the user's, as for an import (ADR 0025,
/// row 6).
fn read_bounded(path: &Path) -> Result<Vec<u8>, Unread> {
    let refuse = |what: &str| Unread::Refused(what.to_owned());
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(Unread::Missing),
        Err(err) => return Err(Unread::Refused(err.to_string())),
    };
    if !meta.file_type().is_file() {
        return Err(refuse("not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(refuse("too large"));
    }
    let file = std::fs::File::open(path).map_err(|err| Unread::Refused(err.to_string()))?;
    let open = file
        .metadata()
        .map_err(|err| Unread::Refused(err.to_string()))?;
    if !open.file_type().is_file() {
        return Err(refuse("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Unread::Refused(err.to_string()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(refuse("too large"));
    }
    Ok(bytes)
}

/// The state in the file's `bytes`: every missing or bad value takes its
/// default, and bytes that are not a key file give the defaults. So do
/// bytes that are not UTF-8 or hold a NUL, which no key file has
/// (`g_key_file_load_from_data` takes a C string: the binding would panic
/// on a NUL).
fn parse(bytes: &[u8]) -> WindowState {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return WindowState::default();
    };
    if text.contains('\0') {
        return WindowState::default();
    }
    let file = glib::KeyFile::new();
    match file.load_from_data(text, glib::KeyFileFlags::NONE) {
        Ok(()) => from_key_file(&file),
        Err(err) => {
            tracing::debug!("window state not read, using the defaults: {err}");
            WindowState::default()
        }
    }
}

/// The state saved in `data_dir`, or the defaults if there is none or it
/// cannot be read. Never fails, never blocks.
pub fn load(data_dir: &Path) -> WindowState {
    match read_bounded(&path(data_dir)) {
        Ok(bytes) => parse(&bytes),
        Err(Unread::Missing) => WindowState::default(),
        Err(Unread::Refused(why)) => {
            tracing::debug!("window state not read, using the defaults: {why}");
            WindowState::default()
        }
    }
}

/// `width` × `height`, made to fit the largest of `monitors` (each a
/// width and a height in application pixels), so a size saved on a larger
/// screen, or edited by hand, never opens a window larger than the screen.
/// Never below the window's minimum; unchanged when no monitor is known.
pub fn fit_to_monitors(width: i32, height: i32, monitors: &[(i32, i32)]) -> (i32, i32) {
    let widest = monitors.iter().map(|m| m.0).max();
    let tallest = monitors.iter().map(|m| m.1).max();
    let fit = |side: i32, screen: Option<i32>, min: i32| match screen {
        Some(screen) if screen > 0 => side.min(screen).max(min),
        _ => side,
    };
    (
        fit(width, widest, MIN_WIDTH),
        fit(height, tallest, MIN_HEIGHT),
    )
}

/// Saves `state` in `data_dir`. `g_file_set_contents_full` with
/// `G_FILE_SET_CONTENTS_CONSISTENT` writes a temporary file created with
/// `mode` (less the umask) and renames it over the old one (`GLib` docs), so
/// a crash leaves the old file or the new one, and a new file is never there
/// with a wider mode than 0600, like the database. An existing file keeps
/// its mode (same docs), so a wider one is narrowed to 0600 afterwards, as
/// the database's files are (`prepare_private_file` in core). A failure is
/// logged.
pub fn save(data_dir: &Path, state: &WindowState) {
    let path = path(data_dir);
    let data = to_key_file(state).to_data();
    if let Err(err) = glib::file_set_contents_full(
        &path,
        data.as_bytes(),
        glib::FileSetContentsFlags::CONSISTENT,
        FILE_MODE,
    ) {
        tracing::warn!("the window state could not be saved: {err}");
        return;
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.permissions().mode() & 0o7777 != FILE_MODE_BITS => {
            if let Err(err) =
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(FILE_MODE_BITS))
            {
                tracing::warn!("the window state's permissions could not be set: {err}");
            }
        }
        Ok(_) => {}
        Err(err) => tracing::warn!("the window state's permissions could not be read: {err}"),
    }
}

/// The deck to reopen from the navigation stack, bottom first: each page's
/// deck id if it is a deck page. The lowest deck page wins, so a review,
/// statistics or All Cards page above it reopens that deck.
pub fn deck_to_reopen(pages: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    pages.into_iter().flatten().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECK_ID: &str = "019a0000-0000-7000-8000-000000000001";

    fn parse(text: &str) -> WindowState {
        super::parse(text.as_bytes())
    }

    #[test]
    fn a_state_survives_a_round_trip() {
        let state = WindowState {
            width: 1200,
            height: 800,
            maximized: true,
            deck_id: Some(DECK_ID.to_owned()),
        };
        assert_eq!(from_key_file(&to_key_file(&state)), state);
        let without_deck = WindowState {
            deck_id: None,
            ..state
        };
        assert_eq!(from_key_file(&to_key_file(&without_deck)), without_deck);
    }

    #[test]
    fn missing_or_broken_values_take_their_defaults() {
        assert_eq!(parse(""), WindowState::default());
        assert_eq!(parse("not a key file\n[window\n"), WindowState::default());
        assert_eq!(
            parse("[window]\nwidth=wide\nheight=\nmaximized=perhaps\ndeck=\n"),
            WindowState::default()
        );
        assert_eq!(
            parse("[other]\nwidth=900\n"),
            WindowState::default(),
            "only the window group counts"
        );
    }

    #[test]
    fn sizes_outside_what_a_window_can_have_are_refused() {
        let state = parse("[window]\nwidth=-5\nheight=99999999\n");
        assert_eq!((state.width, state.height), (DEFAULT_WIDTH, DEFAULT_HEIGHT));
        let state = parse("[window]\nwidth=359\nheight=293\n");
        assert_eq!((state.width, state.height), (DEFAULT_WIDTH, DEFAULT_HEIGHT));
        let state = parse("[window]\nwidth=360\nheight=294\n");
        assert_eq!((state.width, state.height), (360, 294));
        let state = parse("[window]\nwidth=16384\nheight=16385\n");
        assert_eq!((state.width, state.height), (16_384, DEFAULT_HEIGHT));
    }

    #[test]
    fn a_deck_id_that_cannot_be_one_is_dropped() {
        assert_eq!(parse("[window]\ndeck=\n").deck_id, None);
        let long = format!("[window]\ndeck={}\n", "a".repeat(65));
        assert_eq!(parse(&long).deck_id, None);
        let ok = format!("[window]\ndeck={DECK_ID}\n");
        assert_eq!(parse(&ok).deck_id.as_deref(), Some(DECK_ID));
    }

    #[test]
    fn load_and_save_use_the_data_directory() -> Result<(), std::io::Error> {
        let dir = tempfile::tempdir()?;
        assert_eq!(load(dir.path()), WindowState::default());
        let state = WindowState {
            width: 900,
            height: 700,
            maximized: false,
            deck_id: Some(DECK_ID.to_owned()),
        };
        save(dir.path(), &state);
        let meta = std::fs::metadata(dir.path().join(FILE_NAME))?;
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(load(dir.path()), state);
        // An existing file with a wider mode (by hand, or an older
        // version) is narrowed to 0600 again.
        let path = dir.path().join(FILE_NAME);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        save(dir.path(), &WindowState::default());
        let meta = std::fs::metadata(&path)?;
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(load(dir.path()), WindowState::default());
        std::fs::write(dir.path().join(FILE_NAME), b"\xff\xfe garbage")?;
        assert_eq!(load(dir.path()), WindowState::default());
        Ok(())
    }

    #[test]
    fn hostile_values_never_make_a_strange_window() {
        for text in [
            "[window]\nwidth=nan\nheight=NaN\n",
            "[window]\nwidth=inf\nheight=-inf\n",
            "[window]\nwidth=800.5\nheight=6e2\n",
            "[window]\nwidth=0\nheight=0\n",
            "[window]\nwidth=-2147483648\nheight=2147483647\n",
            "[window]\nwidth=99999999999999999999\nheight=0x300\n",
            "[window]\nwidth= 800\nheight=\u{663}\u{660}\u{660}\n",
            "[window]\nwidth=800\nwidth=900\n[window]\nheight=50\n",
            "[window]\nwidth=800\0\n",
            "\u{feff}[window]\nwidth=800\n",
        ] {
            let state = parse(text);
            assert!(
                (MIN_WIDTH..=MAX_SIDE).contains(&state.width)
                    && (MIN_HEIGHT..=MAX_SIDE).contains(&state.height),
                "{text:?}: {state:?}"
            );
        }
        // A NUL anywhere: the defaults.
        let state = parse("[window]\nmaximized=true\ndeck=../../etc\u{0}\n");
        assert_eq!(state, WindowState::default());
        // Not UTF-8: the defaults.
        assert_eq!(
            super::parse(b"[window]\nwidth=\xff800\n"),
            WindowState::default()
        );
    }

    #[test]
    fn a_saved_size_fits_the_largest_screen() {
        let screens = [(1920, 1080), (1280, 1024)];
        assert_eq!(fit_to_monitors(1200, 800, &screens), (1200, 800));
        assert_eq!(fit_to_monitors(16_384, 16_384, &screens), (1920, 1080));
        assert_eq!(fit_to_monitors(1800, 1050, &[(1280, 1024)]), (1280, 1024));
        // Never below the window's minimum, even on a tiny screen.
        assert_eq!(
            fit_to_monitors(640, 560, &[(320, 240)]),
            (MIN_WIDTH, MIN_HEIGHT)
        );
        // No monitor, or one reporting no size: unchanged.
        assert_eq!(fit_to_monitors(16_384, 900, &[]), (16_384, 900));
        assert_eq!(fit_to_monitors(1200, 900, &[(0, -1)]), (1200, 900));
    }

    #[test]
    fn only_a_small_regular_file_is_read() -> Result<(), std::io::Error> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join(FILE_NAME);
        let saved = "[window]\nwidth=900\nheight=700\n";
        // A symbolic link to a real state file is not followed.
        let elsewhere = dir.path().join("elsewhere.ini");
        std::fs::write(&elsewhere, saved)?;
        std::os::unix::fs::symlink(&elsewhere, &file)?;
        assert_eq!(load(dir.path()), WindowState::default());
        // Saving replaces the link with a file of its own, and leaves the
        // file it pointed to alone.
        let state = WindowState {
            width: 1000,
            ..WindowState::default()
        };
        save(dir.path(), &state);
        assert!(std::fs::symlink_metadata(&file)?.file_type().is_file());
        assert_eq!(std::fs::read_to_string(&elsewhere)?, saved);
        assert_eq!(load(dir.path()), state);
        // A link to a device that never ends.
        std::fs::remove_file(&file)?;
        std::os::unix::fs::symlink("/dev/zero", &file)?;
        assert_eq!(load(dir.path()), WindowState::default());
        // A folder.
        std::fs::remove_file(&file)?;
        std::fs::create_dir(&file)?;
        assert_eq!(load(dir.path()), WindowState::default());
        std::fs::remove_dir(&file)?;
        // Too large: a valid state padded past the limit.
        let padded = format!("{saved}#{}\n", "x".repeat(8_192));
        std::fs::write(&file, padded)?;
        assert_eq!(load(dir.path()), WindowState::default());
        // At the limit, it is read.
        let at_limit = format!("{saved}#{}\n", "x".repeat(4_096 - saved.len() - 2));
        assert_eq!(at_limit.len(), 4_096);
        std::fs::write(&file, at_limit)?;
        assert_eq!(load(dir.path()).width, 900);
        // A wider mode is read (the values are checked anyway) and
        // narrowed to 0600 by the next save.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666))?;
        assert_eq!(load(dir.path()).width, 900);
        Ok(())
    }

    /// A pipe in place of the file would block a plain open for ever; it is
    /// never opened. Bounded by the hang limit of `testing::within`.
    #[test]
    fn a_pipe_in_place_of_the_file_does_not_block() -> Result<(), std::io::Error> {
        let dir = tempfile::tempdir()?;
        let fifo = dir.path().join(FILE_NAME);
        let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
        assert!(made.success(), "mkfifo");
        let data_dir = dir.path().to_path_buf();
        let state = crate::testing::within(
            std::time::Duration::from_millis(100),
            "reading a pipe",
            move || load(&data_dir),
        );
        assert_eq!(state, WindowState::default());
        Ok(())
    }

    #[test]
    fn the_lowest_deck_page_is_reopened() {
        let deck = || Some(DECK_ID.to_owned());
        // Deck list only.
        assert_eq!(deck_to_reopen([None]), None);
        // Deck list, All Cards (from the deck list).
        assert_eq!(deck_to_reopen([None, None]), None);
        // Deck list, deck page.
        assert_eq!(deck_to_reopen([None, deck()]), deck());
        // Deck list, deck page, review or statistics or All Cards.
        assert_eq!(deck_to_reopen([None, deck(), None]), deck());
        // Two deck pages never stack, but the lowest would win.
        assert_eq!(
            deck_to_reopen([None, deck(), Some("other".to_owned())]),
            deck()
        );
    }
}
