# 60. Desktop integration drafts: a `.desktop` entry, AppStream metainfo, placeholder icons and store screenshots in `data/`, checked by the tests

- Status: Accepted
- Date: 2026-10-18
- Amends: ADR 0038 (drafts of the packaging data move into phase 2; packaging itself stays in phase 3), ADR 0059 (the extraction also reads the drafts)
- Refers to: SPEC §4 (phase 2, item 7), §5; `docs/briefs/phase-2.md` (slice 8)

## Context

The brief asks for drafts of the desktop integration files: a `.desktop` file and AppStream metainfo (description, releases, content rating, screenshots from the harness), no Flatpak manifest, and a placeholder icon. Packaging (Meson, the Flatpak manifest, install rules) stays in phase 3 (SPEC §5). Nothing installs these files yet, so without checks they would rot: a release added to `CHANGELOG.md` and not to the metainfo, an id or a binary renamed, a translation missing.

## Decision

**Layout: GNOME's, in a top-level `data/` folder**, as GNOME's app template and most GNOME apps have it, apart from the app crate's own `crates/app/data/` (Blueprint files and the resource bundle, compiled into the binary):

```
data/io.github.rsmirke.Rappel.desktop.in
data/io.github.rsmirke.Rappel.metainfo.xml.in
data/icons/hicolor/scalable/apps/io.github.rsmirke.Rappel.svg
data/icons/hicolor/symbolic/apps/io.github.rsmirke.Rappel-symbolic.svg
data/screenshots/{review,decks,editor,statistics,phone}.png
```

