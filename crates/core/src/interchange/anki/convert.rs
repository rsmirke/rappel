//! One Anki card to its two Markdown sides (ADR 0061): the note type's
//! template for the card rendered with the note's fields, the answer
//! without its question part, both converted from HTML to Markdown; and
//! the report of what was left out or kept differently.

use std::collections::BTreeMap;

use super::collection::Notetype;
use super::html::{MediaUse, Resolver, to_blocks};
use super::markdown::{Block, Inl, render};
use super::template::{CardContext, CardFields, Template, TemplateError, after_answer_rule};
use super::{MAX_EXAMPLES, Skip, Skipped};
use crate::content::{self, Inline};
use crate::domain::MAX_CARD_SIDE_BYTES;
use crate::media::MediaRef;

/// The longest example, in characters.
const EXAMPLE_CHARS: usize = 80;

/// What is left out or kept differently, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Report {
    reasons: BTreeMap<Skip, (usize, Vec<String>)>,
}

impl Report {
    /// One more of `reason`, with up to a few examples.
    pub(super) fn add(&mut self, reason: Skip, examples: &[&str]) {
        self.add_n(reason, 1, examples);
    }

    /// `n` more of `reason`.
    pub(super) fn add_n(&mut self, reason: Skip, n: usize, examples: &[&str]) {
        if n == 0 {
            return;
        }
        let entry = self.reasons.entry(reason).or_default();
        entry.0 = entry.0.saturating_add(n);
        for example in examples {
            if entry.1.len() == MAX_EXAMPLES {
                break;
            }
            let example = shorten(example);
            if !example.is_empty() && !entry.1.contains(&example) {
                entry.1.push(example);
            }
        }
    }

    pub(super) fn skipped(&self) -> Vec<Skipped> {
        self.reasons
            .iter()
            .map(|(reason, (count, examples))| Skipped {
                reason: *reason,
                count: *count,
                examples: examples.clone(),
            })
            .collect()
    }
}

/// An example as one line of at most [`EXAMPLE_CHARS`] characters.
pub(super) fn shorten(text: &str) -> String {
    let line: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    if line.chars().count() > EXAMPLE_CHARS {
        let mut cut: String = line.chars().take(EXAMPLE_CHARS - 1).collect();
        cut = cut.trim_end().to_owned();
        cut.push('…');
        cut
    } else {
        line
    }
}

/// A note type's templates, parsed once: for each ord, the question and
/// the answer without its question part.
pub(super) type Parsed<'a> = Vec<Result<(Template<'a>, Template<'a>), Skip>>;

pub(super) fn parse_templates(notetype: &Notetype) -> Parsed<'_> {
    notetype
        .templates
        .iter()
        .map(|t| {
            let (q, a) = t.formats.as_ref().ok_or(Skip::BadTemplate)?;
            let a = after_answer_rule(a).unwrap_or(a);
            let parse = |s| Template::parse(s).map_err(|_: TemplateError| Skip::BadTemplate);
            Ok((parse(q)?, parse(a)?))
        })
        .collect()
}

/// What a card's template needs besides its note's fields.
#[derive(Debug, Clone, Copy)]
pub(super) struct CardInfo<'a> {
    pub(super) ord: i64,
    pub(super) tags: &'a str,
    /// The deck's name as Anki has it (`A::B`).
    pub(super) deck: &'a str,
    pub(super) flag: i64,
}

/// A card's two sides, in Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Sides {
    pub(super) front: String,
    pub(super) back: String,
}

/// Converts a card. `fields` are the note's, which must be as many as the
/// note type has. On `Err`, the card is not imported for that reason.
pub(super) fn card_sides(
    notetype: &Notetype,
    parsed: &Parsed<'_>,
    fields: &[&str],
    card: CardInfo<'_>,
    resolver: &mut dyn Resolver,
    report: &mut Report,
) -> Result<Sides, Skip> {
    if fields.len() != notetype.fields.len() {
        return Err(Skip::BrokenNote);
    }
    let index = if notetype.cloze { 0 } else { card.ord };
    let index = usize::try_from(index).map_err(|_| Skip::BrokenNote)?;
    let (question, answer) = match parsed.get(index) {
        Some(Ok(pair)) => pair,
        Some(Err(skip)) => return Err(*skip),
        None => return Err(Skip::BrokenNote),
    };
    let named: Vec<(&str, &str)> = notetype
        .fields
        .iter()
        .map(String::as_str)
        .zip(fields.iter().copied())
        .collect();
    let template_name = notetype
        .templates
        .get(index)
        .map_or("", |t| t.name.as_str());
    let cloze = u32::try_from(card.ord.saturating_add(1)).unwrap_or(0);
    // Once per card: each field's emptiness and the special fields.
    let prepared = CardFields::new(
        &named,
        card.tags,
        &notetype.name,
        card.deck,
        template_name,
        card.flag,
    );
    let context = |answer| CardContext {
        fields: &prepared,
        cloze,
        answer,
    };
    // The HTML may be larger than the Markdown it gives (tags dropped).
    let cap = MAX_CARD_SIDE_BYTES * 4;
    let q = question.render(&context(false), cap);
    let a = answer.render(&context(true), cap);
    if q.too_long || a.too_long {
        return Err(Skip::TooLong);
    }
    // A cloze card whose number no cloze of its text has is empty in Anki.
    if q.cloze_used && !q.cloze_found {
        return Err(Skip::EmptyFront);
    }
    let mut filters: Vec<&str> = q.unknown_filters.iter().map(String::as_str).collect();
    filters.extend(a.unknown_filters.iter().map(String::as_str));
    if !filters.is_empty() {
        report.add(Skip::UnknownFilter, &filters);
    }
    let mut unknown: Vec<&str> = q.unknown_fields.iter().map(String::as_str).collect();
    unknown.extend(a.unknown_fields.iter().map(String::as_str));
    if !unknown.is_empty() {
        report.add(Skip::UnknownField, &unknown);
    }

    let mut math = MathCount {
        inner: resolver,
        formulas: 0,
        as_text: 0,
    };
    let front = side(&q.html, &mut math);
    let back = side(&a.html, &mut math);
    report.add_n(Skip::MathAsText, math.as_text, &[]);
    if front.trim().is_empty() {
        return Err(Skip::EmptyFront);
    }
    if front.len() > MAX_CARD_SIDE_BYTES || back.len() > MAX_CARD_SIDE_BYTES {
        return Err(Skip::TooLong);
    }
    Ok(Sides { front, back })
}

