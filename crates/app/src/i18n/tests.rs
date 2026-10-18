//! The translations' tests (ADR 0059): `po/rappel.pot` up to date with the
//! sources, every user string marked, each language's catalog complete
//! and correct, the catalogs as the C library reads them, and French in
//! the tests without a French locale ([`in_french`]).
//!
//! `RAPPEL_UPDATE_TRANSLATIONS=1 cargo test -p rappel the_template_is_up_to_date`
//! writes `po/rappel.pot` and `po/POTFILES.in` from the sources and brings
//! each language of `po/LINGUAS` up to date with it (new messages
//! untranslated, removed ones kept as obsolete `#~` entries).

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::extract::{self, Message, Scan};
use super::po::{self, Entry};

/// The repository's root, from the app crate's manifest.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// A catalog in memory, for the tests: the translations by key.
#[derive(Debug)]
struct Catalog {
    strs: HashMap<String, Vec<String>>,
}

/// The French catalog, read once from `po/fr.po`.
fn french() -> &'static Catalog {
    static FRENCH: OnceLock<Catalog> = OnceLock::new();
    FRENCH.get_or_init(|| {
        let entries = po::parse(include_str!("../../../../po/fr.po"))
            .unwrap_or_else(|err| panic!("po/fr.po: {err}"));
        let strs = entries
            .into_iter()
            .filter(|entry| !entry.id.is_empty() && !entry.is_fuzzy() && entry.is_translated())
            .map(|entry| (entry.key(), entry.strs))
            .collect();
        Catalog { strs }
    })
}

thread_local! {
    /// The catalog this thread's lookups use, if a test asked for one.
    static CATALOG: Cell<Option<&'static Catalog>> = const { Cell::new(None) };
}

/// Runs `f` with this thread's lookups in French, as `po/fr.po` has them,
/// then in English again (also if `f` panics). French's plural rule is
/// `n > 1` (`the_catalogs_are_complete_and_correct` checks the header).
pub fn in_french<T>(f: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            CATALOG.with(|catalog| catalog.set(None));
        }
    }
    CATALOG.with(|catalog| catalog.set(Some(french())));
    let _restore = Restore;
    f()
}

/// The translation of a message in the catalog of [`in_french`], or `None`
/// outside it (the real lookup then gives the English text, since the
/// tests bind no domain).
pub fn translated(
    context: Option<&str>,
    id: &str,
    plural: Option<&str>,
    n: usize,
) -> Option<String> {
    let catalog = CATALOG.with(Cell::get)?;
    let key = match context {
        Some(context) => format!("{context}\u{4}{id}"),
        None => id.to_owned(),
    };
    let english = match plural {
        Some(plural) if n != 1 => plural,
        _ => id,
    };
    let Some(strs) = catalog.strs.get(&key) else {
        return Some(english.to_owned());
    };
    let form = usize::from(plural.is_some() && n > 1);
    Some(
        strs.get(form)
            .cloned()
            .unwrap_or_else(|| english.to_owned()),
    )
}

/// Source files that only tests compile (`#[cfg(test)] mod …;`).
const TEST_ONLY_FILES: [&str; 3] = [
    "crates/app/src/testing.rs",
    "crates/app/src/ui_files.rs",
    "crates/app/src/dialogs/shortcuts.rs",
];

