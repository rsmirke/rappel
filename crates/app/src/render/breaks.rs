//! Break opportunities in long runs of text. Pure, so it is tested without
//! a display.
//!
//! A wrapping `GtkLabel`'s first measure grows with the square of its
//! longest run without a break opportunity: about 6 µs per byte plus 38 ns
//! per squared character of each run, measured in a release build (one
//! 2 KiB run 0.2 s, 8 KiB 2.8 s, 32 KiB 40 s; ADR 0025). The time is in the
//! label's measure, not in a plain Pango layout. A pasted hash, base64 blob
//! or long URL would freeze the window. So a run longer than [`MAX_RUN`]
//! characters gets a zero-width space, which draws nothing and only lets
//! the line break there. The card itself is never changed: this is only
//! the text given to the labels, which are not selectable (if code blocks
//! ever become copyable, take the breaks out of what is copied).
//!
//! A break goes only between grapheme clusters, never inside one: not
//! between a letter and its combining mark, inside a Thai, Khmer or
//! Devanagari syllable, inside an emoji sequence or between a flag's two
//! letters. Text written without spaces (Thai, Chinese) can therefore also
//! break at these points, at most once per [`MAX_RUN`] characters.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;

/// The longest run of characters without a break opportunity that a label
/// is given (it can be a cluster longer, see [`Runs`]). A word this long is
/// rare (most URLs are shorter), and one this long already breaks anywhere
/// when it does not fit the line.
pub const MAX_RUN: usize = 64;

/// What is inserted: U+200B ZERO WIDTH SPACE, a break opportunity that
/// draws nothing.
pub const BREAK: char = '\u{200B}';

/// Whether `c` lets a line break after it: white space other than the
/// no-break spaces, or [`BREAK`]. U+00A0, U+2007 and U+202F are white
/// space but join their neighbours (line break class GL), so a run of
/// `x\u{A0}x\u{A0}…` is one run. Other break opportunities (after a
/// hyphen, between CJK characters) are not counted; a break inserted there
/// changes nothing.
fn ends_run(c: char) -> bool {
    match c {
        '\u{A0}' | '\u{2007}' | '\u{202F}' => false,
        BREAK => true,
        c => c.is_whitespace(),
    }
}

/// How much of the last cluster [`Runs`] keeps, in bytes: enough for every
/// rule that looks back (an emoji ZWJ sequence, a flag's pair, an Indic
/// conjunct). A longer cluster (hundreds of combining marks, one per piece
/// of markup) is cut to its end, so each piece costs the same; at worst, a
/// break could then split such a cluster, which is only drawn worse.
const LAST_CLUSTER_BYTES: usize = 128;

/// The state of one label's text while its pieces are appended.
#[derive(Debug, Default)]
pub struct Runs {
    /// How many characters the current run has.
    run: usize,
    /// The end of the text so far: its last grapheme cluster (or the last
    /// [`LAST_CLUSTER_BYTES`] of it), possibly built from several pieces.
    /// The next piece is segmented after it, so a cluster continued in its
    /// own `**…**` is seen whole.
    last: String,
}

impl Runs {
    /// Appends `text` to `out`, with a [`BREAK`] before the first cluster
    /// boundary after each [`MAX_RUN`] characters that have none. A run
    /// split by markup (`x**x**x…`) is counted whole.
    pub fn push(&mut self, out: &mut String, text: &str) {
        let offset = self.last.len();
        let joined = format!("{}{text}", self.last);
        for (start, cluster) in joined.grapheme_indices(true) {
            let end = start + cluster.len();
            if end <= offset {
                // Wholly before `text`: already appended.
                continue;
            }
            // A cluster that starts in the previous piece goes on here.
            let continues = start < offset;
            let own = &joined[start.max(offset)..end];
            if own.chars().all(ends_run) && !continues {
                self.run = 0;
            } else {
                if self.run >= MAX_RUN && !continues {
                    out.push(BREAK);
                    self.run = 0;
                }
                self.run += own.chars().count();
            }
            out.push_str(own);
            self.last.clear();
            self.last.push_str(cluster);
        }
        if self.last.len() > LAST_CLUSTER_BYTES {
            let mut cut = self.last.len() - LAST_CLUSTER_BYTES;
            while !self.last.is_char_boundary(cut) {
                cut += 1;
            }
            self.last.drain(..cut);
        }
    }
}

