//! Anki's field HTML, read with a small tokenizer into a tree, then turned
//! into Markdown blocks ([`super::markdown`]) (ADR 0061). Nothing is ever
//! rendered as HTML: tags are only read for their structure (bold, lists,
//! line breaks…), `<script>` and `<style>` are dropped with their content,
//! every attribute but a link's `href` and an image's `src` and `alt` is
//! dropped, character references are decoded from the full HTML table,
//! and the text comes out escaped for Markdown. `\(…\)` and `\[…\]`
//! become math, `[sound:…]` audio; images and audio are named only
//! through the caller's resolver, never fetched.

use super::entities::ENTITIES;
use super::markdown::{Block, Inl};
use crate::media::MediaRef;

/// How deep elements nest at most; deeper ones are flattened into the
/// deepest.
const MAX_DEPTH: usize = 64;
/// How far ahead a math opener looks for its closer, in bytes.
const MAX_MATH: usize = 64 << 10;

/// Elements dropped with everything inside.
const DROPPED: [&str; 8] = [
    "script", "style", "title", "textarea", "noscript", "template", "head", "xmp",
];
/// Elements that never have content.
const VOID: [&str; 14] = [
    "br", "hr", "img", "input", "meta", "link", "area", "base", "col", "embed", "param", "source",
    "track", "wbr",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Text(String),
    Start {
        name: String,
        attrs: Vec<(String, String)>,
    },
    End(String),
    Math {
        source: String,
        display: bool,
    },
}

/// Decodes the character reference at the start of `s` (just after `&`):
/// what it stands for and how many bytes it takes; `None` if it is not
/// one.
fn reference(s: &str) -> Option<(String, usize)> {
    if let Some(rest) = s.strip_prefix('#') {
        let (hex, digits) = match rest.strip_prefix(['x', 'X']) {
            Some(h) => (true, h),
            None => (false, rest),
        };
        let len = digits
            .bytes()
            .take_while(|b| {
                if hex {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit()
                }
            })
            .count();
        if len == 0 {
            return None;
        }
        let value = u32::from_str_radix(&digits[..len.min(8)], if hex { 16 } else { 10 })
            .unwrap_or(u32::MAX);
        let c = match value {
            0 => '\u{FFFD}',
            v => char::from_u32(v).unwrap_or('\u{FFFD}'),
        };
        let mut used = 1 + usize::from(hex) + len;
        if digits[len..].starts_with(';') {
            used += 1;
        }
        return Some((c.to_string(), used));
    }
    let len = s
        .bytes()
        .take(40)
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    if len == 0 {
        return None;
    }
    let name = &s[..len];
    let with_semi = s[len..].starts_with(';');
    let find = |key: &str| {
        ENTITIES
            .binary_search_by(|(n, _)| n.as_bytes().cmp(key.as_bytes()))
            .ok()
            .map(|i| ENTITIES[i].1)
    };
    if with_semi && let Some(v) = find(&format!("{name};")) {
        return Some((v.to_owned(), len + 1));
    }
    find(name).map(|v| (v.to_owned(), len))
}