/// Strings that look like text but are not translated, each with why.
const NOT_TRANSLATED: &[(&str, &str, &str)] = &[
    (
        "crates/app/src/clock.rs",
        "day offset out of range",
        "an internal error, only logged",
    ),
    (
        "crates/app/src/dialogs/about.rs",
        "Robert Smirke",
        "a person's name",
    ),
    (
        "crates/app/src/dialogs/about.rs",
        "SQLite",
        "a library's name",
    ),
    (
        "crates/app/src/dialogs/about.rs",
        "FSRS (fsrs-rs)",
        "a library's name",
    ),
    (
        "crates/app/src/dialogs/about.rs",
        "© 2009–2010 Design Science, Inc.\n© 2014–2018 Khan Academy",
        "a copyright line, kept as the fonts have it",
    ),
    (
        "crates/app/src/dialogs/card_editor/insert.rs",
        "the picture took too long to read",
        "an IO error's message, only logged (the user sees \"The file could not be read\")",
    ),
    (
        "crates/app/src/dialogs/card_editor/mod.rs",
        "the editor has no data",
        "cannot happen; an error detail in English, like rappel-core's",
    ),
    (
        "crates/app/src/dialogs/card_editor/mod.rs",
        "Adwaita-dark",
        "a style scheme's id",
    ),
    (
        "crates/app/src/dialogs/card_editor/mod.rs",
        "Adwaita",
        "a style scheme's id",
    ),
    (
        "crates/app/src/dialogs/clean_up.rs",
        "the worker stopped",
        "only logged",
    ),
    (
        "crates/app/src/render/markup.rs",
        "<span underline=\"error\">",
        "Pango markup",
    ),
    (
        "crates/app/src/render/math.rs",
        "internal error in the layout",
        "only logged",
    ),
    (
        "crates/app/src/window_state.rs",
        "not a regular file",
        "only logged",
    ),
    ("crates/app/src/window_state.rs", "too large", "only logged"),
];

/// Every source file scanned for messages, relative to the repository,
/// sorted: the app's Rust files (not the translation module itself, nor
/// the files only tests compile) and its Blueprint files.
fn sources() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
            .map(|entry| entry.expect("a readable directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let root = repo();
    let mut files = Vec::new();
    walk(&root.join("crates/app/src"), &mut files);
    walk(&root.join("crates/app/data"), &mut files);
    let mut names: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let name = path.strip_prefix(&root).ok()?.to_str()?.to_owned();
            let extension = path.extension().and_then(|e| e.to_str());
            let rust = extension == Some("rs")
                && !name.starts_with("crates/app/src/i18n")
                && !TEST_ONLY_FILES.contains(&name.as_str());
            (rust || extension == Some("blp")).then_some(name)
        })
        .collect();
    names.sort();
    for file in TEST_ONLY_FILES {
        assert!(
            root.join(file).is_file(),
            "{file} is gone: update TEST_ONLY_FILES"
        );
    }
    names
}

/// Scans every source file.
fn scan_all() -> Vec<(String, Scan)> {
    sources()
        .into_iter()
        .map(|file| {
            let text = read(&repo().join(&file));
            let scan = if Path::new(&file).extension().is_some_and(|e| e == "rs") {
                extract::scan_rust(&file, &text)
            } else {
                extract::scan_blueprint(&file, &text)
            };
            (file, scan)
        })
        .collect()
}

/// The template's text, and the files that have messages.
fn template_text() -> (String, Vec<Entry>, Vec<String>) {
    let scans = scan_all();
    let errors: Vec<&String> = scans.iter().flat_map(|(_, scan)| &scan.errors).collect();
    assert!(
        errors.is_empty(),
        "marked strings the extraction cannot read:\n{errors:#?}"
    );
    let messages: Vec<Message> = scans
        .iter()
        .flat_map(|(_, scan)| scan.messages.iter().cloned())
        .collect();
    let files: Vec<String> = scans
        .iter()
        .filter(|(_, scan)| !scan.messages.is_empty())
        .map(|(file, _)| file.clone())
        .collect();
    let entries = extract::template(&messages).unwrap_or_else(|errors| panic!("{errors:#?}"));
    (po::write(&entries), entries, files)
}

