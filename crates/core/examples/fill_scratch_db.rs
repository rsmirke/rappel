//! Fills a scratch database with many cards, to check that the card list
//! stays smooth (ADR 0013), and optionally makes some of them due now, to
//! try studying without waiting days (ADR 0014).
//!
//! ```text
//! cargo run -p rappel-core --example fill_scratch_db -- /tmp/rappel-scratch [count] [--due N] [--markdown] [--math] [--tags] [--suspended N] [--media]
//! RAPPEL_DATA_DIR=/tmp/rappel-scratch cargo run -p rappel
//! ```
//!
//! It adds one deck, "Scratch (N cards)", with `count` cards (default
//! 5,000; none and no deck with 0). With `--due N`, the first N cards get a
//! review history in the past, recorded through the real scheduler, so
//! they are due today; the rest stay new. With `--markdown`, it also adds
//! the deck "Markdown (one card per construct)": new cards, one per
//! Markdown construct (ADR 0019), all studyable today, to check how they
//! look. With `--math`, it adds the deck "Math": 15 new cards, one per
//! formula of the math spike (ADR 0021: 4 sentences with inline math, then
//! 11 display formulas), and one card with invalid LaTeX. With `--tags`, the
//! cards of the first deck get tags (ADR 0029): a few names that differ by
//! accents and case, one card with a very long list, every fifth card none.
//! With `--suspended N`, the last N cards of the first deck are suspended
//! (ADR 0035), so they never overlap the first `--due N` ones. With
//! `--media`, it adds the deck "Media" (ADR 0054): cards showing small
//! images it draws itself (a square, a wide and a tall PNG), one playing a
//! one-second WAV tone, one naming a file that is not there, and one with a
//! remote image, which is shown as text; the same files every run.
//! The directory
//! must be absolute and is never one of the
//! app's own data directories, release or debug: this is for throwaway
//! data only.

use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use rappel_core::data_dir::{
    BuildProfile, DataDirEnv, LEGACY_RELEASE_DIR_NAME, ensure_data_dir, resolve_data_dir,
};
use rappel_core::domain::Grade;
use rappel_core::scheduling::{DAY_MS, STEP_MS};
use rappel_core::storage::Store;
use rappel_core::time::now_ms;

const DEFAULT_COUNT: usize = 5_000;

