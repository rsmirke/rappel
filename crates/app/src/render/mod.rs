//! A card side's [`Document`] → GTK widgets (ADR 0019). Built from data,
//! so this is the one place widgets are made in Rust and not in Blueprint;
//! only libadwaita style classes are used, no colours.
//!
//! Card content is untrusted: text goes into labels either as plain text
//! (markup off) or as markup from [`markup::inline_markup`], which escapes
//! it. No label is selectable or focusable, so the keyboard focus stays on
//! the review page's card area and its keys keep working. Links cannot be
//! clicked; hovering shows their URL.
//!
//! Math (ADR 0021) is drawn as vectors: inline formulas inside their
//! paragraph's text flow ([`math_paragraph`]), display formulas as centred
//! blocks that scroll sideways when too wide ([`math_view`]). What is over
//! a limit ([`math`]) stays source.

mod breaks;
mod markup;
mod math;
mod math_paragraph;
mod math_view;
mod style;

use gtk::AccessibleRole as Role;
use gtk::pango;
use gtk::prelude::*;
use rappel_core::content::{Align, Block, Cell, Document, Inline, plain_text};

use math::{Budget, Placement};
use math_paragraph::MathParagraph;
use math_view::MathView;

pub use style::back_shown;
use style::{heading_class, list_marker, table_cell_classes, widget_cost};

/// At most this many widgets per side. A hostile card (a 1 MiB list)
/// would otherwise build hundreds of thousands; what does not fit is shown
/// as plain text in one label instead.
const MAX_WIDGETS: usize = 2_000;

/// At most this much of each side is rendered, on the review page and in
/// the editor's preview. Pango lays a paragraph out as one label, and its
/// first layout grows faster than its length: for one paragraph of words,
/// about 40 ms at 8 KiB, 270 ms at 32 KiB, 800 ms at 64 KiB and seconds at
/// 1 MiB (the same in debug and release builds: the time is in Pango). Each
/// later relayout, e.g. a window resize, takes about 10 ms at 32 KiB. Long
/// runs without spaces cost far more; [`breaks`] bounds them. Measured in
/// `the_longest_shown_side_lays_out_in_time` and recorded in ADR 0025.
pub const MAX_SHOWN_BYTES: usize = 32 * 1024;

/// Space between blocks, in pixels.
const BLOCK_SPACING: i32 = 12;

/// The widgets of `doc`, in a vertical box. Both sides of a card look the
/// same: body text, normal weight, aligned to the start. Only the
/// constructs have styles of their own (headings, code, quotes, tables).
pub fn render(doc: &Document) -> gtk::Box {
    let container = blocks_box();
    // A block's cost counts everything inside it, so the budget is spent
    // on top-level blocks only.
    let mut budget = MAX_WIDGETS;
    // Formulas are counted over the whole side, in document order.
    let mut math = Budget::default();
    for (i, block) in doc.blocks.iter().enumerate() {
        let cost = widget_cost(block);
        if cost > budget {
            let rest = Document {
                blocks: doc.blocks[i..].to_vec(),
            };
            container.append(&plain_label(&plain_text(&rest), false));
            break;
        }
        budget -= cost;
        container.append(&block_widget(block, 0, &mut math));
    }
    container
}

/// Replaces what `container` shows with `doc`.
pub fn fill(container: &gtk::Box, doc: &Document) {
    clear(container);
    container.append(&render(doc));
}

/// Removes everything `container` shows.
pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// `blocks` in a vertical box. `lists` is how many lists deep they are.
fn blocks_widget(blocks: &[Block], lists: usize, math: &mut Budget) -> gtk::Box {
    let container = blocks_box();
    for block in blocks {
        container.append(&block_widget(block, lists, math));
    }
    container
}

