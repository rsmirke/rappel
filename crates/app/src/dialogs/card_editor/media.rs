//! What the editor's images need that is not GTK (ADR 0055): the text a
//! reference inserts and where the cursor goes, the file chooser's types,
//! what a paste or a drop holds, and the words of a refusal. Pure and
//! unit-tested.

use rappel_core::Error;
use rappel_core::media::{IMAGE_MAX_BYTES, MediaProblem, MediaRef};

use crate::state::AddImageError;

/// The MIME types the Insert Image… chooser offers: the image kinds of the
/// media store (no SVG). The chooser's filter is a convenience; the file's
/// first bytes decide (`Store::add_image`).
pub const IMAGE_MIME_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// The text inserted for `refs` and the cursor's place in it, in
/// characters from its start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    pub text: String,
    pub cursor: usize,
}

/// `![](media:…)` for each of `refs`, a blank line between them, with the
/// cursor between the first one's `[` and `]` so its alt text can be
/// typed at once (it is the image's accessible name). `before` and
/// `after` are the characters next to the insertion point, if any: an
/// image inserted inside a line gets a space on each side that has text,
/// so it does not glue onto a word.
pub fn insertion(refs: &[MediaRef], before: Option<char>, after: Option<char>) -> Insertion {
    let body = refs
        .iter()
        .map(|media| format!("![]({})", media.reference()))
        .collect::<Vec<_>>()
        .join("\n\n");
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
    Insertion {
        text,
        cursor: usize::from(lead) + 2,
    }
}

/// What a paste or a drop brings, as GTK offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    /// Files: each is added as an image (the original bytes).
    Files,
    /// A picture only (a screenshot, a copied image): its bytes in one of
    /// [`IMAGE_MIME_TYPES`], added as the other program gave them. It is
    /// never decoded in Rappel's process.
    Picture,
    /// Text, or nothing the editor takes as images: the text view's own
    /// paste or drop.
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

/// What the editor says when an image cannot be added, in words.
pub fn add_failed_text(err: &AddImageError) -> String {
    let too_large = || format!("This image is larger than {}", megabytes(IMAGE_MAX_BYTES));
    match err {
        AddImageError::NotLocal => "Only files on this computer can be added".to_owned(),
        AddImageError::NotAFile => "This is not a file".to_owned(),
        AddImageError::TooLarge
        | AddImageError::Store(Error::MediaRefused(MediaProblem::TooLarge { .. })) => too_large(),
        AddImageError::Store(Error::MediaRefused(
            MediaProblem::NotMedia | MediaProblem::NotImage,
        )) => "This file is not a PNG, JPEG, WebP or GIF image".to_owned(),
        AddImageError::Store(Error::MediaRefused(MediaProblem::Empty)) => {
            "This file is empty".to_owned()
        }
        AddImageError::Unreadable(_) | AddImageError::Store(Error::MediaRead(_)) => {
            "The file could not be read".to_owned()
        }
        AddImageError::Store(other) => format!("The image could not be added: {other}"),
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

#[cfg(test)]
mod tests {
    use std::io;

    use rappel_core::media::MediaKind;

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
        let refused = |p| add_failed_text(&AddImageError::Store(Error::MediaRefused(p)));
        assert_eq!(
            refused(MediaProblem::NotMedia),
            "This file is not a PNG, JPEG, WebP or GIF image"
        );
        assert_eq!(
            refused(MediaProblem::NotImage),
            refused(MediaProblem::NotMedia)
        );
        assert_eq!(
            refused(MediaProblem::TooLarge {
                kind: MediaKind::Png,
                limit: IMAGE_MAX_BYTES
            }),
            "This image is larger than 10 MB"
        );
        assert_eq!(refused(MediaProblem::Empty), "This file is empty");
        assert_eq!(
            add_failed_text(&AddImageError::Store(Error::MediaRead(io::Error::other(
                "gone"
            )))),
            "The file could not be read"
        );
        assert_eq!(
            add_failed_text(&AddImageError::Unreadable(io::Error::other("gone"))),
            "The file could not be read"
        );
        assert_eq!(
            add_failed_text(&AddImageError::NotLocal),
            "Only files on this computer can be added"
        );
        assert_eq!(
            add_failed_text(&AddImageError::NotAFile),
            "This is not a file"
        );
        assert_eq!(
            add_failed_text(&AddImageError::TooLarge),
            "This image is larger than 10 MB"
        );
        assert!(
            add_failed_text(&AddImageError::Store(Error::NoMediaDir))
                .starts_with("The image could not be added: ")
        );
    }

    #[test]
    fn the_buttons_name_their_side() {
        assert_eq!(insert_label("Front"), "Insert Image in Front");
        assert_eq!(IMAGE_MIME_TYPES.len(), 4);
        assert!(!IMAGE_MIME_TYPES.iter().any(|m| m.contains("svg")));
    }
}
