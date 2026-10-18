use proptest::prelude::*;

use super::*;

fn text(s: &str) -> Inline {
    Inline::Text(s.to_owned())
}

fn para(inlines: Vec<Inline>) -> Block {
    Block::Paragraph(inlines)
}

fn blocks(md: &str) -> Vec<Block> {
    parse(md).blocks
}

/// The deepest container nesting in `blocks` (a paragraph of text is 1).
fn depth(blocks: &[Block]) -> usize {
    blocks.iter().map(block_depth).max().unwrap_or(0)
}

fn block_depth(block: &Block) -> usize {
    1 + match block {
        Block::Paragraph(inlines)
        | Block::Heading {
            content: inlines, ..
        } => inlines_depth(inlines),
        Block::List { items, .. } => items.iter().map(|item| 1 + depth(item)).max().unwrap_or(0),
        Block::BlockQuote(blocks) => depth(blocks),
        Block::Table { header, rows, .. } => std::iter::once(header)
            .chain(rows)
            .flatten()
            .map(|cell| 2 + inlines_depth(cell))
            .max()
            .unwrap_or(0),
        Block::CodeBlock { .. } | Block::Rule | Block::DisplayMath(_) => 0,
    }
}

fn inlines_depth(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Emphasis(inner)
            | Inline::Strong(inner)
            | Inline::Strikethrough(inner)
            | Inline::Link { content: inner, .. }
            | Inline::Audio { content: inner, .. } => 1 + inlines_depth(inner),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn paragraphs_and_breaks() {
    assert_eq!(
        blocks("one\ntwo  \nthree\n\nfour"),
        [
            para(vec![text("one two"), Inline::LineBreak, text("three")]),
            para(vec![text("four")]),
        ]
    );
}

#[test]
fn blank_sides_are_empty() {
    assert!(parse("").is_empty());
    assert!(parse("  \n\t\n ").is_empty());
    assert!(!parse("x").is_empty());
    assert!(!parse("---").is_empty());
}

#[test]
fn headings_have_their_level() {
    assert_eq!(
        blocks("# One\n###### Six *it*"),
        [
            Block::Heading {
                level: 1,
                content: vec![text("One")]
            },
            Block::Heading {
                level: 6,
                content: vec![text("Six "), Inline::Emphasis(vec![text("it")])]
            },
        ]
    );
}

#[test]
fn emphasis_strong_strikethrough_and_code() {
    assert_eq!(
        blocks("*a* **b** ~~c~~ `d*e*`"),
        [para(vec![
            Inline::Emphasis(vec![text("a")]),
            text(" "),
            Inline::Strong(vec![text("b")]),
            text(" "),
            Inline::Strikethrough(vec![text("c")]),
            text(" "),
            Inline::Code("d*e*".to_owned()),
        ])]
    );
}

#[test]
fn lists_nest() {
    assert_eq!(
        blocks("- a\n  - b\n- c\n\n3. x\n4. y"),
        [
            Block::List {
                start: None,
                items: vec![
                    vec![
                        para(vec![text("a")]),
                        Block::List {
                            start: None,
                            items: vec![vec![para(vec![text("b")])]],
                        },
                    ],
                    vec![para(vec![text("c")])],
                ],
            },
            Block::List {
                start: Some(3),
                items: vec![vec![para(vec![text("x")])], vec![para(vec![text("y")])]],
            },
        ]
    );
}

#[test]
fn code_blocks_keep_their_text() {
    assert_eq!(
        blocks("```rust ignore\nfn main() {}\n  <b>&\n```\n\n    indented"),
        [
            Block::CodeBlock {
                lang: Some("rust".to_owned()),
                code: "fn main() {}\n  <b>&".to_owned(),
            },
            Block::CodeBlock {
                lang: None,
                code: "indented".to_owned(),
            },
        ]
    );
}

#[test]
fn block_quotes_nest() {
    assert_eq!(
        blocks("> a\n>\n> > b"),
        [Block::BlockQuote(vec![
            para(vec![text("a")]),
            Block::BlockQuote(vec![para(vec![text("b")])]),
        ])]
    );
}

#[test]
fn rules() {
    assert_eq!(
        blocks("a\n\n---\n\nb"),
        [para(vec![text("a")]), Block::Rule, para(vec![text("b")])]
    );
}

#[test]
fn tables_have_alignment_header_and_rows() {
    assert_eq!(
        blocks("| a | b | c |\n|:--|:-:|--:|\n| *1* | 2 |\n"),
        [Block::Table {
            align: vec![Align::Left, Align::Center, Align::Right],
            header: vec![vec![text("a")], vec![text("b")], vec![text("c")]],
            rows: vec![vec![
                vec![Inline::Emphasis(vec![text("1")])],
                vec![text("2")],
                vec![],
            ]],
        }]
    );
}

#[test]
fn links_keep_text_and_url() {
    assert_eq!(
        blocks("[the *docs*](https://example.org/a?b=1&c) and <https://x.org>"),
        [para(vec![
            Inline::Link {
                content: vec![text("the "), Inline::Emphasis(vec![text("docs")])],
                url: "https://example.org/a?b=1&c".to_owned(),
            },
            text(" and "),
            Inline::Link {
                content: vec![text("https://x.org")],
                url: "https://x.org".to_owned(),
            },
        ])]
    );
}

/// 64 hex digits for media references in tests.
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn media(ext: &str) -> MediaRef {
    MediaRef::parse(&format!("media:{HASH}.{ext}")).unwrap()
}

#[test]
fn images_are_media_references_or_other() {
    let md = format!(
        "![a *cat*](media:{HASH}.png) ![remote](https://x.org/i.png) ![bad](media:ab12.png) \
         ![upper](media:{}.png) ![sound](media:{HASH}.mp3) ![svg](media:{HASH}.svg)",
        HASH.to_uppercase()
    );
    let image = |alt: &str, target| Inline::Image {
        alt: alt.to_owned(),
        target,
    };
    assert_eq!(
        blocks(&md),
        [para(vec![
            image("a cat", ImageTarget::Media(media("png"))),
            text(" "),
            image(
                "remote",
                ImageTarget::Other("https://x.org/i.png".to_owned())
            ),
            text(" "),
            image("bad", ImageTarget::Other("media:ab12.png".to_owned())),
            text(" "),
            image(
                "upper",
                ImageTarget::Other(format!("media:{}.png", HASH.to_uppercase()))
            ),
            text(" "),
            // An audio file is not an image: shown as text.
            image("sound", ImageTarget::Other(format!("media:{HASH}.mp3"))),
            text(" "),
            image("svg", ImageTarget::Other(format!("media:{HASH}.svg"))),
        ])]
    );
}

#[test]
fn links_to_audio_files_are_audio() {
    let md = format!(
        "[play *me*](media:{HASH}.ogg) [](media:{HASH}.wav) [pic](media:{HASH}.png) \
         [web](https://x.org/a.mp3) [path](file:///tmp/a.mp3)"
    );
    assert_eq!(
        blocks(&md),
        [para(vec![
            Inline::Audio {
                content: vec![text("play "), Inline::Emphasis(vec![text("me")])],
                media: media("ogg"),
            },
            text(" "),
            Inline::Audio {
                content: vec![],
                media: media("wav"),
            },
            text(" "),
            // A link to an image stays a link (its URL shown, never opened).
            Inline::Link {
                content: vec![text("pic")],
                url: format!("media:{HASH}.png"),
            },
            text(" "),
            Inline::Link {
                content: vec![text("web")],
                url: "https://x.org/a.mp3".to_owned(),
            },
            text(" "),
            Inline::Link {
                content: vec![text("path")],
                url: "file:///tmp/a.mp3".to_owned(),
            },
        ])]
    );
    assert_eq!(plain_text(&parse(&md)), "play me  pic web path");
}

#[test]
fn media_refs_lists_each_file_once_in_order() {
    let other = "f".repeat(64);
    let md = format!(
        "![a](media:{HASH}.png)\n\n> - [x](media:{other}.mp3) ![b](media:{HASH}.png)\n\n\
         | ![c](media:{other}.jpg) |\n|--|\n| [](media:{HASH}.png) |\n\n\
         ![sound as image](media:{HASH}.wav) [image as link](media:{other}.gif) \
         ![no](https://x.org/a.png) `![code](media:{HASH}.webp)` ![bad](media:{HASH}.PNG)"
    );
    let at = |hash: &str, ext: &str| MediaRef::parse(&format!("media:{hash}.{ext}")).unwrap();
    assert_eq!(
        media_refs(&md),
        [
            at(HASH, "png"),
            at(&other, "mp3"),
            at(&other, "jpg"),
            at(HASH, "wav"),
            at(&other, "gif"),
        ]
    );
    assert_eq!(media_refs(""), []);
    assert_eq!(media_refs("media:abc ![x](media:)"), []);
}

/// Hostile sides: references by the thousand, nested deep, or almost
/// right; listed within a time bound on a small stack.
#[test]
fn media_refs_is_cheap_on_hostile_sides() {
    let one = format!("![a](media:{HASH}.png)");
    let many = (0..20_000).fold(String::new(), |mut md, i| {
        use std::fmt::Write as _;
        let _ = write!(md, "![](media:{i:064x}.png) ");
        md
    });
    let nested = format!("{}{one}", "> ".repeat(10_000));
    let shallow = format!("{}{one}", "> ".repeat(8));
    let near = format!("![](media:{HASH}.pn) ").repeat(20_000);
    let links = format!("[{}](media:{HASH}.ogg)", "[".repeat(50_000));
    let refs = crate::test_support::within(
        std::time::Duration::from_secs(20),
        "hostile media references",
        move || {
            [
                media_refs(&many).len(),
                media_refs(&nested).len(),
                media_refs(&shallow).len(),
                media_refs(&near).len(),
                media_refs(&links).len(),
            ]
        },
    );
    // Past the nesting cap an image is flattened into text, as it is
    // shown: not a reference (ADR 0019, 0054).
    assert_eq!(refs, [20_000, 0, 1, 0, 1]);
}

#[test]
fn inline_math() {
    assert_eq!(
        blocks(r"Area $\pi r^2$, done"),
        [para(vec![
            text("Area "),
            Inline::Math {
                source: r"\pi r^2".to_owned(),
                display: false,
            },
            text(", done"),
        ])]
    );
}

#[test]
fn display_math_is_a_block() {
    assert_eq!(
        blocks("Before\n$$\\int_0^1 x\\,dx$$\nafter"),
        [
            para(vec![text("Before")]),
            Block::DisplayMath(r"\int_0^1 x\,dx".to_owned()),
            para(vec![text("after")]),
        ]
    );
    assert_eq!(blocks("$$x$$"), [Block::DisplayMath("x".to_owned())]);
    assert_eq!(
        blocks("- $$x$$"),
        [Block::List {
            start: None,
            items: vec![vec![Block::DisplayMath("x".to_owned())]],
        }]
    );
}

#[test]
fn display_math_in_a_heading_stays_inline() {
    assert_eq!(
        blocks("# $$x$$"),
        [Block::Heading {
            level: 1,
            content: vec![Inline::Math {
                source: "x".to_owned(),
                display: true,
            }],
        }]
    );
}

#[test]
fn dollars_as_currency_stay_text() {
    for md in [
        "$5 and $10",
        "costs $5.",
        "$5-$10",
        "from $5-$10 a day",
        r"\$x\$",
        r"\$5 and \$10",
        "$ x $",
        "a $ sign",
    ] {
        let doc = parse(md);
        assert_eq!(doc.blocks.len(), 1, "{md}");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("{md}: {doc:?}");
        };
        assert!(
            inlines.iter().all(|i| matches!(i, Inline::Text(_))),
            "{md}: {inlines:?}"
        );
        assert_eq!(plain_text(&doc), md.replace('\\', ""), "{md}");
    }
}

