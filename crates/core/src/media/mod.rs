//! Media: the images and audio files of cards, content-addressed (SPEC
//! §4.3, §4.9, ADR 0054).
//!
//! A media file is named by the SHA-256 of its bytes and the canonical
//! extension of its kind, `<sha256>.<ext>`, in `<data dir>/media/`, and a
//! card refers to it as `media:<sha256>.<ext>`. Its kind comes from its
//! first bytes only (its magic number), never from a file name or a
//! declared type, so a file is stored under the extension of what it is.
//! Only the kinds of [`MediaKind`] are accepted: no SVG, no HTML, nothing
//! that could hold a script or name another file. Nothing here touches the
//! network; a reference is only ever turned into a path inside the media
//! folder, from a checked hash and a fixed extension.

mod kind;
mod store;
mod structure;
mod trash;
mod used;

use std::fmt;

pub use kind::{AUDIO_MAX_BYTES, DETECT_BYTES, IMAGE_MAX_BYTES, MediaKind};
pub(crate) use store::{
    CopyFailure, Ingested, MEDIA_DIR_NAME, Sync, copy_capped, ingest, insert_media_row, sync_folder,
};
pub use structure::{Malformed, check_structure};
pub use trash::{CleanedUp, PURGE_AFTER_DAYS, TrashTidied, UnusedMedia};
pub(crate) use trash::{recover_all, recover_refs, recover_rows};

/// The scheme of a media reference in card Markdown.
pub const SCHEME: &str = "media:";

/// Why a file is not accepted as media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MediaProblem {
    /// The file has no bytes.
    #[error("the file is empty")]
    Empty,
    /// Its first bytes are not those of a kind Rappel keeps (SVG, HTML and
    /// anything else are refused).
    #[error(
        "the file is not a PNG, JPEG, WebP or GIF image, or an Ogg, MP3, M4A or WAV audio file"
    )]
    NotMedia,
    /// A file of a kind Rappel keeps, but not an image, where only an
    /// image is accepted ([`crate::storage::Store::add_image`]).
    #[error("the file is not a PNG, JPEG, WebP or GIF image")]
    NotImage,
    /// A file of a kind Rappel keeps, but not an audio file, where only
    /// audio is accepted ([`crate::storage::Store::add_audio`]).
    #[error("the file is not an Ogg, MP3, M4A or WAV audio file")]
    NotAudio,
    /// An audio file whose first bytes are of its kind but which is
    /// damaged ([`check_structure`]): cut, out of bounds, a bad checksum,
    /// or junk where its audio should be (ADR 0056).
    #[error("the audio file is damaged")]
    Malformed,
    /// A whole audio file that holds more than plain audio of its kind
    /// ([`check_structure`]): another stream, a reference to another
    /// file, a box or a format audio files do not have (ADR 0056).
    #[error("the audio file holds more than plain audio")]
    NotPlainAudio,
    /// Larger than its kind allows.
    #[error("the file is larger than {limit} bytes")]
    TooLarge {
        /// What the file is.
        kind: MediaKind,
        /// The limit, in bytes.
        limit: u64,
    },
}

/// A media file: its SHA-256 and its kind. Built only from a checked hash
/// and a known kind, so [`Self::file_name`] is always a plain name of 64
/// lowercase hex digits, a dot and a fixed extension.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaRef {
    hash: [u8; 32],
    kind: MediaKind,
}

impl MediaRef {
    /// The file with SHA-256 `hash`, of `kind`.
    #[must_use]
    pub fn new(hash: [u8; 32], kind: MediaKind) -> Self {
        Self { hash, kind }
    }

    /// The SHA-256 of the file's bytes.
    #[must_use]
    pub fn hash(&self) -> &[u8; 32] {
        &self.hash
    }

    /// What the file is.
    #[must_use]
    pub fn kind(&self) -> MediaKind {
        self.kind
    }

    /// The hash as 64 lowercase hex digits.
    #[must_use]
    pub fn hex(&self) -> String {
        hex(&self.hash)
    }

    /// `<sha256>.<ext>`: the file's name in the media folder, and in an
    /// export's `media/` folder.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.hex(), self.kind.extension())
    }

    /// `media:<sha256>.<ext>`, as card Markdown names the file.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("{SCHEME}{}", self.file_name())
    }

    /// The file a file name names: exactly 64 lowercase hex digits, a dot
    /// and the canonical extension of a kind. Anything else (upper-case
    /// hex, another extension, a path) is `None`.
    #[must_use]
    pub fn from_file_name(name: &str) -> Option<Self> {
        Self::from_name_bytes(name.as_bytes())
    }

    /// [`Self::from_file_name`] on bytes (an archive entry's name).
    pub(crate) fn from_name_bytes(name: &[u8]) -> Option<Self> {
        let (hex, rest) = name.split_at_checked(64)?;
        let ext = rest.strip_prefix(b".")?;
        let kind = MediaKind::from_extension(std::str::from_utf8(ext).ok()?)?;
        Some(Self::new(parse_hex(hex)?, kind))
    }

    /// The file a URL of card Markdown names: `media:` followed by a
    /// [`Self::from_file_name`] name. Anything else is not a media
    /// reference: a remote URL, a path, a bad hash or an unknown extension.
    #[must_use]
    pub fn parse(url: &str) -> Option<Self> {
        Self::from_file_name(url.strip_prefix(SCHEME)?)
    }
}

/// The reference, which says everything about it.
impl fmt::Debug for MediaRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MediaRef({})", self.file_name())
    }
}

/// `bytes` as lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    out
}

/// 64 lowercase hex digits as 32 bytes; `None` for anything else
/// (upper-case digits included, so a hash has one spelling).
pub(crate) fn parse_hex(text: &[u8]) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0; 32];
    for (i, [high, low]) in text.as_chunks::<2>().0.iter().enumerate() {
        out[i] = digit(*high)? << 4 | digit(*low)?;
    }
    Some(out)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod trash_tests;