fn block_widget(block: &Block, lists: usize, math: &mut Budget) -> gtk::Widget {
    match block {
        Block::Paragraph(inlines) => {
            text_widget(inlines, None, Role::Label, true, Alignment::Start, math)
        }
        Block::Heading { level, content } => {
            let heading = text_widget(
                content,
                Some(heading_class(*level)),
                Role::Heading,
                true,
                Alignment::Start,
                math,
            );
            heading.update_property(&[gtk::accessible::Property::Level(i32::from(*level))]);
            heading
        }
        Block::List { start, items } => list(*start, items, lists, math).upcast(),
        Block::CodeBlock { code, .. } => {
            let label = plain_label(code, false);
            label.add_css_class("monospace");
            label.set_margin_top(12);
            label.set_margin_bottom(12);
            label.set_margin_start(12);
            label.set_margin_end(12);
            let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame.add_css_class("card");
            frame.append(&label);
            frame.upcast()
        }
        Block::BlockQuote(blocks) => {
            let quote = gtk::Box::new(gtk::Orientation::Horizontal, BLOCK_SPACING);
            quote.append(&gtk::Separator::new(gtk::Orientation::Vertical));
            let inner = blocks_widget(blocks, lists, math);
            inner.add_css_class("dimmed");
            inner.set_hexpand(true);
            quote.append(&inner);
            quote.upcast()
        }
        Block::Rule => gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
        Block::Table {
            align,
            header,
            rows,
        } => table(align, header, rows, math).upcast(),
        Block::DisplayMath(source) => display_math(source, math),
    }
}

fn list(start: Option<u64>, items: &[Vec<Block>], lists: usize, math: &mut Budget) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(6)
        .build();
    for (i, item) in items.iter().enumerate() {
        let row = i32::try_from(i).unwrap_or(i32::MAX);
        let marker = gtk::Label::builder()
            .label(list_marker(start, i, lists))
            .xalign(1.0)
            .valign(gtk::Align::Start)
            .build();
        let content = blocks_widget(item, lists + 1, math);
        content.set_hexpand(true);
        grid.attach(&marker, 0, row, 1, 1);
        grid.attach(&content, 1, row, 1, 1);
    }
    grid
}

/// A table in a horizontal scroller, so a wide one fits a narrow window.
/// The grid has a border, a line between cells and a shaded header row,
/// from the classes of [`table_cell_classes`] and `data/style.css`.
fn table(
    align: &[Align],
    header: &[Cell],
    rows: &[Vec<Cell>],
    math: &mut Budget,
) -> gtk::ScrolledWindow {
    let grid = gtk::Grid::new();
    grid.add_css_class("card-table");
    // GTK CSS has no `overflow`; this clips the shaded header to the
    // rounded border.
    grid.set_overflow(gtk::Overflow::Hidden);
    let row_count = rows.len() + 1;
    let column_count = header.len();
    let all_rows =
        std::iter::once((header, Some("heading"))).chain(rows.iter().map(|r| (r.as_slice(), None)));
    for (r, (cells, class)) in all_rows.enumerate() {
        let row = i32::try_from(r).unwrap_or(i32::MAX);
        for (c, cell) in cells.iter().enumerate() {
            let column = i32::try_from(c).unwrap_or(i32::MAX);
            // A cell keeps its line (no wrapping); the scroller handles the
            // width.
            let alignment = match align.get(c).copied().unwrap_or(Align::None) {
                Align::None | Align::Left => Alignment::Start,
                Align::Center => Alignment::Centre,
                Align::Right => Alignment::End,
            };
            let widget = text_widget(cell, class, Role::Label, false, alignment, math);
            for class in table_cell_classes(r, c, row_count, column_count) {
                widget.add_css_class(class);
            }
            grid.attach(&widget, column, row, 1, 1);
        }
    }
    gtk::ScrolledWindow::builder()
        // Not focusable (it is by default), so the focus stays on the
        // card area; a wide table scrolls with the pointer only.
        .focusable(false)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&grid)
        .build()
}

fn blocks_box() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, BLOCK_SPACING)
}

/// Where text sits in the space it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alignment {
    Start,
    Centre,
    End,
}