/// One side's HTML as Markdown. If the formulas do not all come out of
/// Rappel's parser as math (one that cannot stand where it is), the side
/// is converted again with every formula as text.
fn side(html: &str, math: &mut MathCount<'_>) -> String {
    let before = (math.formulas, math.as_text);
    let blocks = to_blocks(html, math, false);
    let expected = count_math(&blocks);
    let md = render(&blocks);
    if count_parsed_math(&md) == expected {
        return md.replace('\0', "\u{FFFD}");
    }
    let formulas = math.formulas - before.0;
    math.as_text = before.1 + formulas;
    render(&to_blocks(html, math, true)).replace('\0', "\u{FFFD}")
}

/// Counts the formulas a side has and those kept as text.
struct MathCount<'a> {
    inner: &'a mut dyn Resolver,
    formulas: usize,
    as_text: usize,
}

impl Resolver for MathCount<'_> {
    fn media(&mut self, name: &str, use_: MediaUse) -> Option<MediaRef> {
        self.inner.media(name, use_)
    }

    fn remote_image(&mut self, src: &str) {
        self.inner.remote_image(src);
    }

    fn math_ok(&mut self, source: &str, display: bool) -> bool {
        self.formulas += 1;
        let ok = self.inner.math_ok(source, display);
        if !ok {
            self.as_text += 1;
        }
        ok
    }
}

/// The formulas in converted blocks.
fn count_math(blocks: &[Block]) -> usize {
    fn inlines(items: &[Inl]) -> usize {
        items
            .iter()
            .map(|i| match i {
                Inl::Math { .. } => 1,
                Inl::Strong(c) | Inl::Em(c) | Inl::Strike(c) | Inl::Link { children: c, .. } => {
                    inlines(c)
                }
                _ => 0,
            })
            .sum()
    }
    blocks
        .iter()
        .map(|b| match b {
            Block::Para(i) | Block::Heading(_, i) => inlines(i),
            Block::List { items, .. } => items.iter().map(|i| count_math(i)).sum(),
            Block::Quote(b) => count_math(b),
            Block::Table(rows) => rows.iter().flatten().map(|c| inlines(c)).sum(),
            Block::Code(_) | Block::Rule => 0,
        })
        .sum()
}

/// The formulas Rappel's parser finds in `md`.
fn count_parsed_math(md: &str) -> usize {
    fn inlines(items: &[Inline]) -> usize {
        items
            .iter()
            .map(|i| match i {
                Inline::Math { .. } => 1,
                Inline::Emphasis(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Link { content: c, .. }
                | Inline::Audio { content: c, .. } => inlines(c),
                _ => 0,
            })
            .sum()
    }
    fn blocks(items: &[content::Block]) -> usize {
        items
            .iter()
            .map(|b| match b {
                content::Block::Paragraph(i) | content::Block::Heading { content: i, .. } => {
                    inlines(i)
                }
                content::Block::List { items, .. } => items.iter().map(|i| blocks(i)).sum(),
                content::Block::BlockQuote(b) => blocks(b),
                content::Block::Table { header, rows, .. } => header
                    .iter()
                    .chain(rows.iter().flatten())
                    .map(|c| inlines(c))
                    .sum(),
                content::Block::DisplayMath(_) => 1,
                content::Block::CodeBlock { .. } | content::Block::Rule => 0,
            })
            .sum()
    }
    blocks(&content::parse(md).blocks)
}

/// Whether Rappel draws `source` as math: Rappel's Markdown reads it back
/// as exactly that formula, and the math parser (`RaTeX`, `KaTeX`'s language)
/// accepts it. A panic inside the parser counts as no.
pub(super) fn math_drawable(source: &str, display: bool) -> bool {
    // The app's own limit (render::math::MAX_SOURCE_BYTES).
    if source.len() > 4096 || (!display && source.contains(['\n', '\r'])) {
        return false;
    }
    let md = if display {
        format!("$${source}$$")
    } else {
        format!("${source}$")
    };
    let doc = content::parse(&md);
    let read_back = match doc.blocks.as_slice() {
        [content::Block::DisplayMath(s)] => display && s == source,
        [content::Block::Paragraph(items)] => matches!(
            items.as_slice(),
            [Inline::Math { source: s, display: d }] if s == source && *d == display
        ),
        _ => false,
    };
    read_back && std::panic::catch_unwind(|| ratex_parser::parse(source).is_ok()).unwrap_or(false)
}
