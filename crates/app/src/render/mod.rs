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
//!
//! Images (ADR 0055): a `media:` image at the top level of a paragraph is
//! drawn as a picture between the paragraph's runs of text
//! ([`images::pieces`]), decoded off the main thread in glycin's sandbox
//! ([`picture`]); any other image stays text and is never read.
//!
//! Audio (ADR 0056): a `media:` audio link at the top level of a paragraph
//! is a play button between the paragraph's runs of text
//! ([`audio_player`]); nothing plays until the user asks, and only the
//! media folder's file, read and checked, is played.

mod audio;
mod audio_player;
mod breaks;
mod images;
mod markup;
mod math;
mod math_paragraph;
mod math_view;
mod media_file;
mod picture;
mod playback;
mod style;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::AccessibleRole as Role;
use gtk::pango;
use gtk::prelude::*;
use rappel_core::content::{Align, Block, Cell, Document, Inline, plain_text};

use audio::AudioBudget;
pub use audio_player::{CardAudio, stop_all as stop_audio};
use images::{Failure, ImageBudget, Piece, PixelBudget};
use math::{Budget, Placement};
use math_paragraph::MathParagraph;
use math_view::MathView;
use picture::CardImage;
#[cfg(test)]
pub(crate) use playback::force as force_playback;

#[cfg(test)]
pub(crate) use picture::full_size_layout;
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

/// Where a side's images come from, and how tall one may be drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Images {
    /// The media folder; `None` shows every image as missing.
    folder: Option<PathBuf>,
    /// The cap on an image's height, in pixels.
    max_height: i32,
}

impl Images {
    /// Images from the media folder `folder`, in a view `view_height`
    /// pixels tall (`images::max_height`).
    pub fn new(folder: Option<PathBuf>, view_height: i32) -> Self {
        Self {
            folder,
            max_height: images::max_height(view_height),
        }
    }
}

/// What the blocks of one side share while they are built.
struct Side<'a> {
    /// Formulas, counted over the whole side in document order.
    math: Budget,
    /// Images, counted the same way.
    images: ImageBudget,
    /// The pixels of the side's decoded images, shared by them.
    pixels: Rc<RefCell<PixelBudget>>,
    /// Audio controls, counted the same way.
    audio: AudioBudget,
    media: &'a Images,
}

/// The widgets of `doc`, in a vertical box, with its images from
/// `media`. Both sides of a card look the same: body text, normal weight,
/// aligned to the start. Only the constructs have styles of their own
/// (headings, code, quotes, tables).
pub fn render(doc: &Document, media: &Images) -> gtk::Box {
    let container = blocks_box();
    // A block's cost counts everything inside it, so the budget is spent
    // on top-level blocks only.
    let mut budget = MAX_WIDGETS;
    let mut side = Side {
        math: Budget::default(),
        images: ImageBudget::default(),
        pixels: Rc::default(),
        audio: AudioBudget::default(),
        media,
    };
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
        container.append(&block_widget(block, 0, &mut side));
    }
    container
}

/// Replaces what `container` shows with `doc`.
pub fn fill(container: &gtk::Box, doc: &Document, media: &Images) {
    clear(container);
    container.append(&render(doc, media));
}

/// Removes everything `container` shows.
pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// `blocks` in a vertical box. `lists` is how many lists deep they are.
fn blocks_widget(blocks: &[Block], lists: usize, side: &mut Side<'_>) -> gtk::Box {
    let container = blocks_box();
    for block in blocks {
        container.append(&block_widget(block, lists, side));
    }
    container
}