/// The widget for `inlines`: a wrapping label, or, if a formula is drawn in
/// them, a [`MathParagraph`] (which lays the formulas out in the text).
/// `wrap` is off in table cells.
fn text_widget(
    inlines: &[Inline],
    class: Option<&str>,
    role: Role,
    wrap: bool,
    alignment: Alignment,
    math: &mut Budget,
) -> gtk::Widget {
    let markup = markup::inline_markup(inlines, math);
    let widget: gtk::Widget = if markup.formulas.is_empty() {
        let label = wrapping_label(false, role);
        label.set_use_markup(true);
        label.set_label(&markup.markup);
        label.set_wrap(wrap);
        label.set_xalign(match alignment {
            Alignment::Start => 0.0,
            Alignment::Centre => 0.5,
            Alignment::End => 1.0,
        });
        if !markup.links.is_empty() {
            show_link_urls(&label, markup.links);
        }
        label.upcast()
    } else {
        let paragraph = MathParagraph::new(&markup, wrap, role);
        paragraph.set_halign(match alignment {
            Alignment::Start => gtk::Align::Start,
            Alignment::Centre => gtk::Align::Center,
            Alignment::End => gtk::Align::End,
        });
        paragraph.upcast()
    };
    if let Some(class) = class {
        widget.add_css_class(class);
    }
    widget
}

/// A `$$…$$` block: the formula centred in a scroller, or its source.
fn display_math(source: &str, math: &mut Budget) -> gtk::Widget {
    let placement = math.place(source, true);
    match placement {
        Placement::Formula => {
            let view = MathView::new(source, true);
            // Centred when it is narrower than the card; the viewport
            // gives a wider one its full width, and the scroller moves it.
            view.set_halign(gtk::Align::Center);
            math_scroller(&view).upcast()
        }
        Placement::ErrorSource | Placement::Source => {
            let label = plain_label(source, true);
            label.add_css_class("monospace");
            if placement == Placement::ErrorSource {
                label.add_css_class("error");
            }
            label.upcast()
        }
    }
}

/// A horizontal scroller for a formula that may be wider than the space it
/// has. Not focusable (like a table's), so the focus stays on the card area
/// and the keys keep working: it scrolls by pointer and touch only. The
/// `math-scroller` class gives `data/style.css` something to mark that more
/// is there.
fn math_scroller(view: &MathView) -> gtk::ScrolledWindow {
    let scroller = gtk::ScrolledWindow::builder()
        .focusable(false)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .overlay_scrolling(true)
        .propagate_natural_height(true)
        .child(view)
        .build();
    scroller.add_css_class("math-scroller");
    scroller
}

/// The scroller for a formula in a line of text (ADR 0024). It has no
/// scrollbar, so its minimum width is 0 and the box Pango reserves for it is
/// the formula's own width. With a scrollbar policy of `Automatic` (as
/// [`math_scroller`]), `gtk_scrolled_window_measure` (GTK 4.24) takes
/// `MAX (minimum_req, min_scrollbar_width + …)` even while the bar is hidden,
/// which left a gap of about 35 px after a short formula. `External` still
/// scrolls a formula wider than the line: `may_hscroll` is true for it, and
/// the scroll and touch handlers check only that. So a horizontal touchpad
/// swipe, a tilt wheel, Shift+wheel (`scrolled_window_scroll` swaps the
/// axes) and a touch drag scroll it; a plain wheel and a mouse drag do not
/// (the drag and pan gestures are touch-only). A vertical wheel event
/// propagates (`scrolled_in_non_scrollable_dir`), so the page still scrolls
/// over a formula, and the `undershoot` fade is drawn from the adjustment,
/// whatever the policy.
fn inline_math_scroller(view: &MathView) -> gtk::ScrolledWindow {
    let scroller = math_scroller(view);
    scroller.set_hscrollbar_policy(gtk::PolicyType::External);
    scroller
}

