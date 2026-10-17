# 19. Markdown content and rendering

- Status: Accepted
- Date: 2026-10-16
- Amends: ADR 0014 (the review page shows rendered Markdown, not plain text)
- Amended by: ADR 0045 (the review page shows card text larger than body text), ADR 0046 (the style class `dim-label` is now `dimmed`)
- Amended: 2026-10-16, every side has the same layout (no large centred single paragraph); tables get a border and a header row

## Context

Cards are Markdown with LaTeX math (SPEC), but until now both sides were shown as plain text. SPEC §2 puts parsing in `rappel-core` (`content`, a UI-agnostic tree) and widget building in the app (`render`). Card text is untrusted (SPEC §1.5). It may come from an import, so it must never be interpreted as markup, never reach the network, and never be able to crash or freeze the app. A side can be up to 1 MiB (ADR 0018).

## Decision

**Parser: `pulldown-cmark`** (0.13, MIT, no default features). It is the common CommonMark pull parser in Rust: well maintained, fuzzed, and it already supports GFM tables, strikethrough and `$`/`$$` math (`ENABLE_MATH`). Only those three extensions are on. Footnotes, smart punctuation, heading attributes and metadata blocks are off.

**The tree.** `content::parse(md) -> Document` builds blocks (paragraph, heading, list with nested items, code block, block quote, rule, table, display math) and inlines (text, emphasis, strong, strikethrough, code, link with URL, math, image, line break). `parse` never fails. The rules:
- Raw HTML, inline or as a block, becomes literal text. A block keeps its lines.
- Entities follow CommonMark, so `&amp;` gives `&`.
- U+0000 becomes U+FFFD, as CommonMark asks, so no text in the tree has a NUL. glib takes C strings and panics on an interior NUL in debug builds.
- Images keep their alt text and a target: `Media(name)` for `media:<name>`, `Other(url)` for anything else.

**Math and dollars.** pulldown-cmark's rules: `$` opens only if the next character is not a space and closes only if the previous one is not, `$$` is display, and `\$` is an escape. So `$5 and $10` is text. It has no rule for `$5-$10`, so we add Pandoc's rule: inline math whose closing `$` is directly followed by a digit is turned back into its source text. `$$…$$` on its own in a paragraph becomes a `DisplayMath` block, and the text around it becomes paragraphs of their own. In a heading or a table cell it stays an inline `Math { display: true }`. For now math is shown as monospace source, the fallback SPEC §4.2 allows. RaTeX is evaluated in the math slice.

**Nesting cap.** Containers nest at most `MAX_DEPTH` (16) levels. A list's items and a table's rows and cells always open with their parent, so they can add up to two more. Deeper containers are not opened: their text goes into the deepest open container. A flattened block still starts and ends a line (a code or HTML block keeps its lines), and flattened table cells are separated by a space. No text is lost. This bounds every recursive walk (building, dropping, plain text, rendering) and the depth of the widget tree, whatever the input (`>>>>…` 1 MiB long). Property tests check that parsing never panics and stays within the cap.

**Plain text.** `plain_text(&Document)` gives the text without syntax: links as their text, images as alt text, math and code as source. Screen readers get it. `plain_summary(md)` is that text on one line, cut to 200 characters. The card list and the import's conflict lines use it, replacing the app's `front_preview`. It parses only the first 16 KiB of a side, so a deck of large cards still opens quickly.

**Rendering.** `render::render(&Document, Side)` builds a vertical box with one widget per block, using libadwaita style classes only:
- headings: `title-1`…`title-4`, and `heading` for levels 5–6
- code: a `monospace` label in a `card` box; it wraps, so it fits at 360 px
- block quote: a vertical separator beside `dim-label` content
- table: a `GtkGrid` in a scroller that scrolls only horizontally, header cells `heading`
- list: a grid of markers (•, ◦, ▪ by depth, or numbers) beside the items

This UI is built from data, so it is the one place where widgets are made in Rust and not in Blueprint. The fixed frame (front box, separator, back box) stays in `review_page.blp`.

- **Escaping.** Every piece of card text goes through `glib::markup_escape_text`, and the only tags are ours (`<b> <i> <s> <tt> <u>`). Code and display math use labels with markup off. A test parses every produced markup with Pango and checks the shown text.
- **Links are not clickable.** There is no `<a>`, so there is no click, no hand cursor and no focusable link. A link is underlined, and its URL shows as the tooltip when the pointer rests on it: `query-tooltip` maps the pointer to a Pango layout index, and that index is looked up in the link ranges the markup builder recorded. No label is selectable or focusable, and neither is a table's scroller, so the review page's focus and keys (Space, Enter, 1, 2) keep working. Headings have the accessible role Heading and their level.
- **Images.** A media image is a placeholder, `[image: alt]` in italics, until the media slice. Any other image is shown as the Markdown that was typed, never fetched (SPEC §4.3).
- **Layout.** Every side looks the same whatever its structure: body text, normal weight, aligned to the start (amended: a one-paragraph side used to be large and centred, which made cards look different for no reason a user can see). Headings, code, quotes, tables and display math keep their own styles.
- **Tables.** The grid has a border, lines between cells and a shaded header row. This is the app's only stylesheet (`data/style.css`, loaded by `AdwApplication` from the resource base path). It has no literal colours: lines and shading are the text colour at a low opacity, so they follow light and dark themes. In high contrast they are as faint as in normal mode; strengthening them needs a libadwaita newer than the 1.6 floor (ADR 0002).
- **Widget budget.** At most 2,000 widgets per side, counted per top-level block. A block that does not fit, and everything after it, is shown as plain text in one label. A hostile 1 MiB list would otherwise build hundreds of thousands of widgets.

**Review.** The front is rendered when a card is shown and the back when it is revealed. If the back is empty (blank), the reveal shows no separator and no back, only Fail and Pass. The card area's accessible label is the front's plain text, and its description the back's once shown.

## Consequences

- Cards now render as body text aligned to the start, no longer large and centred, whatever their structure.
- A wide table scrolls with the pointer only, not the keyboard.
- A link's URL can be seen only by hovering. Keyboard and screen reader users get the link text but not the URL. A follow-up could add the URLs to the accessible description.
- `fill_scratch_db --markdown` adds a deck with one card per construct, to check them by eye.
- The math slice replaces the monospace fallback and the media slice replaces the image placeholder. Both plug into the same tree.
