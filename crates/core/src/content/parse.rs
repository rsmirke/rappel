//! pulldown-cmark events → [`Document`]. The events come as a flat,
//! well-nested stream; a stack of open containers ([`Frame`]) turns them
//! into a tree. Past [`MAX_DEPTH`], containers are not opened any more:
//! their content goes into the deepest open one (`flat` counts them so
//! their ends are matched).

use std::borrow::Cow;
use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::{Align, Block, Cell, Document, ImageTarget, Inline, MAX_DEPTH};
use crate::media::MediaRef;

/// Parses `md`. Never fails: any text is some Markdown.
///
/// U+0000 is replaced with U+FFFD first, as the `CommonMark` spec asks, so
/// no text in the tree has a NUL (GTK takes C strings).
#[must_use]
pub fn parse(md: &str) -> Document {
    let md: Cow<'_, str> = if md.contains('\0') {
        Cow::Owned(md.replace('\0', "\u{FFFD}"))
    } else {
        Cow::Borrowed(md)
    };
    let md = md.as_ref();
    // Tables, strikethrough and math only. No footnotes, smart
    // punctuation, heading attributes or metadata blocks.
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_MATH;
    let mut builder = Builder::new(md);
    for (event, range) in Parser::new_ext(md, options).into_offset_iter() {
        builder.event(event, range);
    }
    builder.finish()
}

/// An open container.
#[derive(Debug)]
enum Kind {
    Root,
    BlockQuote,
    List(Option<u64>),
    Item,
    Paragraph,
    Heading(u8),
    CodeBlock(Option<String>),
    HtmlBlock,
    Table(Vec<Align>),
    TableHead,
    TableRow,
    TableCell,
    Emphasis,
    Strong,
    Strikethrough,
    Link(String),
    Image(String),
}

impl Kind {
    /// Holds blocks; loose inlines (a tight list item's text) become a
    /// paragraph.
    fn holds_blocks(&self) -> bool {
        matches!(self, Self::Root | Self::BlockQuote | Self::Item)
    }

    /// Collects plain text, not inlines.
    fn holds_text(&self) -> bool {
        matches!(self, Self::CodeBlock(_) | Self::HtmlBlock | Self::Image(_))
    }
}

#[derive(Debug)]
struct Frame {
    kind: Kind,
    blocks: Vec<Block>,
    inlines: Vec<Inline>,
    text: String,
    /// A list's items.
    items: Vec<Vec<Block>>,
    /// A table row's (or head's) cells.
    cells: Vec<Cell>,
    /// A table's header and body rows.
    header: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
}

impl Frame {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            blocks: Vec::new(),
            inlines: Vec::new(),
            text: String::new(),
            items: Vec::new(),
            cells: Vec::new(),
            header: Vec::new(),
            rows: Vec::new(),
        }
    }

    fn push_inline(&mut self, inline: Inline) {
        if self.kind.holds_text() {
            super::plain::push_inline_text(&inline, &mut self.text);
            return;
        }
        match (self.inlines.last_mut(), inline) {
            (Some(Inline::Text(last)), Inline::Text(text)) => last.push_str(&text),
            (_, inline) => self.inlines.push(inline),
        }
    }

    fn push_text(&mut self, text: &str) {
        self.push_inline(Inline::Text(text.to_owned()));
    }

    /// Turns loose inlines into paragraphs (splitting out display math).
    fn flush_inlines(&mut self) {
        if !self.inlines.is_empty() {
            let inlines = std::mem::take(&mut self.inlines);
            self.blocks.extend(paragraphs(inlines));
        }
    }

    fn push_block(&mut self, block: Block) {
        if self.kind.holds_blocks() {
            self.flush_inlines();
            self.blocks.push(block);
        } else {
            // Only reachable for malformed nesting; keep the text anyway.
            let mut text = String::new();
            super::plain::push_block_text(&block, &mut text);
            self.push_line_break();
            self.push_text(&text);
        }
    }

    /// Ends a line in this container, for a flattened block's end.
    fn separate(&mut self) {
        if self.kind.holds_blocks() {
            self.flush_inlines();
        } else if self.kind.holds_text() {
            if !self.text.is_empty() && !self.text.ends_with(['\n', ' ']) {
                self.text.push(if matches!(self.kind, Kind::Image(_)) {
                    ' '
                } else {
                    '\n'
                });
            }
        } else {
            self.push_line_break();
        }
    }

    fn push_line_break(&mut self) {
        if !matches!(self.inlines.last(), None | Some(Inline::LineBreak)) {
            self.inlines.push(Inline::LineBreak);
        }
    }
}

#[derive(Debug)]
struct Builder<'a> {
    source: &'a str,
    /// Never empty: the root is at the bottom.
    stack: Vec<Frame>,
    /// How many containers are open but flattened (not on `stack`).
    flat: usize,
}