/// A wrapping label of plain `text` (markup off: nothing to escape), with
/// breaks in its long runs (`breaks`).
fn plain_label(text: &str, centred: bool) -> gtk::Label {
    let label = wrapping_label(centred, Role::Label);
    label.set_use_markup(false);
    label.set_label(&breaks::breakable(text));
    label
}

/// `role` is construct-only, so it is set here: a heading is announced as
/// one (with its level, set by the caller).
fn wrapping_label(centred: bool, role: Role) -> gtk::Label {
    let label = gtk::Label::builder()
        .accessible_role(role)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .selectable(false)
        .build();
    if centred {
        label.set_justify(gtk::Justification::Center);
        label.set_xalign(0.5);
    } else {
        label.set_xalign(0.0);
    }
    label
}

/// Shows the URL of the link under the pointer as the label's tooltip.
///
/// `query-tooltip` comes when the pointer rests on the widget, with `x`
/// and `y` in the widget's coordinates, and GTK asks again as the pointer
/// moves (gtkwidget.c, `query-tooltip`; gtktooltip.c). In keyboard mode
/// the coordinates are undefined, so no tooltip is offered then.
/// `gtk_label_get_layout_offsets` gives where the label draws its Pango
/// layout, to turn the pointer into a layout position, in Pango units
/// (gtklabel.c).
fn show_link_urls(label: &gtk::Label, links: Vec<(std::ops::Range<usize>, String)>) {
    label.set_has_tooltip(true);
    label.connect_query_tooltip(move |label, x, y, keyboard, tooltip| {
        if keyboard {
            return false;
        }
        let (dx, dy) = label.layout_offsets();
        let (inside, index, _) = label.layout().xy_to_index(
            (x - dx).saturating_mul(pango::SCALE),
            (y - dy).saturating_mul(pango::SCALE),
        );
        let url = usize::try_from(index)
            .ok()
            .filter(|_| inside)
            .and_then(|index| markup::link_at(&links, index));
        match url {
            Some(url) => {
                // Plain text, not markup.
                tooltip.set_text(Some(url));
                true
            }
            None => false,
        }
    });
}

