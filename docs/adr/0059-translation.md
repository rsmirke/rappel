# 59. Translation: gettext through the C library, catalogs compiled without GNU tools, numbers formatted by translated separators

- Status: Accepted
- Date: 2026-10-18
- Amends: ADR 0038 (translation moves from phase 3 into phase 2), ADR 0058 (toast titles are checked in French too)
- Refers to: SPEC §4 (phase 2, item 7), §5; `docs/briefs/phase-2.md` (slice 7)

## Context

The brief asks for gettext infrastructure, every user string marked, plurals, locale-aware numbers and dates ("91 %" in French), and a full French translation the maintainer will proofread. Rappel is a GNOME app, so its translators expect GNOME's layout: `po/LINGUAS`, `po/POTFILES.in`, `po/rappel.pot`, `po/<lang>.po`, and Blueprint's `_("…")`. GNU gettext's tools (`xgettext`, `msgmerge`, `msgfmt`) are not installed on the development machine and there is no Meson build yet (packaging is slice 8 and phase 3), so the catalogs must be built and checked with what `cargo` has.

## Decision

**Lookups: the C library's gettext, through `gettext-rs`** (0.8, MIT, feature `gettext-system`: glibc's libintl, no bundled GNU gettext build). `crate::i18n` wraps `gettext`, `pgettext` (a context for a word with two meanings: "Study" as a button, a page title and a Preferences group) and `ngettext` (every count; the language's plural rule comes from the catalog's header, French's being `n > 1`). A count is passed as `usize`; past `u32::MAX` it keeps its last six digits and stays above one, which is what plural rules test. `npgettext` is known to the extraction but has no wrapper until a message needs one.

**No runtime templates by concatenation.** Every message is a whole sentence with named placeholders (`{count}`, `{name}`, `{error}`), filled by `i18n::fill` in one pass (a value is never searched again, so a deck named "{count}" stays as typed). `ui::text::counted` fills `{count}` with grouped digits; `with_error` fills `{error}`. Lists are joined with a translated separator and "{list} and {last}".

**The domain and GTK.** `i18n::init`, first thing in `main` after the GStreamer re-exec, binds the domain `rappel` in UTF-8 and makes it the default (`textdomain`). Rappel does not call `setlocale`: `gtk_init` does (`setlocale (LC_ALL, "")`, see `gtk_disable_setlocale`'s docs), and `gettext-rs` offers it only as `unsafe`, which the workspace forbids. Nothing is translated before GTK starts. `GtkBuilder` with no domain of its own uses `gettext()` ("If the translation domain is NULL, GtkBuilder uses gettext()", `gtk_builder_set_translation_domain`), so composite templates, the layouts `ui::dialog` loads and `AdwApplication`'s shortcuts dialog are all translated through the default domain; a test checks a plain layout, an app layout and a template in French.

**Where the catalogs are.** `build.rs` compiles each language of `po/LINGUAS` into `<OUT_DIR>/locale/<lang>/LC_MESSAGES/rappel.mo`. At run time the directory is `RAPPEL_LOCALEDIR` if set; else, in a debug build, that `OUT_DIR` folder, so `cargo run` is translated with nothing installed; else, in a release build, `RAPPEL_LOCALEDIR` as it was at compile time, else `/usr/share/locale`. Packaging (Meson, Flatpak) installs the `.mo` files and sets the compile-time variable. Only `.mo` files are read from there, by the C library.

**Catalogs without GNU tools.** `crate::i18n::po` reads `.po` files (comments, flags, contexts, plurals, obsolete `#~` entries skipped) and writes `.mo` files as `msgfmt` does without a hash table (originals sorted by bytes, so `gettext` finds them by binary search; fuzzy and untranslated entries left out, as `msgfmt` does). `build.rs` includes the same file with `#[path]`. Python's `gettext` and the C library both read the result; a test compiles a sample catalog and reads the table back, and another runs the test binary again with a French locale (made by `localedef` from the `locales` package into a temporary `LOCPATH`, since CI's container has none; `C.UTF-8` will not do: glibc 2.44 ignores `LANGUAGE` there as for "C") and checks `gettext`, `pgettext`, the plural forms at 0, 1, 2 and `usize::MAX`, GLib's French month names and the three kinds of layouts.

**Extraction and checks** (`crate::i18n::extract`, test-only). A small lexer per language takes the place of `xgettext`: in Rust, the literal arguments of `gettext`, `pgettext`, `ngettext` and `npgettext` (a call whose arguments are not literals is an error), items under `#[cfg(test)]` skipped; in Blueprint, `_("…")` and `C_("…", "…")`. A comment starting `Translators:` at most three lines above goes into the template as `#.`. References name the file only, not the line, so the template changes only when a message does. The tests:
- `the_template_is_up_to_date`: `po/rappel.pot` and `po/POTFILES.in` equal what the sources give. `RAPPEL_UPDATE_TRANSLATIONS=1 cargo test -p rappel the_template_is_up_to_date` rewrites them and brings each language up to date as `msgmerge` does (translations, flags and translators' comments kept; new messages empty; removed ones kept as `#~` so no work is lost).
- `every_user_string_is_marked`: no string literal outside a marking call looks like words for a person (two letters in a row, and a space, a capital followed by a small letter, or a final "…"), except in logs, panics, assertions, attributes, the log-only "what" argument of `show_error` and its kin, and a short allow-list with reasons (library names, a style scheme id, internal error details). Blueprint strings outside `_()` in a property that is shown are flagged too. Lower-case single words are not caught by the heuristic; the review of this slice went through them by hand.
- `every_message_with_a_placeholder_has_a_comment_for_translators`.
- `the_catalogs_are_complete_and_correct`: each language has every message of the template and nothing else, none fuzzy or empty; each form keeps exactly the English placeholders, a mnemonic (`_`), a final "…"; French's header has `Language: fr` and `nplurals=2; plural=(n > 1);`; and French typography: ’ not ', « » with no-break spaces inside, a no-break space before `:`, `;`, `!`, `?` and `%` ending a word, "…" not "...". Markup tags are left out of the check.

**Numbers.** Rust has no safe access to the C locale's `localeconv`, so the separators are messages: the thousands separator (French: U+202F NARROW NO-BREAK SPACE, "5 210"), the decimal separator (",") and the percent pattern (`{percent} %` with U+202F). `group_digits`, `tenths`, `percent`, the statistics, Card Information's stability and difficulty and every count follow them. Sizes ("4.3 MB") are GLib's `g_format_size`, which follows the C locale and GLib's own translations. Durations and intervals ("4 min 10 s", "3 jours") are messages too; French puts U+00A0 between a number and its unit.

**Dates** are formatted by GLib in the local zone with translatable formats (`C_("date format", "%x %R")`, `"%-d %B"`, …), as GNOME apps do; month and weekday names come from the locale (checked: "octobre").

**Toasts in French** (ADR 0058). The title split accepts French's no-break space before ":" and ";" ("Import impossible : …" gives "Import impossible"). Every test that checks a toast title's length or that it is not the fallback now also runs in French, through `i18n::tests::in_french`, which makes the lookups of the current thread use `po/fr.po` without any locale (thread-local, so tests stay parallel). French Undo titles were chosen to fit 24 characters at 99,999 ("99 999 cartes supprimées", "Réinitialisées : 99 999", "Étiquette ôtée : 99 999").

**French conventions** (GNOME's French team): sentence case for titles and buttons; ’; « » with U+00A0 inside; U+202F before ; : ! ? %; U+00A0 between a number and a unit; "…" kept; every mnemonic kept and unique within each menu. Terms: deck "paquet", card "carte", review "révision", due "à revoir", tag "étiquette", suspend "suspendre", unsuspend "réactiver", Fail / Pass "Raté" / "Réussi", reset progress "réinitialiser la progression". `po/fr.po`'s header says it needs proofreading by the maintainer.

## What stays English

- **`rappel-core`'s error texts** (`{error}` in a message, behind Details in a toast): the core has no gettext and its errors are technical details after a translated sentence that says what failed. Translating them would need either gettext in core or a mapping of every error in the app; left for later. Messages that end in `{error}` say so to translators.
- **GTK's and libadwaita's own strings** (the About dialog's "Website", "Credits", file choosers) come from their own catalogs, which the system's language packs install; the development machine has none, so they show in English in the screenshots.
- `translator-credits` is "Claude (Anthropic)" in French until the maintainer proofreads the catalog and puts his name.
- Right to left is not supported beyond what already holds (names in bidi isolates).

## Consequences

- New dependency `gettext-rs` 0.8 (MIT; it brings `gettext-sys` and `locale_config`, MIT, and a build-time `cc`), and the `locales` package for the tests (README, CI).
- Every new user-visible string must be marked; the gate fails otherwise, and fails when `po/rappel.pot` or `po/fr.po` is out of date or incomplete. Adding a string means: mark it, run the update command, translate the new French entries.
- Numbers in English are now always grouped ("1,000 years", "2,562,047,788,015 h"), where a few places printed them bare.
- `scripts/screenshot.py` has `--decoration-layout`, to see the header bars with GNOME's (and phones') close button alone: with GTK's default minimize and maximize buttons, the longer French subtitles ("8 à revoir · 20 nouvelles", "28 restantes") are cut at 360 px; with `:close` they fit.