/// `text` with its character references decoded and NUL replaced.
pub(crate) fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['&', '\0']) {
        out.push_str(&rest[..at]);
        if rest.as_bytes()[at] == 0 {
            out.push('\u{FFFD}');
            rest = &rest[at + 1..];
            continue;
        }
        if let Some((value, used)) = reference(&rest[at + 1..]) {
            out.push_str(&value);
            rest = &rest[at + 1 + used..];
        } else {
            out.push('&');
            rest = &rest[at + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// Where a math closer was last found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closer {
    /// Not searched for yet.
    Unsearched,
    /// At this byte.
    At(usize),
    /// Nowhere after the last search's start.
    Nowhere,
}

struct Tokenizer<'a> {
    src: &'a str,
    pos: usize,
    /// Read math (`\(…\)`, `\[…\]`) in text.
    math: bool,
    /// Where the next `\)` and `\]` at or after the last search start
    /// (`None` inside: there is none), so text full of openers is read in
    /// linear time.
    closers: [Closer; 2],
}

impl<'a> Tokenizer<'a> {
    fn new(src: &'a str, math: bool) -> Self {
        Self {
            src,
            pos: 0,
            math,
            closers: [Closer::Unsearched, Closer::Unsearched],
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    /// Skips past the next `end` (or to the end).
    fn skip_past(&mut self, end: &str) {
        match self.rest().find(end) {
            Some(i) => self.pos += i + end.len(),
            None => self.pos = self.src.len(),
        }
    }

    /// Skips the content of a dropped element up to its end tag.
    fn skip_element(&mut self, name: &str) {
        let rest = self.rest();
        let mut from = 0;
        while let Some(i) = rest[from..].find("</") {
            let at = from + i;
            let tail = &rest.as_bytes()[at + 2..];
            if tail
                .get(..name.len())
                .is_some_and(|t| t.eq_ignore_ascii_case(name.as_bytes()))
            {
                self.pos += at;
                self.skip_past(">");
                return;
            }
            from = at + 2;
        }
        self.pos = self.src.len();
    }

    fn tag_name(&mut self) -> String {
        let len = self
            .rest()
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b':')
            .count();
        let name = self.rest()[..len].to_ascii_lowercase();
        self.pos += len;
        name
    }

    /// The attributes up to the end of a start tag, which is consumed.
    fn attributes(&mut self) -> Vec<(String, String)> {
        let mut attrs = Vec::new();
        loop {
            let rest = self.rest();
            let skipped = rest.len()
                - rest
                    .trim_start_matches([' ', '\t', '\n', '\r', '\x0c', '/'])
                    .len();
            self.pos += skipped;
            let Some(c) = self.rest().chars().next() else {
                return attrs;
            };
            if c == '>' {
                self.pos += 1;
                return attrs;
            }
            let len = self
                .rest()
                .find(|c: char| c.is_whitespace() || matches!(c, '=' | '>' | '/'))
                .unwrap_or(self.rest().len())
                .max(c.len_utf8());
            let name = self.rest()[..len].to_ascii_lowercase();
            self.pos += len;
            let rest = self.rest();
            let after_space = rest.trim_start();
            let mut value = String::new();
            if let Some(v) = after_space.strip_prefix('=') {
                self.pos += rest.len() - v.len();
                let v = self.rest().trim_start();
                self.pos = self.src.len() - v.len();
                if let Some(quote) = v.chars().next().filter(|q| *q == '"' || *q == '\'') {
                    let body = &v[1..];
                    let end = body.find(quote).unwrap_or(body.len());
                    value = decode(&body[..end]);
                    self.pos += 1 + end + usize::from(end < body.len());
                } else {
                    let end = v
                        .find(|c: char| c.is_whitespace() || c == '>')
                        .unwrap_or(v.len());
                    value = decode(&v[..end]);
                    self.pos += end;
                }
            }
            if attrs.len() < 32 {
                attrs.push((name, value));
            }
        }
    }

    /// A math token at the current position (`\(` or `\[`), if it closes.
    fn math_here(&mut self) -> Option<Token> {
        let rest = self.rest();
        let (display, close) = if rest.starts_with("\\(") {
            (false, "\\)")
        } else if rest.starts_with("\\[") {
            (true, "\\]")
        } else {
            return None;
        };
        let start = self.pos + 2;
        let slot = usize::from(display);
        let next = match self.closers[slot] {
            Closer::At(at) if at >= start => Some(at),
            Closer::Nowhere => None,
            Closer::At(_) | Closer::Unsearched => {
                let found = self.src[start..].find(close).map(|i| start + i);
                self.closers[slot] = found.map_or(Closer::Nowhere, Closer::At);
                found
            }
        }?;
        if next - start > MAX_MATH {
            return None;
        }
        let source = plain_text(&self.src[start..next]);
        self.pos = next + 2;
        Some(Token::Math { source, display })
    }
}

/// The largest char boundary of `s` at or below `at`.
fn floor_boundary(s: &str, at: usize) -> usize {
    if at >= s.len() {
        return s.len();
    }
    let mut at = at;
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

impl Iterator for Tokenizer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                return None;
            }
            if rest.starts_with("<!--") {
                self.skip_past("-->");
                continue;
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                self.skip_past(">");
                continue;
            }
            if let Some(after) = rest.strip_prefix("</")
                && after.starts_with(|c: char| c.is_ascii_alphabetic())
            {
                self.pos += 2;
                let name = self.tag_name();
                self.skip_past(">");
                return Some(Token::End(name));
            }
            if let Some(after) = rest.strip_prefix('<')
                && after.starts_with(|c: char| c.is_ascii_alphabetic())
            {
                self.pos += 1;
                let name = self.tag_name();
                let attrs = self.attributes();
                if DROPPED.contains(&name.as_str()) {
                    self.skip_element(&name);
                    continue;
                }
                return Some(Token::Start { name, attrs });
            }
            if self.math
                && let Some(token) = self.math_here()
            {
                return Some(token);
            }
            // Text up to the next tag or math opener (a lone `<` is text).
            let mut end = 1;
            let bytes = rest.as_bytes();
            while end < bytes.len() {
                let b = bytes[end];
                if b == b'<'
                    || (self.math && b == b'\\' && matches!(bytes.get(end + 1), Some(b'(' | b'[')))
                {
                    break;
                }
                end += 1;
            }
            let end = floor_boundary(rest, end);
            let end = if end == 0 {
                rest.chars().next().map_or(1, char::len_utf8)
            } else {
                end
            };
            self.pos += end;
            return Some(Token::Text(decode(&rest[..end])));
        }
    }
}

