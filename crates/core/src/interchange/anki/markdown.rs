//! Markdown blocks and inlines from Anki's HTML ([`super::html`]), written
//! as Rappel's Markdown (ADR 0061). Text is escaped so nothing in it is
//! ever read as Markdown or math: every character that can start markup
//! anywhere (`\`, `` ` ``, `*`, `_`, `[`, `]`, `<`, `>`, `$`, `~`, `|`, `&`,
//! `#`, and `!`, which a link after it would turn into an image) is
//! backslash-escaped, and so is a `-`, `+` or `=` or a number's
//! `.` or `)` that starts a line. Lines are trimmed, so none is indented
//! into a code block.

use crate::media::MediaRef;

/// A block of Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Block {
    Para(Vec<Inl>),
    Heading(u8, Vec<Inl>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
    },
    Code(String),
    Quote(Vec<Block>),
    Rule,
    /// Rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Inl>>>),
}

/// Inline Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Inl {
    Text(String),
    Strong(Vec<Inl>),
    Em(Vec<Inl>),
    Strike(Vec<Inl>),
    Code(String),
    Link { children: Vec<Inl>, url: String },
    Image { alt: String, media: MediaRef },
    Audio { label: String, media: MediaRef },
    Math { source: String, display: bool },
    Break,
}

/// Escapes text for Markdown (not the line starts, see [`escape_line_start`]).
pub(super) fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '$' | '~' | '|' | '&' | '#' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
}

/// A line that would start a list item or a setext underline with its
/// first characters, escaped.
fn escape_line_start(line: &str) -> String {
    if line.starts_with(['-', '+', '=']) {
        return format!("\\{line}");
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(['.', ')']) {
        return format!("{}\\{}", &line[..digits], &line[digits..]);
    }
    line.to_owned()
}

/// Which emphasis is already open, so it is never nested in itself.
#[derive(Debug, Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags, one per kind of container"
)]
struct Open {
    strong: bool,
    em: bool,
    strike: bool,
    link: bool,
    /// In a table cell, where a `|` ends the cell even in a code span
    /// unless escaped (GFM).
    table: bool,
}

/// One inline rendered: plain Markdown, or emphasis whose delimiters are
/// decided once its neighbours are known.
enum Part {
    Plain(String),
    Wrapped {
        delimiter: &'static str,
        inner: String,
    },
}

fn inlines(items: &[Inl], open: Open, out: &mut String) {
    let parts: Vec<Part> = merged(items).iter().map(|item| part(item, open)).collect();
    let first_char = |p: &Part| match p {
        Part::Plain(s) => s.chars().next(),
        Part::Wrapped { delimiter, .. } => delimiter.chars().next(),
    };
    for (i, p) in parts.iter().enumerate() {
        match p {
            Part::Plain(s) => out.push_str(s),
            Part::Wrapped { delimiter, inner } => {
                let next = parts.get(i + 1).and_then(first_char);
                wrapped(inner, delimiter, out.chars().last(), next, out);
            }
        }
    }
}

/// `items` with neighbouring emphasis of one kind, and neighbouring code
/// spans, merged (`*a**b*` would read as one run of delimiters).
fn merged(items: &[Inl]) -> Vec<Inl> {
    let mut out: Vec<Inl> = Vec::with_capacity(items.len());
    for item in items {
        match (out.last_mut(), item) {
            (Some(Inl::Strong(a)), Inl::Strong(b))
            | (Some(Inl::Em(a)), Inl::Em(b))
            | (Some(Inl::Strike(a)), Inl::Strike(b)) => a.extend(b.iter().cloned()),
            // Two code spans side by side would read as one fence.
            (Some(Inl::Code(a)), Inl::Code(b)) => a.push_str(b),
            _ => out.push(item.clone()),
        }
    }
    out
}

/// Unicode punctuation and symbols, as `CommonMark`'s flanking rules see
/// them (ASCII punctuation included).
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace())
}