#[test]
fn currency_beside_real_math() {
    assert_eq!(
        blocks("$5-$10 for $x$"),
        [para(vec![
            text("$5-$10 for "),
            Inline::Math {
                source: "x".to_owned(),
                display: false,
            },
        ])]
    );
}

#[test]
fn raw_html_stays_literal() {
    assert_eq!(
        blocks("a <b>bold</b> & <script>alert(1)</script> &amp; &lt;i&gt;"),
        [para(vec![text(
            "a <b>bold</b> & <script>alert(1)</script> & <i>"
        )])]
    );
    assert_eq!(
        blocks("<div onclick=\"x()\">\n<img src=\"https://x.org/t.png\">\n</div>\n\nnext"),
        [
            para(vec![
                text("<div onclick=\"x()\">"),
                Inline::LineBreak,
                text("<img src=\"https://x.org/t.png\">"),
                Inline::LineBreak,
                text("</div>"),
            ]),
            para(vec![text("next")]),
        ]
    );
    assert_eq!(
        blocks("<a href=\"https://x.org\">x</a>"),
        [para(vec![text("<a href=\"https://x.org\">x</a>")])]
    );
    assert_eq!(
        blocks("<!-- hidden? -->"),
        [para(vec![text("<!-- hidden? -->")])]
    );
}

