//! Reading `cards.json` as a stream (ADR 0053): the top-level object is
//! walked key by key and each list record by record, and each record is
//! handed to a [`Sink`] and dropped. Nothing of the document is kept here.
//!
//! The records are the serde types of [`super::format`], so a record is
//! parsed exactly as before (unknown fields denied, `deleted_at` required).
//! `serde_json` reads every string whole before handing it over, so the
//! reader under it ([`JsonInput`]) refuses a string longer than any field
//! of the format can be: that, the cap on the entry's size and the bounded
//! notes of the sink are what keep memory bounded.

use std::fmt;
use std::io::{self, BufReader, Read, Seek};

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::archive::{Archive, EntryReader};
use super::format::{
    CardRecord, CardTagRecord, DeckRecord, EventRecord, Header, SettingsRecord, TagRecord,
};
use super::{ImportProblem, Limits};

/// The lists of `cards.json`, in the order an export writes them and the
/// checks run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Table {
    Decks,
    Cards,
    Tags,
    CardTags,
    Events,
}

impl Table {
    /// Every list, in file order.
    pub(super) const ALL: [Self; 5] = [
        Self::Decks,
        Self::Cards,
        Self::Tags,
        Self::CardTags,
        Self::Events,
    ];

    /// The list's key, as problems name it.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Decks => "decks",
            Self::Cards => "cards",
            Self::Tags => "tags",
            Self::CardTags => "card_tags",
            Self::Events => "review_events",
        }
    }

    fn position(self) -> usize {
        match self {
            Self::Decks => 0,
            Self::Cards => 1,
            Self::Tags => 2,
            Self::CardTags => 3,
            Self::Events => 4,
        }
    }
}

/// What one read of `cards.json` looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Focus {
    /// Everything in one read, which needs the lists in file order.
    All,
    /// Every record parsed and dropped, nothing handed over: the shape of
    /// the whole document, as parsing it into one value would check it.
    Structure,
    /// One list, handed over record by record; the rest is skipped.
    Table(Table),
}

/// Marks a read the sink stopped. The sink keeps the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stopped;

/// What a read hands its records to.
pub(super) trait Sink {
    /// `format_version`, with [`Focus::All`]. It must be the first key.
    fn version(&mut self, version: u32) -> Result<(), Stopped>;
    /// `exported_at` (not with [`Focus::Table`]).
    fn exported_at(&mut self, at: i64);
    /// `settings` (not with [`Focus::Table`]).
    fn settings(&mut self, settings: SettingsRecord);
    /// An item of `media` (not with [`Focus::Table`]).
    fn media_item(&mut self);
    /// A list begins, with [`Focus::All`] or its [`Focus::Table`].
    fn table_starts(&mut self, table: Table) -> Result<(), Stopped>;
    fn deck(&mut self, index: usize, deck: DeckRecord) -> Result<(), Stopped>;
    fn card(&mut self, index: usize, card: CardRecord) -> Result<(), Stopped>;
    fn tag(&mut self, index: usize, tag: TagRecord) -> Result<(), Stopped>;
    fn card_tag(&mut self, index: usize, card_tag: CardTagRecord) -> Result<(), Stopped>;
    fn event(&mut self, index: usize, event: EventRecord) -> Result<(), Stopped>;
    /// A list ended (only after all its records were handed over).
    fn table_ends(&mut self, table: Table) -> Result<(), Stopped>;
    /// With [`Focus::All`], the lists must come in file order: a list
    /// came out of it.
    fn out_of_order(&mut self) -> Stopped;
}

/// A record of one of the lists.
trait Row: for<'de> Deserialize<'de> {
    const TABLE: Table;
    fn give<S: Sink>(self, sink: &mut S, index: usize) -> Result<(), Stopped>;
}

impl Row for DeckRecord {
    const TABLE: Table = Table::Decks;
    fn give<S: Sink>(self, sink: &mut S, index: usize) -> Result<(), Stopped> {
        sink.deck(index, self)
    }
}

impl Row for CardRecord {
    const TABLE: Table = Table::Cards;
    fn give<S: Sink>(self, sink: &mut S, index: usize) -> Result<(), Stopped> {
        sink.card(index, self)
    }
}

impl Row for TagRecord {
    const TABLE: Table = Table::Tags;
    fn give<S: Sink>(self, sink: &mut S, index: usize) -> Result<(), Stopped> {
        sink.tag(index, self)
    }
}

