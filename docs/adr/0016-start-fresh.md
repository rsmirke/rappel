# 16. Start Fresh when there is no backup

- Status: Accepted
- Date: 2026-10-15
- Amends: ADR 0011 (the recovery page with no backups)
- Amended by: ADR 0026 (Finish Restore, the follow-up it mentions)
- Amended by: ADR 0050 (row 35: with a waiting copy that cannot be used and no backup, Start Fresh renames the copy aside)

## Context

When the database cannot be opened (damaged, missing next to its journal, another application's file), the recovery page offers the backups. On the first day of use there are none yet, and the page could only say "leave the files alone": the user could not continue (#16). Copying files by hand is what the README warns against.

## Decision

**Start Fresh.** With no backup to restore, the recovery page offers "Start Fresh" after an `AdwAlertDialog` confirmation. `storage::start_fresh` moves the current database and its `-wal`/`-shm`/`-journal` files aside exactly as a restore does (ADR 0007): main file first, to `rappel.sqlite3.replaced-YYYYMMDD-HHMMSS` (with `-N` if taken), mode `0600`. Nothing is deleted. It then opens a new, empty database with `Store::open`, and the app continues. The toast names the kept file.

**When it is not offered.**
- **Backups exist.** Restoring one keeps more of the user's data, and the empty database's daily backups would rotate the real ones out after 14 days. Start Fresh is not shown next to them, and core refuses it too (`Error::BackupsAvailable`, nothing changed), in case a backup appears between listing and confirming.
- **A restore was interrupted** and its validated copy (`rappel.sqlite3.restore.partial`) is waiting. That copy is the user's data. Starting over would hide it behind an empty database whose daily backups would then rotate out the real ones. Core refuses (`Error::InterruptedRestore`, nothing changed) and the page does not offer it. Finishing a restore from that copy when its backup is gone is a separate follow-up.
- **A database from a newer Rappel.** That file is fine; the fix is to update Rappel (ADR 0011). This case has its own page and never reaches the recovery page.

**The backup listing failed.** Start Fresh is not offered either: it is unknown whether backups exist.

## Consequences

- A user is never stuck on the recovery page unless a validated restore copy waits.
- Each Start Fresh leaves a `replaced-…` file in the data folder. They are not cleaned up automatically, for the same reason restores leave theirs.