#[test]
fn nul_becomes_the_replacement_character() {
    let md = "a\0b `c\0` $d\0$\n\n```\ne\0\n```\n\n<p>\0</p>";
    let plain = plain_text(&parse(md));
    assert!(!plain.contains('\0'));
    assert_eq!(plain.matches('\u{FFFD}').count(), 5, "{plain}");
}

#[test]
fn deep_nesting_is_capped_and_keeps_the_text() {
    let quotes = format!("{} deep", ">".repeat(10_000));
    let doc = parse(&quotes);
    assert!(depth(&doc.blocks) <= MAX_DEPTH + 2);
    assert_eq!(plain_text(&doc), "deep");

    let mut list = String::new();
    for i in 0..100 {
        list.push_str(&"  ".repeat(i));
        list.push_str("- item ");
        list.push_str(&i.to_string());
        list.push('\n');
    }
    list.push_str("\nend");
    let doc = parse(&list);
    assert!(depth(&doc.blocks) <= MAX_DEPTH + 2);
    let plain = plain_text(&doc);
    for i in 0..100 {
        assert!(
            plain.contains(&format!("item {i}\n")),
            "item {i} lost: {plain}"
        );
    }

    let stars = format!("{}x{}", "*a ".repeat(500), " b*".repeat(500));
    let doc = parse(&stars);
    assert!(depth(&doc.blocks) <= MAX_DEPTH + 2);
    assert_eq!(plain_text(&doc).matches('a').count(), 500);
}