/// The text of `html` on one line: tags dropped (a line break or block is
/// a space), references decoded, whitespace collapsed and trimmed.
pub(super) fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    for token in Tokenizer::new(html, false) {
        match token {
            Token::Text(t) => out.push_str(&t),
            Token::Start { name, .. } | Token::End(name) => {
                if is_block(&name) || name == "br" {
                    out.push(' ');
                }
            }
            Token::Math { .. } => {}
        }
    }
    collapse(&out).trim().to_owned()
}

/// Calls `each` with every media file name `html` uses: each image's
/// `src` and each `[sound:…]`.
pub(super) fn media_names(html: &str, each: &mut dyn FnMut(&str)) {
    for token in Tokenizer::new(html, false) {
        match token {
            Token::Start { name, attrs } if name == "img" => {
                if let Some((_, src)) = attrs.iter().find(|(k, _)| k == "src") {
                    each(src.trim());
                }
            }
            Token::Text(text) => {
                let mut rest = text.as_str();
                while let Some(at) = rest.find("[sound:") {
                    let Some(end) = rest[at..].find(']').map(|e| at + e) else {
                        break;
                    };
                    each(&rest[at + 7..end]);
                    rest = &rest[end + 1..];
                }
            }
            _ => {}
        }
    }
}

/// Runs of HTML whitespace as one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c') {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "div"
            | "p"
            | "ul"
            | "ol"
            | "li"
            | "pre"
            | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
            | "table"
            | "tr"
            | "td"
            | "th"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "dl"
            | "dt"
            | "dd"
            | "figure"
            | "figcaption"
            | "center"
            | "address"
    )
}

/// One node of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Element {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<Node>,
    },
    Text(String),
    Math {
        source: String,
        display: bool,
    },
    Sound(String),
}

