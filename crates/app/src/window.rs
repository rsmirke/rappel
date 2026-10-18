//! Main application window. Layout lives in `data/window.blp`.

use std::path::{Path, PathBuf};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use rappel_core::domain::{Card, SettingKey};
use rappel_core::storage::Restored;

use crate::dialogs::card_editor::CardEditor;
use crate::pages::{BrowserPage, DeckPage, DecksPage, Recovered};
use crate::startup::{Problem, data_folder_line, setting_reset_text, too_new_text};
use crate::state::AppState;
use crate::transfer;
use crate::ui::toast::{info_toast, persistent_toast, warning_message};
use crate::ui::{nav, worker};
use crate::window_state;

/// The two jobs of the primary menu.
#[derive(Debug, Clone, Copy)]
enum Transfer {
    Export,
    Import,
}

/// The window actions that start a [`Transfer`].
const TRANSFER_ACTIONS: [&str; 2] = ["win.export", "win.import"];

/// The icons of the "too new" and "could not start" pages, as in
/// `data/window.blp`, given back after a short window took them off.
const TOO_NEW_ICON: &str = "software-update-available-symbolic";
const ERROR_ICON: &str = "dialog-error-symbolic";

mod imp {
    use std::cell::{Cell, OnceCell};

    use adw::subclass::prelude::*;
    use gtk::glib;
    use gtk::prelude::*;