impl Row for CardTagRecord {
    const TABLE: Table = Table::CardTags;
    fn give<S: Sink>(self, sink: &mut S, index: usize) -> Result<(), Stopped> {
        sink.card_tag(index, self)
    }
}

impl Row for EventRecord {
    const TABLE: Table = Table::Events;
    fn give<S: Sink>(self, sink: &mut S, index: usize) -> Result<(), Stopped> {
        sink.event(index, self)
    }
}

/// The top-level keys, in the order of [`super::format`]'s fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Key {
    FormatVersion,
    ExportedAt,
    Decks,
    Cards,
    Tags,
    CardTags,
    ReviewEvents,
    Settings,
    Media,
}

/// The top-level field names, in declaration order.
const FIELDS: [&str; 9] = [
    "format_version",
    "exported_at",
    "decks",
    "cards",
    "tags",
    "card_tags",
    "review_events",
    "settings",
    "media",
];

impl Key {
    fn position(self) -> usize {
        self as usize
    }

    fn field(self) -> Field {
        match self {
            Self::FormatVersion => Field::Version,
            Self::ExportedAt => Field::ExportedAt,
            Self::Settings => Field::Settings,
            Self::Media => Field::Media,
            Self::Decks => Field::List(Table::Decks),
            Self::Cards => Field::List(Table::Cards),
            Self::Tags => Field::List(Table::Tags),
            Self::CardTags => Field::List(Table::CardTags),
            Self::ReviewEvents => Field::List(Table::Events),
        }
    }
}

/// What a top-level key holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Version,
    ExportedAt,
    Settings,
    Media,
    List(Table),
}

/// The whole document. It accepts what parsing into one value of the
/// document type accepted: an object with each of [`FIELDS`] once, in any
/// order, and nothing else (the same errors at the same places, so a
/// refusal names the same line and column).
struct TopLevel<'s, S> {
    sink: &'s mut S,
    focus: Focus,
}

impl<'de, S: Sink> DeserializeSeed<'de> for TopLevel<'_, S> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_struct("Document", &FIELDS, self)
    }
}

fn stopped<E: de::Error>() -> E {
    E::custom("stopped")
}

impl<'de, S: Sink> Visitor<'de> for TopLevel<'_, S> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Rappel export")
    }

    /// An array as the document, which serde reads as the fields in
    /// order. Only a one-item array gets here once the version was read
    /// on its own (that read takes one field), and it is refused for its
    /// length as before; with [`Focus::All`] it is left to that read.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        if self.focus == Focus::All {
            return Err(stopped());
        }
        if seq.next_element::<u32>()?.is_none() {
            return Err(de::Error::invalid_length(0, &self));
        }
        if seq.next_element::<i64>()?.is_none() {
            return Err(de::Error::invalid_length(1, &self));
        }
        Err(de::Error::custom("a document as an array"))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let TopLevel { sink, focus } = self;
        let mut seen = [false; 9];
        let mut next_table = 0;
        while let Some(key) = map.next_key::<Key>()? {
            if std::mem::replace(&mut seen[key.position()], true) {
                return Err(de::Error::duplicate_field(FIELDS[key.position()]));
            }
            let first = seen.iter().filter(|s| **s).count() == 1;
            if focus == Focus::All && first && key != Key::FormatVersion {
                return Err(stopped());
            }
            let skip_scalars = matches!(focus, Focus::Table(_));
            match key.field() {
                Field::Version if focus == Focus::All => {
                    let version = map.next_value::<u32>()?;
                    sink.version(version).map_err(|Stopped| stopped())?;
                }
                Field::Version => {
                    map.next_value::<u32>()?;
                }
                Field::ExportedAt | Field::Settings | Field::Media if skip_scalars => {
                    map.next_value::<IgnoredAny>()?;
                }
                Field::ExportedAt => sink.exported_at(map.next_value::<i64>()?),
                Field::Settings => sink.settings(map.next_value::<SettingsRecord>()?),
                Field::Media => map.next_value_seed(MediaSeed { sink: &mut *sink })?,
                Field::List(table) => {
                    let handed = match focus {
                        Focus::All => {
                            if table.position() != next_table {
                                let Stopped = sink.out_of_order();
                                return Err(stopped());
                            }
                            next_table += 1;
                            true
                        }
                        Focus::Table(t) => t == table,
                        Focus::Structure => false,
                    };
                    if handed {
                        sink.table_starts(table).map_err(|Stopped| stopped())?;
                    }
                    let parse = handed || focus == Focus::Structure;
                    match (table, parse) {
                        (_, false) => {
                            map.next_value::<IgnoredAny>()?;
                        }
                        (Table::Decks, true) => {
                            map.next_value_seed(List::<S, DeckRecord>::new(sink, handed))?;
                        }
                        (Table::Cards, true) => {
                            map.next_value_seed(List::<S, CardRecord>::new(sink, handed))?;
                        }
                        (Table::Tags, true) => {
                            map.next_value_seed(List::<S, TagRecord>::new(sink, handed))?;
                        }
                        (Table::CardTags, true) => {
                            map.next_value_seed(List::<S, CardTagRecord>::new(sink, handed))?;
                        }
                        (Table::Events, true) => {
                            map.next_value_seed(List::<S, EventRecord>::new(sink, handed))?;
                        }
                    }
                    if handed {
                        sink.table_ends(table).map_err(|Stopped| stopped())?;
                    }
                }
            }
        }
        // As the derived code does: the first missing field, in
        // declaration order.
        if let Some(missing) = seen.iter().position(|s| !s) {
            return Err(de::Error::missing_field(FIELDS[missing]));
        }
        Ok(())
    }
}

