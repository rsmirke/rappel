//! Documents as plain text, without Markdown syntax: for screen readers
//! and the card list's one-line previews.

use super::{Block, Document, Inline, parse};
use crate::domain::search_key;

/// The longest [`plain_summary`], in characters. A label lays out all of
/// its text even when it ellipsises, so a huge front is cut before it gets
/// there; more than this never fits on one line anyway.
pub const PREVIEW_CHARS: usize = 200;

/// The text of `doc`: blocks on their own lines, links as their text,
/// images as their alt text, math and code as their source.
#[must_use]
pub fn plain_text(doc: &Document) -> String {
    let mut out = String::new();
    push_blocks_text(&doc.blocks, &mut out);
    out
}

/// What the search index holds for one side (ADR 0035): the [`plain_text`]
/// of `md`, in lower case without accents ([`search_key`]), so a query
/// folded the same way matches what the user reads and never the Markdown
/// syntax.
#[must_use]
pub fn search_text(md: &str) -> String {
    search_key(&plain_text(&parse(md)))
}

/// How much of a side [`plain_summary`] parses, in bytes. 200 characters
/// of text never need more source than this in practice, and a deck of
/// large cards still opens quickly.
const SUMMARY_SOURCE_BYTES: usize = 16 * 1024;

/// `md` on one line, for a preview: its [`plain_text`] with every run of
/// whitespace (newlines included) as one space, the ends trimmed, and cut
/// to `PREVIEW_CHARS` characters. Only the first
/// `SUMMARY_SOURCE_BYTES` of `md` are parsed, so the preview of a
/// longer side may show syntax the cut split (a lone `**` or `\`).
#[must_use]
pub fn plain_summary(md: &str) -> String {
    let mut end = md.len().min(SUMMARY_SOURCE_BYTES);
    while !md.is_char_boundary(end) {
        end -= 1;
    }
    one_line_summary(&plain_text(&parse(&md[..end])))
}

/// `text` (a [`plain_text`]) on one line, cut to `PREVIEW_CHARS`
/// characters.
#[must_use]
pub fn one_line_summary(text: &str) -> String {
    let mut out = String::new();
    let mut chars = 0;
    for (i, word) in text.split_whitespace().enumerate() {
        if i > 0 {
            if chars + 1 >= PREVIEW_CHARS {
                break;
            }
            out.push(' ');
            chars += 1;
        }
        // Never copies more than the preview needs, even of a huge word.
        let room = PREVIEW_CHARS - chars;
        if let Some((cut, _)) = word.char_indices().nth(room) {
            out.push_str(&word[..cut]);
            break;
        }
        out.push_str(word);
        chars += word.chars().count();
        if chars >= PREVIEW_CHARS {
            break;
        }
    }
    out
}

fn push_blocks_text(blocks: &[Block], out: &mut String) {
    push_lines(blocks, out, push_block_text);
}

/// Pushes each of `parts` with `push`, on its own line. A part that adds
/// no text (a rule) adds no empty line either.
fn push_lines<T>(parts: &[T], out: &mut String, push: impl Fn(&T, &mut String)) {
    let mut wrote = false;
    for part in parts {
        let mark = out.len();
        if wrote {
            out.push('\n');
        }
        let start = out.len();
        push(part, out);
        if out.len() == start {
            out.truncate(mark);
        } else {
            wrote = true;
        }
    }
}

pub(super) fn push_block_text(block: &Block, out: &mut String) {
    match block {
        Block::Paragraph(inlines)
        | Block::Heading {
            content: inlines, ..
        } => {
            push_inlines_text(inlines, out);
        }
        Block::List { items, .. } => push_lines(items, out, |item, out| {
            push_blocks_text(item, out);
        }),
        Block::CodeBlock { code, .. } => out.push_str(code),
        Block::BlockQuote(blocks) => push_blocks_text(blocks, out),
        Block::Rule => {}
        Block::Table { header, rows, .. } => {
            for (i, row) in std::iter::once(header).chain(rows).enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                for (j, cell) in row.iter().enumerate() {
                    if j > 0 {
                        out.push(' ');
                    }
                    push_inlines_text(cell, out);
                }
            }
        }
        Block::DisplayMath(source) => out.push_str(source),
    }
}

fn push_inlines_text(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        push_inline_text(inline, out);
    }
}

pub(super) fn push_inline_text(inline: &Inline, out: &mut String) {
    match inline {
        Inline::Text(text) | Inline::Code(text) | Inline::Math { source: text, .. } => {
            out.push_str(text);
        }
        Inline::Emphasis(inlines)
        | Inline::Strong(inlines)
        | Inline::Strikethrough(inlines)
        | Inline::Link {
            content: inlines, ..
        }
        | Inline::Audio {
            content: inlines, ..
        } => push_inlines_text(inlines, out),
        Inline::Image { alt, .. } => out.push_str(alt),
        Inline::LineBreak => out.push('\n'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_and_newlines_become_single_spaces() {
        assert_eq!(one_line_summary("  a\n\nb\t c  \r\n"), "a b c");
        assert_eq!(one_line_summary("one line"), "one line");
        assert_eq!(one_line_summary(""), "");
        assert_eq!(one_line_summary(" \n "), "");
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let preview = one_line_summary(&"é".repeat(1_000));
        assert_eq!(preview.chars().count(), PREVIEW_CHARS);
        let preview = one_line_summary(&"漢字 ".repeat(1_000));
        assert_eq!(preview.chars().count(), PREVIEW_CHARS);
        assert!(preview.starts_with("漢字 漢字"));
        // A combining accent counts as its own character.
        let preview = one_line_summary(&"e\u{301}".repeat(150));
        assert_eq!(preview.chars().count(), PREVIEW_CHARS);
    }

    #[test]
    fn short_text_is_not_cut() {
        let text = "word ".repeat(39);
        assert_eq!(one_line_summary(&text), text.trim_end());
    }
}
