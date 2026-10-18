//! Gettext catalogs without GNU gettext's tools (ADR 0059): a reader of
//! `.po` files and a writer of the `.mo` files the C library's `gettext`
//! reads. `build.rs` includes this file to compile `po/*.po`; the tests use
//! it to check the catalogs and to translate in French without a French
//! locale.
//!
//! Only what Rappel's catalogs use is read: comments (`#`, `#.`, `#:`,
//! `#,`; `#|` and obsolete `#~` entries are skipped), `msgctxt`, `msgid`,
//! `msgid_plural`, `msgstr` and `msgstr[N]`, each followed by one or more
//! quoted strings with the escapes `\n`, `\t`, `\r`, `\"` and `\\`.

use std::fmt::Write as _;

/// One entry of a catalog. The header is the entry whose `id` is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// Translators' own comments (`# …`), kept as written.
    pub comments: Vec<String>,
    /// Comments for translators from the source (`#. …`).
    pub extracted: Vec<String>,
    /// Where the message is used (`#: …`).
    pub references: Vec<String>,
    /// Flags (`#, fuzzy`).
    pub flags: Vec<String>,
    pub context: Option<String>,
    pub id: String,
    pub plural: Option<String>,
    /// The translations: one, or one per plural form.
    pub strs: Vec<String>,
}

impl Entry {
    /// Whether a translator marked it as needing review.
    pub fn is_fuzzy(&self) -> bool {
        self.flags.iter().any(|flag| flag == "fuzzy")
    }

    /// Whether every form is translated.
    pub fn is_translated(&self) -> bool {
        !self.strs.is_empty() && self.strs.iter().all(|s| !s.is_empty())
    }

    /// The key `gettext` looks it up by: the context and the id joined by
    /// U+0004, as `pgettext` builds it (`gettext.h`).
    pub fn key(&self) -> String {
        match &self.context {
            Some(context) => format!("{context}\u{4}{}", self.id),
            None => self.id.clone(),
        }
    }
}

/// Which keyword the strings being read belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Context,
    Id,
    Plural,
    Str(usize),
}

/// Reads a `.po` file. An error names the line.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut entry = Entry::default();
    let mut field: Option<Field> = None;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        let fail = |what: &str| format!("line {number}: {what}");
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            // A comment after an entry's strings starts the next entry.
            if field.is_some() {
                entries.push(std::mem::take(&mut entry));
                field = None;
            }
            if comment.starts_with('~') || comment.starts_with('|') {
                continue;
            }
            if let Some(text) = comment.strip_prefix('.') {
                entry.extracted.push(text.trim().to_owned());
            } else if let Some(text) = comment.strip_prefix(':') {
                entry
                    .references
                    .extend(text.split_whitespace().map(str::to_owned));
            } else if let Some(text) = comment.strip_prefix(',') {
                entry.flags.extend(
                    text.split(',')
                        .map(str::trim)
                        .filter(|flag| !flag.is_empty())
                        .map(str::to_owned),
                );
            } else {
                entry
                    .comments
                    .push(comment.strip_prefix(' ').unwrap_or(comment).to_owned());
            }
            continue;
        }
        if line.starts_with('"') {
            let value = unquote(line).map_err(|err| fail(&err))?;
            match field {
                Some(Field::Context) => push(&mut entry.context, &value),
                Some(Field::Id) => entry.id.push_str(&value),
                Some(Field::Plural) => push(&mut entry.plural, &value),
                Some(Field::Str(n)) => entry.strs[n].push_str(&value),
                None => return Err(fail("a string without a keyword")),
            }
            continue;
        }
        let (keyword, rest) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| fail("a keyword without a string"))?;
        let value = unquote(rest.trim()).map_err(|err| fail(&err))?;
        let next = match keyword {
            "msgctxt" => Field::Context,
            "msgid" => Field::Id,
            "msgid_plural" => Field::Plural,
            "msgstr" => Field::Str(0),
            other => {
                let n = other
                    .strip_prefix("msgstr[")
                    .and_then(|rest| rest.strip_suffix(']'))
                    .and_then(|n| n.parse::<usize>().ok())
                    .ok_or_else(|| fail(&format!("unknown keyword {other}")))?;
                Field::Str(n)
            }
        };
        // A msgctxt or msgid after the strings starts the next entry.
        if matches!(next, Field::Context | Field::Id) && matches!(field, Some(Field::Str(_))) {
            entries.push(std::mem::take(&mut entry));
            field = None;
        }
        let in_order = match (field, next) {
            (None, Field::Context | Field::Id)
            | (Some(Field::Context), Field::Id)
            | (Some(Field::Id), Field::Plural | Field::Str(0))
            | (Some(Field::Plural), Field::Str(0)) => true,
            (Some(Field::Str(m)), Field::Str(n)) => entry.plural.is_some() && n == m + 1,
            _ => false,
        };
        if !in_order {
            return Err(fail(&format!("{keyword} out of order")));
        }
        match next {
            Field::Context => entry.context = Some(value),
            Field::Id => entry.id = value,
            Field::Plural => entry.plural = Some(value),
            Field::Str(_) => entry.strs.push(value),
        }
        field = Some(next);
    }
    if field.is_some() {
        entries.push(entry);
    }
    if let Some(entry) = entries.iter().find(|entry| entry.strs.is_empty()) {
        return Err(format!("msgid {:?} has no msgstr", entry.id));
    }
    Ok(entries)
}

