//! HTML to Markdown, pinned: what each construct becomes, and that the
//! result reads back in Rappel's Markdown as meant (escapes hold, math is
//! math, media are media).

use super::super::convert::math_drawable;
use super::super::html::{MediaUse, Resolver, to_blocks};
use super::super::markdown::render;
use crate::content::{self, Block, Inline, plain_text};
use crate::media::{MediaKind, MediaRef};
use crate::test_support::within;

/// Knows `a.png` and `s.mp3`; notes everything else.
#[derive(Default)]
struct Stub {
    missing: Vec<String>,
    remote: Vec<String>,
}

const PNG: MediaRef = MediaRef::new([1; 32], MediaKind::Png);
const MP3: MediaRef = MediaRef::new([2; 32], MediaKind::Mp3);

impl Resolver for Stub {
    fn media(&mut self, name: &str, use_: MediaUse) -> Option<MediaRef> {
        match (name, use_) {
            ("a.png", MediaUse::Image) => Some(PNG),
            ("s.mp3", MediaUse::Audio) => Some(MP3),
            _ => {
                self.missing.push(name.to_owned());
                None
            }
        }
    }

    fn remote_image(&mut self, src: &str) {
        self.remote.push(src.to_owned());
    }

    fn math_ok(&mut self, source: &str, display: bool) -> bool {
        math_drawable(source, display)
    }
}

/// What Rappel shows of `md`, as text.
fn shown(md: &str) -> String {
    plain_text(&content::parse(md))
}

fn md(html: &str) -> String {
    render(&to_blocks(html, &mut Stub::default(), false))
}

#[test]
fn text_formatting() {
    assert_eq!(
        md("<b>bold</b> <strong>s</strong> <i>it</i> <em>e</em> <u>u</u>"),
        "**bold** **s** *it* *e* u"
    );
    assert_eq!(
        md("<s>gone</s> <del>d</del> <sub>2</sub><sup>3</sup>"),
        "~~gone~~ ~~d~~ 23"
    );
    assert_eq!(md("<b> spaced </b>x"), " **spaced** x".trim_start());
    assert_eq!(md("<b><b>twice</b></b>"), "**twice**");
    assert_eq!(md("<b><i>both</i></b>"), "***both***");
    assert_eq!(
        md("<span style=\"color:red\"><font color=blue>plain</font></span>"),
        "plain"
    );
}

#[test]
fn lines_and_paragraphs() {
    assert_eq!(md("a<br>b"), "a\\\nb");
    assert_eq!(md("a<br/><br>b"), "a\n\nb");
    assert_eq!(md("<div>a</div><div>b</div>"), "a\\\nb");
    assert_eq!(md("<div>a</div><div><br></div><div>b</div>"), "a\n\nb");
    assert_eq!(md("<p>one</p><p>two</p>"), "one\n\ntwo");
    assert_eq!(md("<br>lead and trail<br><div></div>"), "lead and trail");
    assert_eq!(md("<b>a<br>b</b>"), "**a**\\\n**b**");
    assert_eq!(md("   many    spaces \n\t here  "), "many spaces here");
}

#[test]
fn lists_quotes_rules_headings_tables() {
    assert_eq!(
        md("<ul><li>a</li><li>b<ul><li>c</li></ul></li></ul>"),
        "- a\n- b\n  - c"
    );
    assert_eq!(
        md("<ul><li>a<p>b</p></li><li>c</li></ul>"),
        "- a\n\n  b\n\n- c"
    );
    assert_eq!(md("<ol start=3><li>x<li>y</ol>"), "3. x\n4. y");
    assert_eq!(md("<blockquote>q<br>r</blockquote>"), "> q\\\n> r");
    assert_eq!(md("a<hr>b"), "a\n\n---\n\nb");
    assert_eq!(md("<h2>Title</h2>text"), "## Title\n\ntext");
    assert_eq!(
        md("<table><tr><th>a</th><th>b|c</th></tr><tr><td>1</td></tr></table>"),
        "| a | b\\|c |\n| --- | --- |\n| 1 |  |"
    );
}

#[test]
fn code() {
    assert_eq!(md("use <code>a_b*</code>"), "use `a_b*`");
    assert_eq!(md("<code>`tick`</code>"), "`` `tick` ``");
    assert_eq!(
        md("<pre>line 1\n  *line* 2<br>3</pre>"),
        "```\nline 1\n  *line* 2\n3\n```"
    );
    assert_eq!(md("<pre>```</pre>"), "````\n```\n````");
}

#[test]
fn markdown_characters_are_escaped() {
    let text = "* not a list\n# not a heading\n1. not a number\n- no\n+ no\n> no\n= no";
    let html = text.replace('\n', "<br>");
    let out = md(&html);
    // Each line stays one line of text.
    let doc = content::parse(&out);
    assert_eq!(doc.blocks.len(), 1, "{out}");
    let text_shown = shown(&out);
    assert_eq!(
        text_shown.replace('\n', " "),
        text.replace('\n', " "),
        "{out}"
    );
    for sample in [
        "a*b*c",
        "_x_",
        "`c`",
        "[l](u)",
        "<b>",
        "$5 and $6",
        "~~s~~",
        "a|b",
        "&amp;",
        "\\(no",
        "!x",
        "\\",
        "[^1]",
        "***",
        "1) one",
        "  4 spaces",
        "<!-- x -->",
    ] {
        let escaped = crate::interchange::anki::html::decode(sample);
        let out = md(&super::super::template::html_escape(&escaped));
        assert_eq!(
            shown(&out),
            escaped.split_whitespace().collect::<Vec<_>>().join(" "),
            "{sample:?} → {out:?}"
        );
    }
}