The `.in` files are the sources packaging will translate (Meson's `i18n.merge_file`, i.e. `msgfmt --desktop` and `msgfmt --xml`) and install without the `.in`. They hold no `@VARIABLE@` yet, so each is also a valid file as it stands.

**The `.desktop` entry.** `Name=Rappel`, `GenericName=Flashcards`, `Comment` = the metainfo summary, translatable `Keywords`, `Exec=rappel` (no `%U` or `%F`: Rappel opens no file given on the command line), `Icon=` the app id, `Categories=Education;GTK;GNOME;` (Education is a main category of the freedesktop menu specification; GTK and GNOME are additional ones), `StartupNotify=true`, and `SingleMainWindow=true` (Desktop Entry 1.5: a second launch presents the one window, ADR 0011). **No `DBusActivatable`**: GApplication owns its id on the session bus, but D-Bus activation also needs an installed D-Bus service file; that is packaging's to add, together with the key. No `X-GNOME-UsesNotifications` (Rappel sends none), and no `X-Purism-FormFactor` (obsolete; the metainfo says it).

**The metainfo.** `desktop-application`, metadata CC0-1.0, project GPL-3.0-or-later; the developer is `io.github.rsmirke`, "Robert Smirke" (the git author; not translated); a summary of 24 characters in the imperative ("Memorize with flashcards", Flathub: 10–35 characters, no app name, no full stop); a description of two paragraphs and six points, only what the app does today; the launchable, the binary, the gettext domain and the repository's links (no network URL is ever fetched by Rappel; these are for the stores). **Content rating: an empty `oars-1.1`**, which AppStream defines as "checked, no restrictions": Rappel has no network access, no chat, no sharing, no ads and no purchases, and the cards are the user's own. **Phones:** `<requires><display_length compare="ge">360</display_length></requires>` (ADR 0058: every screen works at 360 px) and `<supports>` keyboard, pointing and touch, as AppStream advises (controls in `supports`, not `requires`). **Branding:** light `#99c1f1` and dark `#1a5fb4` (GNOME's palette blue 1 and 5, the icon's blue; Flathub wants two colourful primaries, the dark one darker). These are store metadata, not UI colours.

**Releases.** One per version of `CHANGELOG.md`, newest first, with its date and a short note for users. A pre-release is written with `~` (`0.2.0~beta.1`): `appstreamcli vercmp` sorts `0.2.0-beta.1` *after* `0.2.0` but `0.2.0~beta.1` before it, as Debian does. Pre-releases have `type="development"`. **Release notes are not translated** (`translate="no"`): they are written at release time, after the strings of that release are frozen; a note shown in English beats a catalog that is always one release behind. The summary, the description, the captions and the `.desktop` texts are translated.

**Screenshots: committed under `data/screenshots/`**, the only screenshots in the repository (the review screenshots of batch mode stay git-ignored in `docs/briefs/screenshots/`). A store fetches a screenshot by URL, so the metainfo names `https://raw.githubusercontent.com/rsmirke/rappel/main/data/screenshots/<name>.png`; they appear once the files reach `main`. Five, about 125 KB in all: the review page with a formula (default), the deck list, the editor with its preview, the statistics, and a card with a picture at 360 × 640. They are taken by `scripts/screenshot.py` in English, light style, GNOME's window buttons (`--decoration-layout :close`) and accent (`ADW_DEBUG_ACCENT_COLOR=blue`: the development machine's Ubuntu default is orange), at 1000 × 700 (Flathub's largest), with `fill_scratch_db --demo`: six decks that look like a real collection (capitals, French words, chemical elements, art history, calculus with formulas, flags drawn as pictures) and a month of review history. AppStream has no attribute that marks a screenshot as a phone's, so the phone one is an ordinary screenshot with its caption saying so. **Not met:** Flathub wants the window's shadow and rounded corners as GNOME's window screenshot takes them; the harness runs on Xvfb without a compositor, so there is no shadow. Before a Flathub submission the maintainer retakes them on GNOME (Alt+Print) with the same data.

**Icons: placeholders**, hand-written SVG to be replaced by a designed icon (the GNOME design team's App Icon Requests, or a designer) before 1.0: two flashcards, the front one with a check mark, in GNOME's palette, 128 × 128; and a 16 × 16 symbolic. SVG here is the app's own icon, drawn by the desktop's icon loader from a file Rappel ships; the rule "no SVG" (SPEC §4.3) is about untrusted card media and does not apply. A test keeps them plain shapes: no script, no picture, no link, no style sheet, no event attribute. Rappel keeps setting only its icon name (`set_application_icon(APP_ID)` in the About dialog); a build that is not installed finds no icon of that name, as before.

**No Flatpak manifest.** Flathub reviews manifests by hand and wants them written and owned by a person who answers for its sources and permissions; a generated draft would be reviewed as if it were final. It is part of packaging (phase 3).

**Translation.** The extraction of ADR 0059 reads the drafts as `xgettext` does: in the `.desktop` file the values of `Name`, `GenericName`, `Comment` and `Keywords` (the list one message, with a `# Translators:` comment above it); in the metainfo the elements AppStream's ITS rules (`metainfo.its`) translate: name, summary, developer name, keywords, captions, and each `<p>` and `<li>` of the descriptions, with runs of white space made one, and `<!-- Translators: … -->` above. `translate="no"` turns an element and what it holds off. The extraction refuses inline markup (`<em>`, `<code>`) and entities in a translated element, and a translation written into the source (`Name[fr]=`, `xml:lang`), so each message is exactly the text. The drafts are listed in `po/POTFILES.in` and `po/rappel.pot`, and `po/fr.po` translates them. A Keywords list keeps its `;` separators in every language (French typography leaves them alone).

**Checks** (`crates/app/src/desktop_files.rs`, tests only):
- `desktop-file-validate` and `appstreamcli validate --no-net --pedantic` pass on the English files and on the files with French merged in as `msgfmt` will merge them (`Comment[fr]=…`; `<summary xml:lang="fr">…</summary>` after each element), and every translated text has its French. The one pedantic remark allowed is `cid-contains-uppercase-letter`: the id is the app id, fixed since ADR 0020. The validators are in the README's requirements and CI installs them (`appstream`, `desktop-file-utils`), so the tests never skip.
- The ids, `Exec` and `<binary>` (the Cargo binary name), the icon name, the gettext domain, the licence and the links are the code's own.
- The releases are exactly `CHANGELOG.md`'s versions, newest first, with their dates (a version added to the changelog fails the gate until the metainfo has it).
- Every screenshot listed is under the raw URL, committed, a PNG of at most 1000 × 700 and at least 360 wide, under 300 KB; every committed one is listed; one default, first; every one captioned, without a final full stop.

## Consequences

- A release adds its `<release>` to the metainfo, in the same change as `CHANGELOG.md` (slice 11 does it for 0.3.0-beta.1).
- A visible change to a screen in the store set means retaking its screenshot with the commands in the README.
- Packaging (phase 3) adds the Meson rules (`i18n.merge_file`, installing the icons and the screenshots' metainfo), the D-Bus service and `DBusActivatable` if wanted, the Flatpak manifest, and the real icon.
- `appstream` and `desktop-file-utils` are new requirements for running the tests.