    use crate::pages::{DeckPage, DecksPage, RecoveryPage, SettingPage};
    use crate::state::AppState;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/rsmirke/Rappel/window.ui")]
    pub struct RappelWindow {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub navigation_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub decks_page: TemplateChild<DecksPage>,
        #[template_child]
        pub recovery_page: TemplateChild<RecoveryPage>,
        #[template_child]
        pub setting_page: TemplateChild<SettingPage>,
        #[template_child]
        pub too_new_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub too_new_path: TemplateChild<gtk::Label>,
        #[template_child]
        pub error_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub error_details: TemplateChild<gtk::Label>,
        #[template_child]
        pub short_breakpoint: TemplateChild<adw::Breakpoint>,
        /// Set once, when the database is open (at startup or after a
        /// restore). Pages and dialogs get a clone from here.
        pub state: OnceCell<AppState>,
        /// An export or import is running; another one cannot start.
        pub transferring: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RappelWindow {
        const NAME: &'static str = "RappelWindow";
        type Type = super::RappelWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            // The template uses these types, so they must be registered first.
            DecksPage::ensure_type();
            RecoveryPage::ensure_type();
            SettingPage::ensure_type();
            klass.bind_template();
            // Enabled once the deck list is shown.
            klass.install_action("win.new-deck", None, |window, _, _| {
                window.imp().decks_page.new_deck();
            });
            // Ctrl+N (set in main.rs): a new card on a deck page, a new
            // deck on the deck list, nothing while studying.
            klass.install_action("win.new", None, |window, _, _| {
                let imp = window.imp();
                let Some(page) = imp.navigation_view.visible_page() else {
                    return;
                };
                if let Some(deck_page) = page.downcast_ref::<DeckPage>() {
                    deck_page.new_card();
                } else if page.is::<DecksPage>() {
                    imp.decks_page.new_deck();
                }
            });
            // Ctrl+F (set in main.rs) and the deck list's search button: the
            // All Cards page, filtered to the deck when asked from a deck page.
            klass.install_action("win.search", None, |window, _, _| {
                window.search();
            });
            // The primary menu; enabled on the main page while no export
            // or import runs.
            klass.install_action("win.export", None, |window, _, _| {
                window.start_transfer(super::Transfer::Export);
            });
            klass.install_action("win.import", None, |window, _, _| {
                window.start_transfer(super::Transfer::Import);
            });
            // Ctrl+, (set in main.rs); enabled once the deck list is shown.
            klass.install_action("win.preferences", None, |window, _, _| {
                if let Some(state) = window.imp().state.get() {
                    crate::dialogs::preferences::show(window, state);
                }
            });
            // The primary menu. Needs no database, so always enabled.
            klass.install_action("win.about", None, |window, _, _| {
                crate::dialogs::about::show(window);
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RappelWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let window = self.obj();
            window.action_set_enabled("win.new-deck", false);
            window.action_set_enabled("win.new", false);
            window.action_set_enabled("win.search", false);
            window.action_set_enabled("win.preferences", false);
            // The only focusable widgets of their pages.
            crate::ui::focus::unselect_on_first_focus(&self.too_new_path);
            crate::ui::focus::unselect_on_first_focus(&self.error_details);
            for action in super::TRANSFER_ACTIONS {
                window.action_set_enabled(action, false);
            }
            // The on-screen keyboard never covers the focused field
            // (ADR 0058).
            crate::ui::scroll::install(&*window);
            // The startup problem pages on a short window (ADR 0058).
            for (status, icon) in [
                (&*self.too_new_page, super::TOO_NEW_ICON),
                (&*self.error_page, super::ERROR_ICON),
                (
                    &self.recovery_page.status_page(),
                    crate::pages::recovery_page::ICON,
                ),
            ] {
                crate::ui::breakpoint::compact_status_page(&self.short_breakpoint, status, icon);
            }
        }
    }
    impl WidgetImpl for RappelWindow {}
    impl WindowImpl for RappelWindow {
        /// Closing the window with unsaved text in a card editor asks the
        /// editor's "Discard Changes?" first, as Escape does.
        fn close_request(&self) -> glib::Propagation {
            let window = self.obj();
            let Some(editor) = window.unsaved_editor() else {
                window.save_state();
                return self.parent_close_request();
            };
            glib::spawn_future_local(glib::clone!(
                #[weak]
                window,
                #[weak]
                editor,
                async move {
                    if editor.confirm_discard().await {
                        editor.discard();
                        window.close();
                    }
                }
            ));
            glib::Propagation::Stop
        }
    }
    impl ApplicationWindowImpl for RappelWindow {}
    impl AdwApplicationWindowImpl for RappelWindow {}
}

glib::wrapper! {
    pub struct RappelWindow(ObjectSubclass<imp::RappelWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl RappelWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    /// An open card editor holding unsaved text, if any. Every open dialog
    /// is searched, so it is found even under its "Discard Changes?"
    /// question.
    fn unsaved_editor(&self) -> Option<CardEditor> {
        let dialogs = self.dialogs();
        (0..dialogs.n_items())
            .filter_map(|i| dialogs.item(i).and_downcast::<CardEditor>())
            .find(CardEditor::has_unsaved_changes)
    }

    /// Shows `toast` over the window content.
    pub fn add_toast(&self, toast: adw::Toast) {
        self.imp().toast_overlay.add_toast(toast);
    }

    /// Takes the open database and shows the main page, with a toast for
    /// each problem met while opening it.
    pub fn show_main(&self, state: AppState) {
        let imp = self.imp();
        for warning in state.take_open_warnings() {
            self.add_toast(persistent_toast(&warning_message(&warning)));
        }
        // Checked before keeping the state, so a failure leaves the window
        // without one (and a later retry could still call this).
        if let Err(err) = state.list_decks(rappel_core::time::now_ms()) {
            if let rappel_core::Error::DamagedSetting { key, .. } = err {
                // Damaged after the open checked it (another process): the
                // same page as at startup. The state is not kept, so the
                // reset opens the database on its own.
                self.show_problem(Problem::DamagedSetting {
                    key,
                    data_dir: state.data_dir().to_path_buf(),
                });
                return;
            }
            self.decks_unreadable(&err, state.data_dir());
            return;
        }
        if imp.state.set(state).is_err() {
            // Only reachable if startup and a restore both succeeded, but
            // the restore page is only shown when startup failed.
            tracing::error!("the window already has an open database");
            return;
        }
        if let Some(state) = imp.state.get()
            && let Err(err) = imp.decks_page.set_state(state.clone())
        {
            // Never show "No Decks Yet" for decks that could not be read.
            self.decks_unreadable(&err, state.data_dir());
            return;
        }
        tracing::debug!("showing the main page");
        imp.stack.set_visible_child_name("main");
        self.tidy_media_trash();
        // Only at startup, before the window is shown; not after a restore.
        if !self.is_mapped() {
            self.restore_state();
        }
        self.action_set_enabled("win.new-deck", true);
        self.action_set_enabled("win.new", true);
        self.action_set_enabled("win.search", true);
        self.action_set_enabled("win.preferences", true);
        for action in TRANSFER_ACTIONS {
            self.action_set_enabled(action, true);
        }
    }

    /// Brings back the media trash's files the database uses and purges
    /// the old ones nothing uses (ADR 0057), on a worker after the main
    /// page is shown, so it never holds the window back. At startup and
    /// after a restore. Only logged: the user asked for nothing.
    fn tidy_media_trash(&self) {
        let Some(data_dir) = self.imp().state.get().map(|s| s.data_dir().to_path_buf()) else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                let result = worker::run(&window, move || {
                    AppState::tidy_media_trash_on_worker(&data_dir)
                })
                .await;
                match result {
                    Ok(Ok(tidied)) => {
                        if tidied.blocked {
                            tracing::warn!(
                                "the media trash was not purged: a backup cannot be read"
                            );
                        }
                        tracing::debug!(?tidied, "media trash tidied");
                    }
                    Ok(Err(err)) => tracing::warn!("tidying the media trash failed: {err}"),
                    Err(_) => tracing::warn!("tidying the media trash stopped unexpectedly"),
                }
            }
        ));
    }

