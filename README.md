# Rappel

A native GNOME flashcard app. Two grades only (Fail and Pass), Markdown cards with LaTeX math, FSRS scheduling. Everything stays on your computer: no network access, no accounts.

**Status:** early development (0.1.0 released). You can create decks, add Markdown cards and study them with Fail and Pass on an FSRS schedule. Math (`$…$`, `$$…$$`) is drawn like KaTeX does, and PNG, JPEG, WebP and GIF images can be pasted, dropped or inserted into cards. See [SPEC.md](SPEC.md) for what is planned.

## Building

You need:

- Rust 1.92 or newer (`rustup`)
- GTK ≥ 4.20, libadwaita ≥ 1.8 (ADR 0039) and GtkSourceView ≥ 5.10 development files, and
  GtkSourceView's introspection data (Blueprint needs it)
  (Fedora: `gtk4-devel libadwaita-devel gtksourceview5-devel`;
  Debian/Ubuntu: `libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev gir1.2-gtksource-5`)
- the development files of Little CMS 2, libseccomp and fontconfig, for the image decoder
  [glycin](https://gitlab.gnome.org/GNOME/glycin) (ADR 0055)
  (Fedora: `lcms2-devel libseccomp-devel fontconfig-devel`;
  Debian/Ubuntu: `liblcms2-dev libseccomp-dev libfontconfig-dev`)
- to show images, glycin's image loaders and bubblewrap, its sandbox, at run time (most GNOME
  systems have both) (Fedora: `glycin-loaders bubblewrap`; Debian/Ubuntu: `glycin-loaders bubblewrap`)
