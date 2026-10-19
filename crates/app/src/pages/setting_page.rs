//! The page shown when a stored setting is damaged (ADR 0052): which one,
//! what its default is, and "Reset This Setting", which makes a backup and
//! puts the default back on a worker thread. Layout in
//! `data/setting_page.blp`; the texts are in `startup.rs`.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use rappel_core::domain::SettingKey;
use rappel_core::storage::{AfterReset, SettingReset, Store};

use crate::i18n::gettext;
use crate::startup::{damaged_setting_text, data_folder_line, setting_reset_text};
use crate::state::AppState;
use crate::ui::callback::Callback;
use crate::ui::text::with_error;
use crate::ui::toast::{info_toast, show_toast_from};
use crate::ui::worker;

/// Called with the data directory, the open store, the key and whether it
/// was reset (`false`: it was no longer damaged), once no stored setting is
/// damaged any more.
type OnReset = dyn Fn(PathBuf, Store, SettingKey, bool);

/// The status page's icon, left out on a short window.
const ICON: &str = "dialog-warning-symbolic";

mod imp {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use adw::subclass::prelude::*;
    use gtk::glib;
    use gtk::prelude::*;
    use rappel_core::domain::SettingKey;

    use super::OnReset;
    use crate::ui::callback::Callback;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/setting_page.ui")]
    pub struct SettingPage {
        #[template_child]
        pub short_breakpoint: TemplateChild<adw::Breakpoint>,
        #[template_child]
        pub status: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub path_label: TemplateChild<gtk::Label>,
        pub key: Cell<Option<SettingKey>>,
        pub data_dir: RefCell<Option<PathBuf>>,
        pub on_reset: RefCell<Option<Callback<OnReset>>>,
        /// The last failure's toast, replaced by the next one and dismissed
        /// once the reset works.
        pub errors: crate::ui::toast::ErrorSlot,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SettingPage {
        const NAME: &'static str = "RappelSettingPage";
        type Type = super::SettingPage;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SettingPage {
        fn constructed(&self) {
            self.parent_constructed();
            crate::ui::focus::unselect_on_first_focus(&self.path_label);
            crate::ui::breakpoint::compact_status_page(
                &self.short_breakpoint,
                &self.status,
                super::ICON,
            );
            // No confirmation: a backup is made first, and only the one
            // damaged value, which nothing can read, is replaced.
            self.reset_button.connect_clicked(glib::clone!(
                #[weak(rename_to = page)]
                self.obj(),
                move |_| {
                    glib::spawn_future_local(async move { page.reset().await });
                }
            ));
        }
    }
    impl WidgetImpl for SettingPage {}
    impl BinImpl for SettingPage {}
}

glib::wrapper! {
    pub struct SettingPage(ObjectSubclass<imp::SettingPage>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SettingPage {
    /// Explains that `key` is damaged in `data_dir`, and calls `on_reset`
    /// once a reset leaves no damaged setting.
    pub fn show(
        &self,
        key: SettingKey,
        data_dir: PathBuf,
        on_reset: impl Fn(PathBuf, Store, SettingKey, bool) + 'static,
    ) {
        let imp = self.imp();
        imp.path_label.set_label(&data_folder_line(&data_dir));
        imp.data_dir.replace(Some(data_dir));
        imp.on_reset.replace(Some(Callback::new(Rc::new(on_reset))));
        self.set_key(key);
    }

    /// Shows the texts for `key`, with Reset This Setting ready.
    fn set_key(&self, key: SettingKey) {
        let imp = self.imp();
        imp.key.set(Some(key));
        // The description is Pango markup; the text holds no markup.
        imp.status
            .set_description(Some(&glib::markup_escape_text(&damaged_setting_text(key))));
        self.set_busy(false);
    }

    /// Puts the keyboard on Reset This Setting, the page's only action.
    /// Called once the page is the window's visible one; the status page's
    /// scrolled window then brings the button into view on a short window
    /// (`GtkViewport`'s `scroll-to-focus`, on by default), as the recovery
    /// page does for Start Fresh.
    pub fn focus_action(&self) {
        self.imp().reset_button.grab_focus();
    }

    fn set_busy(&self, busy: bool) {
        let imp = self.imp();
        imp.reset_button.set_sensitive(!busy);
        imp.spinner.set_visible(busy);
    }

    /// Resets the damaged setting on a worker thread. Its errors never hold
    /// card or deck text, and come before any database is open: they are
    /// logged in full, as the recovery page's are (ADR 0025, row 13).
    pub(crate) async fn reset(&self) {
        let imp = self.imp();
        let (Some(dir), Some(key)) = (imp.data_dir.borrow().clone(), imp.key.get()) else {
            return;
        };
        self.set_busy(true);
        let worker_dir = dir.clone();
        let result = worker::run(self, move || {
            AppState::reset_setting_unopened(&worker_dir, key)
        })
        .await;
        let message = match result {
            Ok(Ok(SettingReset {
                was_reset,
                then: AfterReset::Opened(store),
            })) => {
                imp.errors.clear();
                // Clone the callback out so no borrow is held while it runs.
                let handler = imp.on_reset.borrow().as_ref().map(Callback::get);
                if let Some(handler) = handler {
                    handler(dir, *store, key, was_reset);
                }
                return;
            }
            Ok(Ok(SettingReset {
                was_reset,
                then: AfterReset::Damaged(next),
            })) => {
                // Another one is damaged too.
                tracing::warn!(%key, %next, "another stored setting is damaged");
                imp.errors.clear();
                self.set_key(next);
                self.focus_action();
                show_toast_from(self, info_toast(&setting_reset_text(key, was_reset)));
                return;
            }
            Ok(Err(err)) => {
                tracing::error!("resetting the setting failed: {err}");
                // Translators: a toast: what is before ":" is its title, the rest is behind Details. {error} is why, in English.
                with_error(&gettext("The setting could not be reset: {error}"), &err)
            }
            Err(_) => {
                tracing::error!("reset setting worker panicked");
                gettext("Resetting the setting stopped unexpectedly")
            }
        };
        self.set_busy(false);
        self.focus_action();
        imp.errors.show(self, "resetting a setting", &message);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A data folder with a deck and one changed setting, whose stored
    /// `damaged` keys then hold text no setting can be, as a hand edit
    /// leaves it. Returns the folder.
    pub(crate) fn damaged_folder(tmp: &tempfile::TempDir, damaged: &[SettingKey]) -> PathBuf {
        let data = tmp.path().join("data");
        let state = AppState::open(data.clone()).expect("a new database opens");
        state.create_deck("Maths").expect("a deck");
        state.set_max_reviews_per_day(70).expect("a setting");
        drop(state);
        let conn = rusqlite::Connection::open(data.join("rappel.sqlite3")).expect("the file opens");
        for key in damaged {
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, 'damaged')",
                [key.as_str()],
            )
            .expect("the value is written");
        }
        data
    }

    /// The key the open refuses in `data`.
    fn refused_key(data: &std::path::Path) -> SettingKey {
        match AppState::open(data.to_path_buf()) {
            Err(rappel_core::Error::DamagedSetting { key, .. }) => key,
            other => panic!("expected DamagedSetting, got {other:?}"),
        }
    }

    #[test]
    fn reset_this_setting_opens_the_database_with_the_default() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let data = damaged_folder(&tmp, &[SettingKey::DesiredRetention]);
            assert_eq!(refused_key(&data), SettingKey::DesiredRetention);
            let page: SettingPage = glib::Object::new();
            let done = Rc::new(RefCell::new(None));
            let sink = Rc::clone(&done);
            page.show(
                SettingKey::DesiredRetention,
                data.clone(),
                move |_, store, key, was_reset| {
                    assert!(was_reset);
                    sink.replace(Some((key, store.settings().ok())));
                },
            );
            let imp = page.imp();
            assert_eq!(imp.status.title(), "A Setting Is Damaged");
            let description = imp.status.description().unwrap_or_default();
            assert!(description.contains("Desired Retention"), "{description}");
            assert!(imp.reset_button.is_sensitive());

            glib::MainContext::default().block_on(page.reset());

            let (key, settings) = done.take().expect("the database opened");
            assert_eq!(key, SettingKey::DesiredRetention);
            let settings = settings.expect("the settings read");
            assert!((settings.desired_retention - 0.90).abs() < f32::EPSILON);
            assert_eq!(settings.max_reviews_per_day, 70, "the others are kept");
            assert!(imp.errors.is_empty());
        });
    }

