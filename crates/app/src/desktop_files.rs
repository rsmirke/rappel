//! Tests of the desktop integration drafts in `data/` (ADR 0060): the
//! `.desktop` entry, the `AppStream` metainfo, the placeholder icons and the
//! store screenshots. Packaging (Meson, Flatpak) is a later phase; until
//! then these tests keep the drafts valid and in step with the code:
//!
//! - both files pass `desktop-file-validate` and `appstreamcli validate
//!   --no-net --pedantic`, in English and with the French catalog merged
//!   in as `msgfmt` will merge it, and every translated text has its French;
//! - the ids, the binary, the icon name, the gettext domain, the licence
//!   and the links are the code's own;
//! - the releases are exactly the versions of `CHANGELOG.md`, with their
//!   dates, newest first;
//! - every screenshot listed is a committed PNG within Flathub's size, and
//!   every committed one is listed;
//! - the icons are plain SVG: no scripts, no pictures, no links.
//!
//! The validators come from the `appstream` and `desktop-file-utils`
//! packages (README.md); CI installs them, so these tests never skip.

use std::path::{Path, PathBuf};
use std::process::Command;

use rappel_core::APP_ID;

use crate::i18n::extract::{self, DESKTOP_KEYS};
use crate::i18n::po;

/// The repository's root, from the app crate's manifest.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The installed program's name: the binary target's.
const BINARY: &str = env!("CARGO_BIN_NAME");

/// Where the store finds the committed screenshots: the raw files of the
/// repository's `main` branch.
const SCREENSHOT_URL: &str =
    "https://raw.githubusercontent.com/rsmirke/rappel/main/data/screenshots/";

/// Flathub's largest window for a screenshot, in logical pixels (its
/// quality guidelines: "1000x700 pixels or smaller").
const MAX_SCREENSHOT: (u32, u32) = (1000, 700);

/// A committed screenshot's largest size: a store fetches each one.
const MAX_SCREENSHOT_BYTES: u64 = 300 * 1024;

/// The issue `appstreamcli --pedantic` raises that Rappel keeps on
/// purpose: the component id has capitals, because it is the app id
/// (`io.github.rsmirke.Rappel`, fixed since ADR 0020).
const ACCEPTED_APPSTREAM_ISSUES: [&str; 1] = ["cid-contains-uppercase-letter"];

fn desktop_source() -> String {
    read(&repo().join(format!("data/{APP_ID}.desktop.in")))
}

fn metainfo_source() -> String {
    read(&repo().join(format!("data/{APP_ID}.metainfo.xml.in")))
}

/// The French translation of a message, from `po/fr.po`.
fn french(id: &str) -> Option<String> {
    static FRENCH: std::sync::OnceLock<Vec<po::Entry>> = std::sync::OnceLock::new();
    let entries = FRENCH.get_or_init(|| {
        po::parse(&read(&repo().join("po/fr.po"))).unwrap_or_else(|err| panic!("fr.po: {err}"))
    });
    entries
        .iter()
        .find(|entry| entry.context.is_none() && entry.id == id && entry.plural.is_none())
        .filter(|entry| !entry.is_fuzzy() && entry.is_translated())
        .map(|entry| entry.strs[0].clone())
}

/// Runs a validator; returns its exit status's success and its output.
fn run(program: &str, args: &[&str], file: &Path) -> (bool, String) {
    let output = Command::new(program)
        .args(args)
        .arg(file)
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "{program} could not run ({err}); install it (README.md: `appstream` and \
                 `desktop-file-utils`)"
            )
        });
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

/// The value of `key` in the `[Desktop Entry]` group.
fn desktop_value<'a>(src: &'a str, key: &str) -> Option<&'a str> {
    src.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

