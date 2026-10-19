//! A small renderer of Anki card templates (ADR 0061): `{{Field}}`,
//! `{{#Field}}…{{/Field}}`, `{{^Field}}…{{/Field}}`, the special fields
//! (`FrontSide`, `Tags`, `Type`, `Deck`, `Subdeck`, `Card`, `CardFlag`)
//! and the filters `cloze`, `cloze-only`, `type`, `hint`, `text`,
//! `furigana`, `kana`, `kanji`, `tts` and `nc`. Another filter shows the
//! field as it is and is reported. Templates are untrusted: nesting is
//! bounded, and the output stops at a cap.

use std::borrow::Cow;

use super::cloze;

/// How deep sections nest at most.
const MAX_DEPTH: usize = 32;

/// The parsed template is not one Anki would render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TemplateError {
    /// A section closed that is not open, or never closed.
    Unbalanced,
    /// Sections nest deeper than [`MAX_DEPTH`].
    TooDeep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node<'a> {
    Text(&'a str),
    /// `{{filters:Field}}`: the filters as written, outermost first.
    Replace {
        filters: Vec<&'a str>,
        field: &'a str,
    },
    Section {
        field: &'a str,
        negated: bool,
        children: Vec<Node<'a>>,
    },
}

/// A template, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Template<'a> {
    nodes: Vec<Node<'a>>,
}

impl<'a> Template<'a> {
    /// Parses `source`.
    pub(super) fn parse(source: &'a str) -> Result<Self, TemplateError> {
        // The open sections: (field, negated, children so far).
        let mut stack: Vec<(&str, bool, Vec<Node<'a>>)> = Vec::new();
        let mut nodes: Vec<Node<'a>> = Vec::new();
        let mut rest = source;
        while !rest.is_empty() {
            let Some(open) = rest.find("{{") else {
                push(&mut stack, &mut nodes, Node::Text(rest));
                break;
            };
            let Some(close) = rest[open + 2..].find("}}") else {
                push(&mut stack, &mut nodes, Node::Text(rest));
                break;
            };
            if open > 0 {
                push(&mut stack, &mut nodes, Node::Text(&rest[..open]));
            }
            let tag = rest[open + 2..open + 2 + close].trim();
            rest = &rest[open + 2 + close + 2..];
            if let Some(field) = tag.strip_prefix('#') {
                if stack.len() == MAX_DEPTH {
                    return Err(TemplateError::TooDeep);
                }
                stack.push((field.trim(), false, Vec::new()));
            } else if let Some(field) = tag.strip_prefix('^') {
                if stack.len() == MAX_DEPTH {
                    return Err(TemplateError::TooDeep);
                }
                stack.push((field.trim(), true, Vec::new()));
            } else if let Some(field) = tag.strip_prefix('/') {
                let (open_field, negated, children) =
                    stack.pop().ok_or(TemplateError::Unbalanced)?;
                if open_field != field.trim() {
                    return Err(TemplateError::Unbalanced);
                }
                push(
                    &mut stack,
                    &mut nodes,
                    Node::Section {
                        field: open_field,
                        negated,
                        children,
                    },
                );
            } else {
                let mut parts: Vec<&str> = tag.split(':').map(str::trim).collect();
                let field = parts.pop().unwrap_or("");
                push(
                    &mut stack,
                    &mut nodes,
                    Node::Replace {
                        filters: parts,
                        field,
                    },
                );
            }
        }
        if !stack.is_empty() {
            return Err(TemplateError::Unbalanced);
        }
        Ok(Self { nodes })
    }

    /// Renders the template for `card`. The output stops once it is
    /// longer than `cap` bytes, or once the work done passes four times
    /// that (the bytes each node reads or writes): then
    /// [`Rendered::too_long`]. A template of many nodes over a large field
    /// so costs at most that much, whatever it asks.
    pub(super) fn render(&self, card: &CardContext<'_>, cap: usize) -> Rendered {
        let mut out = Rendered {
            work: cap.saturating_mul(4),
            ..Rendered::default()
        };
        render(&self.nodes, card, cap, &mut out);
        out
    }
}

fn push<'a>(stack: &mut [(&str, bool, Vec<Node<'a>>)], nodes: &mut Vec<Node<'a>>, node: Node<'a>) {
    match stack.last_mut() {
        Some((_, _, children)) => children.push(node),
        None => nodes.push(node),
    }
}

