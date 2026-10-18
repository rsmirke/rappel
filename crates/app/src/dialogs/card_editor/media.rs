//! What the editor's images and audio need that is not GTK (ADR 0055,
//! 0056): the text a reference inserts and where the cursor goes, the file
//! choosers' types, what a paste or a drop holds, and the words of a
//! refusal. Pure and unit-tested.

use rappel_core::Error;
use rappel_core::media::{AUDIO_MAX_BYTES, IMAGE_MAX_BYTES, MediaKind, MediaProblem, MediaRef};

use crate::state::{Accept, AddMediaError};

/// The MIME types the Insert Image… chooser offers: the image kinds of the
/// media store (no SVG). The chooser's filter is a convenience; the file's
/// first bytes decide (`Store::add_image`).
pub const IMAGE_MIME_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// The MIME types the Insert Audio… chooser offers: the audio kinds of the
/// media store, with the names shared-mime-info also gives them (an Ogg
/// file is `audio/x-vorbis+ogg` or `audio/x-opus+ogg` there, a WAV
/// `audio/vnd.wave`, an M4A `audio/x-m4a`), so the filter shows them
/// whichever the file chooser goes by. A convenience: the file's first
/// bytes decide (`Store::add_audio`).
pub const AUDIO_MIME_TYPES: [&str; 10] = [
    "audio/ogg",
    "audio/x-vorbis+ogg",
    "audio/x-opus+ogg",
    "audio/opus",
    "audio/mpeg",
    "audio/mp4",
    "audio/x-m4a",
    "audio/wav",
    "audio/x-wav",
    "audio/vnd.wave",
];

/// The file name endings the Insert Audio… chooser offers too, for a file
/// chooser that does not know a type.
pub const AUDIO_SUFFIXES: [&str; 6] = ["ogg", "oga", "opus", "mp3", "m4a", "wav"];

/// The text inserted for `refs` and the cursor's place in it, in
/// characters from its start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    pub text: String,
    pub cursor: usize,
}

/// The reference of `media` as card Markdown: `![](media:…)` for an
/// image, `[](media:…)` (a link, SPEC §4.9) for audio.
fn markdown(media: &MediaRef) -> String {
    if media.kind().is_image() {
        format!("![]({})", media.reference())
    } else {
        format!("[]({})", media.reference())
    }
}

/// [`markdown`] for each of `refs`, a blank line between them, with the
/// cursor between the first one's `[` and `]` so its words can be typed at
/// once (an image's alt text, an audio link's words: the accessible name
/// of the picture or of the play button). `before` and `after` are the
/// characters next to the insertion point, if any: a reference inserted
/// inside a line gets a space on each side that has text, so it does not
/// glue onto a word.
pub fn insertion(refs: &[MediaRef], before: Option<char>, after: Option<char>) -> Insertion {
    let body = refs.iter().map(markdown).collect::<Vec<_>>().join("\n\n");
    let lead = before.is_some_and(|c| !c.is_whitespace());
    let trail = after.is_some_and(|c| !c.is_whitespace());
    let mut text = String::with_capacity(body.len() + 2);
    if lead {
        text.push(' ');
    }
    text.push_str(&body);
    if trail {
        text.push(' ');
    }
    let bracket = match refs.first() {
        Some(first) if !first.kind().is_image() => 1,
        _ => 2,
    };
    Insertion {
        text,
        cursor: usize::from(lead) + bracket,
    }
}

/// What a paste or a drop brings, as GTK offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    /// Files: each is added as an image or audio, as its first bytes say
    /// (the original bytes).
    Files,
    /// A picture only (a screenshot, a copied image): its bytes in one of
    /// [`IMAGE_MIME_TYPES`], added as the other program gave them. It is
    /// never decoded in Rappel's process.
    Picture,
    /// Text, or nothing the editor takes as images or audio files: the
    /// text view's own paste or drop.
    Text,
}

/// Which of the clipboard's or the drag's formats the editor takes: files
/// before a picture (a copied image file often offers both, and the file
/// is the original), and text only when there is neither.
pub fn offered(has_files: bool, has_picture: bool) -> Offered {
    if has_files {
        Offered::Files
    } else if has_picture {
        Offered::Picture
    } else {
        Offered::Text
    }
}

/// What a paste or a drop of files does when none of them is on this
/// computer (a browser offers an image's `https:` address as a file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithoutLocal {
    /// The picture offered with them is taken: the image still comes in
    /// and nothing is fetched.
    Picture,
    /// Their text (the addresses) is pasted or dropped as text, as before
    /// images existed: a copied link pastes as a link.
    Text,
    /// Nothing else is offered: the files are refused, unread, with "Only
    /// files on this computer can be added".
    Refuse,
}

/// [`WithoutLocal`] for an offer with or without a picture and text. A
/// picture comes first (it is what a drag of an image means); the refusal
/// only when files are all that is offered, which is a clear request to
/// add them.
pub fn without_local_file(has_picture: bool, has_text: bool) -> WithoutLocal {
    if has_picture {
        WithoutLocal::Picture
    } else if has_text {
        WithoutLocal::Text
    } else {
        WithoutLocal::Refuse
    }
}