impl<'a> Builder<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            stack: vec![Frame::new(Kind::Root)],
            flat: 0,
        }
    }

    fn top(&mut self) -> &mut Frame {
        if self.stack.is_empty() {
            // Unreachable: `end` never pops the root. Restore it rather
            // than panic.
            self.stack.push(Frame::new(Kind::Root));
        }
        let last = self.stack.len() - 1;
        &mut self.stack[last]
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if matches!(self.top().kind, Kind::HtmlBlock | Kind::CodeBlock(_)) {
                    self.top().text.push_str(&text);
                } else if self.flat > 0 {
                    // A flattened code block keeps its lines.
                    self.push_lines(&text);
                } else {
                    self.top().push_text(&text);
                }
            }
            Event::Code(code) => self.top().push_inline(Inline::Code(code.into_string())),
            Event::InlineMath(source) => {
                let inline = if self.is_currency(&range) {
                    Inline::Text(self.source.get(range).unwrap_or_default().to_owned())
                } else {
                    Inline::Math {
                        source: source.into_string(),
                        display: false,
                    }
                };
                self.top().push_inline(inline);
            }
            Event::DisplayMath(source) => self.top().push_inline(Inline::Math {
                source: source.into_string(),
                display: true,
            }),
            // Raw HTML is literal text.
            Event::Html(html) | Event::InlineHtml(html) => {
                if matches!(self.top().kind, Kind::HtmlBlock) {
                    self.top().text.push_str(&html);
                } else {
                    self.push_lines(&html);
                }
            }
            Event::FootnoteReference(name) => self.top().push_text(&format!("[^{name}]")),
            Event::SoftBreak => self.top().push_text(" "),
            Event::HardBreak => {
                let top = self.top();
                if top.kind.holds_text() {
                    top.text.push(' ');
                } else {
                    top.push_inline(Inline::LineBreak);
                }
            }
            Event::Rule => {
                if self.flat > 0 {
                    self.top().separate();
                } else {
                    self.top().push_block(Block::Rule);
                }
            }
            Event::TaskListMarker(done) => self.top().push_text(if done { "[x] " } else { "[ ] " }),
        }
    }

    /// Text that may hold newlines, outside a code or HTML block: each
    /// newline, the last one included, ends a line. pulldown-cmark sends a
    /// code or HTML block one line per event, each ending in a newline.
    fn push_lines(&mut self, text: &str) {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.top().separate();
            }
            if !line.is_empty() {
                self.top().push_text(line);
            }
        }
    }

    /// `$5-$10`: inline math whose closing dollar is directly followed by
    /// a digit is a pair of prices, not math (Pandoc's rule; pulldown-cmark
    /// only refuses a space inside the dollars).
    fn is_currency(&self, range: &Range<usize>) -> bool {
        self.source
            .get(range.end..)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    }

    fn start(&mut self, tag: Tag<'_>) {
        let top = self.stack.last().map(|frame| &frame.kind);
        // A list's items and a table's rows and cells always open with
        // their parent, so a list or table is never left without them.
        let structural = matches!(
            (top, &tag),
            (Some(Kind::List(_)), Tag::Item)
                | (Some(Kind::Table(_)), Tag::TableHead | Tag::TableRow)
                | (Some(Kind::TableHead | Kind::TableRow), Tag::TableCell)
        );
        let too_deep = self.stack.len() > MAX_DEPTH && !structural;
        // Anything inside an image's alt text is flattened into it.
        if self.flat > 0 || too_deep || matches!(top, Some(Kind::Image(_))) {
            self.flatten(&tag);
            return;
        }
        let kind = match tag {
            Tag::Paragraph => Kind::Paragraph,
            Tag::Heading { level, .. } => Kind::Heading(level as u8),
            Tag::BlockQuote(_) => Kind::BlockQuote,
            Tag::CodeBlock(kind) => Kind::CodeBlock(match kind {
                CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_owned),
                CodeBlockKind::Indented => None,
            }),
            Tag::HtmlBlock => Kind::HtmlBlock,
            Tag::List(start) => Kind::List(start),
            Tag::Item => Kind::Item,
            Tag::Table(align) => Kind::Table(align.into_iter().map(Align::from).collect()),
            Tag::TableHead => Kind::TableHead,
            Tag::TableRow => Kind::TableRow,
            Tag::TableCell => Kind::TableCell,
            Tag::Emphasis => Kind::Emphasis,
            Tag::Strong => Kind::Strong,
            Tag::Strikethrough => Kind::Strikethrough,
            Tag::Link { dest_url, .. } => Kind::Link(dest_url.into_string()),
            Tag::Image { dest_url, .. } => Kind::Image(dest_url.into_string()),
            // Not enabled; if they ever come, their content is kept.
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {
                self.flatten(&tag);
                return;
            }
        };
        self.stack.push(Frame::new(kind));
    }

    /// Opens `tag` without a container: its content goes to the deepest
    /// open one, a block on a line of its own.
    fn flatten(&mut self, tag: &Tag<'_>) {
        self.flat += 1;
        if is_block_end(tag.to_end()) {
            self.top().separate();
        }
    }

    fn end(&mut self, tag: TagEnd) {
        if self.flat > 0 {
            self.flat -= 1;
            if is_block_end(tag) {
                self.top().separate();
            } else if tag == TagEnd::TableCell {
                self.top().push_text(" ");
            }
            return;
        }
        if self.stack.len() > 1
            && let Some(frame) = self.stack.pop()
        {
            self.close(frame);
        }
    }

    /// Adds a finished container to its parent.
    fn close(&mut self, mut frame: Frame) {
        let parent = self.top();
        match frame.kind {
            Kind::Root => {}
            Kind::Paragraph => {
                for block in paragraphs(frame.inlines) {
                    parent.push_block(block);
                }
            }
            Kind::Heading(level) => parent.push_block(Block::Heading {
                level,
                content: frame.inlines,
            }),
            Kind::BlockQuote => {
                frame.flush_inlines();
                parent.push_block(Block::BlockQuote(frame.blocks));
            }
            Kind::Item => {
                frame.flush_inlines();
                parent.items.push(frame.blocks);
            }
            Kind::List(start) => parent.push_block(Block::List {
                start,
                items: frame.items,
            }),
            Kind::CodeBlock(lang) => {
                let mut code = frame.text;
                if code.ends_with('\n') {
                    code.pop();
                }
                parent.push_block(Block::CodeBlock { lang, code });
            }
            Kind::HtmlBlock => {
                let mut lines = Frame::new(Kind::Paragraph);
                for (i, line) in frame.text.trim_end_matches('\n').split('\n').enumerate() {
                    if i > 0 {
                        lines.inlines.push(Inline::LineBreak);
                    }
                    if !line.is_empty() {
                        lines.push_text(line);
                    }
                }
                if !lines.inlines.is_empty() {
                    parent.push_block(Block::Paragraph(lines.inlines));
                }
            }
            Kind::Table(align) => parent.push_block(Block::Table {
                align,
                header: frame.header,
                rows: frame.rows,
            }),
            Kind::TableHead => parent.header = frame.cells,
            Kind::TableRow => parent.rows.push(frame.cells),
            Kind::TableCell => parent.cells.push(frame.inlines),
            Kind::Emphasis => parent.push_inline(Inline::Emphasis(frame.inlines)),
            Kind::Strong => parent.push_inline(Inline::Strong(frame.inlines)),
            Kind::Strikethrough => parent.push_inline(Inline::Strikethrough(frame.inlines)),
            Kind::Link(url) => match MediaRef::parse(&url) {
                Some(media) if !media.kind().is_image() => parent.push_inline(Inline::Audio {
                    content: frame.inlines,
                    media,
                }),
                _ => parent.push_inline(Inline::Link {
                    content: frame.inlines,
                    url,
                }),
            },
            Kind::Image(url) => parent.push_inline(Inline::Image {
                alt: frame.text,
                target: ImageTarget::from_url(&url),
            }),
        }
    }

    fn finish(mut self) -> Document {
        // pulldown-cmark closes everything it opens; this is a safeguard.
        while self.stack.len() > 1 {
            if let Some(frame) = self.stack.pop() {
                self.close(frame);
            }
        }
        let root = self.top();
        root.flush_inlines();
        Document {
            blocks: std::mem::take(&mut root.blocks),
        }
    }
}