- to play audio, GStreamer's plugins at run time (GTK plays through GStreamer; most GNOME systems
  have them; ADR 0056): the base and good plugins, and for M4A files GStreamer's `faad` or `fdkaac`
  AAC decoder (Rappel does not let GStreamer load FFmpeg's `libav`, ADR 0062)
  (Debian/Ubuntu: `gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad`, which
  has `faad`, or `gstreamer1.0-fdkaac`; Fedora: `gstreamer1-plugins-base gstreamer1-plugins-good` and a
  package with the `fdkaac` or `faad` plugin).
  Without the base plugins (`playbin3`), every audio button says "Audio playback is not available on this
  system" and nothing is played. CI installs the base, good and bad plugins so the audio tests play for real.
- `blueprint-compiler` and `glib-compile-resources` (from GLib) on your `PATH`
- for the tests, the C library's locale sources, from which a test makes a French locale
  (Debian/Ubuntu: `locales`; Fedora: `glibc-locale-source`)
- for the tests, the validators of the desktop integration drafts, `appstreamcli` and
  `desktop-file-validate` (Debian/Ubuntu: `appstream desktop-file-utils`;
  Fedora: `appstream desktop-file-utils`)
- [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) for the quality gate (`cargo install cargo-deny`)

```sh
cargo run -p rappel                         # run the app
RUST_LOG=rappel=debug cargo run -p rappel    # with debug logs
GTK_DEBUG=interactive cargo run -p rappel   # with the GTK inspector
scripts/check.sh                           # format, lint, test, licence/advisory checks
scripts/install-hooks.sh                   # once per clone: run checks before every commit
```

At launch Rappel starts itself again once, in the same process, to limit what GStreamer may load and plug: only the plugins audio playback needs (`GST_PLUGIN_LOADING_WHITELIST`, ADR 0062), from a registry of its own under `~/.cache/rappel/` (`GST_REGISTRY_1_0`, unless you set it or `GST_REGISTRY`), and never its streaming and network elements (`GST_PLUGIN_FEATURE_RANK`, ADR 0056). A debugger follows it (gdb prints "is executing new program"). To skip the restart, give the variables their values already (run the app once and copy them from `/proc/<pid>/environ`); or, **for debugging only**, set `RAPPEL_GST_ENVIRONMENT_SET=1`, which skips it even without them and so lets GStreamer load every plugin. M4A files need an AAC decoder other than FFmpeg's: `faad` (`gstreamer1.0-plugins-bad`) or `fdkaac`.

Some tests start GTK, so `scripts/check.sh` needs a display: a desktop session, or `xvfb-run -a scripts/check.sh` without one (as CI does).

Cargo rebuilds the UI when a file in `crates/app/data/` changes, judging by the files' modification times. A file copied in with an older time (`cp -p`, `rsync -a`, unpacking an archive) is not noticed; if the app then stops at start with "Unable to retrieve child object …", run `cargo clean -p rappel` (ADR 0047).

To try the app on scratch data instead of your own (60 cards, 30 of them due now):

```sh
cargo run -p rappel-core --example fill_scratch_db -- /tmp/rappel-scratch 60 --due 30
RAPPEL_DATA_DIR=/tmp/rappel-scratch cargo run -p rappel
```

Add `--markdown` for a deck with one card per Markdown construct, and `--math` for a deck with 15 formulas, one per card, plus a card with invalid LaTeX (`… fill_scratch_db -- /tmp/rappel-md 0 --markdown --math` adds only those two decks). `--demo` adds six decks that look like a real collection, with a month of history (the store screenshots use it).

## Translations

Rappel uses gettext, laid out as GNOME projects are (ADR 0059): `po/LINGUAS` lists the languages, `po/rappel.pot` is the template and `po/<lang>.po` each translation. GNU gettext's tools are not needed: `build.rs` compiles the catalogs, and the tests make the template and check the translations.

- **Run Rappel in French** without a French system locale: make one once, then point the C library at it (`LC_ALL`, not `LANG`, so the session's other `LC_*` settings do not ask for locales that folder lacks):
  ```sh
  mkdir -p target/tmp/locales && localedef -i fr_FR -f UTF-8 target/tmp/locales/fr_FR.UTF-8
  LOCPATH=$PWD/target/tmp/locales LC_ALL=fr_FR.UTF-8 LANGUAGE=fr cargo run -p rappel
  ```
  A debug build reads the catalogs `build.rs` compiled; an installed build reads `/usr/share/locale` (or `RAPPEL_LOCALEDIR`).
- **After changing a text**: mark every user-visible string with `gettext("…")`, `pgettext("context", "…")` or `ngettext("…", "…", n)` (Rust) or `_("…")` / `C_("context", "…")` (Blueprint), as whole sentences with named placeholders (`{count}`, filled by `i18n::fill` or `ui::text::counted`), and a `// Translators:` comment above any with a placeholder. Then update the template and the catalogs, and translate the new entries (they are empty) in each `po/<lang>.po`:
  ```sh
  RAPPEL_UPDATE_TRANSLATIONS=1 cargo test -p rappel the_template_is_up_to_date
  ```
  The gate fails while a string is unmarked, the template is out of date, or a catalog misses a message, keeps one fuzzy, or changes its placeholders.
- **Add a language**: copy `po/rappel.pot` to `po/<lang>.po`, fill in its header (`Language:` and `Plural-Forms:`), translate every entry, then add its code to `po/LINGUAS`. A language cannot land partly translated: once it is in `LINGUAS`, the gate refuses its catalog while any message is missing, empty or fuzzy, so finish the translation before listing it.

## Desktop integration

Drafts of the files that make Rappel look like a GNOME app once installed are in [`data/`](data/) (ADR 0060): the desktop entry (`io.github.rsmirke.Rappel.desktop.in`), the AppStream metainfo for software stores (`io.github.rsmirke.Rappel.metainfo.xml.in`: description, releases, content rating, screenshots), **placeholder** icons in `data/icons/` (to be replaced by a designed icon before 1.0), and the store screenshots in `data/screenshots/`. Nothing installs them yet: packaging (a Meson build that merges their translations and installs them, and a Flatpak manifest, which a person writes for Flathub) comes in a later phase.

- **Validate them**: the gate does (`cargo test -p rappel desktop_files`), with `desktop-file-validate` and `appstreamcli validate --no-net --pedantic`, in English and with the French translation merged in. Their texts are in `po/rappel.pot` like the app's.
- **A new release** needs its `<release>` in the metainfo (newest first, a pre-release written `0.3.0~beta.1`); the gate fails while `CHANGELOG.md` has a version the metainfo lacks.
- **Retake the store screenshots** after a visible change (light style, GNOME's window buttons and accent, 1000 × 700 and a phone at 360 × 640):
  ```sh
  cargo run -p rappel-core --example fill_scratch_db -- $PWD/target/tmp/demo 0 --demo
  scripts/screenshot.py --out store --size 1000x700 --data target/tmp/demo \
      --env ADW_DEBUG_ACCENT_COLOR=blue --decoration-layout :close --steps "wait:6 shot:decks"
  ```
  then copy the shots from `docs/briefs/screenshots/` to `data/screenshots/` (`convert … -strip` keeps them small). The harness has no compositor, so the windows have no shadow; for a Flathub submission, retake them on GNOME with Alt+Print.

## Recovering your data

Rappel backs up its database automatically: before every schema upgrade, before an import that adds or brings back anything, and once a day, into `backups/` inside the data directory (`~/.local/share/rappel/backups/`, keeping every backup of the last 24 hours and the newest of each of the last 14 days).

If the database is damaged or you want to go back to an earlier state, **restore a backup through Rappel. Do not copy files by hand.** A copied-in file can be silently mixed with a leftover `rappel.sqlite3-wal` file from the old database, which corrupts it or brings back data you meant to drop. Restoring checks the backup first and refuses a damaged one. It never deletes your current database: it is kept next to the new one as `rappel.sqlite3.replaced-YYYYMMDD-HHMMSS` (with its `-wal`/`-shm`/`-journal` files, if any).

When Rappel finds its database damaged, it shows a recovery screen that lists the backups, newest first, and restores the one you pick. If a restore was interrupted and its backup has since gone, the screen offers **Finish Restore**, which installs the checked copy that was waiting. Restoring a backup while the database is healthy (to go back to an earlier state) is not in the app yet. If a database was made by a newer version of Rappel, update Rappel; do not restore an older backup over it.

## Exporting and importing

**Export…** in the main menu (☰ on the deck list, or F10) saves everything (decks, cards and the whole review history, deleted items included) to one `.rappel` file. Keep it as a copy, or open it on another computer with **Import…**.

Import first shows what the file would add and what is already there, and writes nothing until you confirm. It adds what is new and brings back what you deleted; nothing you have is edited or deleted. A deck or card you deleted after exporting comes back with its review history if the file still has it, and a deck brings back the cards that were deleted with it. If the file disagrees with your data (for example a card edited in both places), nothing is imported and Rappel lists where they differ. Importing the same file twice changes nothing.

### Coming from Anki

In Anki, use **File › Export…**, choose **Anki Deck Package (.apkg)** or **Anki Collection Package (.colpkg)**, and tick **Include scheduling information** and **Include media**. Then in Rappel use **Import…** and choose that file. Packages from Anki 2.0, 2.1 and the current versions all import.

Each Anki card becomes one Rappel card in Markdown, in a deck of the same name (`Languages::French` becomes "Languages › French"; a deck you already have with that name gets the cards). Reversed notes give a card per direction; a cloze card hides its cloze on the front and shows it in bold on the back. Tags, images, sounds, suspended cards and the review history come along: Again counts as Fail, Hard, Good and Easy as Pass, and Rappel works out the schedule from that history. Note types, card styling, add-ons and deck options do not. Before importing, Rappel lists what it leaves out or changes (for example an SVG image, a sound file missing from the package, or a template filter it does not know), and writes nothing until you confirm; your data is backed up first. A card already in Rappel (same deck, front and back) is not added again, so importing the same package twice adds nothing. Reading and importing a large collection can be cancelled from the notice at the bottom of the window.

### Coming from Cards

Rappel was called Cards before 0.2.0. It does not read the old data folder (`~/.local/share/cards`). To move your data: in the last Cards build use **Export…**, then in Rappel use **Import…**. Old `.cards` exports import as they are; the import file chooser shows both `.rappel` and `.cards` files.

## Documentation

- [SPEC.md](SPEC.md): what to build.
- [ARCHITECTURE.md](ARCHITECTURE.md): how the code is organised.
- [docs/adr/](docs/adr/): architecture decision records.
- [CHANGELOG.md](CHANGELOG.md): notable changes.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

## Credits

Math is laid out by [RaTeX](https://github.com/erweixin/RaTeX) (MIT) and drawn with the [KaTeX](https://katex.org) fonts, which are © their authors and licensed under the SIL Open Font License 1.1 ([LICENSES/KaTeX-fonts-OFL-1.1.txt](LICENSES/KaTeX-fonts-OFL-1.1.txt)). The fonts are separate data shipped with the program, not part of its code. A copy of RaTeX's cairo renderer, adapted to our gtk-rs version, is in [third_party/ratex-cairo](third_party/ratex-cairo).