/// `inner` wrapped in `delimiter`, with its leading and trailing spaces
/// outside, where they let the delimiters count as emphasis. If the
/// delimiters could not open or close there (`CommonMark`'s flanking
/// rules: emphasis that starts with punctuation right after a letter, or
/// ends with punctuation right before one), the text stays plain rather
/// than show stray asterisks. `prev` and `next` are the characters around
/// it, `None` at the edge of its container.
fn wrapped(inner: &str, delimiter: &str, prev: Option<char>, next: Option<char>, out: &mut String) {
    let core = inner.trim_matches(' ');
    let (Some(first), Some(last)) = (core.chars().next(), core.chars().last()) else {
        out.push_str(inner);
        return;
    };
    let lead = inner.len() - inner.trim_start_matches(' ').len();
    let trail = inner.len() - inner.trim_end_matches(' ').len();
    let before = if lead > 0 { Some(' ') } else { prev };
    let after = if trail > 0 { Some(' ') } else { next };
    // A delimiter next to the same character would make one longer run.
    let mark = delimiter.chars().next();
    let opens = before != mark
        && (!is_punctuation(first)
            || before.is_none_or(|c| c.is_whitespace() || is_punctuation(c)));
    let closes = after != mark
        && (!is_punctuation(last) || after.is_none_or(|c| c.is_whitespace() || is_punctuation(c)));
    if !(opens && closes) {
        out.push_str(inner);
        return;
    }
    out.push_str(&inner[..lead]);
    out.push_str(delimiter);
    out.push_str(core);
    out.push_str(delimiter);
    out.push_str(&inner[inner.len() - trail..]);
}