/// Tags (by their end) whose content starts and ends a line when their
/// container is flattened.
fn is_block_end(tag: TagEnd) -> bool {
    !matches!(
        tag,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
            | TagEnd::TableCell
    )
}

/// A paragraph's inlines as blocks: display math becomes its own block,
/// and the text around it paragraphs of their own.
fn paragraphs(inlines: Vec<Inline>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Math {
                source,
                display: true,
            } => {
                push_paragraph(&mut blocks, std::mem::take(&mut current));
                blocks.push(Block::DisplayMath(source));
            }
            inline => current.push(inline),
        }
    }
    push_paragraph(&mut blocks, current);
    blocks
}

/// Pushes `inlines` as a paragraph, without the spaces and line breaks at
/// its ends, unless nothing is left.
fn push_paragraph(blocks: &mut Vec<Block>, mut inlines: Vec<Inline>) {
    while matches!(inlines.last(), Some(Inline::LineBreak)) {
        inlines.pop();
    }
    // In one go: a paragraph can start with hundreds of thousands of
    // hard breaks, and removing them one by one would be quadratic.
    let leading = inlines
        .iter()
        .take_while(|inline| matches!(inline, Inline::LineBreak))
        .count();
    inlines.drain(..leading);
    if let Some(Inline::Text(text)) = inlines.last_mut() {
        text.truncate(text.trim_end().len());
    }
    if let Some(Inline::Text(text)) = inlines.first_mut() {
        *text = text.trim_start().to_owned();
    }
    inlines.retain(|inline| !matches!(inline, Inline::Text(text) if text.is_empty()));
    if !inlines.is_empty() {
        blocks.push(Block::Paragraph(inlines));
    }
}

impl From<Alignment> for Align {
    fn from(align: Alignment) -> Self {
        match align {
            Alignment::None => Self::None,
            Alignment::Left => Self::Left,
            Alignment::Center => Self::Center,
            Alignment::Right => Self::Right,
        }
    }
}