/// The languages of `po/LINGUAS`.
fn languages() -> Vec<String> {
    read(&repo().join("po/LINGUAS"))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

const UPDATE: &str = "RAPPEL_UPDATE_TRANSLATIONS";

fn potfiles_text(files: &[String]) -> String {
    let mut text =
        String::from("# Files with messages for translators. Made by the tests (README.md).\n");
    for file in files {
        text.push_str(file);
        text.push('\n');
    }
    text
}

#[test]
fn the_template_is_up_to_date() {
    let (text, template, files) = template_text();
    let root = repo();
    let potfiles = potfiles_text(&files);
    if std::env::var_os(UPDATE).is_some() {
        std::fs::write(root.join("po/rappel.pot"), &text).expect("po/rappel.pot is writable");
        std::fs::write(root.join("po/POTFILES.in"), &potfiles).expect("po/POTFILES.in is writable");
        for language in languages() {
            let path = root.join(format!("po/{language}.po"));
            let old = po::parse(&read(&path)).unwrap_or_else(|err| panic!("{language}.po: {err}"));
            let (merged, obsolete) = extract::merge(&template, &old, 2);
            let mut out = po::write(&merged);
            out.push_str(&extract::obsolete_text(&obsolete));
            std::fs::write(&path, out).expect("the catalog is writable");
        }
        return;
    }
    let fix = format!("run `{UPDATE}=1 cargo test -p rappel the_template_is_up_to_date`");
    assert!(
        read(&root.join("po/rappel.pot")) == text,
        "po/rappel.pot is out of date: {fix}"
    );
    assert!(
        read(&root.join("po/POTFILES.in")) == potfiles,
        "po/POTFILES.in is out of date: {fix}"
    );
}

#[test]
fn every_user_string_is_marked() {
    let mut unmarked = Vec::new();
    let mut allowed_used = BTreeSet::new();
    for (file, scan) in scan_all() {
        for (line, value) in scan.unmarked {
            let allowed = NOT_TRANSLATED
                .iter()
                .position(|(f, v, _)| *f == file && *v == value);
            match allowed {
                Some(at) => {
                    allowed_used.insert(at);
                }
                None => unmarked.push(format!("{file}:{line}: {value:?}")),
            }
        }
    }
    assert!(
        unmarked.is_empty(),
        "strings that look like text for the user but are not marked with gettext() \
         (or _() in Blueprint); mark them, or add them to NOT_TRANSLATED with the reason:\n{}",
        unmarked.join("\n")
    );
    let stale: Vec<_> = NOT_TRANSLATED
        .iter()
        .enumerate()
        .filter(|(at, _)| !allowed_used.contains(at))
        .map(|(_, entry)| entry)
        .collect();
    assert!(
        stale.is_empty(),
        "NOT_TRANSLATED entries no longer found: {stale:#?}"
    );
}

#[test]
fn every_message_with_a_placeholder_has_a_comment_for_translators() {
    let (_, template, _) = template_text();
    let missing: Vec<&str> = template
        .iter()
        .filter(|entry| {
            let text = format!("{}{}", entry.id, entry.plural.as_deref().unwrap_or(""));
            !super::placeholders(&text).is_empty() && entry.extracted.is_empty()
        })
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "add a `// Translators: …` comment above these, saying what each placeholder is:\n{missing:#?}"
    );
}

/// The placeholders of `text`, sorted.
fn placeholder_set(text: &str) -> Vec<&str> {
    let mut names = super::placeholders(text);
    names.sort_unstable();
    names
}

/// The French typography a translation must keep (GNOME's French team
/// conventions): a typographic apostrophe, guillemets with no-break
/// spaces inside, a no-break space before `:`, `;`, `!`, `?` and `%`
/// ending a word, and "…" rather than three dots. Returns what is wrong.
fn french_typography(text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    // Markup's own quotes (`<a href="…">`) are not text.
    let mut bare = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => bare.push(c),
            _ => {}
        }
    }
    let text = bare.as_str();
    let chars: Vec<char> = text.chars().collect();
    let no_break = |c: Option<&char>| matches!(c, Some('\u{202f}' | '\u{a0}'));
    for (i, &c) in chars.iter().enumerate() {
        let before = i.checked_sub(1).and_then(|j| chars.get(j));
        let after = chars.get(i + 1);
        let ends = after.is_none_or(|c| c.is_whitespace() || *c == '\u{a0}');
        match c {
            '\'' => problems.push("an ASCII apostrophe (use ’)".to_owned()),
            '"' => problems.push("ASCII quotes (use « »)".to_owned()),
            '«' if !no_break(after) => {
                problems.push("« without a no-break space after".to_owned());
            }
            '»' if !no_break(before) => {
                problems.push("» without a no-break space before".to_owned());
            }
            ':' | ';' | '!' | '?' if ends && !no_break(before) => {
                problems.push(format!("{c} without a no-break space before"));
            }
            '%' if ends && !no_break(before) => {
                problems.push("% without a no-break space before".to_owned());
            }
            _ => {}
        }
    }
    if text.contains("...") {
        problems.push("three dots (use …)".to_owned());
    }
    problems
}

