# 22. Card editor with GtkSourceView and a live preview

- Status: Accepted
- Date: 2026-10-16
- Amended by: ADR 0043 (point 6: no preview toggle when wide; Edit / Preview instead of the preview below when narrow)

## Context

SPEC §4.4 asks for a Markdown editor: GtkSourceView 5 with Markdown highlighting, monospace and line wrapping, and a live preview, beside or below the text, using the same renderer as the review page. Until now the editor had two plain `GtkTextView`s. A card side can be up to 1 MiB (`MAX_CARD_SIDE_BYTES`), and typing in a long card must stay smooth.

## Decision

1. **GtkSourceView 5 through `sourceview5` 0.11.** It is the gtk-rs binding for our gtk4 0.11 / glib 0.22 release, MIT licensed. Feature `v5_10`. It needs the system library and its introspection data (Blueprint reads `GtkSource-5.typelib`), so CI and the README list `libgtksourceview-5-dev gir1.2-gtksource-5` (Fedora: `gtksourceview5-devel`). `sourceview5::init()` runs at application startup, after GTK is initialised.
2. **Highlighting.** The built-in `markdown` language. The style scheme is the built-in `Adwaita` or `Adwaita-dark`, switched on `AdwStyleManager:dark`. GtkSourceView does not follow libadwaita by itself. If either is missing, the editor still works, without highlighting.
3. **Preview = the review renderer.** Both sides are parsed with `content::parse` and rendered with `render::render` into a pane laid out like the review card: the front, then a separator and the back if `back_shown`. The pane is not focusable, so Tab goes from the back to the header buttons.
4. **Debounced full re-render.** The preview is rebuilt 250 ms after the last change (`ui::debounce::Debounce`), not on every key, and not at all while hidden. Hiding it frees its widgets. Incremental rendering was not worth its complexity: parsing 1 MiB took about 25 ms in a release build.
5. **The preview renders at most 32 KiB per side.** Measuring with a 1 MiB one-paragraph card showed that rendering itself is the cost. The paragraph becomes one Pango label that took about 400 ms per layout (release build), and GTK measures it again on every relayout, so the editor froze. Past 32 KiB the preview shows the start of the side, cut after the last blank line that fits (whole paragraphs), and a note saying so. Saving is not affected. The review page still renders the whole card. That is unchanged here, and it has the same cost for such a card.
6. **Layout.** The dialog opens at 960 × 640. An `AdwBreakpoint` at `max-width: 720sp` puts the preview below the text instead of beside it. The dialog has a minimum size (360 × 294), which libadwaita asks for when a dialog has breakpoints. It still becomes a bottom sheet on narrow windows. The preview toggle is on by default and is not remembered yet (settings come with SPEC §4.8).
7. **Ctrl+B / Ctrl+I** wrap the focused side's selection in `**` / `*`, or insert an empty pair at the cursor. Whitespace at the edges of the selection stays outside the markers, because CommonMark does not read `** word**` as bold. The rules are the pure `format::wrap`. There is no toggle-off: one Ctrl+Z takes the wrap back, because it is a single user action.

## Consequences

- One more system library to build and run the app. The future Flatpak gets it from the GNOME runtime.
- Cards longer than 32 KiB per side get a partial preview.
- The 32 KiB cap is a guess between "most cards preview whole" and "layout stays fast". Raise it if real cards need more.