#[test]
fn the_desktop_entry_is_valid_in_english_and_french() {
    let src = desktop_source();
    assert_eq!(desktop_value(&src, "Exec"), Some(BINARY));
    assert_eq!(desktop_value(&src, "Icon"), Some(APP_ID));
    assert_eq!(desktop_value(&src, "Name"), Some(rappel_core::APP_NAME));
    assert_eq!(desktop_value(&src, "Type"), Some("Application"));
    // GApplication registers the id on the session bus, but D-Bus
    // activation also needs an installed service file: packaging's, later.
    assert_eq!(desktop_value(&src, "DBusActivatable"), None);
    let merged = extract::merge_desktop(&src, "fr", french).expect("the entry reads");
    for key in DESKTOP_KEYS {
        assert!(
            merged.contains(&format!("\n{key}[fr]=")),
            "{key} has no French in po/fr.po"
        );
    }
    let dir = tempfile::tempdir().expect("a temporary directory");
    for (text, language) in [(&src, "English"), (&merged, "French")] {
        // desktop-file-validate wants the installed name.
        let path = dir.path().join(format!("{APP_ID}.desktop"));
        std::fs::write(&path, text).expect("the temporary file is writable");
        let (ok, output) = run("desktop-file-validate", &[], &path);
        assert!(
            ok && output.trim().is_empty(),
            "the desktop entry in {language} is not valid:\n{output}\n{text}"
        );
    }
}

/// What a metainfo text says between `open` and `close`, first found.
fn between<'a>(src: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = src.find(open)? + open.len();
    let end = src[start..].find(close)? + start;
    Some(src[start..end].trim())
}

/// The issues in `appstreamcli validate`'s output: their tags.
fn appstream_issues(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| {
            ["E:", "W:", "I:", "P:"]
                .iter()
                .any(|kind| line.starts_with(kind))
        })
        .map(|line| {
            // "P: id:5: tag value"
            line.split(": ")
                .nth(2)
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or(line)
                .to_owned()
        })
        .collect()
}

#[test]
fn the_metainfo_is_valid_in_english_and_french() {
    let src = metainfo_source();
    assert_eq!(between(&src, "<id>", "</id>"), Some(APP_ID));
    assert_eq!(
        between(&src, "<launchable type=\"desktop-id\">", "</launchable>"),
        Some(format!("{APP_ID}.desktop").as_str())
    );
    assert_eq!(between(&src, "<binary>", "</binary>"), Some(BINARY));
    assert_eq!(
        between(&src, "<translation type=\"gettext\">", "</translation>"),
        Some(crate::i18n::DOMAIN)
    );
    assert_eq!(
        between(&src, "<project_license>", "</project_license>"),
        Some(env!("CARGO_PKG_LICENSE"))
    );
    assert_eq!(
        between(&src, "<url type=\"homepage\">", "</url>"),
        Some(env!("CARGO_PKG_REPOSITORY"))
    );
    assert_eq!(
        between(&src, "<url type=\"bugtracker\">", "</url>"),
        Some(format!("{}/issues", env!("CARGO_PKG_REPOSITORY")).as_str())
    );
    assert_eq!(
        between(&src, "<name>", "</name>"),
        Some(rappel_core::APP_NAME)
    );
    let summary = between(&src, "<summary>", "</summary>").expect("a summary");
    assert!(
        summary.chars().count() <= 35 && !summary.ends_with('.'),
        "Flathub: a summary of at most 35 characters, no full stop: {summary:?}"
    );

    let units = extract::metainfo_units(&src).expect("the metainfo reads");
    let untranslated: Vec<&str> = units
        .iter()
        .filter(|unit| french(&unit.text).is_none())
        .map(|unit| unit.text.as_str())
        .collect();
    assert!(
        untranslated.is_empty(),
        "no French in po/fr.po for: {untranslated:#?}"
    );
    let merged = extract::merge_metainfo(&src, "fr", french).expect("the metainfo reads");
    assert_eq!(
        merged.matches("xml:lang=\"fr\"").count(),
        units.len(),
        "every translated element has its French"
    );
    let dir = tempfile::tempdir().expect("a temporary directory");
    for (text, language) in [(&src, "English"), (&merged, "French")] {
        let path = dir.path().join(format!("{APP_ID}.metainfo.xml"));
        std::fs::write(&path, text).expect("the temporary file is writable");
        // --no-net: nothing is fetched, not even the screenshots' URLs.
        let (ok, output) = run(
            "appstreamcli",
            &["validate", "--no-net", "--pedantic", "--explain"],
            &path,
        );
        let issues = appstream_issues(&output);
        // The remark Rappel accepts is always there: seeing it proves the
        // output is still read right, so no other issue can slip past.
        assert!(
            issues
                .iter()
                .any(|issue| issue == ACCEPTED_APPSTREAM_ISSUES[0]),
            "appstreamcli's output no longer reads as expected:\n{output}"
        );
        let unexpected: Vec<String> = issues
            .into_iter()
            .filter(|issue| !ACCEPTED_APPSTREAM_ISSUES.contains(&issue.as_str()))
            .collect();
        assert!(
            ok && unexpected.is_empty(),
            "the metainfo in {language} is not valid ({unexpected:?}):\n{output}"
        );
    }
}