#[test]
fn flattened_blocks_keep_their_lines_and_cells() {
    let deep = "> ".repeat(MAX_DEPTH + 4);
    let md = format!(
        "{deep}```\n{deep}alpha\n{deep}beta\n{deep}```\n{deep}\n\
         {deep}<div>\n{deep}gamma\n{deep}</div>\n{deep}\n\
         {deep}| a | b |\n{deep}|--|--|\n{deep}| c | d |\n{deep}\n\
         {deep}![alt *x*](media:a.png) [link *y*](u)"
    );
    let doc = parse(&md);
    assert!(depth(&doc.blocks) <= MAX_DEPTH + 2);
    let plain = plain_text(&doc);
    let lines: Vec<&str> = plain.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        [
            "alpha",
            "beta",
            "<div>",
            "gamma",
            "</div>",
            "a b",
            "c d",
            "alt x link y"
        ],
        "{plain:?}"
    );
}

#[test]
fn many_leading_hard_breaks_are_fast() {
    // Quadratic before: minutes for a 1 MiB side.
    let md = format!("- \\\n{}b", "\\\n".repeat(300_000));
    let (used, ()) = crate::test_support::cpu_time_of(|| {
        assert_eq!(plain_text(&parse(&md)), "b");
        let md = format!("$$x$$\\\n{}b", "\\\n".repeat(300_000));
        assert_eq!(plain_text(&parse(&md)), "x\nb");
    });
    // Generous, for slow CI machines; the quadratic version took minutes.
    // CPU time, checked against the machine's slowdown (ADR 0049).
    crate::test_support::assert_cpu_within(
        used,
        std::time::Duration::from_secs(20),
        "leading hard breaks",
    );
}

