//! Card Information (SPEC §4.7, #39): a dialog with a card's dates, counts,
//! memory figures, next due time and history, resets marked. Layout in
//! `data/card_info_dialog.blp`; every text is decided in the pure [`text`]
//! module. Read-only: opening it changes nothing, also during a study
//! session.

mod text;

use adw::prelude::*;
use rappel_core::content::plain_summary;

use self::text::{History, Memory};
use crate::state::AppState;
use crate::ui::dialog;
use crate::ui::time::date_time;
use crate::ui::toast::show_error;
use crate::widgets::HistoryRow;

const FILE: &str = "card_info_dialog.ui";

/// The dialog and the widgets tests look into.
struct Built {
    dialog: adw::Dialog,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "only the tests look into the history list")
    )]
    history: gtk::ListBox,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "only the tests look into the tags row")
    )]
    tags: adw::ActionRow,
}

/// Why the dialog could not be built.
enum Failure {
    /// The card or its log could not be read.
    Read(rappel_core::Error),
    /// The layout lacks an object: a bug, already logged.
    Layout,
}

/// Shows the information of card `card_id` over `parent`, then calls
/// `on_closed`. If it cannot be read, a toast says so instead (a card
/// deleted meanwhile is ignored) and `on_closed` is not called.
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    state: &AppState,
    card_id: &str,
    on_closed: impl Fn() + 'static,
) {
    match build(state, card_id, rappel_core::time::now_ms()) {
        Ok(built) => {
            built.dialog.connect_closed(move |_| on_closed());
            built.dialog.present(Some(parent));
        }
        Err(Failure::Layout) => {}
        Err(Failure::Read(rappel_core::Error::CardNotFound(_))) => {
            tracing::debug!("the card was deleted before its information opened");
        }
        Err(Failure::Read(err)) => show_error(
            parent,
            "reading a card's information",
            &format!("The card's information could not be read: {err}"),
        ),
    }
}

/// The objects of `data/card_info_dialog.blp` this file fills in.
pub(crate) struct Layout {
    dialog: adw::Dialog,
    created: adw::ActionRow,
    edited: adw::ActionRow,
    tags: adw::ActionRow,
    status: adw::ActionRow,
    reviews: adw::ActionRow,
    pass_rate: adw::ActionRow,
    lapses: adw::ActionRow,
    stability: adw::ActionRow,
    difficulty: adw::ActionRow,
    recall: adw::ActionRow,
    next_due: adw::ActionRow,
    front: gtk::Label,
    history: gtk::ListBox,
    note: gtk::Label,
}

impl Layout {
    /// Loads the layout; `Err` with the first missing object's id, after
    /// logging every missing one.
    pub(crate) fn load() -> Result<Self, &'static str> {
        let builder = dialog::builder(FILE);
        let row = |id| dialog::object::<adw::ActionRow>(&builder, FILE, id);
        // Every object is looked up before the first `?`, so each missing
        // one is logged.
        let (dialog, front, history, note) = (
            dialog::object(&builder, FILE, "card_info_dialog"),
            dialog::object(&builder, FILE, "front_label"),
            dialog::object(&builder, FILE, "history_list"),
            dialog::object(&builder, FILE, "history_note"),
        );
        let tags = row("tags_row");
        let (created, edited, status, reviews, pass_rate, lapses) = (
            row("created_row"),
            row("edited_row"),
            row("status_row"),
            row("reviews_row"),
            row("pass_rate_row"),
            row("lapses_row"),
        );
        let (stability, difficulty, recall, next_due) = (
            row("stability_row"),
            row("difficulty_row"),
            row("recall_row"),
            row("next_due_row"),
        );
        Ok(Self {
            dialog: dialog?,
            created: created?,
            edited: edited?,
            tags: tags?,
            status: status?,
            reviews: reviews?,
            pass_rate: pass_rate?,
            lapses: lapses?,
            stability: stability?,
            difficulty: difficulty?,
            recall: recall?,
            next_due: next_due?,
            front: front?,
            history: history?,
            note: note?,
        })
    }
}

