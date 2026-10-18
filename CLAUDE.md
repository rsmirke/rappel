# CLAUDE.md — instructions for Claude Code in this repo

`SPEC.md` is the source of truth for what to build. `ARCHITECTURE.md` describes how the code is organised. Read both before any task. If a request conflicts with them, say so before coding.

## Project facts
- App name **Rappel**, app ID `io.github.rsmirke.Rappel`, licence GPL-3.0-or-later.
- Crates: `rappel-core` in `crates/core` (no UI), `rappel` in `crates/app` (GTK binary).
- Data dir `~/.local/share/rappel/`; debug builds use `~/.local/share/rappel-dev/`. `RAPPEL_DATA_DIR` overrides both. Never point development at the real data dir.
- Status: v0 released as 0.1.0 (decks, plain-text cards, studying with FSRS); v0.2 is in progress. What is done, the build order and the scope are in `SPEC.md` (its status line, then §4).

## About me
- Strong in maths and algorithms, not a Rust or GTK developer, and I do **not** read most diffs. You are responsible for quality. The automated gate and the reviewer agent are how quality is checked, so never weaken them to make a task pass.
- Explain decisions in plain language in your end-of-slice summary, not in long chat messages while working.

## Hard rules
- Never discard uncommitted work, yours or mine. If something must be undone, commit or stash it first, and say so. `.claude/settings.json` denies the usual discarding commands (`git restore`, `git checkout --`, `git reset --hard`, `git clean`, force pushes, `git branch -D`, `rm -rf`); do not work around it.
- `crates/core` never depends on gtk4, glib, gio, libadwaita, or anything UI.
- `review_events` is append-only. Undo is done with a new event that sets `undoes`.
- Schema changes only through a new numbered migration, with a test that migrates a DB created by the previous version. Never edit an existing migration.
- No `unsafe` (`unsafe_code = "forbid"` in workspace lints).
- No network access anywhere in the app. No remote URLs are fetched, ever.
- Card content is untrusted: always escape for Pango, never render HTML, no SVG media.
- No `unwrap()`/`expect()` on IO or DB results outside tests; `expect()` is allowed only for template children and invariants, with a message saying why it cannot fail.
- Do not add a dependency without stating why in the slice summary. Prefer well-maintained crates. Check that the licence is allowed by `deny.toml`.
- Never disable a lint or skip a test without explaining why, and mention it in the summary. Silence a lint only with `#[expect(lint, reason = "…")]`; `#[allow]` is refused by the workspace lints.
- Before using a crate API, read its current docs or source in `~/.cargo/registry`. Do not rely on memory for gtk-rs/libadwaita APIs.
- UI layout lives in Blueprint files. Use libadwaita widgets and style classes as the GNOME HIG intends. No hard-coded colors.