    /// Applies the size, maximised state and open deck saved when the
    /// window last closed (`window_state`). Called before the window is
    /// first presented.
    fn restore_state(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        let saved = window_state::load(state.data_dir());
        let (width, height) =
            window_state::fit_to_monitors(saved.width, saved.height, &monitor_sizes(self));
        self.set_default_size(width, height);
        if saved.maximized {
            // "It's permitted to call this function before showing a window,
            // in which case the window will be maximized when it appears
            // onscreen initially" (gtk_window_maximize docs).
            self.maximize();
        }
        if let Some(id) = saved.deck_id {
            // Straight to the deck page, without the slide from the list.
            let view = &imp.navigation_view;
            let animate = view.is_animate_transitions();
            view.set_animate_transitions(false);
            imp.decks_page.reopen(&id);
            view.set_animate_transitions(animate);
        }
    }

    /// The pages of the navigation stack, bottom first.
    fn pages(&self) -> Vec<adw::NavigationPage> {
        let stack = self.imp().navigation_view.navigation_stack();
        (0..stack.n_items())
            .filter_map(|i| stack.item(i).and_downcast())
            .collect()
    }

    /// Saves the size, maximised state and open deck for the next start.
    /// Only from the main page: a recovery or error page has no database
    /// and nothing to remember.
    fn save_state(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        if imp.stack.visible_child_name().as_deref() != Some("main") {
            return;
        }
        let deck_id = window_state::deck_to_reopen(
            self.pages()
                .iter()
                .map(|page| page.downcast_ref::<DeckPage>().and_then(DeckPage::deck_id)),
        );
        // The size the window has when not maximised: "the recommended way
        // for saving window state across restarts" (gtk_window_get_default_size
        // docs).
        let (width, height) = self.default_size();
        let saved = window_state::WindowState {
            width,
            height,
            maximized: self.is_maximized(),
            deck_id,
        };
        window_state::save(state.data_dir(), &saved);
    }

