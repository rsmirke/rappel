//! The rendering decisions that need no GTK: the style class of a heading,
//! a list item's marker, how many widgets a block costs. Pure and tested.
//! Every side of a card is laid out the same way, whatever its structure.

use rappel_core::content::{Block, Document, Inline};

use super::images::image_count;
use super::math::MAX_FORMULAS;

/// Whether the back is shown after the reveal: not when there is nothing
/// to show, so only the grade buttons appear.
pub fn back_shown(back: &Document) -> bool {
    !back.is_empty()
}

/// The libadwaita style class of a heading of `level` (1 to 6).
pub fn heading_class(level: u8) -> &'static str {
    match level {
        0 | 1 => "title-1",
        2 => "title-2",
        3 => "title-3",
        4 => "title-4",
        _ => "heading",
    }
}

/// The marker of item `index` (from 0) of a list `depth` lists deep (from
/// 0): a bullet that changes with depth, or a number from `start`.
pub fn list_marker(start: Option<u64>, index: usize, depth: usize) -> String {
    match start {
        Some(start) => {
            let index = u64::try_from(index).unwrap_or(u64::MAX);
            format!("{}.", start.saturating_add(index))
        }
        None => ["•", "◦", "▪"][depth % 3].to_owned(),
    }
}

/// The style classes of the table cell at `row` (0 is the header) and
/// `column`, in a table of `rows` rows (the header included) and `columns`
/// columns. Lines go between cells and the grid has its own border, so the
/// last column has no line on its right and the last row none below
/// (`data/style.css`).
pub fn table_cell_classes(
    row: usize,
    column: usize,
    rows: usize,
    columns: usize,
) -> Vec<&'static str> {
    let mut classes = vec!["card-table-cell"];
    if column + 1 < columns {
        classes.push("table-line-end");
    }
    if row + 1 < rows {
        classes.push("table-line-below");
    }
    if row == 0 {
        classes.push("table-header");
    }
    classes
}

/// How many widgets [`super::render`] makes for `block`, to stay within
/// its budget. A formula that is drawn costs two (a scroller and the view,
/// ADR 0023); a block is charged for at most [`MAX_FORMULAS`] of them, the
/// most a side draws.
pub fn widget_cost(block: &Block) -> usize {
    match block {
        Block::Paragraph(inlines) => 1 + formula_cost(inlines) + image_cost(inlines),
        Block::Heading {
            content: inlines, ..
        } => 1 + formula_cost(inlines),
        Block::Rule => 1,
        // A scroller and the view; or a label in a box.
        Block::DisplayMath(_) | Block::CodeBlock { .. } => 2,
        // A grid; per item a marker and a box with the item's blocks.
        Block::List { items, .. } => {
            1 + items
                .iter()
                .map(|item| 2 + item.iter().map(widget_cost).sum::<usize>())
                .sum::<usize>()
        }
        // A box, a separator and a box with the blocks.
        Block::BlockQuote(blocks) => 3 + blocks.iter().map(widget_cost).sum::<usize>(),
        // A scroller, a grid and a widget per cell.
        Block::Table { header, rows, .. } => {
            2 + header
                .iter()
                .chain(rows.iter().flatten())
                .map(|cell| 1 + formula_cost(cell))
                .sum::<usize>()
        }
    }
}

/// The widgets the `media:` images of a paragraph add (ADR 0055): a box
/// for the column, and per image three (its widget, the button and the
/// picture, or the placeholder's line, icon and label). A paragraph also
/// has its text runs, each a label, at most one more than its images,
/// which the 1 of the paragraph and this count's 3 per image cover.
fn image_cost(inlines: &[Inline]) -> usize {
    match image_count(inlines) {
        0 => 0,
        n => 1 + 3 * n,
    }
}

/// The widgets the formulas in `inlines` may add.
fn formula_cost(inlines: &[Inline]) -> usize {
    2 * formulas_in(inlines).min(MAX_FORMULAS)
}