## Quality gate
`scripts/check.sh` runs, in order:
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo deny --locked check -D license-exception-not-encountered --hide-inclusion-graph`.
The gate needs a display: some tests start GTK (`crates/app/src/testing.rs`). In a desktop session it uses that; without one, run `xvfb-run -a scripts/check.sh`. CI runs it under Xvfb (ADR 0024).
A slice is not done until `scripts/check.sh` passes. It also runs as the git pre-commit hook and in CI (`.github/workflows/ci.yml`, required check `check`).

## Workflow for each slice
1. Short plan: files to create or change, migrations, new dependencies. Wait for approval in plan mode.
2. Once approved, create the slice branch from an up-to-date `main` (see Git workflow).
3. Implement in small steps. Write tests for core logic first or alongside.
4. Run `scripts/check.sh` until it passes.
5. Update docs touched by the change: `ARCHITECTURE.md` if structure changed, a new `docs/adr/NNNN-*.md` for any significant decision, `CHANGELOG.md` under "Unreleased".
6. Run the `reviewer` agent on the slice and fix every blocking finding.
7. Commit on the slice branch, push it, and open a PR with `gh pr create` (see Git workflow).
8. End with a summary: what changed, decisions made, anything you are unsure of, what I should look at in the running app, and the PR link.

## Git workflow
- Never commit on `main`. The pre-commit hook refuses it (`scripts/pre-commit.sh`; install with `scripts/install-hooks.sh`). Never bypass it with `--no-verify`.
- One branch per slice, named `feat/…`, `fix/…`, `refactor/…`, `docs/…` or `chore/…` (short kebab-case, e.g. `feat/card-editor`).
- Commit messages follow Conventional Commits: `type: summary` (`feat: …`, `fix: …`, `refactor: …`, `docs: …`, `chore: …`, `test: …`), imperative, lower case, no trailing period. Use `type!:` and a `BREAKING CHANGE:` footer for breaking changes.
- A slice ends with the branch pushed (`git push -u origin <branch>`) and a PR opened with `gh pr create`. Never merge into, push to or rewrite `main` yourself; I merge after review and green CI. Rebasing your slice branch onto `main` is fine.
- No stacked PRs. Base every PR on `main`. If a slice needs an unmerged one, wait for that merge or ask me. If you stack anyway, never let a PR merge into another feature branch: once its base is squash-merged, rebase onto `main` and retarget the PR to `main`. (#18 was based on `feat/decks-ui` and merged into that branch, so it never reached `main` by itself.)

## Batch mode
Active while the brief `docs/briefs/phase-1-finish.md` is being worked through. Where it conflicts with the rules above, batch mode wins.
- In batch mode, slice branches start from `phase-1` and PRs target `phase-1` (`gh pr create --base phase-1`). You may merge your own PR into `phase-1` (squash) only when `scripts/check.sh` passes, CI is green (wait for `gh pr checks` to pass; a ruleset on `phase-1` requires the `check` status), and the reviewer agent has no blocking findings. Never merge into, push to, or target `main`.
- Never stop to ask. Take the most sensible, robust, HIG-compliant default, and record it in the PR description under "Decisions".
- Stop conditions instead: a new migration, an export-format change, a new dependency with a licence not already allowed, or anything that deletes user data. Write the question in `docs/briefs/QUESTIONS.md`, skip to the next independent slice, and continue.
- You may run the app under Xvfb to screenshot your own screens (light, dark, 360 px wide). This overrides the "quick start check only" rule. Keep each run under 2 minutes, and save screenshots in `docs/briefs/screenshots/` (git-ignored). Never commit or upload them; the final report lists their file paths.
- Guard: `.claude/hooks/guard-main.py` (a PreToolUse hook in `.claude/settings.json`) blocks `gh pr merge` when the PR's base is `main` and any `git push` that would update `main`. Do not work around it.

## UI work
- You cannot see the window. Do not claim a UI looks right. Tell me exactly what to check, and I will send screenshots.
- Don't launch the GUI to smoke-test it beyond a quick start check (under a minute). The maintainer checks windows; give him a checklist instead.
- Check every new screen in light mode, dark mode, and at 360 px width.
- Put every UI decision that does not need GTK (texts, counts, focus targets, key handling) in a pure function and unit-test it.
- Before relying on GTK behaviour (signal order, focus, key propagation phase, toast queueing), read the gtk4/libadwaita source or docs and cite it in a comment.
- Every UI PR has a **Try it** section with exact commands, including scratch data from `fill_scratch_db`, and a numbered checklist for me: light, dark, 360 px, keyboard only, screen-reader labels.
- Focus: after pushing an `AdwNavigationPage`, set the focus in its `shown` handler. A `grab_focus()` before or during the push is lost, and the view focuses the first focusable widget (usually Back), which then takes Space and Enter. After removing a row, focus the row now at that index, else the last one, else the empty state's button (`ui::focus::index_after_removal`).
- Toasts, alert dialogs, navigation, row menus and one-time cells go through `crates/app/src/ui/`. Add to it instead of writing a local copy.

## Commands
- Run: `cargo run -p rappel`
- All checks: `scripts/check.sh`
- Install git hooks (once per clone): `scripts/install-hooks.sh`
- Inspector: `GTK_DEBUG=interactive cargo run -p rappel`
- Logs: `RUST_LOG=rappel=debug cargo run -p rappel`
- Scratch data: `cargo run -p rappel-core --example fill_scratch_db -- <dir> <cards> [--due N]`, then `RAPPEL_DATA_DIR=<dir> cargo run -p rappel`