/// What the editor says when a file taken as `accept` cannot be added,
/// in words.
pub fn add_failed_text(err: &AddMediaError, accept: Accept) -> String {
    let too_large = |kind: Option<MediaKind>| match kind {
        Some(kind) if !kind.is_image() => format!(
            "This audio file is larger than {}",
            megabytes(AUDIO_MAX_BYTES)
        ),
        _ => format!("This image is larger than {}", megabytes(IMAGE_MAX_BYTES)),
    };
    let not_of_its_kind = |accept| match accept {
        Accept::Image => "This file is not a PNG, JPEG, WebP or GIF image",
        Accept::Audio => "This file is not an Ogg, MP3, M4A or WAV audio file",
        Accept::Any => "This file is not an image or audio file of a supported kind",
    };
    match err {
        AddMediaError::NotLocal => "Only files on this computer can be added".to_owned(),
        AddMediaError::NotAFile => "This is not a file".to_owned(),
        // Only a picture's bytes are counted before the store.
        AddMediaError::TooLarge => too_large(None),
        AddMediaError::Store(Error::MediaRefused(MediaProblem::TooLarge { kind, .. })) => {
            too_large(Some(*kind))
        }
        AddMediaError::Store(Error::MediaRefused(MediaProblem::NotMedia)) => {
            not_of_its_kind(accept).to_owned()
        }
        AddMediaError::Store(Error::MediaRefused(MediaProblem::NotImage)) => {
            not_of_its_kind(Accept::Image).to_owned()
        }
        AddMediaError::Store(Error::MediaRefused(MediaProblem::NotAudio)) => {
            not_of_its_kind(Accept::Audio).to_owned()
        }
        AddMediaError::Store(Error::MediaRefused(MediaProblem::Malformed)) => {
            "This audio file is damaged".to_owned()
        }
        AddMediaError::Store(Error::MediaRefused(MediaProblem::NotPlainAudio)) => {
            "This audio file holds more than plain audio, so Rappel does not take it".to_owned()
        }
        AddMediaError::Store(Error::MediaRefused(MediaProblem::Empty)) => {
            "This file is empty".to_owned()
        }
        AddMediaError::Unreadable(_) | AddMediaError::Store(Error::MediaRead(_)) => {
            "The file could not be read".to_owned()
        }
        AddMediaError::Store(other) => format!("{}: {other}", could_not_add(accept)),
    }
}

/// "The image could not be added" and its kin, for `accept`.
pub fn could_not_add(accept: Accept) -> &'static str {
    match accept {
        Accept::Image => "The image could not be added",
        Accept::Audio => "The audio file could not be added",
        Accept::Any => "The file could not be added",
    }
}

/// `bytes` as whole megabytes ("10 MB"), the unit the limit is set in.
fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes / 1_000_000)
}

/// The accessible label of a side's Insert Image… button, which names the
/// side (the two buttons look alike).
pub fn insert_label(side: &str) -> String {
    format!("Insert Image in {side}")
}