#[test]
fn summary_parses_a_bounded_prefix() {
    let md = format!("**start** {}", "word ".repeat(1_000_000));
    assert!(plain_summary(&md).starts_with("start word word"));
    // A cut inside a multibyte character is moved to its start.
    let md = "é".repeat(20_000);
    assert_eq!(plain_summary(&md).chars().count(), PREVIEW_CHARS);
    assert_eq!(one_line_summary(" a\n b "), "a b");
}

#[test]
fn plain_text_has_no_syntax() {
    let md = "# Title\n\n**bold** and *it* `code` [link](https://x.org) ![alt](media:a.png) \
              $x^2$\n\n- one\n- two\n\n> quote\n\n---\n\n| h1 | h2 |\n|--|--|\n| c1 | c2 |\n\n$$y$$";
    assert_eq!(
        plain_text(&parse(md)),
        "Title\nbold and it code link alt x^2\none\ntwo\nquote\nh1 h2\nc1 c2\ny"
    );
    assert_eq!(
        plain_summary(md),
        "Title bold and it code link alt x^2 one two quote h1 h2 c1 c2 y"
    );
}

#[test]
fn summary_keeps_html_as_typed() {
    assert_eq!(plain_summary("<b>x</b> & y"), "<b>x</b> & y");
}

#[test]
fn summary_is_cut() {
    let summary = plain_summary(&format!("**{}**", "word ".repeat(1_000).trim_end()));
    assert_eq!(summary.chars().count(), PREVIEW_CHARS - 1);
    assert!(!summary.contains('*'));
}

/// Text made mostly of Markdown syntax, so that random input hits the
/// parser's corner cases.
fn markdownish() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("*".to_owned()),
        Just("_".to_owned()),
        Just("~~".to_owned()),
        Just("`".to_owned()),
        Just("```".to_owned()),
        Just("$".to_owned()),
        Just("$$".to_owned()),
        Just("\\".to_owned()),
        Just("[".to_owned()),
        Just("](".to_owned()),
        Just(")".to_owned()),
        Just("![".to_owned()),
        Just("<".to_owned()),
        Just(">".to_owned()),
        Just("|".to_owned()),
        Just("#".to_owned()),
        Just("- ".to_owned()),
        Just("1. ".to_owned()),
        Just("---".to_owned()),
        Just("&".to_owned()),
        Just("\n".to_owned()),
        Just("\n\n".to_owned()),
        Just("  ".to_owned()),
        Just("media:".to_owned()),
        Just("<b>".to_owned()),
        Just("5".to_owned()),
        "[a-z]{1,4}",
        any::<String>(),
    ];
    prop::collection::vec(piece, 0..200).prop_map(|pieces| pieces.concat())
}

