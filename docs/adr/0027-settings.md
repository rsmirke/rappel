# 27. Settings

- Status: Accepted
- Date: 2026-10-17
- Amends: ADR 0006 (the stored desired retention is used), ADR 0008 (the store no longer owns a scheduler), ADR 0014 (the daily limits and the day start come from the settings), ADR 0017 (settings are not in format version 1; they join with version 2)
- Closes: SPEC §4.8, the preferences part
- Amended by: ADR 0017 (format version 2 carries the stored settings; an import applies them only into a database without decks), ADR 0046 (Preferences)

## Context

Four values were fixed in the code: the desired retention (0.90, ADR 0006), the new cards per deck and day (20, ADR 0014), the hour a study day starts (4 a.m., ADR 0014), and, not yet existing, a limit on reviews. SPEC §3 reserves a `settings` table for them and SPEC §4.8 asks for an `AdwPreferencesDialog`. Three questions needed an answer: where the values live and who reads them, what changing the retention does to the schedule, and whether the settings travel in an export.

## Decision

**A key/value table, with defaults in code.** Migration `0006_settings.sql` creates `settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)` and inserts nothing: a key never written has its default (`domain::Settings`). Values are text and are checked when written (`Settings::check_*`) and again when read, so a hand-edited database gives `Error::InvalidColumn` rather than feeding nonsense to FSRS. Allowed values:
- `desired_retention`: 0.80 to 0.97, default 0.90, stored as the shortest decimal that reads back to the same `f32`.
- `max_new_per_day`, `max_reviews_per_day`: 0 to 9,999, defaults 20 and 200.
- `day_start_hour`: 0 to 23, default 4.

The day start hour is not in SPEC §3's list. It is added there: people who study late, or early, need it, and a fixed 4 a.m. was already a decision this slice had to touch (principle 4: a setting needs a reason).

**No cached scheduler.** `Store` used to own a `Scheduler` built at open. Now every transaction that schedules (`record_review`, an import's rebuild, a migration's rebuild) reads the retention with `scheduler_in(&tx)`. A worker connection (`Store::open_concurrent`, used by import and by the retention change) and a restored backup that is migrated therefore always use the committed value. A migration that runs before the settings table exists uses the default, which is what every database had then.

**Changing the retention rebuilds the cache in the same transaction.** `Store::set_desired_retention` writes the value and replays every card's log with the new scheduler (`write_schedule_cache`) in one immediate transaction: both happen or neither. A rebuild uses the new retention for the whole history, as ADR 0006 already said. Setting the value that is stored does nothing. The review log is not touched, so setting the old value again restores exactly the old schedule (tested). In the app the user is asked first (an `AdwAlertDialog` that says due dates will change), and the rebuild runs on a worker thread on its own connection, with the dialog unable to close meanwhile, because replaying a long log takes a while.

**The limits are per deck, like the new-card limit already was.**
- New cards: unchanged, except that the limit is the setting.
- Reviews: `reviews_today` counts the deck's cards with a review before the day start and one at or after it, deleted cards included (as for new cards, they used the day's budget). The due reviews offered are cut to `max_reviews_per_day - reviews_today`, most overdue first. A card whose first review was today is a new card, not a review, whatever happened to it later that day.
- Cards in the 10-minute step are never limited: a card the user failed always comes back. A card failed before the day start whose step is answered after it still counts in `reviews_today`, because it is an older card reviewed today.
- Nothing is stored for the counts. The log stays the only source of truth.
- `next_study_change` is unchanged. When the review limit is reached, a review becoming due still triggers a re-check of the deck page, which finds nothing new. That is harmless and keeps the function simple.
- A study session already open keeps the queue it started with; a changed setting applies to the next one.

**The day start hour is a parameter of `clock.rs`.** It takes the hour from the settings. Daylight saving, per the GLib documentation of `g_date_time_new`: a local time that does not exist (02:00 on the day clocks go from 02:00 to 03:00) is rounded up to the first one that does (03:00), and an ambiguous one (02:00 when clocks go from 03:00 back to 02:00) is the standard-time occurrence. So each calendar day has exactly one start. Tests sweep every hour across both change days in three zones, one of them changing at midnight.
- Changing the hour moves the boundary at once, for the whole log: a user who moves it from 4 to 6 at 5 a.m. finds the reviews since 4 a.m. counted as yesterday's. Because nothing is stored, this is consistent and nothing drifts.

**Export and import do not carry the settings yet.** Format version 1 has a `settings` field, but it must be empty: ADR 0017 refuses a file where it is not, and 0.2.0-alpha.1 and alpha.2 refuse it too. Filling it is therefore a format change, and SPEC §4.1 already plans one bump (version 2) for tags; undo and reset will change the format as well. The decision is to make that single bump with those slices and put the settings in it. Until then an export always has `"settings": {}`, an import leaves the settings alone, and the settings are kept by the database backups. Tests pin both.

## Consequences

- A stored value that is not valid (only a hand-edited database has one) makes the settings unreadable, and with them the deck list and the Preferences dialog. That is deliberate: guessing a default could schedule cards with the wrong retention. The way out is `set_desired_retention`, which writes over an unreadable value, or restoring a backup.
- A fresh database and an upgraded one behave as before until the user changes something: the defaults are the old constants, except that reviews are now limited to 200 per deck and day. A deck with more than 200 due cards shows 200 (the rest wait). This is the one visible change for existing users and is in the changelog.
- `Scheduler::new` and the retention range are no longer test-only.
- Moving settings between machines needs the version 2 bump. Until then, a user who restores a backup gets their settings back, and one who exports and imports does not.
- The retention change blocks other writers while it runs (an immediate transaction), and takes time in proportion to the log. The app's connection is busy-waiting at most five seconds for it, as for any other writer.