/// A card's fields as its templates see them, prepared once per card: each
/// field's value (HTML) and whether it is empty, and the special fields'
/// values.
#[derive(Debug, Clone)]
pub(super) struct CardFields<'a> {
    fields: Vec<(&'a str, &'a str, bool)>,
    /// `Tags`, `Type`, `Deck`, `Subdeck`, `Card`, `CardFlag`, `FrontSide`,
    /// escaped, with whether each is empty.
    specials: [(&'static str, String, bool); 7],
}

impl<'a> CardFields<'a> {
    /// `fields` are the note's (names and values), `tags` its tags as Anki
    /// writes them, `notetype` its note type's name, `deck` the deck's name
    /// as Anki writes it (`A::B`), `card` the template's name, `flag` the
    /// card's flag (0 for none).
    pub(super) fn new(
        fields: &[(&'a str, &'a str)],
        tags: &str,
        notetype: &str,
        deck: &str,
        card: &str,
        flag: i64,
    ) -> Self {
        let special = |name, value: String| {
            let empty = is_empty_field(&value);
            (name, value, empty)
        };
        Self {
            fields: fields
                .iter()
                .map(|(n, v)| (*n, *v, is_empty_field(v)))
                .collect(),
            specials: [
                special("Tags", html_escape(tags.trim())),
                special("Type", html_escape(notetype)),
                special("Deck", html_escape(deck)),
                special(
                    "Subdeck",
                    html_escape(deck.rsplit("::").next().unwrap_or(deck)),
                ),
                special("Card", html_escape(card)),
                special(
                    "CardFlag",
                    if flag > 0 {
                        format!("flag{flag}")
                    } else {
                        String::new()
                    },
                ),
                special("FrontSide", String::new()),
            ],
        }
    }

    /// The value of the field or special field `name`, and whether it is
    /// empty, without copying it.
    fn get(&self, name: &str) -> Option<(&str, bool)> {
        if let Some((_, v, e)) = self.specials.iter().find(|(n, _, _)| *n == name) {
            return Some((v, *e));
        }
        self.fields
            .iter()
            .find(|(n, _, _)| *n == name)
            .or_else(|| {
                self.fields
                    .iter()
                    .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
            })
            .map(|(_, v, e)| (*v, *e))
    }
}

/// What a card's template sees.
#[derive(Debug, Clone, Copy)]
pub(super) struct CardContext<'a> {
    pub(super) fields: &'a CardFields<'a>,
    /// The cloze number of this card (its template ord plus one).
    pub(super) cloze: u32,
    /// Rendering the answer (else the question).
    pub(super) answer: bool,
}

/// A rendered template.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Rendered {
    pub(super) html: String,
    /// Filters this renderer does not know, each once.
    pub(super) unknown_filters: Vec<String>,
    /// Fields the note type does not have, each once.
    pub(super) unknown_fields: Vec<String>,
    /// Whether a `cloze` filter was used, and whether it found the
    /// card's cloze.
    pub(super) cloze_used: bool,
    pub(super) cloze_found: bool,
    /// The output reached the cap, or the work its budget, and was cut.
    pub(super) too_long: bool,
    /// The work left (bytes read or written).
    work: usize,
}

impl Rendered {
    fn push(&mut self, text: &str, cap: usize) {
        if !self.charge(text.len()) {
            return;
        }
        if self.html.len() + text.len() > cap {
            self.too_long = true;
            return;
        }
        self.html.push_str(text);
    }