proptest! {
    #[test]
    fn parsing_never_panics(md in markdownish()) {
        let doc = parse(&md);
        prop_assert!(depth(&doc.blocks) <= MAX_DEPTH + 2);
        let summary = plain_summary(&md);
        prop_assert!(!summary.contains(['\n', '\r']));
        prop_assert!(summary.chars().count() <= PREVIEW_CHARS);
        prop_assert_eq!(summary.trim(), summary.as_str());
    }

    #[test]
    fn parsing_arbitrary_text_never_panics(md in any::<String>()) {
        prop_assert!(!plain_text(&parse(&md)).contains('\0'));
    }

    #[test]
    fn deep_nesting_never_panics(
        prefix in prop::collection::vec(
            prop_oneof![Just("> "), Just("- "), Just("  "), Just("1. "), Just("*"), Just("[")],
            0..300,
        ),
        tail in markdownish(),
    ) {
        let md = format!("{}{tail}", prefix.concat());
        let doc = parse(&md);
        prop_assert!(depth(&doc.blocks) <= MAX_DEPTH + 2);
    }
}

/// About 1 MiB, the largest card side.
const MIB: usize = 1 << 20;

/// `unit` repeated to about 1 MiB.
fn mib_of(unit: &str) -> String {
    unit.repeat(MIB / unit.len())
}

/// Pathological card sides of about 1 MiB each: deep nesting, long runs of
/// delimiters that never close, huge tables, one huge line.
fn hostile_sides() -> Vec<(&'static str, String)> {
    let mut nested_lists = String::new();
    for i in 0..1_000 {
        nested_lists.push_str(&"  ".repeat(i % 400));
        nested_lists.push_str("- x\n");
    }
    vec![
        ("block quotes", format!("{}x", ">".repeat(MIB))),
        ("spaced block quotes", format!("{}x", mib_of("> "))),
        ("list markers", format!("{}x", mib_of("- "))),
        ("ordered list markers", format!("{}x", mib_of("1. "))),
        (
            "indented lists",
            nested_lists.repeat(MIB / nested_lists.len()),
        ),
        ("open brackets", mib_of("[")),
        ("open image brackets", mib_of("![")),
        ("bracket pairs", mib_of("[a](")),
        (
            "nested links",
            format!("{}x{}", "[".repeat(MIB / 4), "](u)".repeat(MIB / 16)),
        ),
        ("stars", mib_of("*")),
        ("alternating emphasis", mib_of("*_")),
        ("emphasis openers", mib_of("*a ")),
        ("underscores in words", mib_of("a_")),
        ("tildes", mib_of("~")),
        ("backticks", mib_of("`")),
        (
            "growing backtick runs",
            (1..1_400).map(|n| "`".repeat(n) + "x").collect(),
        ),
        ("dollars", mib_of("$")),
        ("double dollars", mib_of("$$")),
        ("dollar text", mib_of("$a")),
        ("display math lines", mib_of("$$\n")),
        ("less-than", mib_of("<")),
        ("html openers", mib_of("<a ")),
        ("html blocks", mib_of("<div>\n")),
        ("autolinks", mib_of("<http://a")),
        ("entities", mib_of("&#")),
        ("backslashes", mib_of("\\")),
        ("hard breaks", mib_of("a\\\n")),
        ("headings", mib_of("# ")),
        ("setext", mib_of("a\n=\n")),
        ("rules", mib_of("---\n")),
        (
            "table columns",
            format!(
                "{}\n{}\n{}",
                mib_of("|a").chars().take(MIB / 3).collect::<String>(),
                "|-".repeat(MIB / 6),
                "|b".repeat(MIB / 6)
            ),
        ),
        ("table rows", format!("|a|b|\n|-|-|\n{}", mib_of("|c|d|\n"))),
        ("pipes", mib_of("|")),
        ("link definitions", mib_of("[a]: b\n")),
        ("link references", format!("[a]: u\n{}", mib_of("[a]"))),
        ("fenced code openers", mib_of("```\n")),
        ("indented code", mib_of("    x\n")),
        ("one long line", mib_of("word ")),
        ("blank lines", mib_of("\n")),
        ("nul characters", mib_of("\0")),
        ("replacement characters", mib_of("\u{FFFD}")),
        ("combining marks", format!("a{}", mib_of("\u{0301}"))),
        ("right-to-left overrides", mib_of("\u{202E}a")),
        ("mixed", mib_of("> - [*`$<|#\\")),
    ]
}

