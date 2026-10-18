//! LaTeX → `RaTeX` display lists, with the limits of ADR 0021 and a cache.
//! Pure (no widgets), so everything here is tested without a display.
//!
//! Card math is untrusted. `RaTeX` has no file or network code (an unknown
//! command such as `\includegraphics` is a parse error), and `\href` /
//! `\url` only show their text: nothing here makes a link of them. What
//! `RaTeX` does accept and we must bound ourselves:
//!
//! - a source longer than [`MAX_SOURCE_BYTES`] is not parsed at all;
//! - a formula laid out larger than [`MAX_EM`] on either axis (`\rule` with
//!   a million ems) is not drawn;
//! - at most [`MAX_FORMULAS`] formulas per card side are drawn ([`Budget`]).
//!
//! In each case the formula stays visible as its source.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_types::{Color, DisplayList, MathStyle};

/// At most this many formulas are drawn per card side; later ones stay
/// source. Many small valid formulas are what can freeze the app (3.2 s for
/// a side of 262,144 `$x$`), hostile ones are cheap (ADR 0021).
pub const MAX_FORMULAS: usize = 200;

/// A formula whose source is longer than this (in bytes) is shown as source
/// without being parsed. It bounds the worst rejection (34 ms for 100,000
/// characters).
pub const MAX_SOURCE_BYTES: usize = 4096;

/// The largest width, and the largest height plus depth, of a laid-out
/// formula, in ems of the formula's own font. A 30-line `aligned` derivation of fractions
/// and sums is about 102 em tall (see the test), which fits; `\rule{1000000em}{1em}`
/// does not. Even this much is only about 2,500 px wide at body size, which
/// the display scroller handles.
pub const MAX_EM: f64 = 128.0;

/// Formulas are 1.21 × the body size, as in `KaTeX`.
pub const MATH_SCALE: f64 = 1.21;

/// How many entries the cache keeps. The oldest goes first.
const CACHE_ENTRIES: usize = 4 * MAX_FORMULAS;

/// Why a formula is not drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Not valid LaTeX (or `RaTeX` gave up): shown as source in the error
    /// style. The message is `RaTeX`'s, for the log.
    Parse(String),
    /// The source is longer than [`MAX_SOURCE_BYTES`].
    TooLong,
    /// Valid, but its layout is over [`MAX_EM`] or `RaTeX`'s own resource limit.
    TooLarge,
}

/// Lays `source` out in `color`. `display` is the display style (`$$…$$`),
/// otherwise the text style.
pub fn lay_out(source: &str, display: bool, color: Color) -> Result<DisplayList, Failure> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Failure::TooLong);
    }
    #[cfg(test)]
    LAYOUTS.with(|n| n.set(n.get() + 1));
    guarded(|| lay_out_unguarded(source, display, color))
}

/// What a panic inside `RaTeX` becomes.
const INTERNAL_ERROR: &str = "internal error in the layout";

/// Runs `f`, turning a panic into a [`Failure::Parse`]. `RaTeX` returns
/// errors for everything we know of; a panic inside it would be a bug of
/// its, and must not take the window down. This needs `panic = "unwind"`,
/// which `main.rs` enforces with a `compile_error!`.
fn guarded(f: impl FnOnce() -> Result<DisplayList, Failure>) -> Result<DisplayList, Failure> {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Failure::Parse(INTERNAL_ERROR.to_owned())))
}

fn lay_out_unguarded(source: &str, display: bool, color: Color) -> Result<DisplayList, Failure> {
    let ast = ratex_parser::parse(source).map_err(|e| Failure::Parse(e.to_string()))?;
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let options = LayoutOptions::default().with_style(style).with_color(color);
    let laid_out = layout(&ast, &options);
    validate_layout(&laid_out).map_err(|_| Failure::TooLarge)?;
    let list = to_display_list(&laid_out);
    if fits(list.width, list.height + list.depth) {
        Ok(list)
    } else {
        Err(Failure::TooLarge)
    }
}