impl Node {
    fn attr(&self, key: &str) -> Option<&str> {
        match self {
            Self::Element { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }
}

/// Splits text into text and `[sound:…]` references.
fn push_text(children: &mut Vec<Node>, text: &str, keep_space: bool) {
    let mut rest = text;
    while let Some(at) = rest.find("[sound:") {
        let Some(end) = rest[at..].find(']').map(|e| at + e) else {
            break;
        };
        let name = &rest[at + 7..end];
        if name.is_empty() {
            break;
        }
        push_plain(children, &rest[..at], keep_space);
        children.push(Node::Sound(name.to_owned()));
        rest = &rest[end + 1..];
    }
    push_plain(children, rest, keep_space);
}

fn push_plain(children: &mut Vec<Node>, text: &str, keep_space: bool) {
    if text.is_empty() {
        return;
    }
    let text = if keep_space {
        text.to_owned()
    } else {
        collapse(text)
    };
    match children.last_mut() {
        Some(Node::Text(last)) => {
            if !keep_space && last.ends_with(' ') && text.starts_with(' ') {
                last.push_str(&text[1..]);
            } else {
                last.push_str(&text);
            }
        }
        _ => children.push(Node::Text(text)),
    }
}

/// An element being read.
struct OpenElement {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

/// Closes the innermost open element into its parent.
fn close_top(stack: &mut Vec<OpenElement>) {
    if stack.len() > 1
        && let Some(open) = stack.pop()
        && let Some(parent) = stack.last_mut()
    {
        parent.children.push(Node::Element {
            name: open.name,
            attrs: open.attrs,
            children: open.children,
        });
    }
}

/// The tree of `html`.
fn tree(html: &str) -> Vec<Node> {
    let mut stack = vec![OpenElement {
        name: String::new(),
        attrs: Vec::new(),
        children: Vec::new(),
    }];
    for token in Tokenizer::new(html, true) {
        match token {
            Token::Text(text) => {
                let in_pre = stack.iter().any(|e| e.name == "pre");
                if let Some(top) = stack.last_mut() {
                    push_text(&mut top.children, &text, in_pre);
                }
            }
            Token::Math { source, display } => {
                if let Some(top) = stack.last_mut() {
                    top.children.push(Node::Math { source, display });
                }
            }
            Token::Start { name, attrs } => {
                // An open item, paragraph, row or cell ends where the next
                // one starts (`<li>a<li>b`).
                let implied: &[&str] = match name.as_str() {
                    "li" => &["li"],
                    "p" => &["p"],
                    "tr" => &["tr", "td", "th"],
                    "td" | "th" => &["td", "th"],
                    _ => &[],
                };
                if stack
                    .last()
                    .is_some_and(|e| implied.contains(&e.name.as_str()))
                {
                    close_top(&mut stack);
                }
                let keep: Vec<(String, String)> = attrs
                    .into_iter()
                    .filter(|(k, _)| matches!(k.as_str(), "href" | "src" | "alt" | "start"))
                    .collect();
                if VOID.contains(&name.as_str()) {
                    if let Some(top) = stack.last_mut() {
                        top.children.push(Node::Element {
                            name,
                            attrs: keep,
                            children: Vec::new(),
                        });
                    }
                } else if stack.len() <= MAX_DEPTH {
                    stack.push(OpenElement {
                        name,
                        attrs: keep,
                        children: Vec::new(),
                    });
                }
            }
            Token::End(name) => {
                if let Some(depth) = stack.iter().skip(1).rposition(|e| e.name == name) {
                    while stack.len() > depth + 1 {
                        close_top(&mut stack);
                    }
                }
            }
        }
    }
    while stack.len() > 1 {
        close_top(&mut stack);
    }
    stack.pop().map(|e| e.children).unwrap_or_default()
}

/// What a field's media reference is used as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MediaUse {
    Image,
    Audio,
}

/// What the conversion needs from its caller.
pub(super) trait Resolver {
    /// The media file named `name` in the package, for `use_`; `None` if it
    /// cannot be used (missing, another kind, refused), which the resolver
    /// notes.
    fn media(&mut self, name: &str, use_: MediaUse) -> Option<MediaRef>;
    /// An image that is not in the package (a remote URL, an embedded
    /// image): shown as text.
    fn remote_image(&mut self, src: &str);
    /// Whether `source` is math Rappel can draw.
    fn math_ok(&mut self, source: &str, display: bool) -> bool;
}

/// One piece of converted content: inline content, or a boundary between
/// lines, paragraphs or blocks.
#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Inline(Inl),
    /// A `<div>`'s edge: a new line if there is text on both sides.
    Line,
    /// A `<br>`.
    Break,
    /// A `<p>`'s edge: a new paragraph.
    Para,
    Block(Block),
}

struct Converter<'r> {
    resolver: &'r mut dyn Resolver,
    /// Math as text, not as math (the second try of a side).
    no_math: bool,
}