/// The dialog for card `card_id` as of `now`, filled in.
fn build(state: &AppState, card_id: &str, now: i64) -> Result<Built, Failure> {
    let card = state.card(card_id).map_err(Failure::Read)?;
    let info = state.card_info(card_id, now).map_err(Failure::Read)?;
    let Layout {
        dialog: card_dialog,
        created,
        edited,
        tags,
        status,
        reviews,
        pass_rate,
        lapses,
        stability,
        difficulty,
        recall,
        next_due,
        front,
        history: list,
        note,
    } = Layout::load().map_err(|_| Failure::Layout)?;
    let tag_names: Vec<String> = state
        .card_tags(card_id)
        .map_err(Failure::Read)?
        .into_iter()
        .map(|tag| tag.name)
        .collect();

    // Card text is untrusted: plain text in labels with markup off.
    front.set_label(&plain_summary(&card.front_md));
    created.set_subtitle(&date_time(info.created_at));
    edited.set_subtitle(&date_time(info.updated_at));
    let tags_value = text::tags_text(&tag_names);
    tags.set_visible(tags_value.is_some());
    tags.set_subtitle(tags_value.as_deref().unwrap_or(""));

    let Memory {
        status: new_status,
        reviews: reviews_text,
        pass_rate: pass_rate_text,
        lapses: lapses_text,
        scheduled,
    } = text::memory(&info, now, &date_time);
    status.set_visible(new_status.is_some());
    status.set_subtitle(new_status.unwrap_or(""));
    reviews.set_subtitle(&reviews_text);
    pass_rate.set_subtitle(&pass_rate_text);
    lapses.set_subtitle(&lapses_text);
    for row in [&stability, &difficulty, &recall, &next_due] {
        row.set_visible(scheduled.is_some());
    }
    if let Some(scheduled) = scheduled {
        stability.set_subtitle(&scheduled.stability);
        difficulty.set_subtitle(&scheduled.difficulty);
        recall.set_subtitle(&scheduled.recall);
        next_due.set_subtitle(&scheduled.next_due);
    }

    fill_history(&list, &note, &text::history(&info, &date_time));
    Ok(Built {
        dialog: card_dialog,
        history: list,
        tags,
    })
}

fn fill_history(list: &gtk::ListBox, note: &gtk::Label, history: &History) {
    for line in &history.lines {
        list.append(&HistoryRow::new(
            line.mark.icon(),
            line.mark.css_class(),
            line.title,
            &line.subtitle,
        ));
    }
    if let Some(empty) = history.empty {
        list.append(&HistoryRow::new("", "", empty, ""));
    }
    note.set_label(history.note.as_deref().unwrap_or(""));
    note.set_visible(history.note.is_some());
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rappel_core::domain::Grade;

    use super::*;

    /// The titles of the history rows, newest first.
    fn history_titles(list: &gtk::ListBox) -> Vec<String> {
        let mut titles = Vec::new();
        let mut child = list.first_child();
        while let Some(row) = child {
            if let Some(row) = row.downcast_ref::<HistoryRow>() {
                titles.push(row.title().to_string());
            }
            child = row.next_sibling();
        }
        titles
    }

    #[test]
    fn the_history_shows_resets_as_markers_newest_first() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let deck = state.create_deck("Deck").unwrap();
            let id = state
                .add_card_with_tags(&deck.id, "front", "back", &[])
                .unwrap()
                .card
                .id;
            let now = rappel_core::time::now_ms();
            let day = crate::ui::time::DAY_MS;
            state
                .record_review(&id, Grade::Pass, Duration::from_secs(3), now - 3 * day)
                .unwrap();
            state.reset_card(&id, now - 2 * day).unwrap();
            state
                .record_review(&id, Grade::Fail, Duration::from_secs(5), now - day)
                .unwrap();
            let Ok(built) = build(&state, &id, now) else {
                panic!("the dialog is built");
            };
            assert_eq!(
                history_titles(&built.history),
                ["Failed", "Progress Reset", "Passed"]
            );
        });
    }

    #[test]
    fn a_card_never_studied_says_so() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let deck = state.create_deck("Deck").unwrap();
            let id = state
                .add_card_with_tags(&deck.id, "front", "back", &[])
                .unwrap()
                .card
                .id;
            let Ok(built) = build(&state, &id, rappel_core::time::now_ms()) else {
                panic!("the dialog is built");
            };
            assert_eq!(history_titles(&built.history), ["Not studied yet"]);
            assert_eq!(built.dialog.title(), "Card Information");
            assert!(!built.tags.is_visible(), "no tags, no row");
        });
    }

    /// Every tag is named in full: on a phone, the row's cut pills and
    /// "+N" have no tooltip (ADR 0058).
    #[test]
    fn the_tags_are_named_in_full() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            let deck = state.create_deck("Deck").unwrap();
            let long = "a tag name much longer than a pill shows";
            let id = state
                .add_card_with_tags(
                    &deck.id,
                    "front",
                    "back",
                    &[long.to_owned(), "maths".to_owned()],
                )
                .unwrap()
                .card
                .id;
            let Ok(built) = build(&state, &id, rappel_core::time::now_ms()) else {
                panic!("the dialog is built");
            };
            assert!(built.tags.is_visible());
            let value = built.tags.subtitle().unwrap_or_default();
            assert!(value.contains(long), "{value}");
            assert!(value.contains("maths"), "{value}");
        });
    }

    #[test]
    fn a_missing_card_is_not_built() {
        crate::testing::with_widgets(|| {
            let (_tmp, state) = crate::testing::temporary_state();
            assert!(matches!(
                build(&state, "no-such-card", rappel_core::time::now_ms()),
                Err(Failure::Read(rappel_core::Error::CardNotFound(_)))
            ));
        });
    }
}