/// How many `$…$` there are in `inlines`, at any depth.
fn formulas_in(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Math { .. } => 1,
            Inline::Emphasis(inner)
            | Inline::Strong(inner)
            | Inline::Strikethrough(inner)
            | Inline::Link { content: inner, .. }
            | Inline::Audio { content: inner, .. } => formulas_in(inner),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use rappel_core::content::parse;

    use super::*;

    #[test]
    fn an_empty_back_is_not_shown() {
        assert!(!back_shown(&parse("")));
        assert!(!back_shown(&parse(" \n\n\t")));
        assert!(back_shown(&parse("x")));
        assert!(back_shown(&parse("---")));
    }

    #[test]
    fn headings_use_title_classes() {
        let classes: Vec<_> = (1..=6).map(heading_class).collect();
        assert_eq!(
            classes,
            [
                "title-1", "title-2", "title-3", "title-4", "heading", "heading"
            ]
        );
    }

    #[test]
    fn widget_costs() {
        let cost = |md| parse(md).blocks.iter().map(widget_cost).sum::<usize>();
        assert_eq!(cost("a\n\nb"), 2);
        assert_eq!(cost("```\nx\n```"), 2);
        assert_eq!(cost("- a\n- b"), 1 + 3 + 3);
        assert_eq!(cost("> a\n>\n> b"), 3 + 2);
        assert_eq!(cost("| a | b |\n|--|--|\n| 1 | 2 |\n| 3 |"), 2 + 2 + 2 + 2);
        // A huge list is costly, so it is shown as plain text.
        assert!(cost(&"- a\n".repeat(10_000)) > 20_000);
    }

    #[test]
    fn a_drawn_formula_costs_two_widgets() {
        let cost = |md| parse(md).blocks.iter().map(widget_cost).sum::<usize>();
        // A display formula: a scroller and the view, where text was one label.
        assert_eq!(cost("$$x$$"), 2);
        // Inline: the paragraph, and two per formula, at any depth.
        assert_eq!(cost("a $x$ b"), 1 + 2);
        assert_eq!(cost("*$x$* [$y$ $z$](u)"), 1 + 2 * 3);
        assert_eq!(cost("# $x$"), 1 + 2);
        assert_eq!(
            cost("| $a$ | b |\n|--|--|\n| $c$ $d$ | e |"),
            2 + 3 + 1 + 5 + 1
        );
        // At most MAX_FORMULAS are charged to a block.
        let many = "$x$ ".repeat(MAX_FORMULAS * 3);
        assert_eq!(cost(&many), 1 + 2 * MAX_FORMULAS);
    }

    #[test]
    fn media_images_cost_their_widgets() {
        let cost = |md: &str| parse(md).blocks.iter().map(widget_cost).sum::<usize>();
        let media = format!("![a](media:{}.png)", "ab".repeat(32));
        assert_eq!(cost(&format!("x {media} y")), 1 + 1 + 3);
        assert_eq!(cost(&format!("{media} {media}")), 1 + 1 + 6);
        // Not drawn: no cost beyond the label.
        assert_eq!(cost("![a](https://e.org/x.png)"), 1);
        assert_eq!(cost(&format!("# {media}")), 1);
        // Many images on many lines stay within the side's widget budget
        // only as far as they fit; the rest is plain text.
        assert!(cost(&format!("{media}\n").repeat(1_000)) > 2_000);
    }

    #[test]
    fn table_cells_get_lines_between_them_and_a_header() {
        assert_eq!(
            table_cell_classes(0, 0, 3, 2),
            [
                "card-table-cell",
                "table-line-end",
                "table-line-below",
                "table-header"
            ]
        );
        assert_eq!(
            table_cell_classes(1, 1, 3, 2),
            ["card-table-cell", "table-line-below"]
        );
        assert_eq!(
            table_cell_classes(2, 0, 3, 2),
            ["card-table-cell", "table-line-end"]
        );
        // A header-only table, one column.
        assert_eq!(
            table_cell_classes(0, 0, 1, 1),
            ["card-table-cell", "table-header"]
        );
    }

    #[test]
    fn list_markers() {
        assert_eq!(list_marker(None, 0, 0), "•");
        assert_eq!(list_marker(None, 5, 1), "◦");
        assert_eq!(list_marker(None, 0, 2), "▪");
        assert_eq!(list_marker(None, 0, 3), "•");
        assert_eq!(list_marker(Some(1), 0, 0), "1.");
        assert_eq!(list_marker(Some(3), 2, 4), "5.");
        assert_eq!(list_marker(Some(u64::MAX), 1, 0), format!("{}.", u64::MAX));
    }
}
