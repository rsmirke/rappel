# 47. UI build inputs and the test of every Blueprint file

- Status: Accepted
- Date: 2026-10-17
- Amends: ADR 0002 (`build.rs` runs `glib-compile-resources` itself; `glib-build-tools` is no longer a dependency)
- Refers to: #62, #57, ADR 0024 (GTK tests under Xvfb)

## Context

#62: after branch switches a build kept an old `review_page.blp`, and the app aborted at start with "Unable to retrieve child object card_menu_button". `cargo clean -p rappel` fixed it. The issue asks that every `.blp` be a tracked build input (perhaps with an `include_bytes!` of the file next to each `#[template]`) and that a test build every template widget, so a mismatch between a `.blp` file and its Rust fails `cargo test`, not the app.

What `build.rs` did: `cargo:rerun-if-changed` for `data/` and for each `data/*.blp`; `glib_build_tools::compile_resources` added one for `resources.gresource.xml` and for each file it bundles, including the `.ui` files `build.rs` had just written to `OUT_DIR`.

An experiment (`cargo build -v`, noting whether the build script ran and `rappel` was compiled again):

| Change | Before | After this ADR |
| --- | --- | --- |
| `touch` a `.blp` (also `shortcuts-dialog.blp`) | rebuilt, and **rebuilt again** on the next, unchanged build | rebuilt once |
| `touch` `style.css` or `resources.gresource.xml` | rebuilt once | rebuilt once |
| a new file in `data/`, then removing it | — | rebuilt each time |
| `git switch` to an older commit and back | rebuilt; the `.ui` in `OUT_DIR` is byte-identical to compiling that commit's `.blp` | (same) |
| a changed `.blp` whose modification time is set **back** (`touch -d 2020-01-01`) | **not rebuilt** | **not rebuilt** |

So every input was already tracked, and a branch switch rebuilds correctly. The second build came from the `.ui` files in `OUT_DIR` being listed: they are newer than the run that wrote them. The one hole is the last row: Cargo decides by modification time, so content that arrives with an older time (`cp -p`, `rsync -a`, `tar x`, a restored backup, a clock that jumped) is not seen. That is the most likely way #62 happened; it could not be reproduced with `git` alone.

## Decision

**Build inputs.** `build.rs` keeps `rerun-if-changed` for `data/` (Cargo scans a directory as a whole) and for each `.blp`, and lists `resources.gresource.xml` and each file it bundles from outside `OUT_DIR` (from `glib-compile-resources --generate-dependencies`), but not the generated `.ui` files. It runs `glib-compile-resources` itself, which is what `glib-build-tools` did in thirty lines; that crate is dropped. A `.blp` edit now costs one build instead of two.

**No `include_bytes!` next to `#[template]`.** It would add nothing: the template is loaded from the gresource at run time, and the crate is already compiled again whenever the gresource changes (`gio::resources_register_include!` is an `include_bytes!` of it, which rustc tracks). rustc tracks included files by modification time too, so it would not close the backdated-file hole either. Nothing on stable Cargo can (`-Zchecksum-freshness` is nightly only), so the test below is the safety net, and `cargo clean -p rappel` is the remedy (README).

**The test of every Blueprint file** (`crates/app/src/ui_files.rs`). A table names every `data/*.blp` file with how it is loaded: a composite template by building its widget (`glib::Object::new`, which runs gtk4-rs's template child checks), a file read with `ui::dialog::builder` through the code's own loader (`Layout::load`, `choose_layout`, `ui::dialog::DELETE_CARD`, …), which logs every object it looks up that is missing or of another type and returns `Err` with the first one's id, which the test prints (tests have no log subscriber). Loaders that mixed lookups with other work were split out for this, and the alert dialogs loaded on their own are named constants in `ui::dialog`, so the test loads exactly what the code loads. A second test fails when a `.blp` file has no entry (or an entry no file), and a third that each is in the gresource. A wrong template child aborts the test binary rather than failing one test: gtk4-rs panics inside GObject's instance initialisation, a C callback that cannot unwind. The output still names the id and the type ("Unable to retrieve child object 'card_menu_button' from class template for type 'RappelReviewPage'"), and the gate fails, which is the point.

**Tests (#57).** The session property test compares the whole `Session` (queue order, steps, seen cards, answers) with a session that only got the surviving answers, not just the summary and the count; moving an undone card to the end of the queue now fails it, as it did not before. The review page's undo tests check the Undo button's sensitivity (and that it agrees with the session), in a window: a button on a page without a root stayed insensitive whatever the action said (GTK 4.24). The review page tests share one fixture. Every test that builds the app's widgets goes through `testing::with_widgets` instead of calling `adw::init` and registering the resources itself.

## Consequences

- A new `.blp` file fails `cargo test` until it has an entry in `ui_files.rs`; a new object lookup belongs in its file's loader, which the test then covers.
- The aborting failure mode is uglier than a failed assertion but cannot be avoided without parsing the templates ourselves.
- `build.rs` depends on `glib-compile-resources` directly, which was already a build requirement (README).