/// `text` with breaks in its long runs (see [`Runs::push`]), for a label of
/// plain text. Borrowed when there is none.
pub fn breakable(text: &str) -> Cow<'_, str> {
    let mut run = 0;
    let long = text.chars().any(|c| {
        run = if ends_run(c) { 0 } else { run + 1 };
        run > MAX_RUN
    });
    if !long {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + text.len() / MAX_RUN * BREAK.len_utf8());
    Runs::default().push(&mut out, text);
    if out.len() == text.len() {
        // One long cluster: nowhere to break.
        return Cow::Borrowed(text);
    }
    Cow::Owned(out)
}

/// `text` without any [`BREAK`]: what screen readers get (ADR 0025 row 3,
/// ADR 0051). A zero-width space the card had itself goes too; it is not
/// read anyway.
pub fn without_breaks(text: &str) -> String {
    text.chars().filter(|&c| c != BREAK).collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn screen_readers_get_the_text_without_the_breaks() {
        let long = format!("{} end", "0123456789abcdef".repeat(10));
        let shown = breakable(&long);
        assert!(shown.contains(BREAK));
        assert_eq!(without_breaks(&shown), long);
        assert_eq!(without_breaks("no breaks"), "no breaks");
    }

    /// The longest run without a break opportunity, in characters.
    fn longest_run(text: &str) -> usize {
        text.split(ends_run)
            .map(|run| run.chars().count())
            .max()
            .unwrap_or(0)
    }

    /// The longest cluster in `text`, in characters.
    fn longest_cluster(text: &str) -> usize {
        text.graphemes(true)
            .map(|g| g.chars().count())
            .max()
            .unwrap_or(0)
    }

    /// Whether every cluster of `broken`, other than the breaks, is a
    /// cluster of `text`: nothing was split.
    fn clusters_kept(text: &str, broken: &str) -> bool {
        let original: Vec<&str> = text.graphemes(true).collect();
        let kept: Vec<&str> = broken
            .graphemes(true)
            .filter(|g| *g != "\u{200B}")
            .collect();
        // A break the text had itself is one cluster in both.
        let original: Vec<&str> = original.into_iter().filter(|g| *g != "\u{200B}").collect();
        original == kept
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        let text = "A sentence with words, and https://example.org/a/short/url.";
        assert!(matches!(breakable(text), Cow::Borrowed(t) if t == text));
        let just_fits = "x".repeat(MAX_RUN);
        assert!(matches!(breakable(&just_fits), Cow::Borrowed(_)));
        assert!(matches!(breakable(""), Cow::Borrowed("")));
    }

    #[test]
    fn a_long_run_gets_a_break_every_max_run_characters() {
        let text = "x".repeat(MAX_RUN * 3 + 1);
        let broken = breakable(&text);
        assert_eq!(broken.matches(BREAK).count(), 3);
        assert_eq!(longest_run(&broken), MAX_RUN);
        assert_eq!(broken.replace(BREAK, ""), text);
    }

    #[test]
    fn white_space_ends_a_run_but_no_break_spaces_do_not() {
        let half = "y".repeat(MAX_RUN / 2 + 1);
        for space in [" ", "\t", "\n", "\u{3000}"] {
            let text = format!("{half}{space}{half}");
            assert!(matches!(breakable(&text), Cow::Borrowed(_)), "{space:?}");
        }
        for space in ["\u{A0}", "\u{2007}", "\u{202F}"] {
            let text = format!("x{space}").repeat(MAX_RUN);
            assert!(breakable(&text).contains(BREAK), "{space:?}");
        }
    }

    #[test]
    fn a_run_split_into_pieces_is_counted_whole() {
        let mut out = String::new();
        let mut runs = Runs::default();
        for _ in 0..MAX_RUN * 2 {
            runs.push(&mut out, "x");
        }
        assert_eq!(out.matches(BREAK).count(), 1);
        assert_eq!(longest_run(&out), MAX_RUN);
    }

    #[test]
    fn clusters_are_never_split() {
        for unit in [
            "e\u{301}",                   // e + combining acute
            "\u{E01}\u{E35}\u{E48}",      // Thai syllable with vowel and tone
            "\u{1780}\u{17D2}\u{1780}",   // Khmer consonant cluster
            "\u{915}\u{94D}\u{937}",      // Devanagari conjunct
            "\u{1F469}\u{200D}\u{1F467}", // emoji ZWJ sequence
            "\u{1F44D}\u{1F3FD}",         // emoji with skin tone
            "\u{1F1EB}\u{1F1F7}",         // a flag
            "\u{2764}\u{FE0F}",           // variation selector
        ] {
            let text = unit.repeat(MAX_RUN * 2);
            let broken = breakable(&text);
            assert!(broken.contains(BREAK), "{unit:?}");
            assert!(clusters_kept(&text, &broken), "{unit:?} split");
            // A cluster continued in its own piece of markup is not split
            // either.
            let mut out = String::new();
            let mut runs = Runs::default();
            for c in text.chars() {
                runs.push(&mut out, &c.to_string());
            }
            assert!(clusters_kept(&text, &out), "{unit:?} split across pieces");
        }
    }

    #[test]
    fn a_cluster_continued_by_a_longer_piece_is_not_split() {
        let pieces: [&[&str]; 4] = [
            &["\u{1F469}", "\u{200D}\u{1F467}x"],          // ZWJ sequence
            &["\u{1F1EB}", "\u{1F1F7}\u{1F1EB}\u{1F1F7}"], // flags
            &["\u{915}", "\u{94D}\u{937}x"],               // Devanagari conjunct
            &["e", "\u{301}\u{302}x"],                     // combining marks
        ];
        for split in pieces {
            let mut out = String::new();
            let mut runs = Runs::default();
            runs.push(&mut out, &"x".repeat(MAX_RUN - 1));
            for piece in split {
                runs.push(&mut out, piece);
            }
            let text = format!("{}{}", "x".repeat(MAX_RUN - 1), split.concat());
            assert!(clusters_kept(&text, &out), "{split:?} split: {out:?}");
            assert_eq!(out.replace(BREAK, ""), text);
        }
    }

    #[test]
    fn a_cluster_built_from_many_pieces_costs_little() {
        // About 75 ms of CPU time in a debug build; 3 s before `last` was
        // capped.
        let bound = std::time::Duration::from_millis(500);
        let (out, took, verdict) = crate::testing::within(
            std::time::Duration::from_secs(5),
            "a cluster of 8,000 pieces",
            move || {
                let (took, out) = crate::testing::cpu_time_of(|| {
                    let mut out = String::new();
                    let mut runs = Runs::default();
                    runs.push(&mut out, "e");
                    for _ in 0..8_000 {
                        runs.push(&mut out, "\u{301}");
                    }
                    out
                });
                // Checked on this thread, which did the work (ADR 0049).
                (out, took, crate::testing::cpu_within(took, bound))
            },
        );
        crate::testing::print_timing("a cluster of 8,000 pieces", took);
        assert_eq!(out.chars().count(), 8_001);
        if let Err(message) = verdict {
            panic!("a cluster of 8,000 pieces: {message}");
        }
    }

    proptest! {
        /// Whatever the text, no run is longer than the limit plus one
        /// cluster, no cluster is split, and taking the breaks out gives
        /// the text back.
        #[test]
        fn runs_are_bounded_and_nothing_else_changes(
            text in "(\\PC|[ \t\n\u{A0}\u{200B}\u{200D}\u{301}\u{FE0F}]|x{1,200}|é{1,100}|👩‍👧{1,50}|\u{E01}\u{E35}{1,80}){0,40}",
        ) {
            let broken = breakable(&text);
            prop_assert!(longest_run(&broken) < MAX_RUN + longest_cluster(&text).max(1));
            prop_assert!(clusters_kept(&text, &broken));
            let mut back = broken.into_owned();
            back.retain(|c| c != BREAK);
            let mut original = text.clone();
            original.retain(|c| c != BREAK);
            prop_assert_eq!(back, original);
        }
    }
}
