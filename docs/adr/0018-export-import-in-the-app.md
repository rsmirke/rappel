# 18. Export and import in the app

- Status: Accepted
- Date: 2026-10-16
- Amends: ADR 0017 (limits enforced when writing), ADR 0011 (a second connection)
- Amended by: ADR 0026 (backups made by imports no longer push out daily ones)
- Amended by: ADR 0053 (the plan holds a fingerprint of the file, not its document; the apply reads the file again and says so if it changed)

## Context

ADR 0017 gave `rappel-core` an export and a two-step import, without UI. The app needs a way to start them, and the work can be slow: an export reads every row, a plan reads and checks a file of up to 256 MiB, and an apply backs up the whole database. On the main thread that would freeze the window.

The UI's only way to the database is `AppState`, an `Rc<RefCell<Store>>` (ADR 0011), which must stay on the main thread. The worker needs another way in.

ADR 0017 also left a gap: the import refuses a deck name over 1 KiB and a card side over 1 MiB, but the app could write longer ones, and then its own export would not import.

## Decision

**Primary menu.** The deck list's header bar gets the primary menu (`open-menu-symbolic`, F10) with Export… and Import…, in a first section. Preferences, Keyboard Shortcuts and About will go in a last section of their own, as the GNOME HIG orders them. The actions are `win.export` and `win.import`, enabled on the main page while no export or import runs.

**A second connection for the worker.** `Store::open_concurrent(data_dir)` opens another connection to the database `Store::open` already opened, checked, backed up and migrated in this process. It never creates the file, so a missing database is an error, not a new empty one. It runs no integrity check, backup or migration, and it refuses a schema other than the latest (`NotMigrated`). It applies the same connection settings (foreign keys, `trusted_schema` off, a 5-second busy timeout, WAL, `synchronous=FULL`). The worker creates it inside `gio::spawn_blocking` and drops it there. Only plain data crosses threads: paths, `ExportSummary`, `ImportPlan`, `ImportReport` and errors. A test asserts they are `Send`.

This is safe for these reasons:
- **Export** reads every table in one read transaction. In WAL mode that is a fixed snapshot of committed data, so a write on the main thread meanwhile cannot make the file inconsistent. A test runs it beside an open write transaction.
- **Plan** compares the file with the database inside one read transaction and writes nothing.
- **Apply** backs up first. The online backup now reads from the worker's connection while the main one may write, and SQLite restarts an online backup whenever another connection writes between two steps. So the backup copies everything in one step (for every backup, not only an import's): one step reads one WAL snapshot, never blocks writers and cannot restart. Then it compares again inside a `BEGIN IMMEDIATE` transaction and refuses with `ImportPlanStale` if anything changed since the plan (ADR 0017). A deck renamed on the main thread between plan and apply is caught, never overwritten. SQLite lets one writer in at a time. Every write in `Store` is an `IMMEDIATE` transaction or a single statement, so the other side waits up to the busy timeout instead of failing at once. Reads are never blocked in WAL mode.
- **Ids** stay unique and increasing: the import writes the file's ids, and each store raises its id generator above the largest stored id before every write that issues one (ADR 0008). A test checks this.
- **The file is never swapped underneath:** restore and Start Fresh exist only on the recovery page, before any store is open.

Taking the `Store` out of `AppState` for the duration, or running the work on the main thread, were the alternatives. The first would make every other UI call fail while a job runs. The second freezes the window.

**One job at a time, never cut off.** While an export or import runs, both actions are disabled and the application is held (`gio::Application::hold`), so closing the window lets the job finish, as a restore does. If the process is killed anyway, nothing is lost: an import is one transaction, and an export is written to `.partial` and then renamed.

**The import dialog.** After the file chooser and the plan, an `AdwAlertDialog` shows one of three states:
- **Import Data?** lists what the import adds, what it brings back that you deleted, and what is already there, and says a backup is made first. It offers Cancel and Import.
- **Cannot Import** appears when there are conflicts. It says nothing will change and lists the first five conflicts, then "and N more". It offers only Close.
- **Nothing to Import** appears when every row is already present (nothing to add or bring back). It offers only Close.

Conflicts are described with the user's own deck names and card fronts, read from the database, never with text from the file. Rows the database has deleted are described generically. After an import, the deck list and every deck page in the navigation stack are read again. A study session keeps the cards it started with. A refused file, a stale plan and other failures are shown in persistent toasts.

**Limits enforced when writing.** `domain::MAX_DECK_NAME_BYTES` (1 KiB, of the trimmed name) and `MAX_CARD_SIDE_BYTES` (1 MiB per side) are shared by the import check and the store. `create_deck` and `rename_deck` refuse a longer name (`DeckNameTooLong`), and `add_card` and `edit_card` refuse a longer side (`CardSideTooLong`). The deck name dialog and the card editor check the same limits as the user types: they disable saving and explain the problem under the field. A test exports a database at every limit and imports it into an empty one.

## Consequences

- Export, the import check and the backup never block the window, and a change made during an import is caught rather than lost.
- A write on the main thread that coincides with the apply transaction waits for it, on the GTK main loop, so the window does freeze for that wait (in practice the length of the insert transaction, well under a second for usual files). If the transaction lasts longer than the 5-second busy timeout (a file near the 256 MiB limit), that write fails with a "database is locked" error toast and changes nothing. For an answer during a study session, the grade is not recorded and the card stays as it was. The user can answer again.
- Limits are in bytes, so a name of accented letters reaches the limit with fewer letters. The messages say "bytes".
- Existing rows over the limits, if any, are not changed (none are expected: nothing in the app came close). They would still export, but not import.
- Backups made by imports count toward the 14 kept and can push out daily ones. Changing the retention rule is #27.
- No progress indicator: typical databases export and check in well under a second. A long-running import shows nothing until its toast.
