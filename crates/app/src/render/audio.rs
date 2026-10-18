//! The decisions about card audio that need no GTK (ADR 0056): the
//! control's texts and accessible names, its states, the elapsed time,
//! and the per-side budget. Pure and unit-tested.
//!
//! Only a link whose target is a `media:` reference to an audio kind
//! (`Inline::Audio`) is ever played, and only from the media folder, by
//! the path the core builds from its checked hash; nothing here opens a
//! path or a URI from card text.

use rappel_core::content::{Block, Document, Inline, plain_text};

/// At most this many audio controls per side; later links are
/// placeholders, never read. A card with more is not a flashcard.
pub const MAX_AUDIO_PER_SIDE: usize = 20;

/// The words of an audio link, on one line: what its button shows and
/// what screen readers read. Empty for `[](media:…)`.
pub fn link_text(content: &[Inline]) -> String {
    let doc = Document {
        blocks: vec![Block::Paragraph(content.to_vec())],
    };
    plain_text(&doc)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the button shows: the link's words, or "Audio" without any.
pub fn button_text(text: &str) -> &str {
    if text.is_empty() { "Audio" } else { text }
}

/// Where an audio control is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not playing: never played, paused, or played to its end. The file
    /// may still be looked for; until it is known missing, it can be
    /// pressed (the play reads it with the same checks).
    Ready,
    /// Its file is being read for the play.
    Loading,
    Playing,
    /// No file, or a file that is not what its name says: disabled.
    Unavailable,
    /// Past [`MAX_AUDIO_PER_SIDE`]: disabled, never read.
    TooMany,
    /// `GStreamer` could not play it (damaged, no decoder, no sound device,
    /// or too slow to start). It can be pressed again to try again.
    Failed,
}

impl State {
    /// Whether the button can be pressed.
    pub fn enabled(self) -> bool {
        !matches!(self, Self::Unavailable | Self::TooMany)
    }

    /// Whether pressing it would pause.
    pub fn playing(self) -> bool {
        matches!(self, Self::Playing | Self::Loading)
    }

    /// The icon of the button.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Ready => "media-playback-start-symbolic",
            Self::Loading | Self::Playing => "media-playback-pause-symbolic",
            Self::Unavailable | Self::TooMany => "audio-x-generic-symbolic",
            Self::Failed => "dialog-warning-symbolic",
        }
    }

    /// The button's tooltip.
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Ready => "Play",
            Self::Loading | Self::Playing => "Pause",
            Self::Unavailable | Self::TooMany => "Audio not available",
            Self::Failed => "Audio could not be played",
        }
    }

    /// The dimmed words beside the button, if the state has any (the time
    /// while playing is [`time_text`]).
    pub fn note(self) -> Option<&'static str> {
        match self {
            Self::Ready | Self::Loading | Self::Playing => None,
            Self::Unavailable => Some("Audio not available"),
            Self::TooMany => Some("Audio not available: too many on this side"),
            Self::Failed => Some("Could not be played"),
        }
    }
}

/// The accessible label of the button: what pressing it does, and the
/// link's words ("Play audio: Bonjour", or "Play audio" without words).
pub fn accessible_label(text: &str, state: State) -> String {
    let action = if state.playing() {
        "Pause audio"
    } else {
        "Play audio"
    };
    if text.is_empty() {
        action.to_owned()
    } else {
        format!("{action}: {text}")
    }
}

/// "0:03 / 0:12": the time played and the length, from GTK's microseconds
/// (`GtkMediaStream:timestamp` and `:duration`). Without a known length,
/// only the time played. Hours show when the length has any.
pub fn time_text(elapsed_us: i64, duration_us: i64) -> String {
    let long = duration_us >= 3_600_000_000;
    let elapsed = clock(elapsed_us, long);
    if duration_us > 0 {
        format!("{elapsed} / {}", clock(duration_us, long))
    } else {
        elapsed
    }
}

