//! Cloze deletions, `{{c1::text}}` and `{{c1::text::hint}}`, nested or not
//! (ADR 0061). For card `n`, the question shows each cloze `n` as `[…]`
//! (or `[hint]`) and the others as their text; the answer shows the whole
//! text with each cloze `n` in bold. The text is the field's HTML; only
//! the cloze markers are touched.

/// How deep clozes nest at most; a deeper marker is plain text.
const MAX_NESTING: usize = 32;

/// One piece of a field's text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node<'a> {
    Text(&'a str),
    Cloze {
        n: u32,
        children: Vec<Node<'a>>,
        hint: Option<&'a str>,
    },
}

/// An opened cloze not yet closed.
struct Frame<'a> {
    n: u32,
    /// Where its marker starts, to give it back as text if it never closes.
    start: usize,
    children: Vec<Node<'a>>,
}

/// The cloze opened at `at` in `text`: its number and where its content
/// starts. `{{c` digits `::`, at most 4 digits.
fn opener(text: &str, at: usize) -> Option<(u32, usize)> {
    let rest = text[at..].strip_prefix("{{c")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 4 {
        return None;
    }
    let after = rest[digits..].strip_prefix("::")?;
    let n = rest[..digits].parse().ok()?;
    Some((n, text.len() - after.len()))
}

fn push<'a>(stack: &mut [Frame<'a>], root: &mut Vec<Node<'a>>, node: Node<'a>) {
    match stack.last_mut() {
        Some(frame) => frame.children.push(node),
        None => root.push(node),
    }
}

fn parse(text: &str) -> Vec<Node<'_>> {
    let mut root: Vec<Node<'_>> = Vec::new();
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut plain = 0;
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'{'
            && stack.len() < MAX_NESTING
            && let Some((n, content)) = opener(text, i)
        {
            if plain < i {
                push(&mut stack, &mut root, Node::Text(&text[plain..i]));
            }
            stack.push(Frame {
                n,
                start: i,
                children: Vec::new(),
            });
            i = content;
            plain = i;
            continue;
        }
        if bytes[i] == b'}' && bytes.get(i + 1) == Some(&b'}') && !stack.is_empty() {
            if plain < i {
                push(&mut stack, &mut root, Node::Text(&text[plain..i]));
            }
            if let Some(mut frame) = stack.pop() {
                // The hint is after the last "::" of the cloze's own text.
                let mut hint = None;
                if let Some(Node::Text(last)) = frame.children.last()
                    && let Some(at) = last.rfind("::")
                {
                    let (before, after) = (&last[..at], &last[at + 2..]);
                    hint = Some(after);
                    frame.children.pop();
                    if !before.is_empty() {
                        frame.children.push(Node::Text(before));
                    }
                }
                push(
                    &mut stack,
                    &mut root,
                    Node::Cloze {
                        n: frame.n,
                        children: frame.children,
                        hint,
                    },
                );
            }
            i += 2;
            plain = i;
            continue;
        }
        i += 1;
    }
    if plain < text.len() {
        push(&mut stack, &mut root, Node::Text(&text[plain..]));
    }
    // Markers never closed are text again, with what they held.
    while let Some(frame) = stack.pop() {
        let opener_end = frame.start
            + 3
            + text[frame.start + 3..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count()
            + 2;
        let mut nodes = vec![Node::Text(&text[frame.start..opener_end])];
        nodes.extend(frame.children);
        match stack.last_mut() {
            Some(parent) => parent.children.extend(nodes),
            None => root.extend(nodes),
        }
    }
    root
}

/// What [`render`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Found {
    /// Whether the text has a cloze `n`.
    pub(super) active: bool,
}

/// `text` for card `n`: on the question each cloze `n` is `[…]` or
/// `[hint]`, on the answer it is in bold; other clozes are their text.
pub(super) fn render(text: &str, n: u32, answer: bool, found: &mut Found) -> String {
    let mut out = String::with_capacity(text.len());
    write(&parse(text), n, answer, found, &mut out);
    out
}