/// The released versions of `CHANGELOG.md`, newest first: (version, date)
/// from each "## [x.y.z] — YYYY-MM-DD" heading (brackets optional), the
/// Unreleased section aside.
fn changelog_releases(changelog: &str) -> Vec<(String, String)> {
    changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .filter(|heading| !heading.starts_with("[Unreleased]"))
        .map(|heading| {
            let (version, date) = heading
                .split_once(" — ")
                .unwrap_or_else(|| panic!("CHANGELOG heading without a date: {heading:?}"));
            let version = version.trim_start_matches('[').trim_end_matches(']');
            (version.to_owned(), date.trim().to_owned())
        })
        .collect()
}

/// A Cargo version as `AppStream` writes it: a pre-release after "~", which
/// `AppStream` sorts before the release ("0.2.0~beta.1" < "0.2.0"; with "-"
/// it would sort after, as `appstreamcli vercmp` shows).
fn appstream_version(version: &str) -> String {
    version.replacen('-', "~", 1)
}

/// The metainfo's releases, in order: (version, date, type).
fn metainfo_releases(src: &str) -> Vec<(String, String, Option<String>)> {
    src.split("<release ")
        .skip(1)
        .map(|rest| {
            // The start tag's text as `attribute` reads it: the name first.
            let tag = format!(
                "release {}",
                &rest[..rest.find('>').expect("a closed <release> tag")]
            );
            let value = |name| extract::attribute(&tag, name).map(str::to_owned);
            (
                value("version").expect("a release has a version"),
                value("date").expect("a release has a date"),
                value("type"),
            )
        })
        .collect()
}

#[test]
fn the_releases_are_the_changelog_s() {
    let changelog = changelog_releases(&read(&repo().join("CHANGELOG.md")));
    assert!(
        changelog.iter().any(|(version, _)| version == "0.1.0"),
        "the CHANGELOG parse found no 0.1.0: {changelog:?}"
    );
    let wanted: Vec<(String, String, Option<String>)> = changelog
        .iter()
        .map(|(version, date)| {
            let kind = version.contains('-').then(|| "development".to_owned());
            (appstream_version(version), date.clone(), kind)
        })
        .collect();
    assert_eq!(
        metainfo_releases(&metainfo_source()),
        wanted,
        "data/{APP_ID}.metainfo.xml.in must list every version of CHANGELOG.md, newest \
         first, with its date (\"~\" before a pre-release, type=\"development\")"
    );
    // Once the version the crate builds is released, it is the newest.
    let built = env!("CARGO_PKG_VERSION");
    if changelog.iter().any(|(version, _)| version == built) {
        assert_eq!(
            changelog.first().map(|(version, _)| version.as_str()),
            Some(built),
            "the crate's version is released but not the newest in CHANGELOG.md"
        );
    }
}

#[test]
fn versions_sort_as_appstream_sorts_them() {
    assert_eq!(appstream_version("0.2.0-beta.1"), "0.2.0~beta.1");
    assert_eq!(appstream_version("0.1.0"), "0.1.0");
    let parsed = changelog_releases(
        "## [Unreleased]\n### Added\n## [0.2.0-beta.1] — 2026-10-18\n## 0.2.0-alpha.1 — 2026-10-16\n",
    );
    assert_eq!(
        parsed,
        [
            ("0.2.0-beta.1".to_owned(), "2026-10-18".to_owned()),
            ("0.2.0-alpha.1".to_owned(), "2026-10-16".to_owned()),
        ]
    );
}

/// A PNG's width and height, from its header; `None` if it is not a PNG.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let header = bytes.get(..24)?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return None;
    }
    let number = |at: usize| {
        u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    Some((number(16), number(20)))
}