/// The part of a side that is rendered: all of it up to
/// [`MAX_SHOWN_BYTES`]; past that, the start, cut after the last blank
/// line that fits (so whole paragraphs are shown), or at the last character
/// that fits if there is none in the second half of the limit. Only `\n`
/// line ends count (CRLF text is cut at a character). The second value says
/// whether it was cut.
pub fn shown_part(md: &str) -> (&str, bool) {
    if md.len() <= MAX_SHOWN_BYTES {
        return (md, false);
    }
    let mut end = MAX_SHOWN_BYTES;
    while !md.is_char_boundary(end) {
        end -= 1;
    }
    let head = &md[..end];
    // A blank line in the first half would show too little of the side.
    let cut = head
        .rfind("\n\n")
        .map(|blank| blank + 1)
        .filter(|&cut| cut >= MAX_SHOWN_BYTES / 2)
        .unwrap_or(end);
    (&md[..cut], true)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn short_sides_are_shown_whole() {
        let side = "x".repeat(MAX_SHOWN_BYTES);
        assert_eq!(shown_part(&side), (side.as_str(), false));
        assert_eq!(shown_part(""), ("", false));
    }

    #[test]
    fn long_sides_are_cut_after_the_last_paragraph_that_fits() {
        let paragraph = format!("{}\n\n", "word ".repeat(100));
        let side = paragraph.repeat(MAX_SHOWN_BYTES / paragraph.len() + 2);
        let (part, cut) = shown_part(&side);
        assert!(cut);
        assert!(part.len() <= MAX_SHOWN_BYTES);
        assert!(part.ends_with("word \n"), "cut inside a paragraph");
        assert!(side.starts_with(part));
    }

    #[test]
    fn one_long_paragraph_is_cut_on_a_character_boundary() {
        // 3-byte characters do not divide the limit: the cut must step back.
        let side = "€".repeat(MAX_SHOWN_BYTES);
        let (part, cut) = shown_part(&side);
        assert!(cut);
        assert_eq!(part.len(), MAX_SHOWN_BYTES / 3 * 3);
        assert!(part.chars().all(|c| c == '€'));
    }

    #[test]
    fn an_early_blank_line_does_not_empty_the_side() {
        let side = format!("\n\n{}", "x".repeat(MAX_SHOWN_BYTES + 10));
        let (part, cut) = shown_part(&side);
        assert!(cut);
        assert_eq!(part.len(), MAX_SHOWN_BYTES);
    }

    /// Times the first layout (the natural width, then the height at a
    /// width) and a relayout at another width, for the longest side that is
    /// shown: words in one paragraph and in many, a run with no spaces
    /// (40 s without `breaks`), shaped scripts and emoji. At most about
    /// 370 ms and 30 ms here; the bounds leave several times that for slow
    /// machines.
    #[test]
    fn the_longest_shown_side_lays_out_in_time() {
        let paragraph = format!("{}\n\n", "word ".repeat(200));
        let sides = [
            ("one paragraph", "word ".repeat(MAX_SHOWN_BYTES / 5)),
            (
                "paragraphs",
                paragraph.repeat(MAX_SHOWN_BYTES / paragraph.len()),
            ),
            // No space: Pango breaks between characters (`WordChar`).
            ("no spaces", "x".repeat(MAX_SHOWN_BYTES)),
            // Shaped scripts and emoji sequences.
            ("Arabic", "مرحبا بالعالم ".repeat(MAX_SHOWN_BYTES / 26)),
            ("emoji", "👩‍👩‍👧‍👦 ".repeat(MAX_SHOWN_BYTES / 26)),
            ("Devanagari", "नमस्ते दुनिया ".repeat(MAX_SHOWN_BYTES / 38)),
            // Runs that look broken but are not, or that may only break
            // between clusters (`breaks`).
            ("no-break spaces", "x\u{A0}".repeat(MAX_SHOWN_BYTES / 3)),
            ("Thai, no spaces", "สวัสดีชาวโลก".repeat(MAX_SHOWN_BYTES / 36)),
            ("Khmer, no spaces", "ក្កសួស្តី".repeat(MAX_SHOWN_BYTES / 30)),
            ("emoji, no spaces", "👩‍👩‍👧‍👦".repeat(MAX_SHOWN_BYTES / 25)),
            (
                "combining marks",
                "e\u{301}\u{302}\u{303}".repeat(MAX_SHOWN_BYTES / 7),
            ),
        ];
        let times = crate::testing::with_gtk(move || {
            sides.map(|(what, side)| {
                assert!(side.len() <= MAX_SHOWN_BYTES);
                let shown = render(&rappel_core::content::parse(&side));
                // CPU time: Pango lays out on this thread, and a loaded
                // machine must not fail the bound (ADR 0049).
                let (first, ()) = crate::testing::cpu_time_of(|| {
                    // GTK asks for the natural width (no width limit) and
                    // then the height at the width it gives.
                    let _ = shown.measure(gtk::Orientation::Horizontal, -1);
                    let _ = shown.measure(gtk::Orientation::Vertical, 600);
                });
                let (relayout, ()) = crate::testing::cpu_time_of(|| {
                    let _ = shown.measure(gtk::Orientation::Vertical, 300);
                });
                // Checked on this thread, which did the work.
                let verdict = crate::testing::cpu_within(first, Duration::from_secs(3)).and(
                    crate::testing::cpu_within(relayout, Duration::from_millis(200)),
                );
                (what, first, relayout, verdict)
            })
        });
        for (what, first, relayout, verdict) in times {
            crate::testing::print_timing(&format!("first layout, 32 KiB, {what}"), first);
            crate::testing::print_timing(&format!("relayout, 32 KiB, {what}"), relayout);
            if let Err(message) = verdict {
                panic!("{what}: first layout {first:?}, relayout {relayout:?}: {message}");
            }
        }
    }
}