fn push(field: &mut Option<String>, value: &str) {
    field.get_or_insert_with(String::new).push_str(value);
}

/// The value of a quoted string, its escapes undone.
fn unquote(quoted: &str) -> Result<String, String> {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| format!("not a quoted string: {quoted}"))?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                other => return Err(format!("unknown escape \\{}", other.unwrap_or(' '))),
            },
            '"' => return Err("a quote inside a string".to_owned()),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// `value` as a quoted `.po` string.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A catalog as a `.po` file, entries in the order given.
pub fn write(entries: &[Entry]) -> String {
    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for comment in &entry.comments {
            if comment.is_empty() {
                out.push_str("#\n");
            } else {
                let _ = writeln!(out, "# {comment}");
            }
        }
        for comment in &entry.extracted {
            let _ = writeln!(out, "#. {comment}");
        }
        for reference in &entry.references {
            let _ = writeln!(out, "#: {reference}");
        }
        if !entry.flags.is_empty() {
            let _ = writeln!(out, "#, {}", entry.flags.join(", "));
        }
        if let Some(context) = &entry.context {
            let _ = writeln!(out, "msgctxt {}", quote(context));
        }
        if entry.id.is_empty() {
            // The header: one line per field, as gettext's tools write it.
            out.push_str("msgid \"\"\nmsgstr \"\"\n");
            for line in entry
                .strs
                .first()
                .map_or("", String::as_str)
                .split_inclusive('\n')
            {
                let _ = writeln!(out, "{}", quote(line));
            }
            continue;
        }
        let _ = writeln!(out, "msgid {}", quote(&entry.id));
        if let Some(plural) = &entry.plural {
            let _ = writeln!(out, "msgid_plural {}", quote(plural));
            for (n, form) in entry.strs.iter().enumerate() {
                let _ = writeln!(out, "msgstr[{n}] {}", quote(form));
            }
        } else {
            let form = entry.strs.first().map_or("", String::as_str);
            let _ = writeln!(out, "msgstr {}", quote(form));
        }
    }
    out
}

/// The `.mo` file of a catalog, as GNU `msgfmt` makes it without a hash
/// table: what the C library's `gettext` reads ("The Format of GNU MO
/// Files", GNU gettext manual 10.3). Entries that are fuzzy or not
/// translated in full are left out, as `msgfmt` leaves them out, so the
/// English text shows instead.
pub fn mo(entries: &[Entry]) -> Vec<u8> {
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = entries
        .iter()
        .filter(|entry| !entry.is_fuzzy() && entry.is_translated())
        .map(|entry| {
            let mut original = entry.key().into_bytes();
            if let Some(plural) = &entry.plural {
                original.push(0);
                original.extend_from_slice(plural.as_bytes());
            }
            (original, entry.strs.join("\0").into_bytes())
        })
        .collect();
    // Sorted by the original's bytes: without a hash table, `gettext`
    // finds a message by binary search.
    pairs.sort();
    pairs.dedup_by(|a, b| a.0 == b.0);

    let count = pairs.len();
    let header = 28;
    let originals_at = header;
    let translations_at = originals_at + 8 * count;
    let data_at = translations_at + 8 * count;
    let mut tables = Vec::with_capacity(16 * count);
    let mut data = Vec::new();
    // The table of originals, then that of translations, then the strings
    // of both, each followed by a NUL that its length does not count.
    for translated in [false, true] {
        for pair in &pairs {
            let string = if translated { &pair.1 } else { &pair.0 };
            tables.extend_from_slice(&word(string.len()));
            tables.extend_from_slice(&word(data_at + data.len()));
            data.extend_from_slice(string);
            data.push(0);
        }
    }
    let mut out = Vec::with_capacity(header + tables.len() + data.len());
    out.extend_from_slice(&0x9504_12de_u32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes()); // revision
    out.extend_from_slice(&word(count));
    out.extend_from_slice(&word(originals_at));
    out.extend_from_slice(&word(translations_at));
    out.extend_from_slice(&0_u32.to_le_bytes()); // hash table size
    out.extend_from_slice(&word(translations_at + 8 * count)); // hash table offset
    out.extend_from_slice(&tables);
    out.extend_from_slice(&data);
    out
}

/// A length or an offset as a little-endian 32-bit word. A catalog is far
/// below 4 GiB; a larger one would be a broken build, so it saturates
/// rather than wraps.
fn word(n: usize) -> [u8; 4] {
    u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes()
}