fn write(nodes: &[Node<'_>], n: u32, answer: bool, found: &mut Found, out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Cloze {
                n: number,
                children,
                hint,
            } if *number == n => {
                found.active = true;
                if answer {
                    out.push_str("<b>");
                    write(children, n, answer, found, out);
                    out.push_str("</b>");
                } else {
                    out.push('[');
                    out.push_str(hint.unwrap_or("…"));
                    out.push(']');
                }
            }
            Node::Cloze { children, .. } => write(children, n, answer, found, out),
        }
    }
}

/// The text of each cloze `n` of `text`, joined by ", " (for
/// `{{cloze-only:…}}` and `{{type:cloze:…}}`).
pub(super) fn only(text: &str, n: u32) -> String {
    let mut parts = Vec::new();
    collect(&parse(text), n, &mut parts);
    parts.join(", ")
}

fn collect(nodes: &[Node<'_>], n: u32, parts: &mut Vec<String>) {
    for node in nodes {
        if let Node::Cloze {
            n: number,
            children,
            ..
        } = node
        {
            if *number == n {
                let mut text = String::new();
                write(children, 0, false, &mut Found::default(), &mut text);
                parts.push(text);
            } else {
                collect(children, n, parts);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str, n: u32) -> String {
        render(text, n, false, &mut Found::default())
    }

    fn a(text: &str, n: u32) -> String {
        render(text, n, true, &mut Found::default())
    }

    #[test]
    fn the_active_cloze_is_hidden_on_the_question_and_bold_on_the_answer() {
        let text = "{{c1::Paris}} is in {{c2::France::country}}; the {{c1::Seine}}.";
        assert_eq!(q(text, 1), "[…] is in France; the […].");
        assert_eq!(a(text, 1), "<b>Paris</b> is in France; the <b>Seine</b>.");
        assert_eq!(q(text, 2), "Paris is in [country]; the Seine.");
        assert_eq!(a(text, 2), "Paris is in <b>France</b>; the Seine.");
        assert_eq!(only(text, 1), "Paris, Seine");
    }

    #[test]
    fn whether_the_card_has_its_cloze_is_found() {
        let mut found = Found::default();
        render("{{c1::a}}", 3, false, &mut found);
        assert!(!found.active);
        render("{{c3::a}}", 3, false, &mut found);
        assert!(found.active);
    }

    #[test]
    fn nested_clozes_render_inside_out() {
        let text = "{{c1::a {{c2::b}} c}}";
        assert_eq!(q(text, 1), "[…]");
        assert_eq!(q(text, 2), "a […] c");
        assert_eq!(a(text, 2), "a <b>b</b> c");
        assert_eq!(a(text, 1), "<b>a b c</b>");
    }

    #[test]
    fn broken_markers_stay_text() {
        assert_eq!(q("{{c1::open", 1), "{{c1::open");
        assert_eq!(
            q("{{c::x}} {{cx::y}} {{c12345::z}}", 1),
            "{{c::x}} {{cx::y}} {{c12345::z}}"
        );
        assert_eq!(q("a }} b", 1), "a }} b");
        assert_eq!(q("{{c1::{{c2::x}}", 2), "{{c1::[…]");
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let text = "{{c1::".repeat(10_000) + &"}}".repeat(10_000);
        let out = q(&text, 1);
        assert!(out.starts_with('['));
        let out = a(&text, 2);
        assert!(out.len() <= text.len());
    }

    #[test]
    fn html_inside_clozes_is_kept() {
        assert_eq!(a("{{c1::<i>x</i>::<b>h</b>}}", 1), "<b><i>x</i></b>");
        assert_eq!(q("{{c1::<i>x</i>::<b>h</b>}}", 1), "[<b>h</b>]");
    }
}
