//! The one way the renderer reads a media file (ADR 0055, ADR 0056):
//! images for glycin, audio for `GStreamer`. Only a file of the media folder,
//! by the name the core builds from a checked hash
//! (`MediaRef::file_name`), is ever opened here, never a path or a URI
//! from card text.
//!
//! The file is opened with `O_NOFOLLOW | O_NONBLOCK` (never through a
//! symbolic link, never blocking on a pipe planted under the name) and
//! checked on the open handle (`fstat`): a regular file, not empty, within
//! its kind's limit (10 MB for an image, 20 MB for audio). Its first bytes
//! must be of the reference's kind (`MediaKind::detect`), and an audio
//! file read for playing must be plain audio of its kind over its whole
//! length (`audio_payload`, ADR 0056): a file stored before that check
//! existed, or put in the folder by hand, is refused here before a byte
//! reaches `GStreamer`. What is checked
//! is what is read: the bytes go to the decoder, never the path, so the
//! file cannot change between the check and the decode.

use std::fs;
use std::io::{self, Read};
use std::ops::Range;
use std::path::Path;

use rappel_core::media::{DETECT_BYTES, MediaKind, MediaRef, audio_payload};

/// Why a media file is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// There is no file under the name (never added, or lost).
    Missing,
    /// The file is there but refused: not a regular file, empty, over its
    /// kind's limit, not of its kind, or it could not be read.
    Refused,
}

/// Opens the media file at `path`, checked as the module says, with its
/// size.
fn open_checked(path: &Path, kind: MediaKind) -> Result<(fs::File, u64), Unreadable> {
    use std::os::unix::fs::OpenOptionsExt;
    // Opened without following a link and without blocking (a pipe under
    // the name would block an open for reading), then checked on the open
    // handle, so what is checked is what is read (as `window_state`'s
    // `read_bounded`).
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => Unreadable::Missing,
            _ => Unreadable::Refused,
        })?;
    let meta = file.metadata().map_err(|_| Unreadable::Refused)?;
    if !meta.file_type().is_file() || meta.len() == 0 || meta.len() > kind.max_bytes() {
        return Err(Unreadable::Refused);
    }
    Ok((file, meta.len()))
}

/// Reads the whole media file `media` at `path`, checked: a regular file
/// (never through a symbolic link), at most its kind's limit, whose first
/// bytes are of `media`'s kind. A file that grew past the limit after the
/// check is refused (one byte past it is read, no more).
pub fn read_media_file(path: &Path, media: MediaRef) -> Result<Vec<u8>, Unreadable> {
    read_checked(path, media).map(|(bytes, _)| bytes)
}

/// The bytes of the audio file `media` at `path` that `GStreamer` is
/// given: [`read_media_file`]'s, cut to what `audio_payload` keeps (an
/// MP3's frames without its tags: `GStreamer` 1.28 aborts the process on
/// an `ID3v2` tag in GTK's stream, ADR 0062).
pub fn read_audio_for_playing(path: &Path, media: MediaRef) -> Result<Vec<u8>, Unreadable> {
    let (mut bytes, payload) = read_checked(path, media)?;
    bytes.truncate(payload.end);
    bytes.drain(..payload.start);
    Ok(bytes)
}

/// [`read_media_file`], with the part a player is given.
fn read_checked(path: &Path, media: MediaRef) -> Result<(Vec<u8>, Range<usize>), Unreadable> {
    let kind = media.kind();
    let (file, len) = open_checked(path, kind)?;
    let limit = kind.max_bytes();
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Unreadable::Refused)?;
    let too_large = u64::try_from(bytes.len()).map_or(true, |n| n > limit);
    if too_large || MediaKind::detect(&bytes) != Some(kind) {
        return Err(Unreadable::Refused);
    }
    match audio_payload(kind, &bytes) {
        Ok(payload) => Ok((bytes, payload)),
        Err(why) => {
            tracing::debug!("a media file is refused: {}", why.why);
            Err(Unreadable::Refused)
        }
    }
}