/// Whether `text` has a mnemonic: `_` before a letter, placeholders aside.
fn has_mnemonic(text: &str) -> bool {
    let mut bare = text.to_owned();
    for name in super::placeholders(text) {
        bare = bare.replace(&format!("{{{name}}}"), "");
    }
    let chars: Vec<char> = bare.chars().collect();
    chars
        .windows(2)
        .any(|pair| pair[0] == '_' && pair[1].is_alphabetic())
}

#[test]
fn the_catalogs_are_complete_and_correct() {
    let (_, template, _) = template_text();
    let wanted: BTreeSet<String> = template
        .iter()
        .filter(|entry| !entry.id.is_empty())
        .map(Entry::key)
        .collect();
    for language in languages() {
        let path = repo().join(format!("po/{language}.po"));
        let entries = po::parse(&read(&path)).unwrap_or_else(|err| panic!("{language}.po: {err}"));
        let header = entries
            .iter()
            .find(|entry| entry.id.is_empty())
            .unwrap_or_else(|| panic!("{language}.po has no header"));
        let mut problems = Vec::new();
        if language == "fr" {
            let fields = &header.strs[0];
            assert!(
                fields.contains("Language: fr\n"),
                "fr.po: no Language field"
            );
            assert!(
                fields.contains("Plural-Forms: nplurals=2; plural=(n > 1);\n"),
                "fr.po: the plural rule must be French's (n > 1), which the tests use"
            );
        }
        let have: BTreeSet<String> = entries
            .iter()
            .filter(|entry| !entry.id.is_empty())
            .map(Entry::key)
            .collect();
        for key in wanted.difference(&have) {
            problems.push(format!("missing: {key:?}"));
        }
        for key in have.difference(&wanted) {
            problems.push(format!("not in the template: {key:?}"));
        }
        for entry in entries.iter().filter(|entry| !entry.id.is_empty()) {
            let id = &entry.id;
            if entry.is_fuzzy() {
                problems.push(format!("fuzzy: {id:?}"));
            }
            if !entry.is_translated() {
                problems.push(format!("not translated: {id:?}"));
                continue;
            }
            for (form, text) in entry.strs.iter().enumerate() {
                let english = match (&entry.plural, form) {
                    (Some(plural), 1..) => plural,
                    _ => id,
                };
                if placeholder_set(text) != placeholder_set(english) {
                    problems.push(format!("placeholders differ: {id:?} → {text:?}"));
                }
                if has_mnemonic(english) != has_mnemonic(text) {
                    problems.push(format!("mnemonic (_) lost or added: {id:?} → {text:?}"));
                }
                if english.ends_with('…') != text.ends_with('…') {
                    problems.push(format!("final … lost or added: {id:?} → {text:?}"));
                }
                if language == "fr" {
                    for problem in french_typography(text) {
                        problems.push(format!("{problem}: {text:?}"));
                    }
                }
            }
        }
        assert!(
            problems.is_empty(),
            "po/{language}.po:\n{}",
            problems.join("\n")
        );
    }
}

/// Reads back a `.mo` file: (original, translation) pairs, in file order.
fn read_mo(bytes: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let word =
        |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize;
    assert_eq!(word(0), 0x9504_12de, "magic number");
    assert_eq!(word(4), 0, "revision");
    let (count, originals, translations) = (word(8), word(12), word(16));
    let string = |table: usize, n: usize| {
        let (length, at) = (word(table + 8 * n), word(table + 8 * n + 4));
        assert_eq!(bytes[at + length], 0, "each string ends with a NUL");
        bytes[at..at + length].to_vec()
    };
    (0..count)
        .map(|n| (string(originals, n), string(translations, n)))
        .collect()
}