/// How many nodes `blocks` has, counting every block, inline, list item
/// and table cell.
fn node_count(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| {
            1 + match block {
                Block::Paragraph(inlines)
                | Block::Heading {
                    content: inlines, ..
                } => inline_count(inlines),
                Block::List { items, .. } => items.iter().map(|item| 1 + node_count(item)).sum(),
                Block::BlockQuote(blocks) => node_count(blocks),
                Block::Table { header, rows, .. } => std::iter::once(header)
                    .chain(rows)
                    .flatten()
                    .map(|cell| 1 + inline_count(cell))
                    .sum(),
                Block::CodeBlock { .. } | Block::Rule | Block::DisplayMath(_) => 0,
            }
        })
        .sum()
}

fn inline_count(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .map(|inline| {
            1 + match inline {
                Inline::Emphasis(inner)
                | Inline::Strong(inner)
                | Inline::Strikethrough(inner)
                | Inline::Link { content: inner, .. } => inline_count(inner),
                _ => 0,
            }
        })
        .sum()
}

/// The text the tree holds, in bytes: what it costs in memory besides
/// the nodes.
fn text_bytes(doc: &Document) -> usize {
    plain_text(doc).len()
}

#[test]
fn hostile_sides_parse_quickly_on_a_small_stack_and_stay_bounded() {
    for (name, md) in hostile_sides() {
        let len = md.len();
        assert!(len <= MIB + 64, "{name}: {len} bytes");
        let doc = crate::test_support::within(
            std::time::Duration::from_secs(HOSTILE_PARSE_SECS),
            name,
            move || parse(&md),
        );
        assert!(depth(&doc.blocks) <= MAX_DEPTH + 2, "{name}");
        // No blow-up: never more nodes than input bytes, and the text is
        // at most the input with each NUL (1 byte) as U+FFFD (3 bytes)
        // and a line break per block.
        let nodes = node_count(&doc.blocks);
        assert!(nodes <= len, "{name}: {nodes} nodes for {len} bytes");
        let text = text_bytes(&doc);
        assert!(text <= 3 * len + nodes, "{name}: {text} bytes of text");
    }
}

#[test]
fn hostile_sides_summarise_quickly() {
    for (name, md) in hostile_sides() {
        let summary = crate::test_support::within(
            std::time::Duration::from_secs(HOSTILE_SUMMARY_SECS),
            name,
            move || plain_summary(&md),
        );
        assert!(summary.chars().count() <= PREVIEW_CHARS, "{name}");
    }
}

/// Bound on parsing one 1 MiB hostile side in a debug test build: about
/// 10 times the slowest measured, 0.56 s (ADR 0025).
const HOSTILE_PARSE_SECS: u64 = 6;
/// Bound on the one-line summary of a hostile side, which parses only the
/// first 16 KiB.
const HOSTILE_SUMMARY_SECS: u64 = 2;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// A random run of syntax repeated to 256 KiB: the shape of the
    /// hostile sides above, found by search instead of by hand.
    #[test]
    fn repeated_random_syntax_parses_quickly_and_stays_bounded(unit in markdownish()) {
        prop_assume!(!unit.is_empty());
        let md = unit.repeat((256 * 1024) / unit.len() + 1);
        let len = md.len();
        let doc = crate::test_support::within(
            std::time::Duration::from_secs(HOSTILE_PARSE_SECS),
            "repeated random syntax",
            move || parse(&md),
        );
        prop_assert!(depth(&doc.blocks) <= MAX_DEPTH + 2);
        let nodes = node_count(&doc.blocks);
        prop_assert!(nodes <= len, "{} nodes for {} bytes", nodes, len);
        prop_assert!(text_bytes(&doc) <= 3 * len + nodes);
    }
}