impl Converter<'_> {
    fn pieces(&mut self, nodes: &[Node], out: &mut Vec<Piece>) {
        for node in nodes {
            self.piece(node, out);
        }
    }

    fn piece(&mut self, node: &Node, out: &mut Vec<Piece>) {
        match node {
            Node::Text(t) => out.push(Piece::Inline(Inl::Text(t.clone()))),
            Node::Math { source, display } => {
                let source = source.trim();
                if !self.no_math && !source.is_empty() && self.resolver.math_ok(source, *display) {
                    out.push(Piece::Inline(Inl::Math {
                        source: source.to_owned(),
                        display: *display,
                    }));
                } else {
                    let (open, close) = if *display {
                        ("\\[", "\\]")
                    } else {
                        ("\\(", "\\)")
                    };
                    out.push(Piece::Inline(Inl::Text(format!("{open}{source}{close}"))));
                }
            }
            Node::Sound(name) => match self.resolver.media(name, MediaUse::Audio) {
                Some(media) => out.push(Piece::Inline(Inl::Audio {
                    label: name.clone(),
                    media,
                })),
                None => out.push(Piece::Inline(Inl::Text(name.clone()))),
            },
            Node::Element { name, children, .. } => self.element(node, name, children, out),
        }
    }

    fn element(&mut self, node: &Node, name: &str, children: &[Node], out: &mut Vec<Piece>) {
        match name {
            "br" => out.push(Piece::Break),
            "hr" => out.push(Piece::Block(Block::Rule)),
            "img" => self.image(node, out),
            "b" | "strong" => self.wrap(children, out, Inl::Strong),
            "i" | "em" | "cite" | "dfn" | "var" => self.wrap(children, out, Inl::Em),
            "s" | "strike" | "del" => self.wrap(children, out, Inl::Strike),
            "code" | "kbd" | "samp" | "tt" => {
                let text = collapse(&text_of(children, false));
                if !text.trim().is_empty() {
                    out.push(Piece::Inline(Inl::Code(text)));
                }
            }
            "a" => self.link(node, children, out),
            "p" => {
                out.push(Piece::Para);
                self.pieces(children, out);
                out.push(Piece::Para);
            }
            "ul" | "ol" => self.list(node, name == "ol", children, out),
            "pre" => {
                let code = text_of(children, true);
                let code = code.trim_matches('\n');
                if !code.trim().is_empty() {
                    out.push(Piece::Block(Block::Code(code.to_owned())));
                }
            }
            "blockquote" => {
                let mut inner = Vec::new();
                self.pieces(children, &mut inner);
                let blocks = assemble(inner);
                if !blocks.is_empty() {
                    out.push(Piece::Block(Block::Quote(blocks)));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.as_bytes()[1] - b'0';
                let line = self.line(children);
                if !line.is_empty() {
                    out.push(Piece::Block(Block::Heading(level, line)));
                }
            }
            "table" => {
                let mut rows = Vec::new();
                self.rows(children, &mut rows, 0);
                if !rows.is_empty() {
                    out.push(Piece::Block(Block::Table(rows)));
                }
            }
            _ if is_block(name) => {
                out.push(Piece::Line);
                self.pieces(children, out);
                out.push(Piece::Line);
            }
            // span, font, u, sub, sup and the rest: their content only.
            _ => self.pieces(children, out),
        }
    }

    /// A link: kept for web and mail addresses only, else its text.
    fn link(&mut self, node: &Node, children: &[Node], out: &mut Vec<Piece>) {
        let href = node.attr("href").unwrap_or("").trim().to_owned();
        let linkable = ["http://", "https://", "mailto:"].iter().any(|s| {
            href.len() > s.len()
                && href
                    .get(..s.len())
                    .is_some_and(|p| p.eq_ignore_ascii_case(s))
        });
        if linkable && !href.contains(['\n', '\r']) {
            let url = href.clone();
            let mut inner = Vec::new();
            self.pieces(children, &mut inner);
            let has_text = inner.iter().any(|p| matches!(p, Piece::Inline(_)));
            if has_text {
                group(inner, out, &|c| Inl::Link {
                    children: c,
                    url: url.clone(),
                });
            } else {
                out.push(Piece::Inline(Inl::Text(href)));
            }
        } else {
            self.pieces(children, out);
        }
    }

    /// A list and its items; text between items is an item of its own.
    fn list(&mut self, node: &Node, ordered: bool, children: &[Node], out: &mut Vec<Piece>) {
        let start = node
            .attr("start")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n <= 999_999_999)
            .unwrap_or(1);
        let mut items: Vec<Vec<Block>> = Vec::new();
        let mut loose: Vec<Piece> = Vec::new();
        for child in children {
            if let Node::Element { name, children, .. } = child
                && name == "li"
            {
                if !loose.is_empty() {
                    items.push(assemble(std::mem::take(&mut loose)));
                }
                let mut item = Vec::new();
                self.pieces(children, &mut item);
                items.push(assemble(item));
            } else {
                self.piece(child, &mut loose);
            }
        }
        if loose
            .iter()
            .any(|p| !matches!(p, Piece::Inline(Inl::Text(t)) if t.trim().is_empty()))
        {
            items.push(assemble(loose));
        }
        items.retain(|i| !i.is_empty());
        if !items.is_empty() {
            out.push(Piece::Block(Block::List {
                ordered,
                start,
                items,
            }));
        }
    }

    fn wrap(&mut self, children: &[Node], out: &mut Vec<Piece>, make: impl Fn(Vec<Inl>) -> Inl) {
        let mut inner = Vec::new();
        self.pieces(children, &mut inner);
        group(inner, out, &make);
    }

    fn image(&mut self, node: &Node, out: &mut Vec<Piece>) {
        let src = node.attr("src").unwrap_or("").trim();
        if src.is_empty() {
            return;
        }
        let alt = node
            .attr("alt")
            .map(|a| collapse(a).trim().to_owned())
            .unwrap_or_default();
        let remote = has_scheme(src) || src.starts_with("//");
        if remote {
            self.resolver.remote_image(src);
            // A data URI can be huge and says nothing; a URL is shown.
            let shown = if src
                .get(..5)
                .is_some_and(|p| p.eq_ignore_ascii_case("data:"))
            {
                alt
            } else {
                src.to_owned()
            };
            if !shown.is_empty() {
                out.push(Piece::Inline(Inl::Text(shown)));
            }
            return;
        }
        match self.resolver.media(src, MediaUse::Image) {
            Some(media) => out.push(Piece::Inline(Inl::Image { alt, media })),
            None => out.push(Piece::Inline(Inl::Text(src.to_owned()))),
        }
    }

    /// Inline content on one line (a heading, a table cell).
    fn line(&mut self, children: &[Node]) -> Vec<Inl> {
        let mut inner = Vec::new();
        self.pieces(children, &mut inner);
        let mut line = Vec::new();
        for piece in inner {
            match piece {
                Piece::Inline(i) => line.push(i),
                Piece::Block(b) => {
                    line.push(Inl::Text(format!(" {} ", super::markdown::block_text(&b))));
                }
                Piece::Line | Piece::Break | Piece::Para => line.push(Inl::Text(" ".to_owned())),
            }
        }
        line
    }

    fn rows(&mut self, nodes: &[Node], rows: &mut Vec<Vec<Vec<Inl>>>, depth: usize) {
        for node in nodes {
            let Node::Element { name, children, .. } = node else {
                continue;
            };
            if name == "tr" {
                let mut cells = Vec::new();
                for cell in children {
                    if let Node::Element { name, children, .. } = cell
                        && (name == "td" || name == "th")
                    {
                        cells.push(self.line(children));
                    }
                }
                if !cells.is_empty() {
                    rows.push(cells);
                }
            } else if depth < MAX_DEPTH && name != "table" {
                // thead, tbody, tfoot.
                self.rows(children, rows, depth + 1);
            }
        }
    }
}