#[test]
fn a_catalog_compiles_to_what_gettext_reads() {
    let entries = po::parse(
        "# A translator's note\n\
         msgid \"\"\n\
         msgstr \"\"\n\
         \"Language: fr\\n\"\n\
         \"Plural-Forms: nplurals=2; plural=(n > 1);\\n\"\n\
         \n\
         #. Translators: {count} is a number.\n\
         #: crates/app/src/ui/text.rs\n\
         msgid \"{count} deck\"\n\
         msgid_plural \"{count} decks\"\n\
         msgstr[0] \"{count} paquet\"\n\
         msgstr[1] \"{count} paquets\"\n\
         \n\
         msgctxt \"number\"\n\
         msgid \"{percent}%\"\n\
         msgstr \"{percent}\u{202f}%\"\n\
         \n\
         #, fuzzy\n\
         msgid \"Deck\"\n\
         msgstr \"Paquet\"\n\
         \n\
         msgid \"Untranslated\"\n\
         msgstr \"\"\n\
         \n\
         msgid \"A \\\"quoted\\\"\\n\"\n\
         \"line\"\n\
         msgstr \"Une « ligne »\"\n\
         #~ msgid \"Gone\"\n\
         #~ msgstr \"Parti\"\n",
    )
    .expect("a valid catalog");
    assert_eq!(entries.len(), 6);
    assert_eq!(entries[0].comments, ["A translator's note"]);
    assert_eq!(entries[1].extracted, ["Translators: {count} is a number."]);
    assert_eq!(entries[1].references, ["crates/app/src/ui/text.rs"]);
    assert!(entries[3].is_fuzzy());
    assert_eq!(entries[5].id, "A \"quoted\"\nline");
    // Written and read again, a catalog is the same.
    assert_eq!(
        po::parse(&po::write(&entries)).expect("written back"),
        entries
    );

    let mo = read_mo(&po::mo(&entries));
    let pairs: Vec<(String, String)> = mo
        .iter()
        .map(|(a, b)| {
            (
                String::from_utf8_lossy(a).into_owned(),
                String::from_utf8_lossy(b).into_owned(),
            )
        })
        .collect();
    // Sorted by original, header first; no fuzzy, untranslated or obsolete
    // entry.
    assert_eq!(
        pairs,
        [
            (
                String::new(),
                "Language: fr\nPlural-Forms: nplurals=2; plural=(n > 1);\n".to_owned()
            ),
            ("A \"quoted\"\nline".to_owned(), "Une « ligne »".to_owned()),
            (
                "number\u{4}{percent}%".to_owned(),
                "{percent}\u{202f}%".to_owned()
            ),
            (
                "{count} deck\0{count} decks".to_owned(),
                "{count} paquet\0{count} paquets".to_owned()
            ),
        ]
    );
}

#[test]
fn broken_catalogs_are_refused_with_their_line() {
    for (text, line) in [
        ("msgid \"a\"\nmsgid \"b\"\n", "line 2"),
        ("msgstr \"a\"\n", "line 1"),
        ("msgid \"a\nmsgstr \"\"\n", "line 1"),
        ("msgid \"a\"\nmsgstr[1] \"x\"\n", "line 2"),
        ("msgid \"\\q\"\nmsgstr \"\"\n", "line 1"),
        ("\"stray\"\n", "line 1"),
    ] {
        let err = po::parse(text).expect_err(text);
        assert!(err.starts_with(line), "{text:?}: {err}");
    }
    assert!(po::parse("msgid \"a\"\n").is_err(), "no msgstr");
}

