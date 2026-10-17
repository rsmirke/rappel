//! What tag pills show and say: the text of a pill, how many pills fit on
//! one line and the "+N" pill for the rest. Pure, so it is unit-tested.

use crate::ui::text::group_digits;

/// The widest a pill gets, in characters; a longer name ends in "…" and
/// has its whole name as the pill's tooltip.
pub const PILL_CHARS: i32 = 16;

/// The most pills a single-line row shows before the "+N" pill: three on
/// a wide row, one on a narrow one (a phone), so the back of the card keeps
/// room beside them.
pub const ROW_PILLS: u32 = 3;
pub const COMPACT_ROW_PILLS: u32 = 1;

/// Whether `text` (a tag name on one line, `ui::text::one_line`) is cut to [`PILL_CHARS`] in its pill,
/// so the pill needs its whole name as a tooltip. Counted in characters,
/// like `max-width-chars`; a name of wide letters may still be cut a
/// little earlier, and then it simply has no tooltip.
pub fn is_cut(text: &str) -> bool {
    text.chars().count() > usize::try_from(PILL_CHARS).unwrap_or(usize::MAX)
}

/// How many of `total` tags get a pill, and how many are left for the
/// "+N" pill. `max` is `None` where the pills wrap (all are shown).
pub fn split(total: usize, max: Option<u32>) -> (usize, usize) {
    let shown = max.map_or(total, |max| {
        total.min(usize::try_from(max).unwrap_or(usize::MAX))
    });
    (shown, total - shown)
}

/// The text of the pill that stands for `hidden` more tags: "+2".
pub fn overflow_text(hidden: usize) -> String {
    format!("+{}", group_digits(hidden))
}

/// The tooltip of the "+N" pill: the tags it stands for, "history, physics".
pub fn overflow_tooltip(hidden: &[String]) -> String {
    hidden.join(", ")
}

/// The tags in words for a row's accessible label: "tags: maths, easy", or
/// `None` without tags. Every tag is named, the hidden ones too.
pub fn spoken(names: &[String]) -> Option<String> {
    if names.is_empty() {
        None
    } else {
        Some(format!("tags: {}", names.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn a_long_name_is_cut_and_gets_a_tooltip() {
        assert!(!is_cut("sixteen-letters!"));
        assert!(is_cut("seventeen-letters"));
        // Characters, not bytes.
        assert!(!is_cut("économie général"));
        assert!(is_cut("économie générale"));
    }

    #[test]
    fn a_row_shows_a_few_pills_and_counts_the_rest() {
        assert_eq!(split(0, Some(ROW_PILLS)), (0, 0));
        assert_eq!(split(2, Some(ROW_PILLS)), (2, 0));
        assert_eq!(split(3, Some(ROW_PILLS)), (3, 0));
        assert_eq!(split(5, Some(ROW_PILLS)), (3, 2));
        assert_eq!(split(5, Some(COMPACT_ROW_PILLS)), (1, 4));
        assert_eq!(split(5, Some(0)), (0, 5));
        // Wrapping pills show every tag.
        assert_eq!(split(40, None), (40, 0));
    }

    #[test]
    fn the_overflow_pill_counts_and_names_the_hidden_tags() {
        assert_eq!(overflow_text(2), "+2");
        assert_eq!(overflow_text(1_200), "+1,200");
        assert_eq!(
            overflow_tooltip(&names(&["history", "physics"])),
            "history, physics"
        );
    }

    #[test]
    fn every_tag_is_spoken() {
        assert_eq!(spoken(&[]), None);
        assert_eq!(
            spoken(&names(&["maths", "easy", "a", "b"])).as_deref(),
            Some("tags: maths, easy, a, b")
        );
    }
}