    /// Takes `bytes` of work from the budget; `false` (and cut) once it is
    /// spent.
    fn charge(&mut self, bytes: usize) -> bool {
        if self.too_long {
            return false;
        }
        if let Some(left) = self.work.checked_sub(bytes.max(1)) {
            self.work = left;
            true
        } else {
            self.too_long = true;
            false
        }
    }
}

fn add_once(list: &mut Vec<String>, item: &str) {
    if list.len() < 16 && !list.iter().any(|i| i == item) {
        list.push(item.to_owned());
    }
}

fn render(nodes: &[Node<'_>], card: &CardContext<'_>, cap: usize, out: &mut Rendered) {
    for node in nodes {
        if out.too_long {
            return;
        }
        match node {
            Node::Text(t) => out.push(t, cap),
            Node::Section {
                field,
                negated,
                children,
            } => {
                if !out.charge(1) {
                    return;
                }
                let filled = card.fields.get(field).is_some_and(|(_, empty)| !empty);
                if filled != *negated {
                    render(children, card, cap, out);
                }
            }
            Node::Replace { filters, field } => {
                if let Some(text) = replacement(filters, field, card, out) {
                    out.push(&text, cap);
                }
            }
        }
    }
}

/// The value of `{{filters:field}}`, borrowed when no filter changes it;
/// `None` once the work budget is spent.
fn replacement<'v>(
    filters: &[&str],
    field: &str,
    card: &CardContext<'v>,
    out: &mut Rendered,
) -> Option<Cow<'v, str>> {
    let Some((value, _)) = card.fields.get(field) else {
        if !field.is_empty() {
            add_once(&mut out.unknown_fields, field);
        }
        return Some(Cow::Borrowed(""));
    };
    // `type:` asks for an answer to be typed: nothing on the question, the
    // expected answer on the answer.
    if filters.contains(&"type") {
        if !card.answer {
            return Some(Cow::Borrowed(""));
        }
        if filters.contains(&"cloze") {
            if !out.charge(value.len()) {
                return None;
            }
            return Some(Cow::Owned(cloze::only(value, card.cloze)));
        }
        return Some(Cow::Borrowed(value));
    }
    // Filters apply from the field outwards: `text:cloze:Text` is cloze,
    // then text. Each that reads the value is charged for it.
    let mut value: Cow<'v, str> = Cow::Borrowed(value);
    for filter in filters.iter().rev() {
        let reads = !matches!(*filter, "hint" | "nc") && !filter.starts_with("tts");
        if reads && is_known(filter) && !out.charge(value.len()) {
            return None;
        }
        value = match *filter {
            "cloze" => {
                out.cloze_used = true;
                let mut found = cloze::Found::default();
                let v = cloze::render(&value, card.cloze, card.answer, &mut found);
                out.cloze_found |= found.active;
                Cow::Owned(v)
            }
            "cloze-only" => Cow::Owned(cloze::only(&value, card.cloze)),
            "text" => Cow::Owned(html_escape(&super::html::plain_text(&value))),
            "furigana" => Cow::Owned(ruby(&value, Ruby::Both)),
            "kana" => Cow::Owned(ruby(&value, Ruby::Reading)),
            "kanji" => Cow::Owned(ruby(&value, Ruby::Base)),
            // Shown as they are: a hint is shown plainly (Rappel has no
            // "show hint" button), text to speech and "no combining" only
            // matter to Anki's audio and search.
            "hint" | "nc" => value,
            other if other.starts_with("tts") => value,
            other => {
                add_once(&mut out.unknown_filters, other);
                value
            }
        };
    }
    Some(value)
}

/// Whether `filter` is one that changes the value (and so reads it).
fn is_known(filter: &str) -> bool {
    matches!(
        filter,
        "cloze" | "cloze-only" | "text" | "furigana" | "kana" | "kanji"
    )
}