/// Whether the media file `media` at `path` could be read: the checks of
/// [`read_media_file`] on its first bytes only, so a card shows at once
/// whether its audio is there without reading 20 MB.
pub fn check_media_file(path: &Path, media: MediaRef) -> Result<(), Unreadable> {
    let kind = media.kind();
    let (file, _) = open_checked(path, kind)?;
    let mut head = Vec::with_capacity(DETECT_BYTES);
    file.take(u64::try_from(DETECT_BYTES).unwrap_or(u64::MAX))
        .read_to_end(&mut head)
        .map_err(|_| Unreadable::Refused)?;
    if MediaKind::detect(&head) == Some(kind) {
        Ok(())
    } else {
        Err(Unreadable::Refused)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn wav() -> Vec<u8> {
        crate::testing::wav_bytes(8)
    }

    fn reference(ext: &str) -> MediaRef {
        MediaRef::parse(&format!("media:{}.{ext}", "ab".repeat(32))).expect("a valid reference")
    }

    #[test]
    fn an_audio_file_is_read_only_when_it_is_what_it_names() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("file");
        let wav_ref = reference("wav");
        // Missing.
        assert_eq!(read_media_file(&path, wav_ref), Err(Unreadable::Missing));
        assert_eq!(check_media_file(&path, wav_ref), Err(Unreadable::Missing));
        // Of its kind.
        std::fs::write(&path, wav()).unwrap();
        assert_eq!(read_media_file(&path, wav_ref), Ok(wav()));
        assert_eq!(check_media_file(&path, wav_ref), Ok(()));
        // Another kind under the name: an image named as audio, HTML.
        for wrong in [reference("png"), reference("mp3"), reference("ogg")] {
            assert_eq!(read_media_file(&path, wrong), Err(Unreadable::Refused));
            assert_eq!(check_media_file(&path, wrong), Err(Unreadable::Refused));
        }
        // An ID3 tag, then a playlist: an MP3 by its first bytes, refused
        // when read whole (the look at the first bytes cannot know).
        let mut playlist = b"ID3\x04\0\0\0\0\0\0".to_vec();
        playlist.extend_from_slice(b"#EXTM3U\nhttps://example.invalid/a.mp3\n");
        std::fs::write(&path, &playlist).unwrap();
        assert_eq!(
            read_media_file(&path, reference("mp3")),
            Err(Unreadable::Refused)
        );
        // A WAV cut short.
        std::fs::write(&path, &wav()[..40]).unwrap();
        assert_eq!(read_media_file(&path, wav_ref), Err(Unreadable::Refused));
        std::fs::write(&path, b"<html><script>x</script></html>").unwrap();
        assert_eq!(read_media_file(&path, wav_ref), Err(Unreadable::Refused));
        assert_eq!(check_media_file(&path, wav_ref), Err(Unreadable::Refused));
        // Empty.
        std::fs::write(&path, b"").unwrap();
        assert_eq!(check_media_file(&path, wav_ref), Err(Unreadable::Refused));
        // Past the audio limit: refused from the handle's size, unread.
        let mut big = std::fs::File::create(&path).unwrap();
        big.write_all(&wav()).unwrap();
        big.set_len(rappel_core::media::AUDIO_MAX_BYTES + 1)
            .unwrap();
        assert_eq!(read_media_file(&path, wav_ref), Err(Unreadable::Refused));
        assert_eq!(check_media_file(&path, wav_ref), Err(Unreadable::Refused));
        // A WAV at the image limit plus one is fine as audio.
        big.set_len(rappel_core::media::IMAGE_MAX_BYTES + 1)
            .unwrap();
        assert_eq!(check_media_file(&path, wav_ref), Ok(()));
        // A folder, a link and a pipe under the name.
        let dir = tmp.path().join("dir");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(read_media_file(&dir, wav_ref), Err(Unreadable::Refused));
        let real = tmp.path().join("real");
        std::fs::write(&real, wav()).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(read_media_file(&link, wav_ref), Err(Unreadable::Refused));
        assert_eq!(check_media_file(&link, wav_ref), Err(Unreadable::Refused));
        let fifo = tmp.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(read_media_file(&fifo, wav_ref), Err(Unreadable::Refused));
        assert_eq!(check_media_file(&fifo, wav_ref), Err(Unreadable::Refused));
    }
}