/// One list: each record parsed with its serde type, then handed over
/// (or dropped, for [`Focus::Structure`]).
struct List<'s, S, R> {
    sink: &'s mut S,
    hand: bool,
    row: std::marker::PhantomData<R>,
}

impl<'s, S, R> List<'s, S, R> {
    fn new(sink: &'s mut S, hand: bool) -> Self {
        Self {
            sink,
            hand,
            row: std::marker::PhantomData,
        }
    }
}

impl<'de, S: Sink, R: Row> DeserializeSeed<'de> for List<'_, S, R> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, S: Sink, R: Row> Visitor<'de> for List<'_, S, R> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the list {}", R::TABLE.name())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut index = 0;
        while let Some(row) = seq.next_element::<R>()? {
            if self.hand {
                row.give(self.sink, index).map_err(|Stopped| stopped())?;
            }
            index += 1;
        }
        Ok(())
    }
}

/// `media`: any JSON values, each skipped as parsing it into a
/// `serde_json::Value` would read it (strings checked, nesting stopped at
/// `serde_json`'s recursion limit), and counted.
struct MediaSeed<'s, S> {
    sink: &'s mut S,
}

impl<'de, S: Sink> DeserializeSeed<'de> for MediaSeed<'_, S> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, S: Sink> Visitor<'de> for MediaSeed<'_, S> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of media")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element::<Skip>()?.is_some() {
            self.sink.media_item();
        }
        Ok(())
    }
}

/// Any JSON value, read and dropped. Unlike [`IgnoredAny`], it goes
/// through `deserialize_any`, so it is read as a `serde_json::Value` is:
/// strings are checked for UTF-8 and nesting counts against the
/// recursion limit.
struct Skip;

impl<'de> Deserialize<'de> for Skip {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SkipVisitor)
    }
}

struct SkipVisitor;

