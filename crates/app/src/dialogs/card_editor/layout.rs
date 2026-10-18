//! Which of the card editor's panes are shown, decided without GTK (ADR
//! 0043). Wide, the text and the preview are side by side. Narrow (the
//! dialog's breakpoint), one of them fills the dialog, and the Edit /
//! Preview toggle in the bottom bar picks which.

/// What the narrow layout shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Edit,
    Preview,
}

impl Mode {
    /// The `AdwToggle` name of each mode, in `data/card_editor.blp`.
    pub const EDIT: &'static str = "edit";
    pub const PREVIEW: &'static str = "preview";

    /// The mode of the toggle named `name`; Edit for any other.
    pub fn from_name(name: Option<&str>) -> Self {
        if name == Some(Self::PREVIEW) {
            Self::Preview
        } else {
            Self::Edit
        }
    }
}

/// The panes on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panes {
    /// The text fields.
    pub edit: bool,
    /// The preview, which is rendered only while shown.
    pub preview: bool,
    /// The line between them.
    pub separator: bool,
}

/// The panes for a `narrow` dialog or a wide one, in `mode`. Wide shows
/// both whatever the mode, so a dialog that grows wide never hides the text.
pub fn panes(narrow: bool, mode: Mode) -> Panes {
    if narrow {
        Panes {
            edit: mode == Mode::Edit,
            preview: mode == Mode::Preview,
            separator: false,
        }
    } else {
        Panes {
            edit: true,
            preview: true,
            separator: true,
        }
    }
}

/// Where the keyboard focus is when the panes are about to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// In the text pane (Front, Back or the tags).
    Edit,
    /// On the Edit / Preview toggle.
    Toggle,
    /// Nowhere a key would reach: no focus, or on a widget that is hidden
    /// or cannot take keys (GTK can leave it on a container when the
    /// focused widget is hidden).
    Lost,
    /// Anywhere else (a header button): left alone.
    Other,
}

/// Where to move the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The front's text.
    Front,
    /// The Edit / Preview toggle.
    Toggle,
}

/// Where the focus goes after the panes become `panes`, from `place`, with
/// the toggle's bar shown or not (`bar`, the breakpoint's setter). The focus
/// never stays where keys would go nowhere: leaving the text for the
/// preview puts it on the toggle; a toggle that goes away (the dialog grew
/// wide) gives it to the front; lost, it goes to the front if the text
/// shows, else to the toggle. `None` keeps it where it is.
pub fn focus_after(place: Place, panes: Panes, bar: bool) -> Option<Target> {
    let fallback = if panes.edit {
        Some(Target::Front)
    } else {
        bar.then_some(Target::Toggle)
    };
    match place {
        Place::Edit if !panes.edit => fallback,
        Place::Toggle if !bar => fallback,
        Place::Lost => fallback,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_shows_both_side_by_side() {
        for mode in [Mode::Edit, Mode::Preview] {
            assert_eq!(
                panes(false, mode),
                Panes {
                    edit: true,
                    preview: true,
                    separator: true
                }
            );
        }
    }

    #[test]
    fn narrow_shows_the_one_the_toggle_picks() {
        let edit = panes(true, Mode::Edit);
        assert!(edit.edit && !edit.preview && !edit.separator);
        let preview = panes(true, Mode::Preview);
        assert!(!preview.edit && preview.preview && !preview.separator);
    }

    #[test]
    fn the_focus_never_stays_where_keys_go_nowhere() {
        let wide = panes(false, Mode::Edit);
        let edit = panes(true, Mode::Edit);
        let preview = panes(true, Mode::Preview);
        // Text to preview: the toggle.
        assert_eq!(
            focus_after(Place::Edit, preview, true),
            Some(Target::Toggle)
        );
        // Still in the text, or on a header button: left alone.
        assert_eq!(focus_after(Place::Edit, edit, true), None);
        assert_eq!(focus_after(Place::Edit, wide, false), None);
        assert_eq!(focus_after(Place::Other, preview, true), None);
        // The toggle stays while narrow, and goes to the front when wide.
        assert_eq!(focus_after(Place::Toggle, preview, true), None);
        assert_eq!(focus_after(Place::Toggle, wide, false), Some(Target::Front));
        // Lost: the front if it shows, else the toggle.
        assert_eq!(focus_after(Place::Lost, wide, false), Some(Target::Front));
        assert_eq!(focus_after(Place::Lost, edit, true), Some(Target::Front));
        assert_eq!(
            focus_after(Place::Lost, preview, true),
            Some(Target::Toggle)
        );
    }

    #[test]
    fn modes_are_named_as_the_toggles() {
        assert_eq!(Mode::from_name(Some("preview")), Mode::Preview);
        assert_eq!(Mode::from_name(Some("edit")), Mode::Edit);
        assert_eq!(Mode::from_name(None), Mode::Edit);
    }
}