#[test]
fn the_screenshots_are_committed_and_within_flathub_s_size() {
    let src = metainfo_source();
    let shots = between(&src, "<screenshots>", "</screenshots>").expect("screenshots");
    let listed: Vec<&str> = shots
        .split("<image>")
        .skip(1)
        .map(|rest| &rest[..rest.find("</image>").expect("a closed <image>")])
        .collect();
    assert!(!listed.is_empty(), "no screenshot");
    assert_eq!(
        shots.matches("<screenshot type=\"default\">").count(),
        1,
        "exactly one default screenshot"
    );
    assert!(
        shots
            .trim_start()
            .starts_with("<screenshot type=\"default\">"),
        "the default screenshot comes first"
    );
    let captions = shots.matches("<caption>").count();
    assert_eq!(captions, listed.len(), "every screenshot has a caption");
    for caption in shots.split("<caption>").skip(1) {
        let caption = &caption[..caption.find("</caption>").expect("a closed caption")];
        assert!(
            !caption.ends_with('.') && !caption.starts_with(|c: char| c.is_ascii_digit()),
            "Flathub: a caption has no full stop and does not start with a number: {caption:?}"
        );
    }
    let folder = repo().join("data/screenshots");
    let mut names = Vec::new();
    for url in &listed {
        let name = url
            .strip_prefix(SCREENSHOT_URL)
            .unwrap_or_else(|| panic!("{url} is not under {SCREENSHOT_URL}"));
        let path = folder.join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let (width, height) = png_size(&bytes).unwrap_or_else(|| panic!("{name} is not a PNG"));
        assert!(
            width <= MAX_SCREENSHOT.0 && height <= MAX_SCREENSHOT.1 && width >= 360,
            "{name} is {width} × {height}: Flathub wants at most {} × {}",
            MAX_SCREENSHOT.0,
            MAX_SCREENSHOT.1
        );
        assert!(
            bytes.len() as u64 <= MAX_SCREENSHOT_BYTES,
            "{name} is {} bytes, over {MAX_SCREENSHOT_BYTES}",
            bytes.len()
        );
        names.push(name.to_owned());
    }
    let mut committed: Vec<String> = std::fs::read_dir(&folder)
        .expect("data/screenshots reads")
        .map(|entry| {
            entry
                .expect("a readable entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    committed.sort();
    names.sort();
    assert_eq!(
        committed, names,
        "every committed screenshot is listed, once"
    );
}

/// Whether `svg` has an attribute like `onload=` or `onclick =`: a word
/// starting with "on" after white space, then `=`.
fn has_event_attribute(svg: &str) -> bool {
    svg.match_indices("on").any(|(at, _)| {
        let before = svg[..at].chars().next_back();
        let rest = svg[at + 2..].trim_start_matches(|c: char| c.is_ascii_alphabetic());
        before.is_some_and(char::is_whitespace) && rest.trim_start().starts_with('=')
    })
}

#[test]
fn event_attributes_are_found() {
    assert!(has_event_attribute(r#"<svg onload="x()">"#));
    assert!(has_event_attribute("<rect\n onclick = 'y'/>"));
    assert!(!has_event_attribute(
        "<!-- the front one answered --><rect fill=\"#fff\"/>"
    ));
}

#[test]
fn the_icons_are_plain_svg_named_after_the_app() {
    let root = repo().join("data/icons/hicolor");
    for (path, size) in [
        (root.join(format!("scalable/apps/{APP_ID}.svg")), 128),
        (
            root.join(format!("symbolic/apps/{APP_ID}-symbolic.svg")),
            16,
        ),
    ] {
        let svg = read(&path);
        let name = path.display();
        assert!(
            svg.contains(&format!("viewBox=\"0 0 {size} {size}\"")),
            "{name}: a square {size} × {size} view box"
        );
        // The one link allowed is SVG's own namespace.
        let bare = svg.replace("xmlns=\"http://www.w3.org/2000/svg\"", "");
        for banned in [
            "<script",
            "<image",
            "href",
            "url(",
            "http",
            "<foreignObject",
            "<style",
            "<!DOCTYPE",
            "<!ENTITY",
        ] {
            assert!(
                !bare.contains(banned),
                "{name} has {banned:?}: an app icon is plain shapes"
            );
        }
        assert!(
            !has_event_attribute(&svg),
            "{name} has an event attribute (onload=…): an app icon is plain shapes"
        );
    }
}