impl<'de> Visitor<'de> for SkipVisitor {
    type Value = Skip;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Skip, E> {
        Ok(Skip)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Skip, E> {
        Ok(Skip)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Skip, E> {
        Ok(Skip)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Skip, E> {
        Ok(Skip)
    }

    fn visit_str<E>(self, _: &str) -> Result<Skip, E> {
        Ok(Skip)
    }

    fn visit_unit<E>(self) -> Result<Skip, E> {
        Ok(Skip)
    }

    fn visit_none<E>(self) -> Result<Skip, E> {
        Ok(Skip)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Skip, A::Error> {
        while seq.next_element::<Skip>()?.is_some() {}
        Ok(Skip)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Skip, A::Error> {
        while map.next_entry::<Skip, Skip>()?.is_some() {}
        Ok(Skip)
    }
}

/// The longest string, as it is in the file, that any field can need: a
/// card side at its limit with every character escaped as `\u00XX` (six
/// bytes for one). A longer string refuses the file before `serde_json`
/// buffers it.
pub(super) fn string_limit(limits: &Limits) -> u64 {
    let longest = limits.card_text.max(limits.deck_name).max(limits.tag_name);
    (longest as u64).saturating_mul(6).max(4 << 10)
}

/// Follows the strings of the JSON text as it is read: whether a string
/// is open, whether the last byte was a backslash, and how long the open
/// string is so far. While the text is valid JSON it is in step with the
/// parser: outside strings no `"` byte appears, and inside one only `\`
/// escapes the next byte (the four digits of `\u` are never `"` or `\`).
/// A raw line break cannot be inside a valid string, so lines are counted
/// outside strings only.
#[derive(Debug, Default)]
struct Strings {
    open: bool,
    escaped: bool,
    run: u64,
    line: u64,
    line_start: u64,
    offset: u64,
}

impl Strings {
    /// The position in `chunk` of the byte that makes a string longer
    /// than `limit`, with its line and column (as `serde_json` counts them).
    fn scan(&mut self, chunk: &[u8], limit: u64) -> Option<(usize, u64, u64)> {
        let mut i = 0;
        while i < chunk.len() {
            if !self.open {
                let Some(p) = chunk[i..].iter().position(|&b| b == b'"' || b == b'\n') else {
                    break;
                };
                i += p;
                if chunk[i] == b'\n' {
                    self.line += 1;
                    self.line_start = self.offset + i as u64 + 1;
                } else {
                    self.open = true;
                    self.run = 0;
                }
                i += 1;
                continue;
            }
            // In a string: the byte after a backslash, then everything up
            // to the next quote or backslash, counts.
            if self.escaped {
                if self.run >= limit {
                    return Some(self.trip(i));
                }
                self.escaped = false;
                self.run += 1;
                i += 1;
                continue;
            }
            let rest = &chunk[i..];
            let stop = rest.iter().position(|&b| b == b'"' || b == b'\\');
            let span = stop.unwrap_or(rest.len());
            if self.run + span as u64 > limit {
                let at = i + usize::try_from(limit - self.run).unwrap_or(0);
                return Some(self.trip(at));
            }
            self.run += span as u64;
            i += span;
            if stop.is_none() {
                break;
            }
            if chunk[i] == b'"' {
                self.open = false;
            } else {
                if self.run >= limit {
                    return Some(self.trip(i));
                }
                self.run += 1;
                self.escaped = true;
            }
            i += 1;
        }
        self.offset += chunk.len() as u64;
        None
    }

    /// Where the string got too long: `at` in the current chunk, with its
    /// line and column.
    fn trip(&self, at: usize) -> (usize, u64, u64) {
        let offset = self.offset + at as u64;
        (at, self.line + 1, offset - self.line_start + 1)
    }
}

/// The reader `serde_json` pulls `cards.json` from: the entry's data
/// (capped and hashed by [`EntryReader`]), with strings longer than
/// [`string_limit`] refused.
struct JsonInput<'a, R: Read> {
    entry: EntryReader<'a, R>,
    strings: Strings,
    limit: u64,
    too_long: Option<ImportProblem>,
}

impl<R: Read> Read for JsonInput<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.too_long.is_some() {
            return Err(io::Error::other("a string is too long"));
        }
        let n = self.entry.read(buf)?;
        let Some((at, line, column)) = self.strings.scan(&buf[..n], self.limit) else {
            return Ok(n);
        };
        self.too_long = Some(ImportProblem::Json(format!(
            "a string longer than {} bytes at line {line}, column {column}",
            self.limit
        )));
        // The bytes before it go to the parser, so an error there is
        // reported first; the next read fails. Nothing at all would read
        // as the end of the file.
        if at == 0 {
            Err(io::Error::other("a string is too long"))
        } else {
            Ok(at)
        }
    }
}

/// How one read of `cards.json` ended.
#[derive(Debug)]
pub(super) struct ReadEnd<T> {
    /// What the parser returned.
    pub(super) parsed: Result<T, serde_json::Error>,
    /// The entry's SHA-256, whole (read to its end or to the failure).
    pub(super) sha256: [u8; 32],
    /// The entry could not be read: damaged, a CRC mismatch, too large.
    pub(super) archive: Option<ImportProblem>,
    /// A string was longer than [`string_limit`].
    pub(super) too_long: Option<ImportProblem>,
}

impl<T> ReadEnd<T> {
    /// The problem this read found in the file, in the order the checks
    /// come: the archive, then a string too long (only if that is what
    /// stopped the parser: an error before it is reported first), then
    /// the JSON. `stopped` is whether the sink stopped the read, whose
    /// reason the sink holds.
    pub(super) fn problem(&self, stopped: bool) -> Option<ImportProblem> {
        if let Some(problem) = &self.archive {
            return Some(problem.clone());
        }
        let err = self.parsed.as_ref().err()?;
        if err.classify() == serde_json::error::Category::Io
            && let Some(problem) = &self.too_long
        {
            return Some(problem.clone());
        }
        if stopped {
            return None;
        }
        Some(json_problem(err))
    }
}

/// Reads `cards.json` once with `parse`, then reads what is left of the
/// entry so its size, CRC and hash are checked whatever the parser did.
fn read_json<'a, R: Read + Seek + 'a, T>(
    archive: &'a mut Archive<R>,
    limits: &Limits,
    parse: impl FnOnce(&mut JsonInput<'a, R>) -> serde_json::Result<T>,
) -> Result<ReadEnd<T>, ImportProblem> {
    let entry = archive.json_entry();
    let mut input = JsonInput {
        entry: archive.open_entry(&entry, limits.json)?,
        strings: Strings::default(),
        limit: string_limit(limits),
        too_long: None,
    };
    let parsed = parse(&mut input);
    input.entry.drain();
    let archive_problem = input.entry.failure();
    let too_long = input.too_long.take();
    let (_, sha256) = input.entry.finish();
    Ok(ReadEnd {
        parsed,
        sha256,
        archive: archive_problem,
        too_long,
    })
}

/// A JSON reader over `input`. `serde_json` reads byte by byte, so the
/// buffer keeps that cheap.
fn deserializer<'i, 'a, R: Read>(
    input: &'i mut JsonInput<'a, R>,
) -> serde_json::Deserializer<serde_json::de::IoRead<BufReader<&'i mut JsonInput<'a, R>>>> {
    serde_json::Deserializer::from_reader(BufReader::with_capacity(64 << 10, input))
}

/// Reads only `format_version`, as before the rest of the file is looked
/// at: everything else is skipped, so a file of another version is refused
/// for its version, not for fields this version does not know.
pub(super) fn read_header<R: Read + Seek>(
    archive: &mut Archive<R>,
    limits: &Limits,
) -> Result<ReadEnd<Header>, ImportProblem> {
    read_json(archive, limits, |input| {
        let mut de = deserializer(input);
        let header = Header::deserialize(&mut de)?;
        de.end()?;
        Ok(header)
    })
}

/// Reads the document with `focus`, handing records to `sink`.
pub(super) fn read_document<R: Read + Seek, S: Sink>(
    archive: &mut Archive<R>,
    limits: &Limits,
    sink: &mut S,
    focus: Focus,
) -> Result<ReadEnd<()>, ImportProblem> {
    read_json(archive, limits, |input| {
        let mut de = deserializer(input);
        TopLevel { sink, focus }.deserialize(&mut de)?;
        de.end()
    })
}

/// Only the kind of error and its place: serde's message can quote any
/// amount of the file's text (a string where a number belongs).
pub(super) fn json_problem(err: &serde_json::Error) -> ImportProblem {
    let kind = match err.classify() {
        serde_json::error::Category::Io => "cannot be read",
        serde_json::error::Category::Syntax => "invalid JSON",
        serde_json::error::Category::Data => "unexpected field, value or type",
        serde_json::error::Category::Eof => "ends too early",
    };
    ImportProblem::Json(format!(
        "{kind} at line {}, column {}",
        err.line(),
        err.column()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trips(text: &[u8], limit: u64) -> Option<(usize, u64, u64)> {
        Strings::default().scan(text, limit)
    }

    #[test]
    fn strings_are_measured_as_written() {
        assert_eq!(trips(br#"{"a": "abc"}"#, 3), None);
        assert_eq!(trips(br#"{"a": "abcd"}"#, 3), Some((10, 1, 11)));
        // Escapes count as the bytes they take; an escaped quote does not
        // end the string.
        assert_eq!(trips(br#"["\"\"x"]"#, 4), Some((6, 1, 7)));
        assert_eq!(trips(br#"["\\"]"#, 4), None);
        // Outside strings nothing counts, whitespace included.
        assert_eq!(trips(&[b' '; 100], 3), None);
        // Lines and columns.
        assert_eq!(trips(b"[\n\n  \"abcd\"]", 3), Some((9, 3, 7)));
    }

    #[test]
    fn a_string_cut_across_chunks_is_followed() {
        let mut strings = Strings::default();
        assert_eq!(strings.scan(br#"["ab"#, 3), None);
        assert_eq!(strings.scan(br#"\"c"#, 3), Some((1, 1, 6)));
    }

    #[test]
    fn the_limit_fits_the_longest_side_written_with_escapes() {
        let limits = Limits::DEFAULT;
        assert!(string_limit(&limits) >= 6 * limits.card_text as u64);
        let tight = Limits {
            card_text: 1,
            deck_name: 1,
            tag_name: 1,
            ..Limits::DEFAULT
        };
        // Ids and keys always fit.
        assert!(string_limit(&tight) >= 64);
    }
}