#[test]
fn the_extraction_finds_marked_calls_comments_and_unmarked_text() {
    let src = r##"
        fn shown() -> String {
            // Translators: {count} is a number of cards.
            let a = ngettext("{count} card", "{count} cards", n);
            let b = pgettext("grade", "Pass");
            let c = gettext("Line one \
                             and two");
            let d = gettext(name);
            tracing::warn!("Not for users: {err}");
            let e = format!("{} due", x);
            let f = r#"Raw "text" here"#;
            let g = ('"', b'"', 'a', "dimmed");
            label.set_text(&gettext("Text with \"quotes\" and \u{2026}"));
        }
        #[cfg(test)]
        mod tests {
            fn t() { gettext("Test only"); let x = "Also test only"; }
        }
        fn after<'a>(x: &'a str) -> &'a str { gettext("After the tests") }
    "##;
    let scan = extract::scan_rust("x.rs", src);
    let ids: Vec<&str> = scan.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "{count} card",
            "Pass",
            "Line one and two",
            "Text with \"quotes\" and …",
            "After the tests"
        ]
    );
    assert_eq!(scan.messages[0].plural.as_deref(), Some("{count} cards"));
    assert_eq!(
        scan.messages[0].comment.as_deref(),
        Some("Translators: {count} is a number of cards.")
    );
    assert_eq!(scan.messages[1].context.as_deref(), Some("grade"));
    assert_eq!(scan.messages[1].comment, None);
    assert_eq!(scan.errors.len(), 1, "{:?}", scan.errors);
    assert!(scan.errors[0].starts_with("x.rs:8:"), "{:?}", scan.errors);
    let unmarked: Vec<&str> = scan.unmarked.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(unmarked, ["{} due", "Raw \"text\" here"]);

    let blp = r#"
        Label {
          // Translators: the deck's name follows.
          label: _("Deck");
          tooltip-text: C_("action", "Study");
          icon-name: "view-more-symbolic";
          styles ["Dimmed Class"]
          title: "Not Marked";
          name: "Page Name";
        }
        menu m { item (_("_Export…"), "win.export") }
    "#;
    let scan = extract::scan_blueprint("x.blp", blp);
    let found: Vec<(Option<&str>, &str, Option<&str>)> = scan
        .messages
        .iter()
        .map(|m| (m.context.as_deref(), m.id.as_str(), m.comment.as_deref()))
        .collect();
    assert_eq!(
        found,
        [
            (None, "Deck", Some("Translators: the deck's name follows.")),
            (Some("action"), "Study", None),
            (None, "_Export…", None),
        ]
    );
    let unmarked: Vec<&str> = scan.unmarked.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(unmarked, ["Not Marked"]);
}

#[test]
fn a_merge_keeps_translations_and_puts_removed_ones_aside() {
    let entry = |id: &str, strs: &[&str]| Entry {
        id: id.to_owned(),
        strs: strs.iter().map(|s| (*s).to_owned()).collect(),
        ..Entry::default()
    };
    let header = entry("", &["Language: fr\n"]);
    let template = vec![entry("", &[""]), entry("Kept", &[""]), entry("New", &[""])];
    let old = vec![
        header.clone(),
        entry("Gone", &["Parti"]),
        entry("Kept", &["Gardé"]),
    ];
    let (merged, obsolete) = extract::merge(&template, &old, 2);
    assert_eq!(
        merged,
        vec![header, entry("Kept", &["Gardé"]), entry("New", &[""])]
    );
    assert_eq!(obsolete, vec![entry("Gone", &["Parti"])]);
    assert_eq!(
        extract::obsolete_text(&obsolete),
        "\n#~ msgid \"Gone\"\n#~ msgstr \"Parti\"\n"
    );
}

#[test]
fn prose_is_told_from_ids_and_classes() {
    for text in ["Deck deleted", "Study", "{count} due", "Export…", "Ça va"] {
        assert!(extract::is_prose(text), "{text}");
    }
    for text in [
        "dimmed",
        "win.export",
        "GST_PLUGIN_FEATURE_RANK",
        "{count}",
        "",
        "3 %",
        "x y",
    ] {
        assert!(!extract::is_prose(text), "{text}");
    }
}

#[test]
fn french_typography_is_checked() {
    assert_eq!(
        french_typography("Paquet supprimé\u{202f}: « Maths\u{a0}»").len(),
        1
    );
    assert_eq!(
        french_typography("Supprimer\u{202f}? 91\u{202f}% l’été…"),
        Vec::<String>::new()
    );
    assert_eq!(french_typography("C'est: 3 % ...").len(), 4);
    assert_eq!(
        french_typography("media:abc 10:30 %-d %B"),
        Vec::<String>::new()
    );
    assert!(has_mnemonic("_Annuler"));
    assert!(!has_mnemonic("{file_name} copié"));
}

#[test]
fn french_lookups_pick_the_plural_by_french_rules() {
    in_french(|| {
        assert_eq!(
            translated(None, "No such message", None, 0).as_deref(),
            Some("No such message")
        );
        assert_eq!(
            translated(None, "{count} x", Some("{count} xs"), 2).as_deref(),
            Some("{count} xs")
        );
    });
    assert_eq!(translated(None, "Anything", None, 0), None);
}

/// Set for the child process of `the_c_library_reads_the_french_catalog`.
const REAL_GETTEXT: &str = "RAPPEL_TEST_REAL_GETTEXT";

