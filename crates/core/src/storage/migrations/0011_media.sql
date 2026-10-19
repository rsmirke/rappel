-- Migration 0011: the media table (ADR 0054). NEVER EDIT THIS FILE; add a
-- new migration.
--
-- One row per file in the media folder, `<data dir>/media/<hash>.<ext>`:
-- the SHA-256 of its bytes as 64 lowercase hex digits, its MIME type (one
-- of the kinds Rappel keeps, which fixes the extension), its size and when
-- it was first added. Nothing references it: cards name a file in their
-- Markdown (`media:<hash>.<ext>`). Another kind needs a new migration that
-- rebuilds the table with a wider CHECK.

CREATE TABLE media (
    hash       TEXT    PRIMARY KEY NOT NULL
                       CHECK (length(hash) = 64 AND hash NOT GLOB '*[^0-9a-f]*'),
    mime       TEXT    NOT NULL CHECK (mime IN (
                           'image/png', 'image/jpeg', 'image/webp', 'image/gif',
                           'audio/ogg', 'audio/mpeg', 'audio/mp4', 'audio/wav'
                       )),
    bytes      INTEGER NOT NULL CHECK (bytes > 0),
    created_at INTEGER NOT NULL
) STRICT;