/// Whether a formula `width` ems wide and `height` ems tall (the height
/// above the baseline plus the depth below it) is within [`MAX_EM`]. Not a
/// number (or infinity) is never within.
pub fn fits(width: f64, height: f64) -> bool {
    (0.0..=MAX_EM).contains(&width) && (0.0..=MAX_EM).contains(&height)
}

type Key = (String, bool, [u32; 4]);

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Result<Rc<DisplayList>, Failure>>,
    /// Keys, oldest first.
    order: VecDeque<Key>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
    /// How many layouts this thread did, for the tests.
    #[cfg(test)]
    static LAYOUTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn color_key(color: Color) -> [u32; 4] {
    [
        color.r.to_bits(),
        color.g.to_bits(),
        color.b.to_bits(),
        color.a.to_bits(),
    ]
}

/// The display list of `source` in `color`: [`lay_out`], remembered. Retyping
/// in the editor preview, and drawing a formula again after a save, then skip
/// the parsing and layout. Only one colour is current at a time, so a theme
/// switch lays each visible formula out once more.
pub fn formula(source: &str, display: bool, color: Color) -> Result<Rc<DisplayList>, Failure> {
    // Not stored: the key would be as large as the source.
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Failure::TooLong);
    }
    let key: Key = (source.to_owned(), display, color_key(color));
    if let Some(found) = CACHE.with(|c| c.borrow().entries.get(&key).cloned()) {
        return found;
    }
    let result = lay_out(source, display, color).map(Rc::new);
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        while cache.order.len() >= CACHE_ENTRIES {
            if let Some(oldest) = cache.order.pop_front() {
                cache.entries.remove(&oldest);
            }
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, result.clone());
    });
    result
}

/// What to show for one `$…$` or `$$…$$` of a card side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The formula, drawn.
    Formula,
    /// Invalid LaTeX: the source in monospace, in the error style.
    ErrorSource,
    /// The source in monospace: over a limit.
    Source,
}

/// Counts the formulas of one card side against [`MAX_FORMULAS`].
#[derive(Debug, Default)]
pub struct Budget {
    seen: usize,
}

impl Budget {
    /// Decides the next formula of the side, in document order. Every formula
    /// counts, valid or not, so the work stays bounded.
    pub fn place(&mut self, source: &str, display: bool) -> Placement {
        self.seen += 1;
        if self.seen > MAX_FORMULAS {
            return Placement::Source;
        }
        match formula(source, display, Color::BLACK) {
            Ok(_) => Placement::Formula,
            Err(Failure::Parse(message)) => {
                // Never the card's text: RaTeX's message can quote it.
                tracing::debug!(%message, "invalid LaTeX in a card");
                Placement::ErrorSource
            }
            Err(Failure::TooLong | Failure::TooLarge) => Placement::Source,
        }
    }
}

/// The size of the body text in pixels, from its size in Pango units
/// (points × 1024 unless `absolute`, where it is device units × 1024) and the
/// display resolution in dots per inch (`gtk-xft-dpi` / 1024, or 96 if
/// unset).
pub fn font_px(size_pango: i32, absolute: bool, dpi: f64) -> f64 {
    let size = f64::from(size_pango) / 1024.0;
    if absolute { size } else { size * dpi / 72.0 }
}

/// The resolution from the `gtk-xft-dpi` setting: 1024 × dots per inch, or
/// -1 (or nonsense) for the default, 96.
pub fn dpi_from_setting(xft_dpi: i32) -> f64 {
    if xft_dpi > 0 {
        f64::from(xft_dpi) / 1024.0
    } else {
        96.0
    }
}

/// The width of the box Pango reserves for a formula `width` pixels wide in
/// a line `line` pixels wide (-1: no limit): never wider than the line, so
/// text wraps at the card width and a wider formula scrolls in its box.
pub fn box_width(width: f64, line: i32) -> f64 {
    if line > 0 {
        width.min(f64::from(line))
    } else {
        width
    }
}

