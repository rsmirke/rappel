//! Reading and writing the global settings (ADR 0027).

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::migrations::{MIGRATIONS, Migration};
use super::schedule::{shown_value, write_schedule_cache};
use super::{BACKUP_DIR_NAME, DB_FILE_NAME, Protect, Store, create_backup};
use crate::domain::{DEFAULT_DESIRED_RETENTION, SettingKey, Settings};
use crate::error::{Error, Result};
use crate::scheduling::Scheduler;
use crate::time::now_ms;

const RETENTION: &str = SettingKey::DesiredRetention.as_str();
const MAX_NEW: &str = SettingKey::MaxNewPerDay.as_str();
const MAX_REVIEWS: &str = SettingKey::MaxReviewsPerDay.as_str();
const DAY_START: &str = SettingKey::DayStartHour.as_str();

impl Store {
    /// The settings; a key never written has its default.
    ///
    /// # Errors
    /// SQLite errors, or [`Error::DamagedSetting`] for a stored value that
    /// is not valid for its key.
    pub fn settings(&self) -> Result<Settings> {
        settings_in(&self.conn)
    }

    /// Sets the most new cards each deck introduces per study day.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`](crate::Error::InvalidSetting) above
    /// `MAX_DAILY_LIMIT`; SQLite errors.
    pub fn set_max_new_per_day(&mut self, value: u32) -> Result<()> {
        write(
            &self.conn,
            MAX_NEW,
            &Settings::check_max_new(value)?.to_string(),
        )
    }

    /// Sets the most due reviews each deck shows per study day.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`](crate::Error::InvalidSetting) above
    /// `MAX_DAILY_LIMIT`; SQLite errors.
    pub fn set_max_reviews_per_day(&mut self, value: u32) -> Result<()> {
        write(
            &self.conn,
            MAX_REVIEWS,
            &Settings::check_max_reviews(value)?.to_string(),
        )
    }

    /// Sets the local hour at which a study day begins.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`](crate::Error::InvalidSetting) above 23;
    /// SQLite errors.
    pub fn set_day_start_hour(&mut self, value: u8) -> Result<()> {
        write(
            &self.conn,
            DAY_START,
            &Settings::check_day_start_hour(value)?.to_string(),
        )
    }

    /// Sets the desired retention and rebuilds the whole `card_schedule`
    /// cache from the review log with it, in one transaction: either both
    /// happen or neither. Every card's due date may change; the log does
    /// not. Does nothing if the value is already the stored one.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`](crate::Error::InvalidSetting) outside
    /// 0.80–0.97; scheduler and SQLite errors, with the old value and
    /// cache left as they were.
    pub fn set_desired_retention(&mut self, value: f32) -> Result<()> {
        let scheduler = Scheduler::new(value)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // An unreadable stored settings table must not stop the user from
        // writing a good value over it, so only a successful read can say
        // "unchanged".
        if settings_in(&tx).is_ok_and(|s| s.desired_retention.to_bits() == value.to_bits()) {
            return Ok(());
        }
        write(&tx, RETENTION, &value.to_string())?;
        let cards = write_schedule_cache(&tx, &scheduler)?;
        tx.commit()?;
        tracing::info!(cards, "desired retention changed, schedule cache rebuilt");
        Ok(())
    }
}

/// The settings on `conn`, the defaults for keys never written and for a
/// database that has no settings table yet (a migration's rebuild).
pub(crate) fn settings_in(conn: &Connection) -> Result<Settings> {
    let stored = stored_settings(conn)?;
    let defaults = Settings::default();
    Ok(Settings {
        desired_retention: stored
            .desired_retention
            .unwrap_or(defaults.desired_retention),
        max_new_per_day: stored.max_new_per_day.unwrap_or(defaults.max_new_per_day),
        max_reviews_per_day: stored
            .max_reviews_per_day
            .unwrap_or(defaults.max_reviews_per_day),
        day_start_hour: stored.day_start_hour.unwrap_or(defaults.day_start_hour),
    })
}

/// The settings that are stored, each `None` where the key was never
/// written. What an export carries (ADR 0027).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct StoredSettings {
    pub desired_retention: Option<f32>,
    pub max_new_per_day: Option<u32>,
    pub max_reviews_per_day: Option<u32>,
    pub day_start_hour: Option<u8>,
}

/// The stored settings on `conn`, each value checked; none before the
/// settings table exists.
///
/// # Errors
/// [`Error::DamagedSetting`] for the first key, in [`SettingKey::ALL`]
/// order, whose stored value is not valid; SQLite errors.
pub(crate) fn stored_settings(conn: &Connection) -> Result<StoredSettings> {
    let mut settings = StoredSettings::default();
    if !table_exists(conn)? {
        return Ok(settings);
    }
    let mut stored: HashMap<String, String> = HashMap::new();
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (key, value) = row?;
        stored.insert(key, value);
    }
    for key in SettingKey::ALL {
        let Some(value) = stored.get(key.as_str()) else {
            continue;
        };
        let damaged = || damaged(key, value);
        match key {
            SettingKey::DesiredRetention => {
                settings.desired_retention = Some(parse_retention(value).ok_or_else(damaged)?);
            }
            SettingKey::MaxNewPerDay => {
                settings.max_new_per_day = Some(parse_max_new(value).ok_or_else(damaged)?);
            }
            SettingKey::MaxReviewsPerDay => {
                settings.max_reviews_per_day = Some(parse_max_reviews(value).ok_or_else(damaged)?);
            }
            SettingKey::DayStartHour => {
                settings.day_start_hour = Some(parse_day_start(value).ok_or_else(damaged)?);
            }
        }
    }
    Ok(settings)
}