fn main() -> ExitCode {
    match run() {
        Ok(message) => {
            // Nothing useful is left to do if stdout is closed.
            let _ = writeln!(io::stdout(), "{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            let _ = writeln!(io::stderr(), "fill_scratch_db: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let usage = "usage: fill_scratch_db <absolute scratch dir> [count] [--due N] [--markdown] [--math] [--tags] [--suspended N] [--media]";
    let number = |arg: Option<std::ffi::OsString>| {
        arg.as_ref()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or(usage)
    };
    let mut args = std::env::args_os().skip(1);
    let dir = PathBuf::from(args.next().ok_or(usage)?);
    let (mut count, mut due, mut markdown, mut math, mut tags) = (None, 0, false, false, false);
    let (mut suspended, mut media) = (0, false);
    while let Some(arg) = args.next() {
        if arg == "--due" {
            due = number(args.next())?;
        } else if arg == "--markdown" {
            markdown = true;
        } else if arg == "--math" {
            math = true;
        } else if arg == "--tags" {
            tags = true;
        } else if arg == "--suspended" {
            suspended = number(args.next())?;
        } else if arg == "--media" {
            media = true;
        } else if count.is_none() {
            count = Some(number(Some(arg))?);
        } else {
            return Err(usage.to_owned());
        }
    }
    let count = count.unwrap_or(DEFAULT_COUNT);
    if count == 0 && !markdown && !math && !media {
        return Err("nothing to add: count is 0 and no --markdown, --math or --media".to_owned());
    }
    if due > count {
        return Err(format!("--due {due} is more than the {count} cards"));
    }
    if due.saturating_add(suspended) > count {
        return Err(format!(
            "--due {due} and --suspended {suspended} are more than the {count} cards"
        ));
    }
    if !dir.is_absolute() {
        return Err(format!("{} is not an absolute path", dir.display()));
    }
    // `..` through a folder that does not exist yet would defeat the
    // comparison below (it cannot be resolved before it is created).
    if dir.components().any(|c| c == Component::ParentDir) {
        return Err(format!("{} must not contain `..`", dir.display()));
    }
    refuse_app_data_dirs(&dir)?;

    ensure_data_dir(&dir).map_err(|e| e.to_string())?;
    let mut store = Store::open(&dir).map_err(|e| e.to_string())?;
    let mut added = Vec::new();
    if count > 0 {
        added.push(fill_main_deck(&mut store, count, due, suspended, tags)?);
    }
    if markdown {
        let deck = store
            .create_deck("Markdown (one card per construct)")
            .map_err(|e| e.to_string())?;
        for (front, back) in MARKDOWN_CARDS {
            store
                .add_card(&deck.id, front, back)
                .map_err(|e| e.to_string())?;
        }
        added.push(format!("{} cards to “{}”", MARKDOWN_CARDS.len(), deck.name));
    }
    if math {
        let deck = store.create_deck("Math").map_err(|e| e.to_string())?;
        let cards = math_cards();
        for (front, back) in &cards {
            store
                .add_card(&deck.id, front, back)
                .map_err(|e| e.to_string())?;
        }
        added.push(format!("{} cards to “{}”", cards.len(), deck.name));
    }
    if media {
        added.push(fill_media_deck(&mut store)?);
    }
    Ok(format!(
        "Added {} in {}",
        added.join(" and "),
        dir.display()
    ))
}

/// The "Media" deck for `--media`: images drawn here, a WAV tone, a
/// missing file and a remote image. Returns what to report.
fn fill_media_deck(store: &mut Store) -> Result<String, String> {
    let deck = store.create_deck("Media").map_err(|e| e.to_string())?;
    let add = |bytes: &[u8]| {
        store
            .add_media_bytes(bytes)
            .map(|m| m.reference())
            .map_err(|e| e.to_string())
    };
    let square = add(&png(64, 64)?)?;
    let wide = add(&png(600, 80)?)?;
    let tall = add(&png(80, 400)?)?;
    let large = add(&png(1600, 1000)?)?;
    let tone = add(&wav_tone())?;
    // Files that were never added: 64 zeros of hash.
    let missing = format!("media:{}.png", "0".repeat(64));
    let missing_audio = format!("media:{}.wav", "0".repeat(64));
    let cards = [
        (
            format!("![A square of colours]({square})"),
            "A 64 × 64 PNG".to_owned(),
        ),
        (
            "Which way is it wider?".to_owned(),
            format!("![A wide band of colours]({wide})\n\nWider than tall."),
        ),
        (
            format!("![A tall band of colours]({tall})"),
            "Taller than wide".to_owned(),
        ),
        (
            "What does a large picture look like?".to_owned(),
            format!("![A large gradient]({large})\n\nScaled down to the card's width."),
        ),
        (
            "What do you hear?".to_owned(),
            format!("[A 440 Hz tone]({tone})"),
        ),
        (
            format!("Listen: [the tone on the front]({tone})\n\nWhich note is it?"),
            "A (440 Hz)".to_owned(),
        ),
        (
            format!("[A sound that is not there]({missing_audio})"),
            "Shown as not available".to_owned(),
        ),
        (
            format!("![A file that is not there]({missing})"),
            "Shown as missing".to_owned(),
        ),
        (
            "![A remote image](https://example.org/cat.png)".to_owned(),
            "Shown as text, never fetched".to_owned(),
        ),
    ];
    for (front, back) in &cards {
        store
            .add_card(&deck.id, front, back)
            .map_err(|e| e.to_string())?;
    }
    Ok(format!(
        "{} cards and 5 media files to “{}”",
        cards.len(),
        deck.name
    ))
}

/// A `width` × `height` RGB PNG of a colour gradient, encoded here (an
/// `IHDR`, one zlib-compressed `IDAT` of unfiltered rows, an `IEND`).
fn png(width: u32, height: u32) -> Result<Vec<u8>, String> {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    let mut rows = Vec::new();
    for y in 0..height {
        rows.push(0); // Filter: none.
        for x in 0..width {
            let red = u8::try_from(x * 255 / width.max(1)).unwrap_or(u8::MAX);
            let green = u8::try_from(y * 255 / height.max(1)).unwrap_or(u8::MAX);
            rows.extend_from_slice(&[red, green, 160]);
        }
    }
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
    zlib.write_all(&rows).map_err(|e| e.to_string())?;
    let data = zlib.finish().map_err(|e| e.to_string())?;
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    // 8 bits per channel, RGB, deflate, no filter method, no interlace.
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, body) in [(b"IHDR", header), (b"IDAT", data), (b"IEND", Vec::new())] {
        let len = u32::try_from(body.len()).map_err(|e| e.to_string())?;
        out.extend_from_slice(&len.to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(&body);
        let crc = crc32fast::hash(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    Ok(out)
}

/// One second of a 440 Hz tone (a triangle wave), 8 kHz, 8-bit mono WAV.
fn wav_tone() -> Vec<u8> {
    const RATE: u32 = 8_000;
    let samples: Vec<u8> = (0..RATE)
        .map(|i| {
            let phase = (i * 440 * 256 / RATE) % 256;
            let rise = if phase < 128 { phase } else { 255 - phase };
            u8::try_from(68 + rise * 120 / 127).unwrap_or(128)
        })
        .collect();
    let data = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    // PCM, mono, the rate, bytes per second, block align 1, 8 bits.
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.extend_from_slice(&samples);
    out
}

/// The first deck: `count` cards, the first `due` of them with a past review
/// history, the last `suspended` of them suspended. Returns what to report.
fn fill_main_deck(
    store: &mut Store,
    count: usize,
    due: usize,
    suspended: usize,
    tags: bool,
) -> Result<String, String> {
    let deck = store
        .create_deck(&format!("Scratch ({count} cards)"))
        .map_err(|e| e.to_string())?;
    let now = now_ms();
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let (front, back) = sample(i);
        let card = store
            .add_card(&deck.id, &front, &back)
            .map_err(|e| e.to_string())?;
        ids.push(card.id.clone());
        if tags {
            for name in sample_tags(i) {
                store.add_tag(&card.id, name).map_err(|e| e.to_string())?;
            }
        }
        if i < due {
            for (grade, at) in past_reviews(i, now) {
                store
                    .record_review(&card.id, grade, Duration::from_secs(8), at)
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    store
        .set_suspended(&ids[count - suspended..], true)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "{count} cards ({due} due now, {suspended} suspended) to “{}”",
        deck.name
    ))
}

/// The tags of card `i` for `--tags`: spellings that differ by case and
/// accents, none on every fifth card, and a long list on every 50th.
fn sample_tags(i: usize) -> Vec<&'static str> {
    const NAMES: [&str; 8] = [
        "maths",
        "Économie",
        "algebra",
        "Algèbre",
        "history",
        "économie générale",
        "physics",
        "ÉCOLOGIE",
    ];
    if i % 5 == 4 {
        Vec::new()
    } else if i.is_multiple_of(50) {
        NAMES.to_vec()
    } else {
        vec![NAMES[i % NAMES.len()], NAMES[(i / 3) % NAMES.len()]]
    }
}

/// The sentences of the math spike with inline formulas (ADR 0021), formulas
/// 1 to 4. `$…$` delimits the math.
const MATH_INLINE: [&str; 4] = [
    r"Soit $f \in \mathcal{C}^1([a,b],\mathbb{R})$ et $\lVert f \rVert_\infty = \sup_{x \in [a,b]} |f(x)|$.",
    r"Le rayon de convergence vérifie $\frac{1}{R} = \limsup_{n \to +\infty} |a_n|^{1/n}$.",
    r"Le vecteur de Poynting $\overrightarrow{\Pi} = \frac{\vec{E} \wedge \vec{B}}{\mu_0}$ a pour flux la puissance rayonnée.",
    r"On a $n! \underset{n \to +\infty}{\sim} \sqrt{2\pi n}\left(\frac{n}{e}\right)^n$ (Stirling).",
];

/// The display formulas of the math spike, 5 to 15, without dollars.
const MATH_DISPLAY: [&str; 11] = [
    r"\hat{f}(\xi) = \int_{-\infty}^{+\infty} f(x)\, e^{-2i\pi x\xi} \,\mathrm{d}x",
    r"\frac{1}{2\pi}\int_{0}^{2\pi} |f(t)|^2 \,\mathrm{d}t = \sum_{n=-\infty}^{+\infty} |c_n(f)|^2",
    r"f(x) = \sum_{k=0}^{n} \frac{f^{(k)}(a)}{k!}(x-a)^k + \int_a^x \frac{(x-t)^n}{n!} f^{(n+1)}(t)\,\mathrm{d}t",
    r"\begin{aligned} \operatorname{div} \vec{E} &= \frac{\rho}{\varepsilon_0} & \overrightarrow{\operatorname{rot}}\, \vec{E} &= -\frac{\partial \vec{B}}{\partial t} \\ \operatorname{div} \vec{B} &= 0 & \overrightarrow{\operatorname{rot}}\, \vec{B} &= \mu_0 \vec{j} + \mu_0\varepsilon_0 \frac{\partial \vec{E}}{\partial t} \end{aligned}",
    r"\frac{\partial u_{\text{em}}}{\partial t} + \operatorname{div} \overrightarrow{\Pi} = -\vec{j} \cdot \vec{E}",
    r"\begin{vmatrix} 1 & x_1 & \cdots & x_1^{n-1} \\ 1 & x_2 & \cdots & x_2^{n-1} \\ \vdots & \vdots & \ddots & \vdots \\ 1 & x_n & \cdots & x_n^{n-1} \end{vmatrix} = \prod_{1 \le i < j \le n} (x_j - x_i)",
    r"\mathbb{1}_{[0,1]}(x) = \begin{cases} 1 & \text{si } x \in [0,1], \\ 0 & \text{sinon.} \end{cases}",
    r"\left| \int_a^b f g \right| \le \left( \int_a^b f^2 \right)^{\frac{1}{2}} \left( \int_a^b g^2 \right)^{\frac{1}{2}}",
    r"\oiint_{\Sigma} \vec{E} \cdot \mathrm{d}\vec{S} = \frac{Q_{\text{int}}}{\varepsilon_0} \qquad \oint_{\mathcal{C}} \vec{B} \cdot \mathrm{d}\vec{\ell} = \mu_0 I_{\text{enlacé}}",
    r"i\hbar \frac{\partial \psi}{\partial t} = -\frac{\hbar^2}{2m} \Delta\psi + V\psi \qquad \frac{1}{1 + \cfrac{1}{1 + \cfrac{1}{x}}}",
    r"\exp(A) = \sum_{k=0}^{+\infty} \frac{A^k}{k!}, \quad P = \begin{pmatrix} \cos\theta & -\sin\theta \\ \sin\theta & \cos\theta \end{pmatrix}, \quad \binom{n}{k} = \frac{n!}{k!\,(n-k)!}",
];

/// The cards of the "Math" deck: formula 1 to 15 of the spike, one per
/// card, then one with invalid LaTeX. The front is the formula, so a card
/// shows it at once; the back says what to look for.
fn math_cards() -> Vec<(String, String)> {
    let mut cards = Vec::new();
    for (i, sentence) in MATH_INLINE.iter().enumerate() {
        cards.push((
            format!("Formula {}\n\n{sentence}", i + 1),
            "Inline math sits on the text baseline, and wraps with the text at 360 px.".to_owned(),
        ));
    }
    for (i, formula) in MATH_DISPLAY.iter().enumerate() {
        let number = MATH_INLINE.len() + i + 1;
        let note = match number {
            11 => "Known limit: `\\mathbb{1}` is an upright 1 (KaTeX has no blackboard digits).",
            13 => {
                "Known limit: `\\oiint` draws the ring and the two integral signs on top of each other."
            }
            _ => "Check light, dark and 360 px: a wide formula scrolls sideways.",
        };
        cards.push((
            format!("Formula {number}\n\n$${formula}$$"),
            note.to_owned(),
        ));
    }
    cards.push((
        "Invalid LaTeX\n\nInline: $\\notacommand{x}$ and $x^$, then display:\n\n$$\\left( x$$"
            .to_owned(),
        "All three show their source in the error style, and the app carries on.".to_owned(),
    ));
    cards
}

/// One card per Markdown construct, front and back, in study order. The
/// front says what to check. At most 20, the new cards allowed per day,
/// so all of them can be studied at once.
const MARKDOWN_CARDS: [(&str, &str); 20] = [
    (
        "Inline styles: *emphasis*, **strong**, ~~struck~~, `inline code`, and ***both***.",
        "Each word looks as its name says. `code` is monospace.",
    ),
    (
        "# Heading 1\n## Heading 2\n### Heading 3\n#### Heading 4\n##### Heading 5\n###### Heading 6",
        "Six headings, each smaller than the one before (5 and 6 look alike).",
    ),
    (
        "A bullet list, nested:\n\n- one\n- two\n  - two a\n  - two b\n    - deeper\n- three",
        "Numbered, from 3:\n\n3. third\n4. fourth\n   1. nested one\n   2. nested two\n5. fifth",
    ),
    (
        "A code block:\n\n```rust\nfn main() {\n    println!(\"<b>not bold</b> & stays\");\n}\n```",
        "A very long line must wrap, not widen the window:\n\n```\nlet a_very_long_identifier_name = some_function(with, many, arguments, that, keep, going, and, going, past, the, edge);\n```",
    ),
    (
        "A block quote:\n\n> Quoted text, dimmed, with a line at the start.\n>\n> > A quote inside the quote.",
        "Text after a quote:\n\n> quoted\n\nnot quoted",
    ),
    (
        "Above the rule\n\n---\n\nBelow the rule",
        "One separator line between the two paragraphs.",
    ),
    (
        "A table:\n\n| Left | Centre | Right |\n|:-----|:------:|------:|\n| a | b | c |\n| *longer cell* | `code` | 12.50 |",
        "A wide table scrolls sideways at 360 px:\n\n| Column one | Column two | Column three | Column four | Column five | Column six |\n|--|--|--|--|--|--|\n| some text | more text | even more text | and more | still more | the end |",
    ),
    (
        "Inline math: the area is $\\pi r^2$ and $e^{i\\pi} + 1 = 0$.",
        "Shown as monospace source until math rendering comes.",
    ),
    (
        "Display math:\n\n$$\\int_0^1 x^2 \\, dx = \\frac{1}{3}$$\n\nText after it.",
        "The formula is centred, monospace, on its own line.",
    ),
    (
        "Currency is not math: $5 and $10, from $5-$10 a day, and \\$x\\$.",
        "All dollar signs show as typed; nothing is monospace.",
    ),
    (
        "A [link to example.org](https://example.org/a?b=1&c=2) and an autolink <https://gnome.org>.",
        "Hover each underlined link: its URL shows as a tooltip. Clicking does nothing.",
    ),
    (
        "A media image: ![a diagram](media:0123abcd.png) and one without alt text: ![](media:ffff.png)",
        "A remote image is never fetched and shows as typed: ![logo](https://example.org/logo.png)",
    ),
    (
        "Raw HTML shows as typed: <b>not bold</b>, <i>not italic</i>, <span foreground=\"red\">not red</span>, & and &amp;",
        "<div>\n<script>alert(1)</script>\n<img src=\"https://example.org/x.png\">\n</div>",
    ),
    (
        "This card has an empty back: after Show Answer, only Fail and Pass appear.",
        "",
    ),
    (
        "Hard line break:  \nthis is on a new line.\nThis is not (a soft break).",
        "Two paragraphs:\n\nfirst\n\nsecond",
    ),
    (
        "Deep nesting is flattened after 16 levels, keeping the text:\n\n>>>>>>>>>>>>>>>>>>>>>>>> deep quote",
        "- 1\n  - 2\n    - 3\n      - 4\n        - 5\n          - 6\n            - 7\n              - 8\n                - 9\n                  - 10\n                    - 11\n                      - 12\n                        - 13\n                          - 14\n                            - 15\n                              - 16\n                                - 17\n                                  - 18",
    ),
    (
        "A simple front: one short paragraph, like every other card.",
        "A simple back.",
    ),
    (
        "# A long mixed card\n\nScroll it at 360 px. It has a paragraph with **strong** text and a [link](https://example.org).\n\n- a list item with `code`\n- another, with $x^2$\n\n> a quote\n\n```\ncode block\n```\n\n| a | b |\n|--|--|\n| 1 | 2 |\n\n$$\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}$$\n\n---\n\nThe end of the front.",
        "## Back heading\n\nThe back can be rich too.\n\n1. first\n2. second",
    ),
    (
        "Escapes: \\*not emphasis\\*, \\`not code\\`, \\# not a heading, 1\\. not a list.",
        "All shown with the characters, without the backslashes.",
    ),
    (
        "Strikethrough and code in a heading:\n\n## ~~old~~ `new` title",
        "Rule only on the back:\n\n---",
    ),
];

/// Fails if `dir` is, or is inside, the release, debug or pre-rename data
/// directory the app would use without `RAPPEL_DATA_DIR`.
fn refuse_app_data_dirs(dir: &Path) -> Result<(), String> {
    let env = DataDirEnv {
        rappel_data_dir: None,
        ..DataDirEnv::from_process()
    };
    let mut app_dirs: Vec<PathBuf> = [BuildProfile::Release, BuildProfile::Debug]
        .into_iter()
        .filter_map(|profile| resolve_data_dir(&env, profile).ok())
        .collect();
    // Data from before the rename, until it has been moved with export/import.
    let legacy = app_dirs
        .first()
        .and_then(|release| release.parent())
        .map(|base| base.join(LEGACY_RELEASE_DIR_NAME));
    app_dirs.extend(legacy);
    for app_dir in app_dirs {
        let same = |a: &Path, b: &Path| a.starts_with(b); // a is b or inside it
        let resolved = (dir.canonicalize(), app_dir.canonicalize());
        let inside = same(dir, &app_dir)
            || matches!(&resolved, (Ok(d), Ok(a)) if same(d, a))
            || matches!(&resolved, (Err(_), Ok(a)) if same(dir, a));
        if inside {
            return Err(format!(
                "{} is the app's data directory ({}); use a scratch directory",
                dir.display(),
                app_dir.display()
            ));
        }
    }
    Ok(())
}

/// A past history after which card `i` is due today: learned 40 days ago
/// (Pass, then Pass out of the 10-minute step). Every third card also
/// lapsed 20 days ago (Fail, then Pass out of the step). FSRS gives such
/// cards intervals of days, so they are all overdue now.
fn past_reviews(i: usize, now: i64) -> Vec<(Grade, i64)> {
    let learned = now - 40 * DAY_MS;
    let mut reviews = vec![(Grade::Pass, learned), (Grade::Pass, learned + STEP_MS)];
    if i.is_multiple_of(3) {
        let lapsed = now - 20 * DAY_MS;
        reviews.extend([(Grade::Fail, lapsed), (Grade::Pass, lapsed + STEP_MS)]);
    }
    reviews
}

/// Card `i`: mostly short fronts, with some long, multi-line and
/// markup-like ones to check ellipsising and escaping.
fn sample(i: usize) -> (String, String) {
    let n = i + 1;
    let front = match i % 10 {
        0 => format!(
            "Card {n}: a long front that should not fit on one line, so the list must \
             ellipsise it at the end instead of wrapping or widening the window. {}",
            "More words. ".repeat(20)
        ),
        1 => format!("Card {n}: first line\nsecond line\n\nthird line"),
        2 => format!("Card {n}: <b>not bold</b> & <i>not italic</i> &amp; must show as typed"),
        3 => format!("Card {n}: what is $\\frac{{d}}{{dx}} x^2$?"),
        _ => format!("Card {n}: what is {n} × 7?"),
    };
    let back = format!("Answer {n}: {}", n * 7);
    (front, back)
}