/// The left edge of a Pango rectangle, which has a negative width in
/// right-to-left text.
pub fn left_edge(x: i32, width: i32) -> i32 {
    x.min(x.saturating_add(width))
}

/// Whole pixels, rounded up so a formula is never clipped. At most half of
/// `i32::MAX`, so sums of sizes cannot overflow.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to the i32 range first"
)]
pub fn ceil_px(value: f64) -> i32 {
    if value.is_nan() {
        return 0;
    }
    value.ceil().clamp(0.0, f64::from(i32::MAX / 2)) as i32
}

/// Pango units (1/1024 pixel) for `px` pixels, rounded.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to the i32 range first"
)]
pub fn to_pango_units(px: f64) -> i32 {
    if px.is_nan() {
        return 0;
    }
    (px * 1024.0)
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Pixels for `units` Pango units.
pub fn from_pango_units(units: i32) -> f64 {
    f64::from(units) / 1024.0
}

/// A coordinate for GTK's single-precision geometry. Sizes here are a few
/// thousand pixels, which `f32` holds exactly.
#[expect(
    clippy::cast_possible_truncation,
    reason = "GTK geometry is f32; pixel values are far inside its exact range"
)]
pub fn f32_px(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use proptest::prelude::*;

    use super::*;
    use crate::testing::{SMALL_STACK, within_stack};

    /// The 4 inline sentences' formulas and the 11 display formulas of the
    /// spike (ADR 0021): real course material.
    pub const SPIKE: [(&str, bool); 16] = [
        (r"f \in \mathcal{C}^1([a,b],\mathbb{R})", false),
        (r"\lVert f \rVert_\infty = \sup_{x \in [a,b]} |f(x)|", false),
        (r"\frac{1}{R} = \limsup_{n \to +\infty} |a_n|^{1/n}", false),
        (
            r"\overrightarrow{\Pi} = \frac{\vec{E} \wedge \vec{B}}{\mu_0}",
            false,
        ),
        (
            r"n! \underset{n \to +\infty}{\sim} \sqrt{2\pi n}\left(\frac{n}{e}\right)^n",
            false,
        ),
        (
            r"\hat{f}(\xi) = \int_{-\infty}^{+\infty} f(x)\, e^{-2i\pi x\xi} \,\mathrm{d}x",
            true,
        ),
        (
            r"\frac{1}{2\pi}\int_{0}^{2\pi} |f(t)|^2 \,\mathrm{d}t = \sum_{n=-\infty}^{+\infty} |c_n(f)|^2",
            true,
        ),
        (
            r"f(x) = \sum_{k=0}^{n} \frac{f^{(k)}(a)}{k!}(x-a)^k + \int_a^x \frac{(x-t)^n}{n!} f^{(n+1)}(t)\,\mathrm{d}t",
            true,
        ),
        (
            r"\begin{aligned} \operatorname{div} \vec{E} &= \frac{\rho}{\varepsilon_0} & \overrightarrow{\operatorname{rot}}\, \vec{E} &= -\frac{\partial \vec{B}}{\partial t} \\ \operatorname{div} \vec{B} &= 0 & \overrightarrow{\operatorname{rot}}\, \vec{B} &= \mu_0 \vec{j} + \mu_0\varepsilon_0 \frac{\partial \vec{E}}{\partial t} \end{aligned}",
            true,
        ),
        (
            r"\frac{\partial u_{\text{em}}}{\partial t} + \operatorname{div} \overrightarrow{\Pi} = -\vec{j} \cdot \vec{E}",
            true,
        ),
        (
            r"\begin{vmatrix} 1 & x_1 & \cdots & x_1^{n-1} \\ 1 & x_2 & \cdots & x_2^{n-1} \\ \vdots & \vdots & \ddots & \vdots \\ 1 & x_n & \cdots & x_n^{n-1} \end{vmatrix} = \prod_{1 \le i < j \le n} (x_j - x_i)",
            true,
        ),
        (
            r"\mathbb{1}_{[0,1]}(x) = \begin{cases} 1 & \text{si } x \in [0,1], \\ 0 & \text{sinon.} \end{cases}",
            true,
        ),
        (
            r"\left| \int_a^b f g \right| \le \left( \int_a^b f^2 \right)^{\frac{1}{2}} \left( \int_a^b g^2 \right)^{\frac{1}{2}}",
            true,
        ),
        (
            r"\oiint_{\Sigma} \vec{E} \cdot \mathrm{d}\vec{S} = \frac{Q_{\text{int}}}{\varepsilon_0} \qquad \oint_{\mathcal{C}} \vec{B} \cdot \mathrm{d}\vec{\ell} = \mu_0 I_{\text{enlacé}}",
            true,
        ),
        (
            r"i\hbar \frac{\partial \psi}{\partial t} = -\frac{\hbar^2}{2m} \Delta\psi + V\psi \qquad \frac{1}{1 + \cfrac{1}{1 + \cfrac{1}{x}}}",
            true,
        ),
        (
            r"\exp(A) = \sum_{k=0}^{+\infty} \frac{A^k}{k!}, \quad P = \begin{pmatrix} \cos\theta & -\sin\theta \\ \sin\theta & \cos\theta \end{pmatrix}, \quad \binom{n}{k} = \frac{n!}{k!\,(n-k)!}",
            true,
        ),
    ];

    const INVALID: [&str; 6] = [
        r"\frac{",
        r"\notacommand{x}",
        r"x^",
        r"\begin{aligned} a & b",
        r"}}}",
        r"\left( x",
    ];

    fn black(source: &str, display: bool) -> Result<DisplayList, Failure> {
        lay_out(source, display, Color::BLACK)
    }

    #[test]
    fn the_spike_formulas_lay_out_within_the_limits() {
        for (source, display) in SPIKE {
            let list = black(source, display).unwrap_or_else(|e| panic!("{source}: {e:?}"));
            assert!(fits(list.width, list.height + list.depth), "{source}");
        }
    }

    #[test]
    fn the_scratch_math_deck_holds_the_spike_formulas() {
        // `fill_scratch_db --math` cannot share code with the app, so this
        // keeps its copy of the formulas from drifting.
        let example = include_str!("../../../core/examples/fill_scratch_db.rs");
        for (source, _) in SPIKE {
            assert!(
                example.contains(source),
                "missing from the example: {source}"
            );
        }
    }

    #[test]
    fn the_scratch_invalid_card_is_invalid_everywhere() {
        use rappel_core::content::{Block, Inline, parse};
        let doc = parse(
            "Invalid LaTeX\n\nInline: $\\notacommand{x}$ and $x^$, then display:\n\n$$\\left( x$$",
        );
        let mut budget = Budget::default();
        let mut placed = Vec::new();
        for block in &doc.blocks {
            match block {
                Block::Paragraph(inlines) => {
                    for inline in inlines {
                        if let Inline::Math { source, display } = inline {
                            placed.push(budget.place(source, *display));
                        }
                    }
                }
                Block::DisplayMath(source) => placed.push(budget.place(source, true)),
                _ => {}
            }
        }
        assert_eq!(placed, [Placement::ErrorSource; 3]);
    }

    #[test]
    fn invalid_latex_is_a_parse_failure() {
        for source in INVALID {
            assert!(
                matches!(black(source, false), Err(Failure::Parse(_))),
                "{source}"
            );
        }
        // No file is read: an unknown command is an error.
        assert!(matches!(
            black(r"\includegraphics{/etc/passwd}", true),
            Err(Failure::Parse(_))
        ));
    }

    #[test]
    fn hostile_nesting_fails_without_panicking() {
        for source in [
            "{".repeat(3_000),
            r"\frac{1}{".repeat(400),
            r"\sqrt{".repeat(500),
        ] {
            assert!(black(&source, true).is_err());
        }
    }

    #[test]
    fn a_panic_in_the_layout_is_a_parse_failure() {
        let failed = guarded(|| panic!("a bug in RaTeX"));
        assert_eq!(
            failed.err(),
            Some(Failure::Parse(INTERNAL_ERROR.to_owned()))
        );
    }

    /// The stack the attack tests lay out on. `RaTeX` stops nesting at
    /// depth 32 (`MAX_INPUT_DEPTH` in `ratex-parser`'s `stack_safety.rs`),
    /// so its stack use stops growing there, however long the source. The
    /// deepest case found, nested `\begin{matrix}`, needs 1.5 MiB in a debug
    /// build and 256 KiB in a release build, where frames are much smaller.
    /// The app lays out on the main thread, usually 8 MiB: 32 times what a
    /// release build needs. So debug tests get 2 MiB, release tests the
    /// small stack of the other attack tests (ADR 0025).
    const RATEX_STACK: usize = if cfg!(debug_assertions) {
        2 * 1024 * 1024
    } else {
        SMALL_STACK
    };

    /// Lays `source` out on [`RATEX_STACK`] within `limit`, and checks
    /// that `RaTeX` neither panicked nor drew something over the limits.
    fn lays_out_safely(source: String, limit: Duration, what: &str) {
        let result = within_stack(RATEX_STACK, limit, what, move || {
            (black(&source, true), black(&source, false), source)
        });
        let (display, text, source) = result;
        for laid_out in [display, text] {
            match laid_out {
                Ok(list) => assert!(fits(list.width, list.height + list.depth), "{source}"),
                Err(Failure::Parse(message)) => {
                    assert_ne!(message, INTERNAL_ERROR, "RaTeX panicked on {source}");
                }
                Err(Failure::TooLarge | Failure::TooLong) => {}
            }
        }
    }

    /// The longest source that is parsed, as `piece` repeated, then `end`.
    fn at_the_limit(piece: &str, end: &str) -> String {
        let room = MAX_SOURCE_BYTES - end.len();
        let mut source = piece.repeat(room / piece.len());
        source.push_str(end);
        source
    }

    /// Hand-made worst cases at the source limit, each in its display and
    /// text style. All parse and lay out in under 65 ms in a debug build
    /// (`\kern` runs are the slowest); the bound leaves 10 times that.
    #[test]
    fn hostile_sources_at_the_limit_are_safe_and_fast() {
        let sources = [
            at_the_limit("{", ""),
            at_the_limit("{x}", ""),
            at_the_limit(r"\frac{1}{", ""),
            at_the_limit(r"\frac{x}{y}", ""),
            at_the_limit(r"\sqrt{", ""),
            at_the_limit(r"\sqrt[3]{x}", ""),
            at_the_limit("x^{", ""),
            at_the_limit("x^", "x"),
            at_the_limit("x_", "x"),
            at_the_limit(r"\left(", r"\right)"),
            at_the_limit(r"\left( x \right)", ""),
            at_the_limit(r"\overbrace{", ""),
            at_the_limit(r"\begin{matrix}", ""),
            at_the_limit(r"x & y \\ ", ""),
            format!(
                r"\begin{{aligned}}{}\end{{aligned}}",
                r"x &= y \\ ".repeat(330)
            ),
            format!(r"\begin{{array}}{{{}}}x\end{{array}}", "c".repeat(4_000)),
            at_the_limit(r"\def\a{\a\a}", r"\a"),
            format!(
                r"\def\a{{xxxxxxxxxx}}{}",
                r"\def\a{\a\a\a\a\a\a\a\a\a\a}".repeat(100)
            ) + r"\a",
            at_the_limit(r"\newcommand{\b}[1]{#1#1}\b{", ""),
            at_the_limit(r"\color{red}{", ""),
            at_the_limit(r"\text{", ""),
            at_the_limit(r"\mathbb{", ""),
            at_the_limit(r"\rule{100em}{100em}", ""),
            at_the_limit(r"\hspace{100em}", ""),
            at_the_limit(r"\kern-100em", "x"),
            at_the_limit("\u{202e}\u{fffd}\u{1f600}", ""),
            at_the_limit(r"\\", ""),
        ];
        for source in sources {
            assert!(source.len() <= MAX_SOURCE_BYTES, "{source}");
            let start: String = source.chars().take(24).collect();
            let what = format!("{start}… ({} bytes)", source.len());
            lays_out_safely(source, Duration::from_millis(650), &what);
        }
    }

    /// Pieces of LaTeX syntax that random sources are made of: what nests,
    /// what expands, what takes arguments, and what ends things early.
    const PIECES: &[&str] = &[
        "{",
        "}",
        "x",
        "1",
        " ",
        "^",
        "_",
        "'",
        "&",
        r"\\",
        r"\frac",
        r"\sqrt",
        r"\sqrt[",
        "]",
        r"\left(",
        r"\right)",
        r"\left.",
        r"\right|",
        r"\middle|",
        r"\begin{aligned}",
        r"\end{aligned}",
        r"\begin{matrix}",
        r"\end{matrix}",
        r"\begin{cases}",
        r"\end{cases}",
        r"\over",
        r"\choose",
        r"\def\a{\a\a}",
        r"\a",
        r"\def\b#1{#1#1}",
        r"\b",
        "#1",
        r"\color{red}",
        r"\textcolor{blue}",
        r"\rule{50em}{1em}",
        r"\hspace{-10em}",
        r"\kern",
        r"\text{",
        r"\mathrm",
        r"\overbrace",
        r"\underbrace",
        r"\hat",
        r"\vec",
        r"\sum",
        r"\int",
        r"\limits",
        r"\displaystyle",
        r"\scriptscriptstyle",
        r"\big(",
        r"\Bigg]",
        r"\stackrel",
        r"\overset",
        r"\xrightarrow",
        r"\operatorname",
        r"\href{x}",
        r"\url{",
        r"\phantom",
        r"\boxed",
        r"\cancel",
        r"\not",
        "%",
        "$",
        "~",
        r"\",
        "\u{0}",
        "\u{202e}",
        "é",
        "∑",
        "\u{1f600}",
        "\u{fffd}",
    ];

    proptest! {
        // A hang times out after 2 s per case; without a shrink limit,
        // proptest would then shrink it for a long time, 2 s per step.
        #![proptest_config(ProptestConfig {
            cases: 128,
            max_shrink_time: 30_000,
            ..ProptestConfig::default()
        })]

        /// Random sources up to the limit, from syntax pieces. Each lays out
        /// in a few ms; the bound is for a hang.
        #[test]
        fn random_sources_are_safe(
            pieces in prop::collection::vec(prop::sample::select(PIECES), 0..1_200),
        ) {
            let mut source = String::new();
            for piece in pieces {
                if source.len() + piece.len() > MAX_SOURCE_BYTES {
                    break;
                }
                source.push_str(piece);
            }
            lays_out_safely(source, Duration::from_secs(2), "a random source");
        }

        /// One random piece of syntax repeated to the limit: the deepest
        /// nesting and the longest runs a source can have.
        #[test]
        fn repeated_random_syntax_is_safe(
            pieces in prop::collection::vec(prop::sample::select(PIECES), 1..4),
        ) {
            let source = at_the_limit(&pieces.concat(), "");
            lays_out_safely(source, Duration::from_secs(2), "repeated syntax");
        }
    }

    #[test]
    fn a_long_source_is_not_parsed() {
        let long = "x+".repeat(MAX_SOURCE_BYTES / 2 + 1);
        assert_eq!(black(&long, false).err(), Some(Failure::TooLong));
        assert_eq!(
            formula(&long, false, Color::BLACK).err(),
            Some(Failure::TooLong)
        );
        let just_fits = "x".repeat(MAX_SOURCE_BYTES);
        assert!(!matches!(black(&just_fits, false), Err(Failure::TooLong)));
    }

    #[test]
    fn huge_rules_are_too_large() {
        for source in [
            r"\rule{1000000em}{1000000em}",
            r"\rule{1em}{1000000em}",
            r"\rule{1000000em}{1em}",
            r"x\rule{200em}{1em}",
        ] {
            assert_eq!(
                black(source, true).err(),
                Some(Failure::TooLarge),
                "{source}"
            );
        }
        assert!(black(r"\rule{10em}{1em}", true).is_ok());
    }

    #[test]
    fn a_thirty_line_derivation_fits() {
        let lines: Vec<String> = (0..30)
            .map(|i| {
                format!(r"f_{{{i}}}(x) &= \frac{{x^{{{i}}}}}{{{i}!}} + \sum_{{k=0}}^{{n}} a_k")
            })
            .collect();
        let source = format!(r"\begin{{aligned}} {} \end{{aligned}}", lines.join(r" \\ "));
        let list = black(&source, true).expect("30 lines lay out");
        let height = list.height + list.depth;
        // ADR 0021 guessed 75 em for a plainer derivation; this one is 102 em.
        assert!((50.0..=MAX_EM).contains(&height), "{height} em");
    }

    #[test]
    fn href_and_url_show_their_text_only() {
        for source in [
            r"\href{https://example.org}{x}",
            r"\url{https://example.org}",
        ] {
            let list = black(source, false).expect("accepted");
            assert_ne!(list.items.len(), 0);
        }
    }

    #[test]
    fn fits_rejects_nan_infinity_and_negative() {
        assert!(fits(0.0, 0.0));
        assert!(fits(MAX_EM, MAX_EM));
        for bad in [f64::NAN, f64::INFINITY, -1.0, MAX_EM + 0.001] {
            assert!(!fits(bad, 1.0));
            assert!(!fits(1.0, bad));
        }
    }

    #[test]
    fn the_cache_returns_the_same_list_per_colour() {
        let a = formula("x^2", false, Color::BLACK).expect("valid");
        let b = formula("x^2", false, Color::BLACK).expect("valid");
        assert!(Rc::ptr_eq(&a, &b));
        let white = Color::new(1.0, 1.0, 1.0, 1.0);
        let c = formula("x^2", false, white).expect("valid");
        assert!(!Rc::ptr_eq(&a, &c));
        let d = formula("x^2", true, Color::BLACK).expect("valid");
        assert!(!Rc::ptr_eq(&a, &d));
    }

    #[test]
    fn the_cache_holds_what_two_full_sides_show() {
        // Both sides of a card, each with MAX_FORMULAS distinct formulas, in
        // one colour: the second round must not lay anything out.
        let layouts = || LAYOUTS.with(std::cell::Cell::get);
        let sources: Vec<String> = (0..2 * MAX_FORMULAS)
            .map(|i| format!("w_{{{i}}}"))
            .collect();
        for source in &sources {
            formula(source, false, Color::BLACK).expect("valid");
        }
        let before = layouts();
        for source in &sources {
            formula(source, false, Color::BLACK).expect("valid");
        }
        assert_eq!(layouts(), before);
    }

    #[test]
    fn the_cache_remembers_failures_and_stays_bounded() {
        let layouts = || LAYOUTS.with(std::cell::Cell::get);
        assert!(formula(r"\frac{", false, Color::BLACK).is_err());
        let before = layouts();
        assert!(formula(r"\frac{", false, Color::BLACK).is_err());
        assert_eq!(layouts(), before, "a failure is cached too");
        for i in 0..CACHE_ENTRIES * 2 {
            formula(&format!("y_{i}"), false, Color::BLACK).expect("valid");
        }
        CACHE.with(|c| {
            let cache = c.borrow();
            assert_eq!(cache.entries.len(), cache.order.len());
            assert!(cache.entries.len() <= CACHE_ENTRIES);
        });
    }

    #[test]
    fn the_budget_counts_every_formula() {
        let mut budget = Budget::default();
        assert_eq!(budget.place("x", false), Placement::Formula);
        assert_eq!(budget.place(r"\frac{", false), Placement::ErrorSource);
        assert_eq!(budget.place(&"x".repeat(5000), false), Placement::Source);
        assert_eq!(
            budget.place(r"\rule{1000000em}{1em}", true),
            Placement::Source
        );
        // 4 used; 196 more are drawn, then the rest is source.
        for _ in 0..MAX_FORMULAS - 4 {
            assert_eq!(budget.place("x", false), Placement::Formula);
        }
        assert_eq!(budget.place("x", false), Placement::Source);
        assert_eq!(budget.place(r"\frac{", false), Placement::Source);
    }

    /// The longest `a_1+a_2+…` that still fits [`MAX_EM`], so as costly as a
    /// valid formula can be (any source that is 4 KiB and valid is wider).
    fn largest_fitting_sum() -> String {
        let sum = |n: usize| {
            (1..=n)
                .map(|i| format!("a_{i}"))
                .collect::<Vec<_>>()
                .join("+")
        };
        let (mut low, mut high) = (1_usize, 1_000_usize);
        while low < high {
            let mid = (low + high).div_ceil(2);
            if black(&sum(mid), false).is_ok() {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        sum(low)
    }

    #[test]
    fn a_side_of_the_largest_valid_formulas_is_drawn_and_bounded() {
        // 200 distinct formulas as large as allowed: the worst valid side.
        // ADR 0023 records the time of this loop in a release build; here it
        // only guards against a blow-up.
        let biggest = largest_fitting_sum();
        assert!(biggest.len() < MAX_SOURCE_BYTES);
        let (used, ()) = crate::testing::cpu_time_of(|| {
            let mut budget = Budget::default();
            for i in 0..MAX_FORMULAS {
                let source = format!("{biggest}+b_{i}");
                // `+ b_i` may push it over the cap; both outcomes are bounded.
                let placed = budget.place(&source, false);
                assert!(matches!(placed, Placement::Formula | Placement::Source));
            }
        });
        crate::testing::assert_cpu_within(used, Duration::from_secs(60), "200 largest formulas");
    }

    #[test]
    fn a_formula_box_is_never_wider_than_the_line() {
        assert!((box_width(300.0, 200) - 200.0).abs() < 1e-9);
        assert!((box_width(100.0, 200) - 100.0).abs() < 1e-9);
        assert!((box_width(300.0, -1) - 300.0).abs() < 1e-9);
        assert!((box_width(300.0, 0) - 300.0).abs() < 1e-9);
        assert_eq!(left_edge(10, 50), 10);
        assert_eq!(left_edge(60, -50), 10);
    }

    #[test]
    fn pixel_conversions_are_safe_at_the_edges() {
        assert_eq!(ceil_px(0.2), 1);
        assert_eq!(ceil_px(-5.0), 0);
        assert_eq!(ceil_px(f64::NAN), 0);
        assert_eq!(ceil_px(f64::INFINITY), i32::MAX / 2);
        assert_eq!(to_pango_units(1.5), 1536);
        assert_eq!(to_pango_units(-2.0), -2048);
        assert_eq!(to_pango_units(f64::NAN), 0);
        assert_eq!(to_pango_units(f64::INFINITY), i32::MAX);
        assert!((from_pango_units(1536) - 1.5).abs() < 1e-12);
        assert!((f64::from(f32_px(12.25)) - 12.25).abs() < 1e-12);
    }

    #[test]
    fn font_size_in_pixels() {
        // 11 pt at 96 dpi is 14.67 px.
        assert!((font_px(11 * 1024, false, 96.0) - 11.0 * 96.0 / 72.0).abs() < 1e-9);
        // Text scaling 1.5 is a resolution of 144 dpi.
        assert!((font_px(11 * 1024, false, 144.0) - 22.0).abs() < 1e-9);
        // An absolute size is already in pixels.
        assert!((font_px(16 * 1024, true, 144.0) - 16.0).abs() < 1e-9);
        assert!((dpi_from_setting(96 * 1024) - 96.0).abs() < 1e-9);
        assert!((dpi_from_setting(-1) - 96.0).abs() < 1e-9);
        assert!((dpi_from_setting(0) - 96.0).abs() < 1e-9);
    }
}
