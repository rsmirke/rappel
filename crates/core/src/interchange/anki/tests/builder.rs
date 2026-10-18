//! Builds Anki packages by hand: a schema 11 collection (the legacy
//! layout, which every format may hold) with any rows, hostile ones
//! included, zipped as Anki zips it.

use std::io::{Cursor, Write};

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// The Basic note type's id in [`Builder::new`].
pub(super) const BASIC: i64 = 1;
/// The Cloze note type's id.
pub(super) const CLOZE: i64 = 2;
/// The Default deck's id.
pub(super) const DEFAULT_DECK: i64 = 1;

/// The package's container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Container {
    /// `collection.anki21` and a JSON media list, no `meta` (Anki 2.1).
    Legacy,
    /// `meta` (version 3), `collection.anki21b`, a protobuf media list and
    /// media files, all zstd (the current format; the collection inside is
    /// still schema 11 here).
    Current,
}

pub(super) struct Builder {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    conn: Connection,
    models: serde_json::Map<String, Value>,
    decks: serde_json::Map<String, Value>,
    media: Vec<(String, Vec<u8>)>,
    /// Entries added as they are (name, data, compressed with zstd).
    raw: Vec<(String, Vec<u8>)>,
    version: i64,
}

impl Builder {
    /// A collection with the Basic and Cloze note types and the Default
    /// deck, and nothing else.
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.anki2");
        let conn = Connection::open(&path).unwrap();
        // Loose types, so a test can put anything anywhere.
        conn.execute_batch(
            "CREATE TABLE col (id integer primary key, crt, mod, scm, ver, dty, usn, ls,
                               conf, models, decks, dconf, tags);
             CREATE TABLE notes (id integer primary key, guid, mid, mod, usn, tags, flds,
                                 sfld, csum, flags, data);
             CREATE TABLE cards (id primary key, nid, did, ord, mod, usn, type, queue,
                                 due, ivl, factor, reps, lapses, left, odue, odid, flags, data);
             CREATE TABLE revlog (id integer primary key, cid, usn, ease, ivl, lastIvl, factor,
                                  time, type);
             CREATE INDEX ix_revlog_cid ON revlog (cid);",
        )
        .unwrap();
        let mut b = Self {
            _dir: dir,
            path,
            conn,
            models: serde_json::Map::new(),
            decks: serde_json::Map::new(),
            media: Vec::new(),
            raw: Vec::new(),
            version: 11,
        };
        b.model(
            BASIC,
            json!({
                "name": "Basic", "type": 0,
                "flds": [{"name": "Front", "ord": 0}, {"name": "Back", "ord": 1}],
                "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}",
                           "afmt": "{{FrontSide}}\n\n<hr id=answer>\n\n{{Back}}"}]
            }),
        );
        b.model(
            CLOZE,
            json!({
                "name": "Cloze", "type": 1,
                "flds": [{"name": "Text", "ord": 0}, {"name": "Back Extra", "ord": 1}],
                "tmpls": [{"name": "Cloze", "ord": 0, "qfmt": "{{cloze:Text}}",
                           "afmt": "{{cloze:Text}}<br>\n{{Back Extra}}"}]
            }),
        );
        b.deck(DEFAULT_DECK, "Default");
        b
    }

    pub(super) fn model(&mut self, id: i64, model: Value) -> &mut Self {
        self.models.insert(id.to_string(), model);
        self
    }

    pub(super) fn deck(&mut self, id: i64, name: &str) -> &mut Self {
        self.decks
            .insert(id.to_string(), json!({"name": name, "dyn": 0}));
        self
    }

    pub(super) fn version(&mut self, version: i64) -> &mut Self {
        self.version = version;
        self
    }

    /// A note with `fields` (joined by U+001F) and `tags`.
    pub(super) fn note(&mut self, id: i64, mid: i64, fields: &[&str], tags: &str) -> &mut Self {
        self.conn
            .execute(
                "INSERT INTO notes VALUES (?1, 'g', ?2, 1700000000, 0, ?3, ?4, '', 0, 0, '')",
                params![id, mid, tags, fields.join("\u{1f}")],
            )
            .unwrap();
        self
    }

    /// A new card of note `nid` in deck `did`.
    pub(super) fn card(&mut self, id: i64, nid: i64, did: i64, ord: i64) -> &mut Self {
        self.card_with(id, nid, did, ord, 0, 0, 0)
    }

    /// A card with its home deck `odid` (0 for none), type and queue.
    #[expect(clippy::too_many_arguments, reason = "a test builder of one table row")]
    pub(super) fn card_with(
        &mut self,
        id: i64,
        nid: i64,
        did: i64,
        ord: i64,
        odid: i64,
        kind: i64,
        queue: i64,
    ) -> &mut Self {
        self.conn
            .execute(
                "INSERT INTO cards VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, ?6, 0, 0, 0, 0, 0, 0, 0, ?7, 0, '')",
                params![id, nid, did, ord, kind, queue, odid],
            )
            .unwrap();
        self
    }

    /// A log entry.
    pub(super) fn rev(&mut self, id: i64, cid: i64, ease: i64, time: i64, kind: i64) -> &mut Self {
        self.conn
            .execute(
                "INSERT INTO revlog VALUES (?1, ?2, 0, ?3, 0, 0, 0, ?4, ?5)",
                params![id, cid, ease, time, kind],
            )
            .unwrap();
        self
    }

    /// Any SQL on the collection.
    pub(super) fn sql(&mut self, sql: &str) -> &mut Self {
        self.conn.execute_batch(sql).unwrap();
        self
    }

    pub(super) fn media(&mut self, name: &str, bytes: &[u8]) -> &mut Self {
        self.media.push((name.to_owned(), bytes.to_vec()));
        self
    }

    /// An entry added as it is.
    pub(super) fn raw(&mut self, name: &str, bytes: &[u8]) -> &mut Self {
        self.raw.push((name.to_owned(), bytes.to_vec()));
        self
    }

    /// The collection's bytes, with its `col` row.
    pub(super) fn collection(&mut self) -> Vec<u8> {
        self.conn.execute("DELETE FROM col", []).unwrap();
        self.conn
            .execute(
                "INSERT INTO col VALUES (1, 0, 0, 0, ?1, 0, 0, 0, '{}', ?2, ?3, '{}', '{}')",
                params![
                    self.version,
                    Value::Object(self.models.clone()).to_string(),
                    Value::Object(self.decks.clone()).to_string()
                ],
            )
            .unwrap();
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        std::fs::read(&self.path).unwrap()
    }

    /// The package.
    pub(super) fn build(&mut self, container: Container) -> Vec<u8> {
        let collection = self.collection();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Deflated);
        let stored = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
        let raw_names: Vec<String> = self.raw.iter().map(|(n, _)| n.clone()).collect();
        let has = |name: &str| raw_names.iter().any(|n| n == name);
        match container {
            Container::Legacy => {
                if !has("collection.anki21") {
                    zip.start_file("collection.anki21", deflated).unwrap();
                    zip.write_all(&collection).unwrap();
                }
                if !has("media") {
                    let map: serde_json::Map<String, Value> = self
                        .media
                        .iter()
                        .enumerate()
                        .map(|(i, (name, _))| (i.to_string(), Value::String(name.clone())))
                        .collect();
                    zip.start_file("media", deflated).unwrap();
                    zip.write_all(Value::Object(map).to_string().as_bytes())
                        .unwrap();
                }
                for (i, (_, bytes)) in self.media.iter().enumerate() {
                    zip.start_file(i.to_string(), stored).unwrap();
                    zip.write_all(bytes).unwrap();
                }
            }
            Container::Current => {
                if !has("meta") {
                    zip.start_file("meta", stored).unwrap();
                    zip.write_all(&[0x08, 3]).unwrap();
                }
                if !has("collection.anki21b") {
                    zip.start_file("collection.anki21b", stored).unwrap();
                    zip.write_all(&zstd(&collection)).unwrap();
                }
                if !has("media") {
                    let mut list = Vec::new();
                    for (name, bytes) in &self.media {
                        let mut entry = Vec::new();
                        proto_bytes(&mut entry, 1, name.as_bytes());
                        proto_varint(&mut entry, 2, bytes.len() as u64);
                        proto_bytes(&mut list, 1, &entry);
                    }
                    zip.start_file("media", stored).unwrap();
                    zip.write_all(&zstd(&list)).unwrap();
                }
                for (i, (_, bytes)) in self.media.iter().enumerate() {
                    zip.start_file(i.to_string(), stored).unwrap();
                    zip.write_all(&zstd(bytes)).unwrap();
                }
            }
        }
        for (name, bytes) in &self.raw {
            zip.start_file(name.as_str(), stored).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
}

