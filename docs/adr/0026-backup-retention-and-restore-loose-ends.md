# 26. Backup retention and restore loose ends

- Status: Accepted
- Date: 2026-10-16
- Amends: ADR 0005 (which backups are kept), ADR 0007 (the restored backup, the moved-aside set), ADR 0016 (the follow-up it names), ADR 0017 (which backups an import can push out), ADR 0018 (backups made by imports no longer push out daily ones)
- Closes: #8, #9, #22, #27
- Amended by: ADR 0050 (rule 4 keeps the newest backup of each of the 14 latest future days; finishing an interrupted restore prunes nothing; #50)

## Context

Four open issues sat in the backup and restore code.

- **#27.** ADR 0005 kept the 14 newest backups. Since ADR 0017 every import that adds rows makes a backup, and so does every migration. Importing several files in one afternoon could push every older daily backup out, leaving 14 copies from one day: the opposite of what daily backups are for (SPEC §1, data first).
- **#8.** A restore opens the installed database with `Store::open`, which makes the day's first backup and prunes. Restoring the oldest backup, the natural choice when going back as far as possible, could delete the very file just restored.
- **#9.** `move_aside` renames the main file and then its sidecars. A crash between two renames, then a retry, gave the stray `-wal` a new `replaced-…` name. The main file and the commits only in its `-wal` ended up under different names.
- **#22.** After an interrupted restore, the validated copy waits as `rappel.sqlite3.restore.partial` and `Store::open` refuses. The only way out was to restore a backup again. If that backup was gone, the page offered nothing, and Start Fresh is refused on purpose (ADR 0016).

## Decision

### Retention (#27, #8)

`backups_to_keep(names, now, protected)` is a pure function over backup names. A backup is kept if any of these holds:

1. It was made in the last 24 hours. Every import and migration backup of today stays.
2. It is the newest of its UTC day, for each of the last 14 UTC days (today included) that has one.
3. It is protected: the backup just made, and, after a restore, the backup just restored.
4. It is dated after now (the clock went backwards) and is the newest such backup. One is kept, so a clock that was far ahead cannot crowd out real days (before, 14 such files kept all the slots).
5. It is the newest backup that is not dated in the future, however old. After a long pause the last copy from before it is not pruned. (Found by the property test: without this rule, 40 idle days made every backup fall out of the window.)
6. Its name parses but holds no real date. It is not understood, so it is not ours to delete.

A file that does not parse as a backup name is never touched, and leftover `.partial` files are still removed. The bound is 14 + (backups in the last 24 h) + 3.

`prune_backups(dir, now, protected)` replaces the count-based `prune_backups(dir, keep, protect)`. `Store::open` and `apply_import` pass the backup they just made; `restore_with` also passes the backup it installed, through `Store::open_protecting`.

Tested with a unit test per rule, the UTC day boundary, and a property test: random times, never removes the newest or a protected backup, stays within the bound, keeps one backup for each of the last 14 days that has one, and a second prune changes nothing.

### Keeping the moved-aside set together (#9)

Before its first rename, `move_aside` writes the chosen `replaced-…` file name to `rappel.sqlite3.moving-aside` (mode `0600`, synced), and removes the file after the last rename. A retry that finds the marker moves what is left to that same name, so a `-wal` ends up beside its main file. Rules:

- The marker must name a `rappel.sqlite3.replaced-…` file, with no path separator. Anything else is refused (`Error::Io`, nothing moved): a name from elsewhere is never followed.
- A target that exists is never overwritten.
- A marker left by a finished move (the crash came after the last rename) is not followed when a main file is present again and the recorded set already has one: it was a new database, so a new name is chosen. With no file to move, the marker is just removed.

Restore and Start Fresh both go through `move_aside`.

### Finish Restore (#22)

`storage::finish_restore(data_dir, current)` installs the waiting copy. It copies it to `rappel.sqlite3.restore.checking`, checks that copy again (and migrates it if this build is newer, as for a backup), replaces the waiting file with the checked copy, moves any database or sidecar files in place aside, installs it and opens it. A missing copy (or one that is not a regular file) is `Error::NoRestoreWaiting`. A damaged one is `Error::RestoreRefused`. Both change nothing, and the copy is kept.

The recovery page shows **Finish Restore** exactly while the reason is an interrupted restore, with or without backups. Start Fresh is never shown beside it. It asks no confirmation: the user already chose this restore, nothing is deleted, and the old files are moved aside as for any restore. The window shows a "Restore finished" toast.

## Consequences

- A user who imports many files in a day keeps the daily backups of the 14 days before. The directory can hold more than 14 files on a busy day; each is a full copy of the database, so a very large database with many imports in a day takes more space for that day.
- Backups from the last 24 hours are kept even when they are not the newest of their UTC day, so on the next day an afternoon of imports is pruned down to its newest backup once it is older than 24 hours.
- ADR 0007's two known limitations (the restored backup could be pruned; a split `-wal` had to be paired by hand) are gone. A restore from a version-1 backup keeps its old-version file now, which was the reason for #8.
- A marker can be left behind by a crash. It is small, harmless, and removed by the next move.
