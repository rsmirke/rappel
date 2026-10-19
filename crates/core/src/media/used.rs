//! Which media files a database uses (ADR 0057): the hashes its card sides
//! name, live and deleted cards alike, and the hashes of its `media` rows.
//!
//! A hash is what counts, never the extension a card wrote: a card naming
//! `<hash>.png` keeps every file of that hash, so nothing a card could
//! show is ever taken for unused.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

use super::{SCHEME, parse_hex};
use crate::content::media_refs;
use crate::error::Result;

/// A set of SHA-256 hashes.
pub(crate) type Hashes = HashSet<[u8; 32]>;

/// Adds to `out` every hash `md` names: each reference
/// [`media_refs`] finds (an image or a link, whatever its kind; it follows
/// the parsed tree, so a target spelled with an escape or an entity
/// counts), and, to be safe, every `media:` followed by 64 lowercase hex
/// digits anywhere in the text, even where it is not a reference (in code,
/// say). The second can only keep more files, never fewer.
pub(crate) fn side_hashes(md: &str, out: &mut Hashes) {
    for media in media_refs(md) {
        out.insert(*media.hash());
    }
    for (at, _) in md.match_indices(SCHEME) {
        let rest = &md.as_bytes()[at + SCHEME.len()..];
        if let Some(hash) = rest.get(..64).and_then(parse_hex) {
            out.insert(hash);
        }
    }
}

/// Whether the side `md` can name a media file at all: only with the
/// letters `media` in it, or with an entity reference (`&#109;edia:`,
/// which starts with `&`). `CommonMark` decodes entities and backslash
/// escapes in a link target (`media\:` is `media:`), and a letter cannot
/// be backslash-escaped. [`card_hashes`] asks SQLite the same question.
pub(crate) fn may_name_media(md: &str) -> bool {
    md.contains("media") || md.contains('&')
}

/// The hashes the card sides of the database at `conn` name: every card,
/// live or deleted, in a live or deleted deck. Only the `cards` table is
/// read, with the columns every schema version has (migration 0001), so
/// this also reads an old backup.
///
/// Only cards with a side for which [`may_name_media`] holds are read (the
/// same test, in SQL); a property test checks it finds what parsing every
/// side finds.
pub(crate) fn card_hashes(conn: &Connection) -> Result<Hashes> {
    let mut out = Hashes::new();
    let mut stmt = conn.prepare(
        "SELECT front_md, back_md FROM cards
         WHERE instr(front_md, 'media') > 0 OR instr(front_md, '&') > 0
            OR instr(back_md, 'media') > 0 OR instr(back_md, '&') > 0",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let front: String = row.get(0)?;
        let back: String = row.get(1)?;
        side_hashes(&front, &mut out);
        side_hashes(&back, &mut out);
    }
    Ok(out)
}

/// The hashes of the `media` rows of the database at `conn`; none if it has
/// no `media` table (a backup made before migration 0011). A row whose hash
/// is not 64 lowercase hex digits (which the schema's CHECK forbids) names
/// no file and is left out.
pub(crate) fn row_hashes(conn: &Connection) -> Result<Hashes> {
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'media'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let mut out = Hashes::new();
    if !has_table {
        return Ok(out);
    }
    let mut stmt = conn.prepare("SELECT hash FROM media")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let hash: String = row.get(0)?;
        if let Some(hash) = parse_hex(hash.as_bytes()) {
            out.insert(hash);
        }
    }
    Ok(out)
}

/// Everything the database at `conn` uses: its rows and its cards' hashes.
pub(crate) fn used_hashes(conn: &Connection) -> Result<Hashes> {
    let mut out = row_hashes(conn)?;
    out.extend(card_hashes(conn)?);
    Ok(out)
}