/// `bytes` as one zstd frame.
pub(super) fn zstd(bytes: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(bytes, ruzstd::encoding::CompressionLevel::Fastest)
}

/// A zstd frame of `total` zero bytes made of RLE blocks (4 bytes per 128
/// KiB): a bomb, tiny however large it says it is.
pub(super) fn zstd_bomb(total: u64) -> Vec<u8> {
    zstd_bomb_after(&[], total)
}

/// [`zstd_bomb`] after the bytes of `prefix` (a raw block), so the
/// data starts as a file of a kind would.
pub(super) fn zstd_bomb_after(prefix: &[u8], total: u64) -> Vec<u8> {
    const BLOCK: u64 = 128 << 10;
    // Magic, a frame header without content size or checksum, a window
    // of 128 KiB (exponent 7).
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 7 << 3];
    if !prefix.is_empty() {
        // A raw block (type 0), not the last.
        let header = u32::try_from(prefix.len()).unwrap() << 3;
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(prefix);
    }
    let mut left = total;
    while left > 0 {
        let size = left.min(BLOCK);
        left -= size;
        let last = u32::from(left == 0);
        let header = last | (1 << 1) | (u32::try_from(size).unwrap() << 3);
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.push(0);
    }
    out
}

pub(super) fn proto_varint(out: &mut Vec<u8>, field: u64, mut value: u64) {
    varint(out, field << 3);
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap();
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

pub(super) fn proto_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    varint(out, (field << 3) | 2);
    varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap();
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// The entries of a zip archive, as (name, data).
pub(super) fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..archive.len())
        .map(|i| {
            let mut file = archive.by_index(i).unwrap();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut data).unwrap();
            (file.name().unwrap().into_owned(), data)
        })
        .collect()
}

/// A zip archive of `entries`, each stored.
pub(super) fn zip_of(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::DEFAULT.compression_method(CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(name.as_str(), stored).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
