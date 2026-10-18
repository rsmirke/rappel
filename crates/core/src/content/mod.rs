//! Card content: Markdown parsed into a [`Document`], a block and inline
//! tree that knows nothing about any UI (ADR 0019). The app turns it into
//! widgets; [`plain_text`] and [`plain_summary`] turn it into text.
//!
//! Card text is untrusted. Raw HTML is kept as literal text, never
//! interpreted, and nothing here touches the network or the filesystem.
//! The tree nests at most `MAX_DEPTH` levels, so a hostile card cannot
//! make anything that walks it recurse without bound.

mod parse;
mod plain;

use crate::media::MediaRef;

pub use parse::parse;
#[cfg(test)]
use plain::PREVIEW_CHARS;
pub use plain::{one_line_summary, plain_summary, plain_text, search_text};

/// How deep containers (block quotes, lists, emphasis, links…) nest at
/// most. Deeper content is flattened into the deepest container, so no
/// text is lost. A list item or a table cell may add up to two levels
/// below this.
pub(crate) const MAX_DEPTH: usize = 16;

/// A parsed card side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    /// `true` if there is nothing to show (an empty or blank side).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// A block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    /// `level` is 1 to 6.
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    /// A bullet list (`start` is `None`) or a numbered list starting at
    /// `start`. Each item is a list of blocks, so lists nest.
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    /// `lang` is the first word of a fenced block's info string.
    CodeBlock {
        lang: Option<String>,
        code: String,
    },
    BlockQuote(Vec<Block>),
    /// A thematic break (`---`).
    Rule,
    /// `align` has one entry per column; a row may have fewer cells.
    Table {
        align: Vec<Align>,
        header: Vec<Cell>,
        rows: Vec<Vec<Cell>>,
    },
    /// `$$…$$` on its own in a paragraph: the LaTeX source, without the
    /// dollars.
    DisplayMath(String),
}

/// A table cell.
pub type Cell = Vec<Inline>;

/// The alignment of a table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// An inline element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// Literal text, also for raw HTML, which is never interpreted.
    Text(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Code(String),
    /// A link. The URL is shown, never opened.
    Link {
        content: Vec<Inline>,
        url: String,
    },
    /// `$…$` math (`display: false`), or `$$…$$` where it cannot be a
    /// block (a heading, a table cell). The LaTeX source, without dollars.
    Math {
        source: String,
        display: bool,
    },
    Image {
        alt: String,
        target: ImageTarget,
    },
    /// A link to an audio file of the media store,
    /// `[label](media:<sha256>.<ext>)` with an audio kind (SPEC §4.9, ADR
    /// 0054): played only when the user asks, never fetched.
    Audio {
        content: Vec<Inline>,
        media: MediaRef,
    },
    /// A hard line break.
    LineBreak,
}

/// Where an image points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageTarget {
    /// `media:<sha256>.<ext>` naming an image of the app's media store
    /// (ADR 0054): a checked hash and an image kind, never a path.
    Media(MediaRef),
    /// Anything else (a remote URL, a path, a `media:` URL with a bad
    /// hash, an unknown extension or an audio kind). Never fetched or
    /// opened: it is shown as text (SPEC §4.3).
    Other(String),
}

impl ImageTarget {
    fn from_url(url: &str) -> Self {
        match MediaRef::parse(url) {
            Some(media) if media.kind().is_image() => Self::Media(media),
            _ => Self::Other(url.to_owned()),
        }
    }
}

/// The media files `md` refers to, each once, in the order they first
/// appear: every image or link target that is a valid `media:` reference
/// ([`MediaRef::parse`]), whatever its kind, including one that is shown
/// as text (an audio file as an image target), so a file a card names
/// counts as used. Parsing is bounded as [`parse`] is.
#[must_use]
pub fn media_refs(md: &str) -> Vec<MediaRef> {
    let mut refs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |media: MediaRef| {
        if seen.insert(media) {
            refs.push(media);
        }
    };
    walk_blocks(&parse(md).blocks, &mut |inline| match inline {
        Inline::Image {
            target: ImageTarget::Media(media),
            ..
        }
        | Inline::Audio { media, .. } => add(*media),
        Inline::Image {
            target: ImageTarget::Other(url),
            ..
        }
        | Inline::Link { url, .. } => {
            if let Some(media) = MediaRef::parse(url) {
                add(media);
            }
        }
        _ => {}
    });
    refs
}

/// Calls `f` on every inline of `blocks`, depth first. The tree nests at
/// most [`MAX_DEPTH`] levels (plus two), so the recursion is bounded.
fn walk_blocks(blocks: &[Block], f: &mut impl FnMut(&Inline)) {
    for block in blocks {
        match block {
            Block::Paragraph(inlines)
            | Block::Heading {
                content: inlines, ..
            } => {
                walk_inlines(inlines, f);
            }
            Block::List { items, .. } => {
                for item in items {
                    walk_blocks(item, f);
                }
            }
            Block::BlockQuote(blocks) => walk_blocks(blocks, f),
            Block::Table { header, rows, .. } => {
                for cell in header.iter().chain(rows.iter().flatten()) {
                    walk_inlines(cell, f);
                }
            }
            Block::CodeBlock { .. } | Block::Rule | Block::DisplayMath(_) => {}
        }
    }
}

fn walk_inlines(inlines: &[Inline], f: &mut impl FnMut(&Inline)) {
    for inline in inlines {
        f(inline);
        match inline {
            Inline::Emphasis(inner)
            | Inline::Strong(inner)
            | Inline::Strikethrough(inner)
            | Inline::Link { content: inner, .. }
            | Inline::Audio { content: inner, .. } => walk_inlines(inner, f),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