/// `us` microseconds as "m:ss", or "h:mm:ss" when `hours`.
fn clock(us: i64, hours: bool) -> String {
    let seconds = us.max(0) / 1_000_000;
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{}:{s:02}", seconds / 60)
    }
}

/// The per-side count of audio controls, in document order, like
/// `images::ImageBudget`.
#[derive(Debug, Default)]
pub struct AudioBudget {
    used: usize,
}

impl AudioBudget {
    /// Whether the next audio link may be a working control; counts it.
    pub fn take(&mut self) -> bool {
        self.used += 1;
        self.used <= MAX_AUDIO_PER_SIDE
    }
}

#[cfg(test)]
mod tests {
    use rappel_core::content::parse;

    use super::*;

    fn audio_content(md: &str) -> Vec<Inline> {
        match parse(md).blocks.into_iter().next() {
            Some(Block::Paragraph(inlines)) => match inlines.into_iter().next() {
                Some(Inline::Audio { content, .. }) => content,
                other => panic!("not audio: {other:?}"),
            },
            other => panic!("not a paragraph: {other:?}"),
        }
    }

    #[test]
    fn the_link_s_words_name_the_control() {
        let hash = "ab".repeat(32);
        let content = audio_content(&format!("[Say *bonjour*\n  please](media:{hash}.ogg)"));
        let text = link_text(&content);
        assert_eq!(text, "Say bonjour please");
        assert_eq!(button_text(&text), "Say bonjour please");
        assert_eq!(
            accessible_label(&text, State::Ready),
            "Play audio: Say bonjour please"
        );
        assert_eq!(
            accessible_label(&text, State::Playing),
            "Pause audio: Say bonjour please"
        );
        let empty = link_text(&audio_content(&format!("[](media:{hash}.wav)")));
        assert_eq!(empty, "");
        assert_eq!(button_text(&empty), "Audio");
        assert_eq!(accessible_label(&empty, State::Ready), "Play audio");
        assert_eq!(accessible_label(&empty, State::Loading), "Pause audio");
        assert_eq!(accessible_label(&empty, State::Unavailable), "Play audio");
    }

    #[test]
    fn each_state_says_what_it_is() {
        use State::{Failed, Loading, Playing, Ready, TooMany, Unavailable};
        for state in [Ready, Loading, Playing, Failed] {
            assert!(state.enabled(), "{state:?}");
        }
        assert!(!Unavailable.enabled());
        assert!(!TooMany.enabled());
        assert_eq!(Ready.tooltip(), "Play");
        assert_eq!(Playing.tooltip(), "Pause");
        assert_eq!(Ready.icon(), "media-playback-start-symbolic");
        assert_eq!(Playing.icon(), "media-playback-pause-symbolic");
        assert_eq!(Unavailable.note(), Some("Audio not available"));
        assert_eq!(Failed.note(), Some("Could not be played"));
        assert_eq!(Failed.tooltip(), "Audio could not be played");
        assert!(TooMany.note().unwrap().starts_with("Audio not available"));
        assert_eq!(Ready.note(), None);
        assert!(Playing.playing() && Loading.playing() && !Ready.playing());
    }

    #[test]
    fn times_read_as_a_clock() {
        assert_eq!(time_text(0, 12_000_000), "0:00 / 0:12");
        assert_eq!(time_text(3_400_000, 12_000_000), "0:03 / 0:12");
        assert_eq!(time_text(61_000_000, 125_000_000), "1:01 / 2:05");
        assert_eq!(time_text(5_000_000, 0), "0:05");
        assert_eq!(time_text(-1, -1), "0:00");
        assert_eq!(time_text(61_000_000, 3_725_000_000), "0:01:01 / 1:02:05");
    }

    #[test]
    fn a_side_has_at_most_its_share_of_audio() {
        let mut budget = AudioBudget::default();
        let taken = (0..MAX_AUDIO_PER_SIDE + 2)
            .filter(|_| budget.take())
            .count();
        assert_eq!(taken, MAX_AUDIO_PER_SIDE);
    }
}