/// Whether `src` is an address rather than a file name: it starts with a
/// URL scheme (`[a-zA-Z][a-zA-Z0-9+.-]*:`) followed by `//` (a web or file
/// address), or is a `data:`, `blob:` or `javascript:` URL. A file name may
/// hold a `:` elsewhere (Anki allows it on most systems): "12:30.png" and
/// "a:b.png" are names, looked up in the package.
fn has_scheme(src: &str) -> bool {
    let Some((scheme, rest)) = src.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let is_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    is_scheme
        && (rest.starts_with("//")
            || ["data", "blob", "javascript"]
                .iter()
                .any(|s| scheme.eq_ignore_ascii_case(s)))
}

/// Wraps the runs of inline pieces in `make`, keeping boundaries between
/// them (bold text with a line break in it is bold on both lines).
fn group(inner: Vec<Piece>, out: &mut Vec<Piece>, make: &dyn Fn(Vec<Inl>) -> Inl) {
    let mut run = Vec::new();
    for piece in inner {
        match piece {
            Piece::Inline(i) => run.push(i),
            other => {
                if !run.is_empty() {
                    out.push(Piece::Inline(make(std::mem::take(&mut run))));
                }
                out.push(other);
            }
        }
    }
    if !run.is_empty() {
        out.push(Piece::Inline(make(run)));
    }
}