/// [`Error::DamagedSetting`] for `value` stored under `key`, its text cut.
fn damaged(key: SettingKey, value: &str) -> Error {
    Error::DamagedSetting {
        key,
        value: shown_value(&format!("{value:?}")),
    }
}

fn parse_retention(value: &str) -> Option<f32> {
    value
        .parse()
        .ok()
        .and_then(|n| Settings::check_retention(n).ok())
}

fn parse_max_new(value: &str) -> Option<u32> {
    value
        .parse()
        .ok()
        .and_then(|n| Settings::check_max_new(n).ok())
}

fn parse_max_reviews(value: &str) -> Option<u32> {
    value
        .parse()
        .ok()
        .and_then(|n| Settings::check_max_reviews(n).ok())
}

fn parse_day_start(value: &str) -> Option<u8> {
    value
        .parse()
        .ok()
        .and_then(|n| Settings::check_day_start_hour(n).ok())
}

/// Whether `value` is a valid stored value for `key`.
fn is_valid(key: SettingKey, value: &str) -> bool {
    match key {
        SettingKey::DesiredRetention => parse_retention(value).is_some(),
        SettingKey::MaxNewPerDay => parse_max_new(value).is_some(),
        SettingKey::MaxReviewsPerDay => parse_max_reviews(value).is_some(),
        SettingKey::DayStartHour => parse_day_start(value).is_some(),
    }
}

/// Whether the value stored under `key` on `conn` is damaged (stored and
/// not valid). A key never written is not.
fn is_damaged(conn: &Connection, key: SettingKey) -> Result<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [key.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value.is_some_and(|v| !is_valid(key, &v)))
}

/// Opens the database in `data_dir` whose stored setting `key` is damaged
/// ([`Error::DamagedSetting`] from [`Store::open`]), puts that setting's
/// default back, and returns the open store (ADR 0052).
///
/// The database is opened as [`Store::open`] opens it (checks, daily
/// backup, migrations), except that the settings are not checked first.
/// Then, only if the value stored under `key` is still damaged:
/// 1. a backup is made with the online backup API, as before a migration;
///    if it fails, nothing is changed and its error is returned;
/// 2. in one transaction, that one row is removed, so the default applies,
///    and for the desired retention the whole schedule cache is rebuilt
///    with the default retention, as [`Store::set_desired_retention`] does.
///
/// No other setting, card or log entry is touched. A `key` whose value is
/// valid (or not stored) is left alone, and no backup is made for it.
///
/// # Errors
/// - [`Error::DamagedSetting`] naming another key if that one is damaged
///   too; `key` was reset (with its backup) and the page asks again.
/// - The errors of [`Store::open`], backup errors, scheduler and SQLite
///   errors. A failure before the commit changes no setting.
pub fn reset_setting(data_dir: &Path, key: SettingKey) -> Result<Store> {
    reset_setting_with(
        &data_dir.join(DB_FILE_NAME),
        &data_dir.join(BACKUP_DIR_NAME),
        key,
        now_ms(),
        MIGRATIONS,
    )
}

/// [`reset_setting`] with every input explicit, for tests.
pub(super) fn reset_setting_with(
    db_path: &Path,
    backup_dir: &Path,
    key: SettingKey,
    now_ms: i64,
    migrations: &[Migration],
) -> Result<Store> {
    let mut store =
        Store::open_unchecked(db_path, backup_dir, now_ms, migrations, Protect::Nothing)?;
    if is_damaged(&store.conn, key)? {
        let path = create_backup(&store.conn, backup_dir, now_ms)?;
        tracing::info!(path = %path.display(), %key, "database backed up before resetting a setting");
        remove_damaged(&mut store.conn, key)?;
    }
    store.checked()
}

/// Removes the row of damaged setting `key` in one transaction, rebuilding
/// the schedule cache with the default retention when `key` is the desired
/// retention. Nothing is written if the value is no longer damaged.
fn remove_damaged(conn: &mut Connection, key: SettingKey) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Checked again inside the write: another process may have fixed it.
    if !is_damaged(&tx, key)? {
        return Ok(());
    }
    tx.execute("DELETE FROM settings WHERE key = ?1", [key.as_str()])?;
    if key == SettingKey::DesiredRetention {
        // The default, not `scheduler_in`: another stored setting may be
        // damaged too, and the retention just removed is the default now.
        let cards = write_schedule_cache(&tx, &Scheduler::new(DEFAULT_DESIRED_RETENTION)?)?;
        tracing::info!(cards, "schedule cache rebuilt with the default retention");
    }
    tx.commit()?;
    tracing::info!(%key, "damaged setting reset to its default");
    Ok(())
}

/// Makes the settings table hold exactly `settings`, inside the caller's
/// transaction (an import into an empty database). The values must have
/// been checked. Does not rebuild the schedule cache.
pub(crate) fn replace_settings(conn: &Connection, settings: &StoredSettings) -> Result<()> {
    conn.execute("DELETE FROM settings", [])?;
    let values = [
        (RETENTION, settings.desired_retention.map(|v| v.to_string())),
        (MAX_NEW, settings.max_new_per_day.map(|v| v.to_string())),
        (
            MAX_REVIEWS,
            settings.max_reviews_per_day.map(|v| v.to_string()),
        ),
        (DAY_START, settings.day_start_hour.map(|v| v.to_string())),
    ];
    for (key, value) in values {
        if let Some(value) = value {
            write(conn, key, &value)?;
        }
    }
    Ok(())
}

/// A scheduler with the stored desired retention. Read inside each
/// transaction that schedules, so every connection uses the committed value.
pub(crate) fn scheduler_in(conn: &Connection) -> Result<Scheduler> {
    Scheduler::new(settings_in(conn)?.desired_retention)
}

fn table_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'settings')",
        [],
        |r| r.get(0),
    )?)
}

fn write(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