/// The catalog `build.rs` compiled is read by the C library's own
/// `gettext`, as the app reads it, in French: in a new process of this
/// test binary (the domain and the locale belong to the process), run with
/// `LC_ALL=fr_FR.UTF-8` and a French locale made for it by `localedef` in a
/// temporary folder (`LOCPATH`), since a system may have none (CI's
/// container has only C; `locales` brings `localedef`'s sources). The
/// plain C locale would not do: glibc then ignores `LANGUAGE`, also for
/// C.UTF-8 (checked with glibc 2.44).
#[test]
fn the_c_library_reads_the_french_catalog() {
    if std::env::var_os(REAL_GETTEXT).is_some() {
        return;
    }
    let locales = tempfile::tempdir().expect("a temporary directory");
    let made = std::process::Command::new("localedef")
        .args(["-i", "fr_FR", "-f", "UTF-8"])
        .arg(locales.path().join("fr_FR.UTF-8"))
        .output();
    match made {
        Ok(out) if out.status.success() => {}
        other => panic!(
            "localedef could not make a French locale ({other:?}); install the `locales` \
             package (README.md), which has its sources"
        ),
    }
    let exe = std::env::current_exe().expect("the test binary has a path");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "i18n::tests::french_through_the_c_library",
            "--test-threads=1",
        ])
        .env(REAL_GETTEXT, "1")
        .env("LOCPATH", locales.path())
        // LC_ALL, not LANG alone: a session's own LC_TIME, LC_NUMERIC… name
        // locales that LOCPATH does not have, and setlocale would fail.
        .env("LC_ALL", "fr_FR.UTF-8")
        .env_remove("LANGUAGE")
        .env_remove(super::LOCALEDIR_VARIABLE)
        .output()
        .expect("the test binary runs again");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "French through the C library failed ({}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The child of `the_c_library_reads_the_french_catalog`. Does nothing in
/// a normal run.
#[test]
fn french_through_the_c_library() {
    if std::env::var_os(REAL_GETTEXT).is_none() {
        return;
    }
    crate::testing::with_widgets(|| {
        // `gtk_init` has set the locale from the environment.
        super::init();
        assert_eq!(super::gettext("Deck deleted"), "Paquet supprimé");
        assert_eq!(
            super::pgettext("percent", "{percent}%"),
            "{percent}\u{202f}%"
        );
        assert_eq!(super::pgettext("thousands separator", ","), "\u{202f}");
        // French's plural rule, from the catalog's header: 0 and 1 are
        // singular.
        let cards = |n| super::ngettext("{count} card", "{count} cards", n);
        assert_eq!(cards(0), "{count} carte");
        assert_eq!(cards(1), "{count} carte");
        assert_eq!(cards(2), "{count} cartes");
        assert_eq!(cards(usize::MAX), "{count} cartes");
        // An unknown message is the English text.
        assert_eq!(super::gettext("Not in any catalog"), "Not in any catalog");
        assert_eq!(crate::ui::text::group_digits(5_210), "5\u{202f}210");
        // GLib names the months from the locale (16 October 2026, noon UTC).
        let day = crate::ui::time::day_long(1_792_152_000_000);
        assert!(day.contains("octobre"), "{day}");
        // GtkBuilder translates through the default domain: a plain layout,
        // one of the app's, and a composite template.
        let builder = gtk::Builder::from_string(
            "<interface><object class=\"GtkLabel\" id=\"label\">\
             <property name=\"label\" translatable=\"yes\">Deck deleted</property>\
             </object></interface>",
        );
        let label: gtk::Label = builder.object("label").expect("the label is built");
        assert_eq!(label.label(), "Paquet supprimé");
        let (_, entry, _) = crate::dialogs::deck_name_dialog::layout().expect("the layout loads");
        assert_eq!(adw::prelude::PreferencesRowExt::title(&entry), "Nom");
        let row = crate::widgets::BackupRow::new("17/10/2026 12:00:00", 1_000);
        let button = row.restore_button();
        assert_eq!(
            gtk::prelude::ButtonExt::label(&button).as_deref(),
            Some("_Restaurer")
        );
    });
}