fn block_widget(block: &Block, lists: usize, side: &mut Side<'_>) -> gtk::Widget {
    let math = &mut side.math;
    match block {
        Block::Paragraph(inlines) => paragraph(inlines, side),
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
        Block::List { start, items } => list(*start, items, lists, side).upcast(),
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
            let inner = blocks_widget(blocks, lists, side);
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

/// A paragraph: one label (or math paragraph) for its text, or, when it
/// has `media:` images or audio links, its runs of text, its images and
/// its audio controls one under the other (`images::pieces`).
fn paragraph(inlines: &[Inline], side: &mut Side<'_>) -> gtk::Widget {
    let pieces = images::pieces(inlines);
    if let [Piece::Text(run)] = pieces.as_slice() {
        return text_widget(
            run,
            None,
            Role::Label,
            true,
            Alignment::Start,
            &mut side.math,
        );
    }
    let column = gtk::Box::new(gtk::Orientation::Vertical, BLOCK_SPACING / 2);
    for piece in pieces {
        let widget = match piece {
            Piece::Text(run) => text_widget(
                run,
                None,
                Role::Label,
                true,
                Alignment::Start,
                &mut side.math,
            ),
            Piece::Image { alt, media } => image_widget(alt, media, side).upcast(),
            Piece::Audio { content, media } => audio_widget(content, media, side).upcast(),
        };
        column.append(&widget);
    }
    column.upcast()
}

/// One `media:` image: drawn, or past the side's share of images
/// ([`images::MAX_IMAGES_PER_SIDE`]) a placeholder that reads nothing.
fn image_widget(alt: &str, media: rappel_core::media::MediaRef, side: &mut Side<'_>) -> CardImage {
    if side.images.take() {
        CardImage::new(
            side.media.folder.as_deref(),
            media,
            alt,
            side.media.max_height,
            Rc::clone(&side.pixels),
        )
    } else {
        CardImage::not_read(alt, Failure::TooMany)
    }
}

/// One `media:` audio link: a play button, or past the side's share
/// ([`audio::MAX_AUDIO_PER_SIDE`]) a disabled one that reads nothing.
fn audio_widget(
    content: &[Inline],
    media: rappel_core::media::MediaRef,
    side: &mut Side<'_>,
) -> CardAudio {
    let text = audio::link_text(content);
    if side.audio.take() {
        CardAudio::new(side.media.folder.as_deref(), media, &text)
    } else {
        CardAudio::not_read(&text)
    }
}

/// Every audio control under `widget`, in document order: what R on the
/// review page chooses from.
pub fn audio_controls(widget: &impl IsA<gtk::Widget>) -> Vec<CardAudio> {
    let mut out = Vec::new();
    let mut child = widget.as_ref().first_child();
    while let Some(c) = child {
        if let Ok(audio) = c.clone().downcast::<CardAudio>() {
            out.push(audio);
        } else {
            out.extend(audio_controls(&c));
        }
        child = c.next_sibling();
    }
    out
}

fn list(start: Option<u64>, items: &[Vec<Block>], lists: usize, side: &mut Side<'_>) -> gtk::Grid {
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
        let content = blocks_widget(item, lists + 1, side);
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
        name_without_breaks(&label);
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
    name_without_breaks(&label);
    label
}

/// Gives `label` an accessible name without the breaks its text has for
/// layout (`breaks`), as the math paragraphs do (ADR 0025 row 3, ADR 0051).
/// GTK names a label by its text: each new text sets
/// `GTK_ACCESSIBLE_PROPERTY_LABEL` to it, for every role that may be named
/// (`gtk_label_set_text_internal`, gtk/gtklabel.c, GTK 4.24), so this runs
/// after the text is set. The labels here are Label or Heading roles, both
/// named from content. Returns the name set, if the text had breaks.
///
/// The AT-SPI Text interface of a label still gives its text with the
/// breaks (`gtk_label_accessible_text_get_contents` returns
/// `gtk_label_get_text`); its offsets and extents must match what is drawn.
fn name_without_breaks(label: &gtk::Label) -> Option<String> {
    let text = label.text();
    if !text.contains(breaks::BREAK) {
        return None;
    }
    let name = breaks::without_breaks(&text);
    label.update_property(&[gtk::accessible::Property::Label(&name)]);
    Some(name)
}

/// `role` is construct-only, so it is set here: a heading is announced as
/// one (with its level, set by the caller). Every `set_label` on such a
/// label must be followed by [`name_without_breaks`]: GTK names the label
/// again from each new text, breaks included.
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

    /// Every label under `widget`, in order.
    fn labels_under(widget: &gtk::Widget) -> Vec<gtk::Label> {
        let mut out = Vec::new();
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Ok(label) = c.clone().downcast::<gtk::Label>() {
                out.push(label);
            }
            out.extend(labels_under(&c));
            child = c.next_sibling();
        }
        out
    }

    #[test]
    fn labels_with_breaks_are_named_without_them() {
        crate::testing::with_widgets(|| {
            let run = "0123456789abcdef".repeat(10);
            let md = format!("Hash: {run}\n\n# {run}\n\n**{run}** and *more*");
            let side = render(&rappel_core::content::parse(&md), &Images::default());
            let labels = labels_under(side.upcast_ref());
            assert_eq!(labels.len(), 3);
            for label in &labels {
                // Drawn with the breaks: the text a label lays out has them.
                assert!(label.text().contains(breaks::BREAK), "{}", label.text());
                assert!(matches!(
                    label.accessible_role(),
                    Role::Label | Role::Heading
                ));
                // Named without them. GTK has no public getter for the value
                // of an accessible property (`gtk_test_accessible_check_property`
                // is only in gtk4-sys, variadic and unsafe), so this cannot
                // read the name `render` set, and does not notice if a call
                // to `name_without_breaks` is removed; it calls it again to
                // check what it sets. The AT-SPI walk in ADR 0051 read the
                // names in the running app.
                let name = name_without_breaks(label).expect("the text has breaks");
                assert!(!name.contains(breaks::BREAK));
                assert!(name.contains(&run), "{name}");
            }
            // Plain text (a side over the widget limit, a formula's source).
            let label = plain_label(&run, false);
            assert!(label.text().contains(breaks::BREAK));
            assert_eq!(name_without_breaks(&label).as_deref(), Some(run.as_str()));
            // Short runs: GTK's own name, the text, is left.
            let label = plain_label("short words", false);
            assert_eq!(name_without_breaks(&label), None);
        });
    }

    /// Every `CardImage` under `widget`, in order.
    fn images_under(widget: &gtk::Widget) -> Vec<CardImage> {
        let mut out = Vec::new();
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Ok(image) = c.clone().downcast::<CardImage>() {
                out.push(image);
            } else {
                out.extend(images_under(&c));
            }
            child = c.next_sibling();
        }
        out
    }

    #[test]
    fn media_images_are_drawn_up_to_the_side_s_share() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let reference = |n: usize| format!("media:{n:064x}.png");
            // One paragraph with text around an image, and a side with
            // more images than one side draws.
            let md = format!("Before ![a]({}) after", reference(1));
            let images = Images::new(state.media_folder(), 720);
            let side = render(&rappel_core::content::parse(&md), &images);
            let found = images_under(side.upcast_ref());
            assert_eq!(found.len(), 1);
            // The file is looked for on a worker thread.
            assert!(crate::testing::settle_until(
                || found[0].shown_failure().is_some(),
                Duration::from_secs(30)
            ));
            assert_eq!(found[0].shown_failure(), Some(Failure::Missing));
            // The two runs of text and the placeholder's line.
            assert_eq!(labels_under(side.upcast_ref()).len(), 3);
            let many = (0..images::MAX_IMAGES_PER_SIDE + 3)
                .map(|n| format!("![]({})", reference(n)))
                .collect::<Vec<_>>()
                .join("\n\n");
            let side = render(&rappel_core::content::parse(&many), &images);
            let found = images_under(side.upcast_ref());
            assert_eq!(found.len(), images::MAX_IMAGES_PER_SIDE + 3);
            assert!(crate::testing::settle_until(
                || found.iter().all(|image| image.shown_failure().is_some()),
                Duration::from_secs(30)
            ));
            let too_many = found
                .iter()
                .filter(|image| image.shown_failure() == Some(Failure::TooMany))
                .count();
            assert_eq!(too_many, 3);
            // Other targets stay text: nothing to draw, nothing read.
            let side = render(
                &rappel_core::content::parse("![x](https://e.org/a.png) ![y](file:///etc/hosts)"),
                &images,
            );
            assert_eq!(images_under(side.upcast_ref()).len(), 0);
        });
    }

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
                let shown = render(&rappel_core::content::parse(&side), &Images::default());
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
