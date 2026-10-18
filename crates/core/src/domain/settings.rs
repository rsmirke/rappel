//! The user's global settings (SPEC §3, §4.8; ADR 0027).

use crate::error::{Error, Result};

/// The desired retention used until the user changes it.
pub const DEFAULT_DESIRED_RETENTION: f32 = 0.90;

/// The range of desired retention the user may choose.
pub const DESIRED_RETENTION_RANGE: std::ops::RangeInclusive<f32> = 0.80..=0.97;

/// The most cards a daily limit may allow.
pub const MAX_DAILY_LIMIT: u32 = 9_999;

/// The last hour a study day may start at.
pub const MAX_DAY_START_HOUR: u8 = 23;

/// One of the stored settings, by its key in the `settings` table. A
/// damaged stored value is reported with this key, never with the text the
/// file holds (ADR 0052).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingKey {
    /// [`Settings::desired_retention`].
    DesiredRetention,
    /// [`Settings::max_new_per_day`].
    MaxNewPerDay,
    /// [`Settings::max_reviews_per_day`].
    MaxReviewsPerDay,
    /// [`Settings::day_start_hour`].
    DayStartHour,
}

impl SettingKey {
    /// Every key, in the order the settings are checked.
    pub const ALL: [Self; 4] = [
        Self::DesiredRetention,
        Self::MaxNewPerDay,
        Self::MaxReviewsPerDay,
        Self::DayStartHour,
    ];

    /// The key as the `settings` table stores it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DesiredRetention => "desired_retention",
            Self::MaxNewPerDay => "max_new_per_day",
            Self::MaxReviewsPerDay => "max_reviews_per_day",
            Self::DayStartHour => "day_start_hour",
        }
    }
}

impl std::fmt::Display for SettingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The global settings. A setting that was never written has its default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Probability of recall FSRS aims for when a card comes due.
    pub desired_retention: f32,
    /// Most new cards each deck introduces per study day.
    pub max_new_per_day: u32,
    /// Most due reviews each deck shows per study day.
    pub max_reviews_per_day: u32,
    /// The local hour (0–23) at which a study day begins.
    pub day_start_hour: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            desired_retention: DEFAULT_DESIRED_RETENTION,
            max_new_per_day: 20,
            max_reviews_per_day: 200,
            day_start_hour: 4,
        }
    }
}

impl Settings {
    /// `value` if it is a desired retention the user may choose.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`] if it is outside [`DESIRED_RETENTION_RANGE`]
    /// or not a number.
    pub fn check_retention(value: f32) -> Result<f32> {
        if DESIRED_RETENTION_RANGE.contains(&value) {
            Ok(value)
        } else {
            Err(invalid(SettingKey::DesiredRetention, value))
        }
    }

    /// `value` if it is a valid new-cards-per-day limit.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`] if it is above [`MAX_DAILY_LIMIT`].
    pub fn check_max_new(value: u32) -> Result<u32> {
        check_limit(SettingKey::MaxNewPerDay, value)
    }

    /// `value` if it is a valid reviews-per-day limit.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`] if it is above [`MAX_DAILY_LIMIT`].
    pub fn check_max_reviews(value: u32) -> Result<u32> {
        check_limit(SettingKey::MaxReviewsPerDay, value)
    }

    /// `value` if it is an hour of the day.
    ///
    /// # Errors
    /// [`Error::InvalidSetting`] if it is above [`MAX_DAY_START_HOUR`].
    pub fn check_day_start_hour(value: u8) -> Result<u8> {
        if value <= MAX_DAY_START_HOUR {
            Ok(value)
        } else {
            Err(invalid(SettingKey::DayStartHour, value))
        }
    }
}

fn check_limit(key: SettingKey, value: u32) -> Result<u32> {
    if value <= MAX_DAILY_LIMIT {
        Ok(value)
    } else {
        Err(invalid(key, value))
    }
}

fn invalid(key: SettingKey, value: impl std::fmt::Display) -> Error {
    Error::InvalidSetting {
        key: key.as_str(),
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid() {
        let s = Settings::default();
        assert_eq!(
            Settings::check_retention(s.desired_retention).unwrap(),
            0.90
        );
        Settings::check_max_new(s.max_new_per_day).unwrap();
        Settings::check_max_reviews(s.max_reviews_per_day).unwrap();
        Settings::check_day_start_hour(s.day_start_hour).unwrap();
    }

    #[test]
    fn every_key_is_listed_once_with_its_stored_name() {
        let names: Vec<&str> = SettingKey::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "desired_retention",
                "max_new_per_day",
                "max_reviews_per_day",
                "day_start_hour"
            ]
        );
        assert_eq!(SettingKey::DayStartHour.to_string(), "day_start_hour");
    }

    #[test]
    fn retention_bounds() {
        for ok in [0.80, 0.90, 0.97] {
            assert!(Settings::check_retention(ok).is_ok(), "{ok}");
        }
        for bad in [0.79, 0.98, 0.0, 1.0, -0.9, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(
                    Settings::check_retention(bad),
                    Err(Error::InvalidSetting {
                        key: "desired_retention",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn limit_and_hour_bounds() {
        assert!(Settings::check_max_new(0).is_ok());
        assert!(Settings::check_max_new(MAX_DAILY_LIMIT).is_ok());
        assert!(Settings::check_max_new(MAX_DAILY_LIMIT + 1).is_err());
        assert!(Settings::check_max_reviews(0).is_ok());
        assert!(Settings::check_max_reviews(MAX_DAILY_LIMIT).is_ok());
        assert!(Settings::check_max_reviews(MAX_DAILY_LIMIT + 1).is_err());
        assert!(Settings::check_day_start_hour(0).is_ok());
        assert!(Settings::check_day_start_hour(23).is_ok());
        assert!(Settings::check_day_start_hour(24).is_err());
    }
}
