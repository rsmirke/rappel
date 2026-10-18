//! Startup: resolve the data directory, open the database, and when that
//! fails, decide which screen explains it. No GTK here, so the mapping from
//! error to screen is unit-tested (ADR 0011).

use std::path::{Path, PathBuf};

use rappel_core::Error;
use rappel_core::data_dir::{self, BuildProfile, DataDirEnv};

use crate::state::AppState;

/// Why the database needs a backup restored before the app can continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryReason {
    /// The integrity check found damage (or the file is not a database).
    Damaged,
    /// The file is some other application's database.
    NotRappel,
    /// The database file is missing but its journal is there.
    MissingFile,
    /// A restore moved the old database aside and did not finish.
    InterruptedRestore,
    /// As [`Self::InterruptedRestore`], and Finish Restore found that the
    /// waiting copy cannot be used (ADR 0050, row 35). Only the recovery
    /// page reaches it, after that refusal; the open reports the waiting
    /// copy as an interrupted restore.
    UnusableRestoreCopy,
}

impl RecoveryReason {
    pub fn title(self) -> &'static str {
        match self {
            Self::Damaged => "Database Is Damaged",
            Self::NotRappel => "Not a Rappel Database",
            Self::MissingFile => "Database File Is Missing",
            Self::InterruptedRestore => "Restore Did Not Finish",
            Self::UnusableRestoreCopy => "Restore Copy Cannot Be Used",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Damaged => {
                "Rappel found damage in its database and did not open it, so nothing in it was \
                 changed. Restore a backup to continue. The damaged database is kept in the \
                 data folder, not deleted."
            }
            Self::NotRappel => {
                "The database file was not made by Rappel, so Rappel did not open it. Restore a \
                 backup to continue. The file is kept in the data folder, not deleted."
            }
            Self::MissingFile => {
                "The database file is missing but its journal is still there, so Rappel did not \
                 start an empty database. Restore a backup to continue. The journal is kept in \
                 the data folder, not deleted."
            }
            Self::InterruptedRestore => {
                "A restore was interrupted before it finished. Its checked copy is waiting in \
                 the data folder: Finish Restore installs it, or you can restore a backup \
                 instead. The previous database is kept in the data folder, not deleted."
            }
            Self::UnusableRestoreCopy => {
                "A restore was interrupted, and the copy it left waiting is damaged or was not \
                 made by this version of Rappel. Restore a backup to continue; the copy and the \
                 previous database stay in the data folder."
            }
        }
    }
}

/// Why the app cannot show its main page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The database must be replaced by a backup.
    Recovery {
        reason: RecoveryReason,
        data_dir: PathBuf,
    },
    /// The database comes from a newer Rappel. Nothing to restore: the file
    /// is fine, this build is too old.
    TooNew {
        found: i64,
        supported: u32,
        data_dir: PathBuf,
    },
    /// Anything else: a short message, the full error, and the data folder
    /// when it is known.
    Failed {
        message: String,
        details: String,
        data_dir: Option<PathBuf>,
    },
}

/// Resolves and creates the data directory and opens the database in it.
pub fn start() -> Result<AppState, Problem> {
    let dir = prepare_data_dir().map_err(|err| {
        tracing::error!("data directory unavailable: {err}");
        data_dir_problem(&err)
    })?;
    AppState::open(dir.clone()).map_err(|err| {
        tracing::error!("cannot open the database: {err}");
        open_problem(&err, &dir)
    })
}

/// Resolves and creates the data directory.
fn prepare_data_dir() -> rappel_core::Result<PathBuf> {
    let profile = if cfg!(debug_assertions) {
        BuildProfile::Debug
    } else {
        BuildProfile::Release
    };
    let env = DataDirEnv::from_process();
    if profile == BuildProfile::Debug
        && env
            .rappel_data_dir
            .as_ref()
            .is_some_and(|dir| !dir.as_os_str().is_empty())
    {
        // The guard only knows the default release locations; real data kept
        // in a custom RAPPEL_DATA_DIR inherited from the session is not caught.
        tracing::warn!("debug build is using RAPPEL_DATA_DIR; make sure it is not real data");
    }
    let dir = data_dir::resolve_data_dir(&env, profile)?;
    data_dir::ensure_data_dir(&dir)?;
    tracing::info!(path = %dir.display(), ?profile, "data directory");
    Ok(dir)
}

/// "Data folder: …", the line under a problem that names the data folder.
pub fn data_folder_line(dir: &Path) -> String {
    format!("Data folder: {}", dir.display())
}

/// The "too new" page's description: the database is version `found`, and
/// this build supports up to `supported`.
pub fn too_new_text(found: i64, supported: u32) -> String {
    format!(
        "This database was made by a newer version of Rappel (database version {found}; \
         this version supports up to {supported}). Update Rappel to open it. The database \
         has not been changed."
    )
}