/// The text of nodes, for code: line breaks and blocks as newlines in a
/// `<pre>` (`pre`), else spaces.
fn text_of(nodes: &[Node], pre: bool) -> String {
    let mut out = String::new();
    text_walk(nodes, pre, &mut out, 0);
    out
}

fn text_walk(nodes: &[Node], pre: bool, out: &mut String, depth: usize) {
    {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Math { source, display } => {
                    let (open, close) = if *display {
                        ("\\[", "\\]")
                    } else {
                        ("\\(", "\\)")
                    };
                    out.push_str(open);
                    out.push_str(source);
                    out.push_str(close);
                }
                // In code, a sound is its name and an image its alt text:
                // code shows neither.
                Node::Sound(name) => out.push_str(name),
                Node::Element { name, .. } if name == "img" => {
                    out.push_str(node.attr("alt").unwrap_or(""));
                }
                Node::Element { name, children, .. } => {
                    let breaks = name == "br" || is_block(name);
                    if breaks && !out.is_empty() && !out.ends_with('\n') {
                        out.push(if pre { '\n' } else { ' ' });
                    }
                    if depth < MAX_DEPTH {
                        text_walk(children, pre, out, depth + 1);
                    }
                    if breaks && is_block(name) && !out.ends_with('\n') {
                        out.push(if pre { '\n' } else { ' ' });
                    }
                }
            }
        }
    }
}