#[test]
fn links_are_kept_as_links_only_for_web_and_mail() {
    assert_eq!(
        md("<a href=\"https://x.org/a b\">site</a>"),
        "[site](<https://x.org/a b>)"
    );
    assert_eq!(
        md("<a href=\"mailto:a@b.c\">mail</a>"),
        "[mail](<mailto:a@b.c>)"
    );
    assert_eq!(md("<a href=\"javascript:alert(1)\">js</a>"), "js");
    assert_eq!(md("<a href=\"file:///etc/passwd\">f</a>"), "f");
    assert_eq!(md("<a href=\"https://x.org\"></a>"), "https://x.org");
    let doc = content::parse(&md("<a href=\"https://x.org/(a)\">t</a>"));
    let Block::Paragraph(items) = &doc.blocks[0] else {
        panic!()
    };
    assert!(matches!(&items[0], Inline::Link { url, .. } if url == "https://x.org/(a)"));
}

#[test]
fn media_become_references_and_nothing_is_fetched() {
    let mut stub = Stub::default();
    let out = render(&to_blocks(
        "<img src=\"a.png\" alt=\"an [alt]\"> [sound:s.mp3] <img src=\"https://x.org/p.png\"> \
         <img src=\"data:image/png;base64,AAAA\" alt=\"inline\"> <img src=\"b.svg\"> [sound:a.png]",
        &mut stub,
        false,
    ));
    assert_eq!(
        out,
        format!(
            "![an \\[alt\\]]({}) [s.mp3]({}) https://x.org/p.png inline b.svg a.png",
            PNG.reference(),
            MP3.reference()
        )
    );
    assert_eq!(stub.missing, ["b.svg", "a.png"]);
    assert_eq!(
        stub.remote,
        ["https://x.org/p.png", "data:image/png;base64,AAAA"]
    );
    let refs = content::media_refs(&out);
    assert_eq!(refs, [PNG, MP3]);
}

#[test]
fn math_becomes_dollars_when_rappel_can_draw_it() {
    assert_eq!(md("\\(x^2\\)"), "$x^2$");
    assert_eq!(md("a \\(x &lt; y\\) b"), "a $x < y$ b");
    assert_eq!(md("\\[\\frac{1}{2}\\]"), "$$\\frac{1}{2}$$");
    assert_eq!(md("\\(a<br>b\\)"), "$a b$");
    // Not LaTeX RaTeX reads: kept as text, escaped.
    assert_eq!(md("\\(\\notacommand{\\)"), "\\\\(\\\\notacommand{\\\\)");
    assert_eq!(md("\\( \\)"), "\\\\(\\\\)");
    // A dollar inside math cannot be math in Markdown.
    let out = md("\\(\\text{$}\\)");
    assert!(!out.contains("$\\text"), "{out}");
    assert!(math_drawable("x", false));
    assert!(!math_drawable(&"x".repeat(5000), false));
}

#[test]
fn script_and_style_are_dropped_with_their_content() {
    assert_eq!(
        md("a<script>alert('x')</script>b<style>p{color:red}</style>c"),
        "abc"
    );
    assert_eq!(md("a<SCRIPT type=x>no</SCRIPT >b"), "ab");
    assert_eq!(md("a<script>never closed"), "a");
    assert_eq!(md("<!-- comment -->a<!doctype html><?xml?>b"), "ab");
    assert_eq!(md("<img src=x onerror=alert(1)>"), "x");
}

#[test]
fn hostile_html_converts_in_bounded_time_and_stack() {
    within(std::time::Duration::from_secs(4), "deep HTML", || {
        let deep = "<div><ul><li><b>".repeat(5_000) + "x" + &"</b></li></ul></div>".repeat(5_000);
        let out = md(&deep);
        assert!(out.contains('x'));
        // Rappel's own parser reads it, bounded too.
        let _ = content::parse(&out);
        let unclosed = "<i><b><s><a href='https://x'>".repeat(5_000) + "y";
        assert!(md(&unclosed).contains('y'));
        let tags = "<".repeat(100_000) + &"<a ".repeat(50_000) + &"&".repeat(100_000);
        let _ = md(&tags);
        let math = "\\(".repeat(100_000);
        let _ = md(&math);
        let sounds = "[sound:".repeat(100_000);
        let _ = md(&sounds);
        let scripts = "<script>".repeat(20_000);
        let _ = md(&scripts);
    });
}

#[test]
fn every_output_reads_back_as_its_text() {
    // Random HTML-ish input: the Markdown's plain text is the input's
    // text (no character became markup).
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let pieces = [
        "a", " ", "*", "_", "`", "[", "]", "(", ")", "#", "-", "+", "1.", "$", "\\", "|", "~", "&",
        "<b>", "</b>", "<i>", "</i>", "<br>", "<div>", "</div>", "&lt;", "&amp;", "!", ">", "=",
        "<s>", "</s>", "<p>", "</p>", "1)", "\t", "**", "é", ".", ",", ":", "'", "\"", "{", "}",
    ];
    for _ in 0..10_000 {
        let mut html = String::new();
        for _ in 0..12 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            html.push_str(pieces[usize::try_from(state % pieces.len() as u64).unwrap()]);
        }
        if html.contains("\\(") || html.contains("\\[") {
            // Math, which is meant to change.
            continue;
        }
        let out = md(&html);
        let expected =
            super::super::html::plain_text(&html.replace("<br>", " ").replace("<div>", " "));
        // Line breaks may differ (a stray `</div>`), never the characters.
        let got: String = shown(&out).split_whitespace().collect();
        let expected: String = expected.split_whitespace().collect();
        assert_eq!(got, expected, "{html:?} → {out:?}");
    }
}