fn part(item: &Inl, open: Open) -> Part {
    let mut out = String::new();
    match item {
        Inl::Text(t) => escape(t, &mut out),
        Inl::Break => out.push_str("\\\n"),
        Inl::Strong(c) | Inl::Em(c) | Inl::Strike(c) => {
            let (already, delimiter, inner_open) = match item {
                Inl::Strong(_) => (
                    open.strong,
                    "**",
                    Open {
                        strong: true,
                        ..open
                    },
                ),
                Inl::Em(_) => (open.em, "*", Open { em: true, ..open }),
                _ => (
                    open.strike,
                    "~~",
                    Open {
                        strike: true,
                        ..open
                    },
                ),
            };
            let mut inner = String::new();
            inlines(c, inner_open, &mut inner);
            if already || inner.contains('\n') {
                // Emphasis over a line break is split by the converter;
                // only nested emphasis of one kind reaches here.
                return Part::Plain(inner);
            }
            return Part::Wrapped { delimiter, inner };
        }
        Inl::Code(code) => code_span(code, open.table, &mut out),
        Inl::Link { children, url } => {
            let mut inner = String::new();
            inlines(children, Open { link: true, ..open }, &mut inner);
            if open.link || inner.trim().is_empty() {
                return Part::Plain(inner);
            }
            out.push('[');
            out.push_str(inner.trim());
            out.push_str("](<");
            for c in url.chars() {
                // `&` too: `&amp;` would be read as a character reference.
                if matches!(c, '<' | '>' | '\\' | '&') {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push_str(">)");
        }
        Inl::Image { alt, media } => {
            out.push_str("![");
            escape(alt, &mut out);
            out.push_str("](");
            out.push_str(&media.reference());
            out.push(')');
        }
        Inl::Audio { label, media } => {
            out.push('[');
            escape(label, &mut out);
            out.push_str("](");
            out.push_str(&media.reference());
            out.push(')');
        }
        Inl::Math { source, display } => {
            let d = if *display { "$$" } else { "$" };
            out.push_str(d);
            out.push_str(source);
            out.push_str(d);
        }
    }
    Part::Plain(out)
}

/// A code span: backticks one more than the longest run inside, and a
/// space inside each end when the code starts or ends with one. In a table
/// cell (`table`), each `|` is escaped, which GFM reads back as `|`.
fn code_span(code: &str, table: bool, out: &mut String) {
    let mut code = code.replace(['\n', '\r'], " ");
    if table {
        code = code.replace('|', "\\|");
    }
    let fence = "`".repeat(longest_run(&code, '`') + 1);
    let pad = code.starts_with('`') || code.ends_with('`');
    out.push_str(&fence);
    if pad {
        out.push(' ');
    }
    out.push_str(&code);
    if pad {
        out.push(' ');
    }
    out.push_str(&fence);
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        if ch == c {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Inline content as one paragraph: each line trimmed and its start
/// escaped.
fn paragraph(items: &[Inl]) -> String {
    paragraph_in(items, Open::default())
}

/// [`paragraph`] with what is open around it.
fn paragraph_in(items: &[Inl], open: Open) -> String {
    let md = paragraph_once(items, open);
    // Emphasis delimiters that Rappel's parser would read otherwise (runs
    // that merge, flanking rules) would show as stray asterisks: the
    // paragraph is written again without emphasis.
    if text_of_parsed(&md) == text_of_tree(items) {
        md
    } else {
        paragraph_once(&without_emphasis(items), open)
    }
}

/// The text runs of `items`, without whitespace.
fn text_of_tree(items: &[Inl]) -> String {
    fn walk(items: &[Inl], out: &mut String) {
        for i in items {
            match i {
                Inl::Text(t) => out.extend(t.chars().filter(|c| !c.is_whitespace())),
                Inl::Strong(c) | Inl::Em(c) | Inl::Strike(c) | Inl::Link { children: c, .. } => {
                    walk(c, out);
                }
                Inl::Audio { label, .. } => {
                    out.extend(label.chars().filter(|c| !c.is_whitespace()));
                }
                _ => {}
            }
        }
    }
    let mut out = String::new();
    walk(items, &mut out);
    out
}

/// The text runs Rappel's parser reads in `md`, without whitespace.
fn text_of_parsed(md: &str) -> String {
    use crate::content::{Block as B, Inline as I};
    fn walk(items: &[I], out: &mut String) {
        for i in items {
            match i {
                I::Text(t) => out.extend(t.chars().filter(|c| !c.is_whitespace())),
                I::Emphasis(c)
                | I::Strong(c)
                | I::Strikethrough(c)
                | I::Link { content: c, .. }
                | I::Audio { content: c, .. } => {
                    walk(c, out);
                }
                _ => {}
            }
        }
    }
    let mut out = String::new();
    for block in crate::content::parse(md).blocks {
        match block {
            B::Paragraph(items) | B::Heading { content: items, .. } => walk(&items, &mut out),
            // Not a paragraph any more: never the same text.
            _ => out.push('\u{0}'),
        }
    }
    out
}

/// `items` with every emphasis replaced by its content.
fn without_emphasis(items: &[Inl]) -> Vec<Inl> {
    let mut out = Vec::new();
    for i in items {
        match i {
            Inl::Strong(c) | Inl::Em(c) | Inl::Strike(c) => out.extend(without_emphasis(c)),
            Inl::Link { children, url } => out.push(Inl::Link {
                children: without_emphasis(children),
                url: url.clone(),
            }),
            other => out.push(other.clone()),
        }
    }
    out
}

fn paragraph_once(items: &[Inl], open: Open) -> String {
    let mut raw = String::new();
    inlines(items, open, &mut raw);
    raw.split('\n')
        .map(|line| {
            // A hard break's backslash ends the line; an escaped one is
            // two, so an odd count means a break.
            let trailing = line.len() - line.trim_end_matches('\\').len();
            let brk = trailing % 2 == 1;
            let body = if brk { &line[..line.len() - 1] } else { line };
            let body = body.trim_matches(|c: char| c == ' ' || c == '\u{a0}');
            let mut out = escape_line_start(body);
            if brk {
                out.push('\\');
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line of inline content (a heading, a table cell): breaks are
/// spaces.
fn one_line(items: &[Inl], table: bool) -> String {
    let flat: Vec<Inl> = items
        .iter()
        .map(|i| match i {
            Inl::Break => Inl::Text(" ".to_owned()),
            other => other.clone(),
        })
        .collect();
    let open = Open {
        table,
        ..Open::default()
    };
    paragraph_in(&flat, open)
        .replace("\\\n", " ")
        .replace('\n', " ")
}

/// Writes `blocks` as Markdown.
pub(super) fn render(blocks: &[Block]) -> String {
    let parts: Vec<String> = blocks.iter().map(block).filter(|b| !b.is_empty()).collect();
    parts.join("\n\n")
}

fn block(b: &Block) -> String {
    match b {
        Block::Para(items) => paragraph(items),
        Block::Heading(level, items) => {
            let line = one_line(items, false);
            if line.is_empty() {
                String::new()
            } else {
                format!("{} {line}", "#".repeat(usize::from(*level)))
            }
        }
        Block::Rule => "---".to_owned(),
        Block::Code(code) => {
            let fence = "`".repeat(longest_run(code, '`').max(2) + 1);
            format!("{fence}\n{code}\n{fence}")
        }
        Block::Quote(blocks) => render(blocks)
            .lines()
            .map(|l| {
                if l.is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {l}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::List {
            ordered,
            start,
            items,
        } => {
            // A paragraph with a list under it keeps the list tight, if
            // the list may interrupt a paragraph (CommonMark: a numbered
            // one only when it starts at 1).
            let tight_item = |item: &Vec<Block>| {
                item.len() <= 1
                    || (matches!(item[0], Block::Para(_))
                        && item[1..].iter().all(|b| match b {
                            Block::List { ordered, start, .. } => !ordered || *start == 1,
                            _ => false,
                        }))
            };
            let loose = !items.iter().all(tight_item);
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}. ", start.saturating_add(i as u64))
                } else {
                    "- ".to_owned()
                };
                let indent = " ".repeat(marker.len());
                let body = if loose {
                    render(item)
                } else {
                    item.iter()
                        .map(block)
                        .filter(|b| !b.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let mut text = String::new();
                for (n, line) in body.lines().enumerate() {
                    if n == 0 {
                        text.push_str(&marker);
                    } else {
                        text.push('\n');
                        if !line.is_empty() {
                            text.push_str(&indent);
                        }
                    }
                    text.push_str(line);
                }
                if body.is_empty() {
                    text.push_str(marker.trim_end());
                }
                out.push(text);
            }
            out.join(if loose { "\n\n" } else { "\n" })
        }
        Block::Table(rows) => {
            let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
            if columns == 0 {
                return String::new();
            }
            let row = |cells: &[Vec<Inl>]| {
                let mut line = String::from("|");
                for c in 0..columns {
                    let cell = cells.get(c).map(|c| one_line(c, true)).unwrap_or_default();
                    line.push(' ');
                    line.push_str(&cell);
                    line.push_str(" |");
                }
                line
            };
            let mut lines = vec![row(&rows[0])];
            lines.push(format!("|{}", " --- |".repeat(columns)));
            lines.extend(rows[1..].iter().map(|r| row(r)));
            lines.join("\n")
        }
    }
}

/// The plain text of a block, for a block inside a line (a list in a
/// table cell).
pub(super) fn block_text(b: &Block) -> String {
    fn text(items: &[Inl], out: &mut String) {
        for i in items {
            match i {
                Inl::Text(t) | Inl::Code(t) => out.push_str(t),
                Inl::Strong(c) | Inl::Em(c) | Inl::Strike(c) | Inl::Link { children: c, .. } => {
                    text(c, out);
                }
                Inl::Image { alt, .. } => out.push_str(alt),
                Inl::Audio { label, .. } => out.push_str(label),
                Inl::Math { source, .. } => out.push_str(source),
                Inl::Break => out.push(' '),
            }
        }
    }
    let mut out = String::new();
    match b {
        Block::Para(items) | Block::Heading(_, items) => text(items, &mut out),
        Block::Code(code) => out.push_str(code),
        Block::Quote(blocks) => {
            for b in blocks {
                out.push_str(&block_text(b));
                out.push(' ');
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for b in item {
                    out.push_str(&block_text(b));
                    out.push(' ');
                }
            }
        }
        Block::Rule => {}
        Block::Table(rows) => {
            for cells in rows {
                for cell in cells {
                    text(cell, &mut out);
                    out.push(' ');
                }
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