/// Pieces into blocks: inline runs become paragraphs; one line boundary
/// or line break between text is a line break, two make a new paragraph
/// (an empty `<div><br></div>` line, `<br><br>`), as are `<p>` edges;
/// boundaries at the start or end of a paragraph are dropped.
fn assemble(pieces: Vec<Piece>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<Inl> = Vec::new();
    let mut pending = 0_u8;
    let flush = |para: &mut Vec<Inl>, blocks: &mut Vec<Block>| {
        if !para.iter().all(is_blank) {
            blocks.push(Block::Para(std::mem::take(para)));
        }
        para.clear();
    };
    for piece in pieces {
        match piece {
            Piece::Inline(inline) if pending > 0 && is_blank(&inline) => {
                // Whitespace between boundaries changes nothing.
            }
            Piece::Inline(inline) => {
                let has_content = !para.iter().all(is_blank);
                if has_content && pending == 1 {
                    para.push(Inl::Break);
                } else if pending >= 2 {
                    flush(&mut para, &mut blocks);
                }
                pending = 0;
                para.push(inline);
            }
            Piece::Line => pending = pending.max(1),
            Piece::Break => pending = pending.saturating_add(1),
            Piece::Para => pending = pending.max(2),
            Piece::Block(block) => {
                flush(&mut para, &mut blocks);
                pending = 0;
                blocks.push(block);
            }
        }
    }
    flush(&mut para, &mut blocks);
    blocks
}

/// Whether an inline shows nothing but whitespace.
fn is_blank(inline: &Inl) -> bool {
    match inline {
        Inl::Text(t) => t.trim().is_empty(),
        Inl::Strong(c) | Inl::Em(c) | Inl::Strike(c) => c.iter().all(is_blank),
        _ => false,
    }
}

/// Converts a side's HTML to Markdown blocks. Math that Rappel cannot draw
/// (by the resolver) or, with `no_math`, all math stays text.
pub(super) fn to_blocks(html: &str, resolver: &mut dyn Resolver, no_math: bool) -> Vec<Block> {
    let nodes = tree(html);
    let mut converter = Converter { resolver, no_math };
    let mut pieces = Vec::new();
    converter.pieces(&nodes, &mut pieces);
    assemble(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_decode_from_the_whole_table() {
        assert_eq!(
            decode("caf&eacute; &#233;&#x1F600; &amp;&lt;&gt; &nbsp;"),
            "café é😀 &<> \u{a0}"
        );
        assert_eq!(
            decode("&notanentity; &amp &#0; &#xD800; &#99999999999;"),
            "&notanentity; & \u{FFFD} \u{FFFD} \u{FFFD}"
        );
        assert_eq!(decode("a\0b &"), "a\u{FFFD}b &");
        assert_eq!(
            decode("&Aacute;&zwj;&NotNestedGreaterGreater;"),
            "Á\u{200d}\u{2aa2}\u{338}"
        );
    }

    #[test]
    fn plain_text_drops_tags_and_scripts() {
        assert_eq!(
            plain_text("<b>a</b><br>b<div>c</div><script>x()</script>  d"),
            "a b c d"
        );
        assert_eq!(plain_text("x <style>p{}</style><!-- c --> y"), "x y");
        assert_eq!(plain_text("a < b"), "a < b");
    }

    #[test]
    fn attributes_are_read_with_any_quoting() {
        let tokens: Vec<Token> = Tokenizer::new(
            r#"<img src="a b.png" alt='x&amp;y' data-x=1 hidden/>"#,
            false,
        )
        .collect();
        assert_eq!(
            tokens,
            [Token::Start {
                name: "img".to_owned(),
                attrs: vec![
                    ("src".to_owned(), "a b.png".to_owned()),
                    ("alt".to_owned(), "x&y".to_owned()),
                    ("data-x".to_owned(), "1".to_owned()),
                    ("hidden".to_owned(), String::new()),
                ],
            }]
        );
    }

    fn depth(nodes: &[Node]) -> usize {
        nodes
            .iter()
            .map(|n| match n {
                Node::Element { children, .. } => 1 + depth(children),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let html = "<div><b>".repeat(20_000) + "x" + &"</b></div>".repeat(20_000);
        let nodes = tree(&html);
        assert!(depth(&nodes) <= MAX_DEPTH + 1);
    }

    #[test]
    fn unclosed_math_is_text() {
        let tokens: Vec<Token> = Tokenizer::new("a \\( b", true).collect();
        assert!(tokens.iter().all(|t| matches!(t, Token::Text(_))));
    }
}