/// Whether a field counts as empty for `{{#…}}`: only whitespace, `<br>`
/// and `<div>` tags, as Anki decides.
pub(super) fn is_empty_field(value: &str) -> bool {
    const TAGS: [&str; 5] = ["<br>", "<br/>", "<br />", "<div>", "</div>"];
    let lower = value.to_ascii_lowercase();
    let mut rest = lower.as_str();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return true;
        }
        match TAGS.iter().find(|t| rest.starts_with(**t)) {
            Some(tag) => rest = &rest[tag.len()..],
            None => return false,
        }
    }
}

/// HTML-escapes text for the template's output.
pub(super) fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ruby {
    /// `base(reading)`.
    Both,
    Reading,
    Base,
}

/// Anki's ruby notation, `漢字[かんじ]` (the base runs back to a space or
/// `>`, a space before it is dropped): the base, the reading, or both.
fn ruby(text: &str, keep: Ruby) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|c| open + c) else {
            break;
        };
        let reading = &rest[open + 1..close];
        let before = &rest[..open];
        let base_start = before.rfind([' ', '>']).map_or(0, |i| i + 1);
        let base = &before[base_start..];
        if base.is_empty() || reading.is_empty() {
            out.push_str(&rest[..=open]);
            rest = &rest[open + 1..];
            continue;
        }
        let mut lead = &before[..base_start];
        if let Some(stripped) = lead.strip_suffix(' ') {
            lead = stripped;
        }
        out.push_str(lead);
        match keep {
            Ruby::Both => {
                out.push_str(base);
                out.push('(');
                out.push_str(reading);
                out.push(')');
            }
            Ruby::Reading => out.push_str(reading),
            Ruby::Base => out.push_str(base),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The part of an answer template after `<hr id=answer>` (any quoting,
/// any case), which is what Rappel shows below the question; `None` if it
/// has none (then the whole template is the answer, without
/// `{{FrontSide}}`, which renders as nothing).
pub(super) fn after_answer_rule(template: &str) -> Option<&str> {
    let lower = template.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<hr").map(|i| from + i) {
        let end = lower[at..].find('>').map(|i| at + i)?;
        let attrs: String = lower[at + 3..end]
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ' ' | '/'))
            .collect();
        if attrs.contains("id=answer") {
            return Some(&template[end + 1..]);
        }
        from = end + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared<'a>(fields: &[(&'a str, &'a str)]) -> CardFields<'a> {
        CardFields::new(
            fields,
            " geo::europe leech ",
            "Basic",
            "A::B::C",
            "Card 1",
            0,
        )
    }

    fn card<'a>(fields: &'a CardFields<'a>, answer: bool) -> CardContext<'a> {
        CardContext {
            fields,
            cloze: 1,
            answer,
        }
    }

    fn render_str(template: &str, fields: &[(&str, &str)], answer: bool) -> Rendered {
        let prepared = prepared(fields);
        Template::parse(template)
            .unwrap()
            .render(&card(&prepared, answer), 1 << 20)
    }

    #[test]
    fn fields_and_sections_render() {
        let fields = [
            ("Front", "<b>F</b>"),
            ("Back", "B"),
            ("Empty", " <br> <div></div>"),
        ];
        let out = render_str(
            "{{Front}}{{#Back}} has {{Back}}{{/Back}}{{^Empty}} empty{{/Empty}}{{#Empty}}no{{/Empty}}",
            &fields,
            false,
        );
        assert_eq!(out.html, "<b>F</b> has B empty");
        assert_eq!(out.unknown_fields.len(), 0);
    }

    #[test]
    fn special_fields_render() {
        let out = render_str(
            "{{Tags}}|{{Type}}|{{Deck}}|{{Subdeck}}|{{Card}}|{{CardFlag}}|{{FrontSide}}",
            &[],
            false,
        );
        assert_eq!(out.html, "geo::europe leech|Basic|A::B::C|C|Card 1||");
    }

    #[test]
    fn filters_apply_from_the_field_outwards() {
        let fields = [("Text", "{{c1::<i>Paris</i>}} &amp; {{c2::Lyon}}")];
        let q = render_str("{{cloze:Text}}", &fields, false);
        assert_eq!(q.html, "[…] &amp; Lyon");
        assert!(q.cloze_used && q.cloze_found);
        let a = render_str("{{text:cloze:Text}}", &fields, true);
        assert_eq!(a.html, "Paris &amp; Lyon");
        assert_eq!(
            render_str("{{cloze-only:Text}}", &fields, true).html,
            "<i>Paris</i>"
        );
    }

    #[test]
    fn type_shows_nothing_on_the_question_and_the_answer_on_the_answer() {
        let fields = [("Back", "Rome"), ("Text", "{{c1::a}} {{c1::b}}")];
        assert_eq!(render_str("{{type:Back}}", &fields, false).html, "");
        assert_eq!(render_str("{{type:Back}}", &fields, true).html, "Rome");
        assert_eq!(
            render_str("{{type:cloze:Text}}", &fields, true).html,
            "a, b"
        );
    }

    #[test]
    fn unknown_filters_and_fields_are_reported() {
        let fields = [("Word", "x")];
        let out = render_str(
            "{{myaddon:Word}} {{Nope}} {{hint:Word}} {{tts en_US:Word}}",
            &fields,
            false,
        );
        assert_eq!(out.html, "x  x x");
        assert_eq!(out.unknown_filters, ["myaddon"]);
        assert_eq!(out.unknown_fields, ["Nope"]);
    }

    #[test]
    fn ruby_filters() {
        let fields = [("W", "日本語[にほんご] 漢字[かんじ]です")];
        assert_eq!(
            render_str("{{kanji:W}}", &fields, false).html,
            "日本語漢字です"
        );
        assert_eq!(
            render_str("{{kana:W}}", &fields, false).html,
            "にほんごかんじです"
        );
        assert_eq!(
            render_str("{{furigana:W}}", &fields, false).html,
            "日本語(にほんご)漢字(かんじ)です"
        );
    }

    #[test]
    fn malformed_templates_are_errors() {
        assert_eq!(Template::parse("{{#A}}x"), Err(TemplateError::Unbalanced));
        assert_eq!(Template::parse("{{/A}}"), Err(TemplateError::Unbalanced));
        assert_eq!(
            Template::parse("{{#A}}{{/B}}"),
            Err(TemplateError::Unbalanced)
        );
        let deep = "{{#A}}".repeat(100) + &"{{/A}}".repeat(100);
        assert_eq!(Template::parse(&deep), Err(TemplateError::TooDeep));
        // Unclosed braces are text.
        assert_eq!(render_str("a {{b", &[], false).html, "a {{b");
    }

    #[test]
    fn the_output_stops_at_the_cap() {
        let big = "x".repeat(1000);
        let fields = [("F", big.as_str())];
        let template = "{{F}}".repeat(10_000);
        let prepared = prepared(&fields);
        let out = Template::parse(&template)
            .unwrap()
            .render(&card(&prepared, false), 50_000);
        assert!(out.too_long);
        assert!(out.html.len() <= 50_000);
    }

    #[test]
    fn the_answer_rule_ends_the_question_part() {
        assert_eq!(
            after_answer_rule("{{FrontSide}}\n<hr id=answer>\n{{Back}}"),
            Some("\n{{Back}}")
        );
        assert_eq!(after_answer_rule("x<HR ID=\"answer\" />y"), Some("y"));
        assert_eq!(after_answer_rule("x<hr>y<hr id='answer'>z"), Some("z"));
        assert_eq!(after_answer_rule("{{FrontSide}}<hr>{{Back}}"), None);
        assert_eq!(after_answer_rule("<hr id=answer"), None);
    }

    #[test]
    fn empty_fields_are_whitespace_and_breaks_only() {
        assert!(is_empty_field(""));
        assert!(is_empty_field(" <br/> <DIV></div>\n"));
        assert!(!is_empty_field("&nbsp;"));
        assert!(!is_empty_field("<img src=x>"));
    }
}