    /// Runs an export or import unless one is already running. Its actions
    /// are disabled meanwhile. `transfer` holds the application around
    /// each worker (`ui::worker`), so closing the window lets the work finish.
    fn start_transfer(&self, transfer: Transfer) {
        let imp = self.imp();
        let Some(state) = imp.state.get().cloned() else {
            return;
        };
        if imp.transferring.replace(true) {
            return;
        }
        for action in TRANSFER_ACTIONS {
            self.action_set_enabled(action, false);
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                match transfer {
                    Transfer::Export => transfer::export(&window, &state).await,
                    Transfer::Import => transfer::import(&window, &state).await,
                }
                window.imp().transferring.set(false);
                for action in TRANSFER_ACTIONS {
                    window.action_set_enabled(action, true);
                }
            }
        ));
    }

    /// Opens All Cards (filtered to the deck of a deck page), or, if it is
    /// open, puts the keyboard in its search field. Not while studying, and
    /// not under an open dialog (the editor): the page would open behind it.
    fn search(&self) {
        let imp = self.imp();
        let Some(state) = imp.state.get() else {
            return;
        };
        if self.visible_dialog().is_some() {
            return;
        }
        let Some(page) = imp.navigation_view.visible_page() else {
            return;
        };
        let deck_id = if let Some(browser) = page.downcast_ref::<BrowserPage>() {
            browser.focus_search();
            return;
        } else if let Some(deck) = page.downcast_ref::<DeckPage>() {
            deck.deck_id()
        } else if page.is::<DecksPage>() {
            None
        } else {
            return;
        };
        // The visible page is not All Cards (see above); the helper keeps
        // the rule in one place.
        if let Err(err) = nav::push_unless_on_top(&imp.navigation_view, || {
            BrowserPage::new(state.clone(), deck_id.as_deref())
        }) {
            crate::ui::toast::show_error(
                self,
                "opening All Cards",
                &format!("The cards could not be read: {err}"),
            );
        }
    }

    /// After a reset or its undo: the counts and the new cards of every open
    /// deck page, and the deck list.
    pub fn study_changed(&self) {
        let imp = self.imp();
        for page in self.pages() {
            if let Some(deck) = page.downcast_ref::<DeckPage>() {
                deck.update_study();
                deck.refresh_new_flags();
            } else if let Some(browser) = page.downcast_ref::<BrowserPage>() {
                // A deck page's Undo (suspend, reset) changed cards it lists.
                browser.requery();
            }
        }
        imp.decks_page.refresh();
    }

    /// After an import, or a change made on All Cards or the review page
    /// (suspend, delete and their Undo): reads the decks and every open deck page's cards
    /// again. A study session keeps the cards it started with.
    pub fn data_changed(&self) {
        for page in self.pages() {
            if let Some(decks) = page.downcast_ref::<DecksPage>() {
                decks.refresh();
            } else if let Some(deck) = page.downcast_ref::<DeckPage>() {
                deck.reload();
            } else if let Some(browser) = page.downcast_ref::<BrowserPage>() {
                browser.reload_all();
            }
        }
    }

    /// Shows the error page for decks that could not be read in `data_dir`.
    fn decks_unreadable(&self, err: &rappel_core::Error, data_dir: &Path) {
        tracing::error!("cannot list decks: {err}");
        self.show_problem(Problem::Failed {
            message: "The decks could not be read.".to_owned(),
            details: err.to_string(),
            data_dir: Some(data_dir.to_path_buf()),
        });
    }

    /// After an Undo from a toast brought `card` back: shows it on the
    /// visible deck page if that is its deck, and updates the deck counts.
    pub fn card_restored(&self, card: &Card) {
        let imp = self.imp();
        if let Some(page) = imp
            .navigation_view
            .visible_page()
            .and_downcast::<DeckPage>()
        {
            page.restored(card);
        }
        // An open All Cards page lists the card too.
        for page in self.pages() {
            if let Some(browser) = page.downcast_ref::<BrowserPage>() {
                browser.requery();
            }
        }
        imp.decks_page.refresh();
    }

    /// Shows the full-window page that explains `problem`.
    pub fn show_problem(&self, problem: Problem) {
        let imp = self.imp();
        tracing::debug!(?problem, "showing a startup problem page");
        match problem {
            Problem::Recovery { reason, data_dir } => {
                let dir = data_dir.clone();
                imp.recovery_page.set_on_too_new(glib::clone!(
                    #[weak(rename_to = window)]
                    self,
                    move |found, supported| window.show_problem(Problem::TooNew {
                        found,
                        supported,
                        data_dir: dir.clone(),
                    })
                ));
                imp.recovery_page.set_on_damaged_setting(glib::clone!(
                    #[weak(rename_to = window)]
                    self,
                    #[strong]
                    data_dir,
                    move |key, how, replaced, set_aside| {
                        window.show_problem(Problem::DamagedSetting {
                            key,
                            data_dir: data_dir.clone(),
                        });
                        // The files were moved all the same: say where.
                        window.add_toast(persistent_toast(&recovered_message(
                            how,
                            replaced.as_deref(),
                            set_aside.as_deref(),
                        )));
                    }
                ));
                imp.recovery_page.show(
                    reason,
                    data_dir,
                    glib::clone!(
                        #[weak(rename_to = window)]
                        self,
                        move |dir, restored, how| window.restored(dir, restored, how)
                    ),
                );
                imp.stack.set_visible_child_name("recovery");
            }
            Problem::DamagedSetting { key, data_dir } => {
                imp.setting_page.show(
                    key,
                    data_dir,
                    glib::clone!(
                        #[weak(rename_to = window)]
                        self,
                        move |dir, store, key, was_reset| {
                            window.setting_reset(dir, store, key, was_reset);
                        }
                    ),
                );
                imp.stack.set_visible_child_name("setting");
                // After the page is the visible one: its only action takes
                // the keyboard, as Start Fresh does on the recovery page.
                imp.setting_page.focus_action();
            }
            Problem::TooNew {
                found,
                supported,
                data_dir,
            } => {
                imp.too_new_page
                    .set_description(Some(&glib::markup_escape_text(&too_new_text(
                        found, supported,
                    ))));
                imp.too_new_path.set_label(&data_folder_line(&data_dir));
                imp.stack.set_visible_child_name("too-new");
            }
            Problem::Failed {
                message,
                details,
                data_dir,
            } => {
                imp.error_page
                    .set_description(Some(&glib::markup_escape_text(&message)));
                let mut text = details;
                if let Some(dir) = data_dir {
                    text.push_str("\n\n");
                    text.push_str(&data_folder_line(&dir));
                }
                imp.error_details.set_label(&text);
                imp.stack.set_visible_child_name("error");
            }
        }
    }

    /// After the setting page reset the damaged setting: the main page, and
    /// a toast that says what was reset to what.
    fn setting_reset(
        &self,
        data_dir: PathBuf,
        store: rappel_core::storage::Store,
        key: SettingKey,
        was_reset: bool,
    ) {
        self.show_main(AppState::new(data_dir, store));
        self.add_toast(info_toast(&setting_reset_text(key, was_reset)));
    }

    fn restored(&self, data_dir: PathBuf, restored: Restored, how: Recovered) {
        let message = recovered_message(
            how,
            restored.replaced.as_deref(),
            restored.set_aside.as_deref(),
        );
        self.show_main(AppState::new(data_dir, restored.store));
        self.add_toast(persistent_toast(&message));
    }
}

