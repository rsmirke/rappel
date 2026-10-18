//! What a card row of the deck page says to a screen reader. Pure, so it
//! is unit-tested (ADR 0042).

use crate::widgets::tag_pills::text::spoken;

/// The front as a screen reader names the card, in the row's label and its
/// menu button's ("Actions for …"): its preview, or "empty front" when it
/// has no text (e.g. a side that is only an empty formula).
pub fn spoken_front(front: &str) -> &str {
    if front.is_empty() {
        "empty front"
    } else {
        front
    }
}

/// The accessible label of a card row: the front, the back, every tag
/// (the ones folded into "+N" too) and whether the card is suspended, so a
/// screen reader says what the row shows: "2 + 2, back: 4, tags: maths,
/// easy, suspended". The previews are already one plain line each.
pub fn row_label(front: &str, back: &str, tags: &[String], suspended: bool) -> String {
    let mut parts = vec![spoken_front(front).to_owned()];
    if !back.is_empty() {
        parts.push(format!("back: {back}"));
    }
    parts.extend(spoken(tags));
    if suspended {
        parts.push("suspended".to_owned());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn a_row_is_read_as_it_shows() {
        assert_eq!(row_label("2 + 2", "", &[], false), "2 + 2");
        assert_eq!(row_label("2 + 2", "4", &[], false), "2 + 2, back: 4");
        assert_eq!(
            row_label("2 + 2", "4", &names(&["maths", "easy"]), false),
            "2 + 2, back: 4, tags: maths, easy"
        );
        assert_eq!(
            row_label("2 + 2", "", &names(&["maths"]), true),
            "2 + 2, tags: maths, suspended"
        );
        // A front with no text (e.g. only an empty formula) still says
        // something.
        assert_eq!(row_label("", "4", &[], false), "empty front, back: 4");
    }

    #[test]
    fn every_tag_is_read_even_past_the_pills_shown() {
        let tags = names(&["a", "b", "c", "d", "e"]);
        assert_eq!(
            row_label("Front", "", &tags, false),
            "Front, tags: a, b, c, d, e"
        );
    }
}