    #[test]
    fn a_second_damaged_setting_shows_the_page_again_for_it() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let data = damaged_folder(&tmp, &[SettingKey::MaxNewPerDay, SettingKey::DayStartHour]);
            let first = refused_key(&data);
            assert_eq!(first, SettingKey::MaxNewPerDay);
            let page: SettingPage = glib::Object::new();
            let done = Rc::new(std::cell::Cell::new(None));
            let sink = Rc::clone(&done);
            page.show(first, data.clone(), move |_, _, key, _| sink.set(Some(key)));
            let context = glib::MainContext::default();

            context.block_on(page.reset());

            assert_eq!(done.get(), None, "still one damaged");
            let imp = page.imp();
            assert_eq!(imp.key.get(), Some(SettingKey::DayStartHour));
            let description = imp.status.description().unwrap_or_default();
            assert!(description.contains("Day Starts At"), "{description}");
            assert!(imp.reset_button.is_sensitive());

            context.block_on(page.reset());
            assert_eq!(done.get(), Some(SettingKey::DayStartHour));
        });
    }

    #[test]
    fn a_failed_reset_keeps_the_page_with_an_error_toast() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let data = damaged_folder(&tmp, &[SettingKey::MaxReviewsPerDay]);
            // No backup can be made: a file where the backup folder belongs.
            let backups = data.join("backups");
            if backups.exists() {
                std::fs::remove_dir_all(&backups).expect("the backups are removed");
            }
            std::fs::write(&backups, b"not a folder").expect("a file in the way");
            let page: SettingPage = glib::Object::new();
            let done = Rc::new(std::cell::Cell::new(false));
            let sink = Rc::clone(&done);
            page.show(
                SettingKey::MaxReviewsPerDay,
                data.clone(),
                move |_, _, _, _| {
                    sink.set(true);
                },
            );

            glib::MainContext::default().block_on(page.reset());

            assert!(!done.get());
            let imp = page.imp();
            assert!(!imp.errors.is_empty(), "an error toast");
            assert!(imp.reset_button.is_sensitive(), "it can be tried again");
            assert!(!imp.spinner.is_visible());
            assert_eq!(
                refused_key(&data),
                SettingKey::MaxReviewsPerDay,
                "nothing changed"
            );
        });
    }
}