/// The screen for a data directory that could not be resolved or created.
pub fn data_dir_problem(err: &Error) -> Problem {
    let (message, data_dir) = match err {
        Error::DebugUsesReleaseDir { path, .. } => (
            "This debug build refused to use the data folder of an installed Rappel (or of Cards, its former name).",
            Some(path.clone()),
        ),
        Error::CreateDir { path, .. } => {
            ("The data folder could not be created.", Some(path.clone()))
        }
        Error::NotADirectory(path) => (
            "The data folder path points to a file, not a folder.",
            Some(path.clone()),
        ),
        Error::RelativeDataDir(_) => ("RAPPEL_DATA_DIR must be an absolute path.", None),
        Error::NoHomeDir => ("The data folder could not be found.", None),
        _ => ("The data folder could not be used.", None),
    };
    Problem::Failed {
        message: message.to_owned(),
        details: err.to_string(),
        data_dir,
    }
}

/// The screen for a database in `data_dir` that [`AppState::open`] refused.
pub fn open_problem(err: &Error, data_dir: &Path) -> Problem {
    let data_dir = data_dir.to_path_buf();
    let reason = match err {
        Error::IntegrityCheckFailed { .. } => RecoveryReason::Damaged,
        Error::NotARappelDatabase => RecoveryReason::NotRappel,
        Error::OrphanedSidecar(_) => RecoveryReason::MissingFile,
        Error::InterruptedRestore(_) => RecoveryReason::InterruptedRestore,
        Error::UnsupportedVersion { found, supported } => {
            return Problem::TooNew {
                found: *found,
                supported: *supported,
                data_dir,
            };
        }
        _ => {
            return Problem::Failed {
                message: "The database could not be opened.".to_owned(),
                details: err.to_string(),
                data_dir: Some(data_dir),
            };
        }
    };
    Problem::Recovery { reason, data_dir }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/data/rappel-dev")
    }

    #[test]
    fn an_interrupted_restore_points_to_finish_restore() {
        assert!(
            RecoveryReason::InterruptedRestore
                .description()
                .contains("Finish Restore")
        );
    }

    fn recovery(reason: RecoveryReason) -> Problem {
        Problem::Recovery {
            reason,
            data_dir: dir(),
        }
    }

    #[test]
    fn damaged_databases_offer_recovery() {
        let err = Error::IntegrityCheckFailed {
            messages: vec!["page 2 is never used".into()],
        };
        assert_eq!(
            open_problem(&err, &dir()),
            recovery(RecoveryReason::Damaged)
        );
        assert_eq!(
            open_problem(&Error::NotARappelDatabase, &dir()),
            recovery(RecoveryReason::NotRappel)
        );
    }

    #[test]
    fn interrupted_restores_offer_recovery() {
        let wal = dir().join("rappel.sqlite3-wal");
        assert_eq!(
            open_problem(&Error::OrphanedSidecar(wal), &dir()),
            recovery(RecoveryReason::MissingFile)
        );
        let partial = dir().join("rappel.sqlite3.restore.partial");
        assert_eq!(
            open_problem(&Error::InterruptedRestore(partial), &dir()),
            recovery(RecoveryReason::InterruptedRestore)
        );
    }

    #[test]
    fn newer_database_does_not_offer_recovery() {
        let err = Error::UnsupportedVersion {
            found: 99,
            supported: 5,
        };
        assert_eq!(
            open_problem(&err, &dir()),
            Problem::TooNew {
                found: 99,
                supported: 5,
                data_dir: dir(),
            }
        );
    }

    #[test]
    fn the_too_new_text_names_both_versions() {
        assert_eq!(
            too_new_text(7, 5),
            "This database was made by a newer version of Rappel (database version 7; \
             this version supports up to 5). Update Rappel to open it. The database has \
             not been changed."
        );
    }

    #[test]
    fn other_open_errors_show_the_error_and_the_path() {
        let err = Error::Io {
            path: dir().join("rappel.sqlite3"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let Problem::Failed {
            details, data_dir, ..
        } = open_problem(&err, &dir())
        else {
            panic!("expected Failed");
        };
        assert_eq!(details, err.to_string());
        assert_eq!(data_dir, Some(dir()));
    }

    #[test]
    fn data_dir_errors_show_the_path_when_known() {
        let err = Error::NotADirectory(dir());
        let Problem::Failed { data_dir, .. } = data_dir_problem(&err) else {
            panic!("expected Failed");
        };
        assert_eq!(data_dir, Some(dir()));

        let Problem::Failed { data_dir, .. } = data_dir_problem(&Error::NoHomeDir) else {
            panic!("expected Failed");
        };
        assert_eq!(data_dir, None);
    }

    fn failed_dir(err: &Error) -> Option<PathBuf> {
        match data_dir_problem(err) {
            Problem::Failed { data_dir, .. } => data_dir,
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn every_data_dir_error_maps_to_the_error_page() {
        let release = PathBuf::from("/data/rappel");
        let debug_err = Error::DebugUsesReleaseDir {
            path: release.clone(),
            release: release.clone(),
        };
        assert_eq!(failed_dir(&debug_err), Some(release));
        let create_err = Error::CreateDir {
            path: dir(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(failed_dir(&create_err), Some(dir()));
        let relative = Error::RelativeDataDir(PathBuf::from("rappel"));
        assert_eq!(failed_dir(&relative), None);
    }
}