/// The accessible label of a side's Insert Audio… button.
pub fn insert_audio_label(side: &str) -> String {
    format!("Insert Audio in {side}")
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    fn media(n: u8, kind: MediaKind) -> MediaRef {
        MediaRef::new([n; 32], kind)
    }

    #[test]
    fn a_reference_puts_the_cursor_in_its_alt_text() {
        let png = media(0xab, MediaKind::Png);
        let got = insertion(&[png], None, None);
        assert_eq!(got.text, format!("![](media:{}.png)", "ab".repeat(32)));
        assert_eq!(got.cursor, 2);
        assert_eq!(&got.text[..got.cursor], "![");
    }

    #[test]
    fn several_images_are_paragraphs_of_their_own() {
        let refs = [media(1, MediaKind::Png), media(2, MediaKind::Gif)];
        let got = insertion(&refs, Some('\n'), Some('\n'));
        let lines: Vec<_> = got.text.split("\n\n").collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with(".gif)"));
        assert_eq!(got.cursor, 2);
    }

    #[test]
    fn inside_a_line_the_image_does_not_glue_onto_words() {
        let png = media(3, MediaKind::Png);
        let got = insertion(&[png], Some('a'), Some('b'));
        assert!(got.text.starts_with(" ![]("));
        assert!(got.text.ends_with(") "));
        assert_eq!(got.cursor, 3);
        assert_eq!(&got.text[..got.cursor], " ![");
        let got = insertion(&[png], Some(' '), Some('.'));
        assert!(got.text.starts_with("![]("));
        assert!(got.text.ends_with(") "));
    }

    #[test]
    fn files_come_before_pictures_and_text_last() {
        assert_eq!(offered(true, true), Offered::Files);
        assert_eq!(offered(true, false), Offered::Files);
        assert_eq!(offered(false, true), Offered::Picture);
        assert_eq!(offered(false, false), Offered::Text);
    }

    #[test]
    fn without_a_local_file_the_picture_then_the_text_is_taken() {
        assert_eq!(without_local_file(true, true), WithoutLocal::Picture);
        assert_eq!(without_local_file(true, false), WithoutLocal::Picture);
        assert_eq!(without_local_file(false, true), WithoutLocal::Text);
        assert_eq!(without_local_file(false, false), WithoutLocal::Refuse);
    }

    #[test]
    fn refusals_are_said_in_words() {
        let refused =
            |p, accept| add_failed_text(&AddMediaError::Store(Error::MediaRefused(p)), accept);
        let not_image = "This file is not a PNG, JPEG, WebP or GIF image";
        let not_audio = "This file is not an Ogg, MP3, M4A or WAV audio file";
        assert_eq!(refused(MediaProblem::NotMedia, Accept::Image), not_image);
        assert_eq!(refused(MediaProblem::NotImage, Accept::Image), not_image);
        assert_eq!(refused(MediaProblem::NotMedia, Accept::Audio), not_audio);
        assert_eq!(refused(MediaProblem::NotAudio, Accept::Audio), not_audio);
        assert_eq!(
            refused(MediaProblem::NotMedia, Accept::Any),
            "This file is not an image or audio file of a supported kind"
        );
        assert_eq!(
            refused(
                MediaProblem::TooLarge {
                    kind: MediaKind::Png,
                    limit: IMAGE_MAX_BYTES
                },
                Accept::Any
            ),
            "This image is larger than 10 MB"
        );
        assert_eq!(
            refused(
                MediaProblem::TooLarge {
                    kind: MediaKind::Mp3,
                    limit: AUDIO_MAX_BYTES
                },
                Accept::Any
            ),
            "This audio file is larger than 20 MB"
        );
        for accept in [Accept::Image, Accept::Audio, Accept::Any] {
            assert_eq!(refused(MediaProblem::Empty, accept), "This file is empty");
            assert_eq!(
                add_failed_text(
                    &AddMediaError::Store(Error::MediaRead(io::Error::other("gone"))),
                    accept
                ),
                "The file could not be read"
            );
            assert_eq!(
                add_failed_text(&AddMediaError::Unreadable(io::Error::other("gone")), accept),
                "The file could not be read"
            );
            assert_eq!(
                add_failed_text(&AddMediaError::NotLocal, accept),
                "Only files on this computer can be added"
            );
            assert_eq!(
                add_failed_text(&AddMediaError::NotAFile, accept),
                "This is not a file"
            );
            assert!(
                add_failed_text(&AddMediaError::Store(Error::NoMediaDir), accept)
                    .starts_with(&format!("{}: ", could_not_add(accept)))
            );
        }
        assert_eq!(
            add_failed_text(&AddMediaError::TooLarge, Accept::Image),
            "This image is larger than 10 MB"
        );
        assert_eq!(
            refused(MediaProblem::Malformed, Accept::Audio),
            "This audio file is damaged"
        );
        assert_eq!(
            refused(MediaProblem::NotPlainAudio, Accept::Any),
            "This audio file holds more than plain audio, so Rappel does not take it"
        );
        assert_eq!(could_not_add(Accept::Image), "The image could not be added");
        assert_eq!(
            could_not_add(Accept::Audio),
            "The audio file could not be added"
        );
    }

    #[test]
    fn audio_is_a_link_with_the_cursor_in_its_words() {
        let wav = media(0xcd, MediaKind::Wav);
        let got = insertion(&[wav], None, None);
        assert_eq!(got.text, format!("[](media:{}.wav)", "cd".repeat(32)));
        assert_eq!(&got.text[..got.cursor], "[");
        let got = insertion(&[wav], Some('a'), None);
        assert_eq!(&got.text[..got.cursor], " [");
        // An image and a sound dropped together: each as its kind.
        let png = media(1, MediaKind::Png);
        let got = insertion(&[png, wav], None, None);
        let parts: Vec<_> = got.text.split("\n\n").collect();
        assert!(parts[0].starts_with("![]("), "{}", got.text);
        assert!(parts[1].starts_with("[]("), "{}", got.text);
        assert_eq!(&got.text[..got.cursor], "![");
        // What is inserted parses back to an audio link to what was added.
        let doc = rappel_core::content::parse(&insertion(&[wav], None, None).text);
        match doc.blocks.as_slice() {
            [rappel_core::content::Block::Paragraph(inlines)] => assert!(matches!(
                inlines.as_slice(),
                [rappel_core::content::Inline::Audio { media, .. }] if *media == wav
            )),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_buttons_name_their_side() {
        assert_eq!(insert_label("Front"), "Insert Image in Front");
        assert_eq!(insert_audio_label("Back"), "Insert Audio in Back");
        for mime in AUDIO_MIME_TYPES {
            assert!(mime.starts_with("audio/"), "{mime}");
        }
        assert_eq!(IMAGE_MIME_TYPES.len(), 4);
        assert!(!IMAGE_MIME_TYPES.iter().any(|m| m.contains("svg")));
    }
}