/// The toast after the recovery page got a database: what happened, where
/// the previous database was kept, and where the copy an interrupted restore
/// had left waiting was kept (`set_aside`): Start Fresh moves one that could
/// not be used (ADR 0050, row 35), a restore sets any aside (ADR 0052).
fn recovered_message(how: Recovered, replaced: Option<&Path>, set_aside: Option<&Path>) -> String {
    // The first sentence is the toast's title when the paths make the text
    // too long for it, so it is short (`ui::toast::toast_text`).
    let (done, short) = match how {
        Recovered::Backup => ("Backup restored", "Backup restored"),
        Recovered::Finished => ("Restore finished", "Restore finished"),
        Recovered::Fresh => ("Started with an empty database", "Started fresh"),
    };
    let mut message = match replaced {
        Some(path) => format!(
            "{short}. The previous database was kept as {}",
            path.display()
        ),
        None if set_aside.is_some() => short.to_owned(),
        None => done.to_owned(),
    };
    if let Some(path) = set_aside {
        message.push_str(". The copy an earlier restore left waiting was kept as ");
        message.push_str(&path.display().to_string());
    }
    message
}

/// The size of each monitor of `widget`'s display, in application pixels
/// (`gdk_monitor_get_geometry`), for fitting a saved window size to them.
fn monitor_sizes(widget: &impl IsA<gtk::Widget>) -> Vec<(i32, i32)> {
    let monitors = widget.display().monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gtk::gdk::Monitor>())
        .map(|monitor| {
            let geometry = monitor.geometry();
            (geometry.width(), geometry.height())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window opened as at startup on a data directory whose saved state
    /// reopens a deck of `cards` cards; not shown. Returns the window, its
    /// deck page and the directory, which must outlive them.
    fn reopened_with(cards: usize) -> (RappelWindow, DeckPage, tempfile::TempDir) {
        let (tmp, state) = crate::testing::temporary_state();
        let data_dir = state.data_dir().to_path_buf();
        let deck = state.create_deck("Deck").unwrap();
        for i in 0..cards {
            state
                .add_card_with_tags(&deck.id, &format!("front {i}"), "back", &[])
                .unwrap();
        }
        window_state::save(
            &data_dir,
            &window_state::WindowState {
                deck_id: Some(deck.id),
                ..window_state::WindowState::default()
            },
        );
        let window: RappelWindow = glib::Object::new();
        window.show_main(state);
        let page = window
            .imp()
            .navigation_view
            .visible_page()
            .and_downcast::<DeckPage>()
            .expect("the saved deck is reopened");
        (window, page, tmp)
    }

    /// The window's focus widget (it need not be shown for that).
    fn focus_of(window: &RappelWindow) -> gtk::Widget {
        gtk::prelude::RootExt::focus(window).expect("the window has a focus widget")
    }

    #[test]
    fn a_deck_reopened_at_startup_has_the_focus_on_its_first_card() {
        crate::testing::with_widgets(|| {
            let (window, page, _tmp) = reopened_with(3);
            let focus = focus_of(&window);
            assert!(focus.is_ancestor(&page), "not Back or the deck list");
            let list = focus
                .ancestor(gtk::ListView::static_type())
                .expect("a card of the list has the focus");
            assert!(list.is_ancestor(&page));
            window.destroy();
        });
    }

    #[test]
    fn an_empty_deck_reopened_at_startup_has_the_focus_on_add_card() {
        crate::testing::with_widgets(|| {
            let (window, page, _tmp) = reopened_with(0);
            let focus = focus_of(&window);
            assert!(focus.is_ancestor(&page));
            let button = focus.downcast::<gtk::Button>().expect("a button");
            assert_eq!(button.action_name().as_deref(), Some("deck.new-card"));
            window.destroy();
        });
    }

    #[test]
    fn recovery_toasts_say_what_happened_and_where_the_old_file_is() {
        let kept = Path::new("/data/rappel.sqlite3.replaced-20261015-120000");
        assert_eq!(
            recovered_message(Recovered::Backup, Some(kept), None),
            "Backup restored. The previous database was kept as \
             /data/rappel.sqlite3.replaced-20261015-120000"
        );
        assert_eq!(
            recovered_message(Recovered::Backup, None, None),
            "Backup restored"
        );
        assert_eq!(
            recovered_message(Recovered::Finished, None, None),
            "Restore finished"
        );
        assert_eq!(
            recovered_message(Recovered::Fresh, Some(kept), None),
            "Started fresh. The previous database was kept as \
             /data/rappel.sqlite3.replaced-20261015-120000"
        );
        assert_eq!(
            recovered_message(Recovered::Fresh, None, None),
            "Started with an empty database"
        );
        let copy = Path::new("/data/rappel.sqlite3.restore.refused-20261015-120000");
        assert_eq!(
            recovered_message(Recovered::Fresh, Some(kept), Some(copy)),
            "Started fresh. The previous database was kept as \
             /data/rappel.sqlite3.replaced-20261015-120000. The copy an earlier restore left \
             waiting was kept as /data/rappel.sqlite3.restore.refused-20261015-120000"
        );
        let aside = Path::new("/data/rappel.sqlite3.restore.set-aside-20261015-120000");
        assert_eq!(
            recovered_message(Recovered::Backup, Some(kept), Some(aside)),
            "Backup restored. The previous database was kept as \
             /data/rappel.sqlite3.replaced-20261015-120000. The copy an earlier restore left \
             waiting was kept as /data/rappel.sqlite3.restore.set-aside-20261015-120000"
        );
        // Each fits a toast whole, or starts with a title that does.
        for how in [Recovered::Backup, Recovered::Finished, Recovered::Fresh] {
            for replaced in [None, Some(kept)] {
                for set_aside in [None, Some(aside)] {
                    let text = recovered_message(how, replaced, set_aside);
                    if let crate::ui::toast::ToastText::Detailed { title, .. } =
                        crate::ui::toast::toast_text(&text)
                    {
                        assert_ne!(title, crate::ui::toast::FALLBACK_TITLE, "{text}");
                    }
                }
            }
        }
    }

    /// A damaged stored setting at startup shows "A Setting Is Damaged" with
    /// Reset This Setting focused; the reset shows the main page (ADR 0052).
    #[test]
    fn a_damaged_setting_at_startup_can_be_reset_from_its_page() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let data = crate::pages::damaged_setting_folder(
                &tmp,
                &[rappel_core::domain::SettingKey::DesiredRetention],
            );
            let err = AppState::open(data.clone()).expect_err("the open is refused");
            let problem = crate::startup::open_problem(&err, &data);
            let window: RappelWindow = glib::Object::new();
            window.show_problem(problem);
            let imp = window.imp();
            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("setting"));
            let focus = focus_of(&window);
            let button = focus
                .downcast::<gtk::Button>()
                .expect("a button has the focus");
            assert_eq!(button.label().as_deref(), Some("_Reset This Setting"));

            glib::MainContext::default().block_on(imp.setting_page.reset());

            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("main"));
            let settings = imp.state.get().expect("a database is open").settings();
            assert!(settings.is_ok());
            window.destroy();
        });
    }

    /// Finish Restore on a waiting copy from a newer Rappel shows the "too
    /// new" page, as a database that is too new does, with its version
    /// (ADR 0050, row 35).
    #[test]
    fn a_waiting_copy_from_a_newer_rappel_shows_the_too_new_page() {
        crate::testing::with_widgets(|| {
            let tmp = tempfile::tempdir().expect("a temporary directory");
            let (data, partial) = crate::pages::interrupted_restore_folder(&tmp, false);
            // `user_version`, bytes 60 to 63 of the header, big-endian.
            let mut bytes = std::fs::read(&partial).expect("the copy is read");
            bytes[60..64].copy_from_slice(&10_000_u32.to_be_bytes());
            std::fs::write(&partial, bytes).expect("the copy is written");
            let window: RappelWindow = glib::Object::new();
            window.show_problem(Problem::Recovery {
                reason: crate::startup::RecoveryReason::InterruptedRestore,
                data_dir: data,
            });
            let imp = window.imp();
            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("recovery"));
            glib::MainContext::default().block_on(imp.recovery_page.finish_restore());
            assert_eq!(imp.stack.visible_child_name().as_deref(), Some("too-new"));
            let description = imp.too_new_page.description().unwrap_or_default();
            assert!(
                description.contains("database version 10000"),
                "{description}"
            );
            assert!(partial.exists(), "the copy is kept");
            window.destroy();
        });
    }
}
