//! The window's size, its maximised state and the deck that was open, kept
//! across restarts in `<data dir>/window-state.ini` (ADR 0039).
//!
//! A key file in the data directory and not `GSettings`: it follows
//! `RAPPEL_DATA_DIR` and the debug data directory, so a development build
//! never touches the real app's state; it needs no installed schema; and it
//! stays out of the database, whose settings are exported and backed up.
//! This state belongs to one device. A missing or broken file gives the
//! defaults, and a failed save is only logged: neither may stop the app.

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

/// The state saved in `data_dir`, or the defaults if there is none or it
/// cannot be read.
pub fn load(data_dir: &Path) -> WindowState {
    let file = glib::KeyFile::new();
    match file.load_from_file(path(data_dir), glib::KeyFileFlags::NONE) {
        Ok(()) => from_key_file(&file),
        Err(err) => {
            // Missing on the first run; anything else is worth a debug line.
            if !err.matches(glib::FileError::Noent) {
                tracing::debug!("window state not read, using the defaults: {err}");
            }
            WindowState::default()
        }
    }
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
        let file = glib::KeyFile::new();
        // A file that does not parse at all is treated by `load` as missing.
        let _ = file.load_from_data(text, glib::KeyFileFlags::NONE);
        from_key_file(&file)
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
