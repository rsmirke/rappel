//! The texts of export and import: pure functions, unit-tested below.

use std::path::Path;

use rappel_core::interchange::{
    Conflict, FILE_EXTENSION, ImportProblem, ImportReport, ImportSummary, SettingsImport,
};

use rappel_core::{Error, content};

use super::names::Lookup;
use crate::ui::text::{count, decks_cards_reviews};

/// How many conflicts the "Cannot Import" dialog lists by name.
pub(super) const SHOWN_CONFLICTS: usize = 5;

/// How many characters of a card front a conflict line shows.
const FRONT_CHARS: usize = 60;

/// The front on one line, without Markdown syntax (as the card list shows
/// it), cut to [`FRONT_CHARS`] with "…" when anything was left out.
pub(super) fn short_front(front: &str) -> String {
    let text = content::plain_text(&content::parse(front));
    let preview = content::one_line_summary(&text);
    let words: Vec<&str> = text.split_whitespace().collect();
    let whole: usize =
        words.iter().map(|w| w.chars().count()).sum::<usize>() + words.len().saturating_sub(1);
    let shown: String = preview.chars().take(FRONT_CHARS).collect();
    if whole > shown.chars().count() {
        format!("{}…", shown.trim_end())
    } else {
        shown
    }
}

/// `rappel-YYYY-MM-DD.rappel`, the export's suggested name.
pub(super) fn export_file_name(year: i32, month: i32, day: i32) -> String {
    if year <= 0 {
        // No local date: GLib could not read the time zone.
        return format!("rappel.{FILE_EXTENSION}");
    }
    format!("rappel-{year:04}-{month:02}-{day:02}.{FILE_EXTENSION}")
}

/// The export's toast: the file's name, and how many media files could
/// not go in (missing from the media folder or damaged, ADR 0054).
pub(super) fn exported_text(path: &Path, missing_media: usize) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    match missing_media {
        0 => format!("Exported to “{name}”"),
        1 => format!("Exported to “{name}”, without 1 media file that is missing or damaged"),
        n => format!(
            "Exported to “{name}”, without {} that are missing or damaged",
            count(n, "media file", "media files")
        ),
    }
}

/// The toast for a refused file. A file from an early test version says
/// so in its own words: its message already says it cannot be imported.
pub(super) fn refused_text(problem: &ImportProblem) -> String {
    match problem {
        ImportProblem::EarlyTestVersion => {
            "This file was made by an early test version of Rappel and cannot be imported"
                .to_owned()
        }
        other => format!("This file cannot be imported: {other}"),
    }
}

/// The toast when checking a file failed for a reason other than the
/// file being refused ([`refused_text`]).
pub(super) fn check_failed_text(err: &Error) -> String {
    match err {
        Error::ImportFileChanged => {
            "The file changed while it was being checked. Import it again.".to_owned()
        }
        other => format!("The file could not be checked: {other}"),
    }
}

/// The toast when an import the user confirmed failed. Nothing was
/// imported in every case (ADR 0018, ADR 0053).
pub(super) fn import_failed_text(err: &Error) -> String {
    match err {
        Error::ImportPlanStale => "Your data changed while the file was being checked. \
             Nothing was imported; import it again."
            .to_owned(),
        Error::ImportFileChanged => {
            "The file changed since it was checked. Nothing was imported; import it again."
                .to_owned()
        }
        other => format!("The import failed, and nothing was imported: {other}"),
    }
}

/// "reviews" here counts every entry of the log, resets and undos too
/// (ADR 0028). Those are rare beside reviews, and the counts only tell
/// the user roughly how much a file holds, so they are not named apart.
pub(super) fn imported_text(report: &ImportReport) -> String {
    let added = decks_cards_reviews(report.decks, report.cards, report.review_events);
    let revived = decks_cards_reviews(report.revived_decks, report.revived_cards, 0);
    match (added, revived) {
        (Some(added), Some(revived)) => format!("Imported {added}, and brought back {revived}"),
        (Some(added), None) => format!("Imported {added}"),
        (None, Some(revived)) => format!("Brought back {revived}"),
        (None, None) => {
            let tags = report.tags + report.card_tags > 0;
            let parts: Vec<&str> = [
                (tags, "tags"),
                (report.media > 0, "media files"),
                (report.settings_applied, "settings"),
            ]
            .into_iter()
            .filter_map(|(on, part)| on.then_some(part))
            .collect();
            match parts.as_slice() {
                [] => "Nothing was imported: everything was already here".to_owned(),
                [one] => format!("Imported {one}"),
                [rest @ .., last] => format!("Imported {} and {last}", rest.join(", ")),
            }
        }
    }
}

/// What the import dialog says.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PlanView {
    pub(super) heading: &'static str,
    pub(super) body: String,
    /// Whether Import is offered.
    pub(super) can_import: bool,
}

pub(super) fn plan_view(summary: &ImportSummary) -> PlanView {
    let conflicts = summary.conflicts.len();
    if conflicts > 0 {
        return PlanView {
            heading: "Cannot Import",
            body: format!(
                "This file disagrees with your data in {}. Nothing is imported, \
                 and your data stays as it is.",
                count(conflicts, "place", "places")
            ),
            can_import: false,
        };
    }
    let new = decks_cards_reviews(
        summary.decks.new,
        summary.cards.new,
        summary.review_events.new,
    );
    let revived = decks_cards_reviews(summary.decks.revived, summary.cards.revived, 0);
    if summary.is_noop() {
        return PlanView {
            heading: "Nothing to Import",
            body: "Everything in this file is already here.".to_owned(),
            can_import: false,
        };
    }
    let mut sentences = Vec::new();
    if let Some(new) = new {
        sentences.push(format!("This adds {new}."));
    }
    if summary.tags.new > 0 {
        sentences.push(format!(
            "This adds {}.",
            count(summary.tags.new, "tag", "tags")
        ));
    }
    if summary.card_tags.new > 0 {
        sentences.push(format!(
            "This puts {} on cards.",
            count(summary.card_tags.new, "tag", "tags")
        ));
    }
    if summary.media.new > 0 {
        sentences.push(format!(
            "This adds {}.",
            count(summary.media.new, "media file", "media files")
        ));
    }
    if let Some(revived) = revived {
        sentences.push(format!("This brings back {revived} you deleted."));
    }
    let (decks, cards, reviews) = (
        summary.decks.present,
        summary.cards.present,
        summary.review_events.present,
    );
    if let Some(present) = decks_cards_reviews(decks, cards, reviews) {
        let verb = if decks + cards + reviews == 1 {
            "is"
        } else {
            "are"
        };
        sentences.push(format!(
            "{present} {verb} already here and will be skipped."
        ));
    }
    match summary.settings {
        SettingsImport::Applied => {
            sentences.push("Your settings are replaced by the file’s.".to_owned());
        }
        SettingsImport::Kept => {
            sentences.push("Your settings stay as they are.".to_owned());
        }
        SettingsImport::Unchanged => {}
    }
    sentences.push("Your data is backed up first.".to_owned());
    let body = sentences.join(" ");
    PlanView {
        heading: "Import Data?",
        body,
        can_import: true,
    }
}

/// The first [`SHOWN_CONFLICTS`] conflicts, one per line, then how many
/// more. `deck_name` and `card_front` look up the user's own deck names
/// and card fronts.
pub(super) fn conflicts_text(
    conflicts: &[Conflict],
    deck_name: impl Fn(&str) -> Lookup,
    card_front: impl Fn(&str) -> Lookup,
) -> String {
    let mut lines: Vec<String> = conflicts
        .iter()
        .take(SHOWN_CONFLICTS)
        .map(|conflict| {
            let line = match conflict {
                Conflict::DeckDiffers(id) => match deck_name(id) {
                    Lookup::Found(name) => format!("The deck “{name}” is different in the file"),
                    Lookup::Deleted => "A deleted deck is different in the file".to_owned(),
                    Lookup::Unknown => "A deck is different in the file".to_owned(),
                },
                Conflict::CardDiffers(id) => match card_front(id) {
                    Lookup::Found(front) => {
                        format!("The card “{front}” is different in the file")
                    }
                    Lookup::Deleted => "A deleted card is different in the file".to_owned(),
                    Lookup::Unknown => "A card is different in the file".to_owned(),
                },
                Conflict::EventDiffers(_) => {
                    "A review is recorded differently in the file".to_owned()
                }
                Conflict::DeckNameTaken { existing_id, .. } => match deck_name(existing_id) {
                    Lookup::Found(name) => format!("The file has another deck named “{name}”"),
                    Lookup::Deleted | Lookup::Unknown => {
                        "The file has another deck with the name of one of yours".to_owned()
                    }
                },
                Conflict::TagDiffers(_) => "A tag has another name in the file".to_owned(),
                Conflict::MediaDiffers(_) => "A media file is different in the file".to_owned(),
                Conflict::AlreadyUndone { .. } => {
                    "The file undoes a review or reset that is already undone here".to_owned()
                }
            };
            format!("• {line}")
        })
        .collect();
    let more = conflicts.len().saturating_sub(SHOWN_CONFLICTS);
    if more > 0 {
        lines.push(format!("and {more} more"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use rappel_core::interchange::TableCounts;

    use super::*;

    const DECK: &str = "01920000-0000-7000-8000-000000000001";
    const CARD: &str = "01920000-0000-7000-8000-000000000002";
    const EVENT: &str = "01920000-0000-7000-8000-000000000003";
    const OTHER: &str = "01920000-0000-7000-8000-000000000004";

    fn counts(new: usize, present: usize) -> TableCounts {
        TableCounts {
            new,
            present,
            revived: 0,
        }
    }

    fn revived(revived: usize) -> TableCounts {
        TableCounts {
            revived,
            ..TableCounts::default()
        }
    }

    fn summary(decks: TableCounts, cards: TableCounts, reviews: TableCounts) -> ImportSummary {
        ImportSummary {
            decks,
            cards,
            review_events: reviews,
            ..ImportSummary::default()
        }
    }

    #[test]
    fn the_export_is_named_by_the_local_date() {
        assert_eq!(export_file_name(2026, 10, 16), "rappel-2026-10-16.rappel");
        assert_eq!(export_file_name(2027, 1, 5), "rappel-2027-01-05.rappel");
        assert_eq!(export_file_name(0, 0, 0), "rappel.rappel");
    }

    #[test]
    fn the_export_toast_names_only_the_file() {
        let path = Path::new("/home/me/Documents/rappel-2026-10-16.rappel");
        assert_eq!(
            exported_text(path, 0),
            "Exported to “rappel-2026-10-16.rappel”"
        );
        assert_eq!(
            exported_text(path, 1),
            "Exported to “rappel-2026-10-16.rappel”, without 1 media file that is missing or damaged"
        );
        assert_eq!(
            exported_text(path, 1_200),
            "Exported to “rappel-2026-10-16.rappel”, without 1,200 media files that are missing or damaged"
        );
    }

    #[test]
    fn new_media_files_are_counted() {
        let mut s = summary(counts(0, 1), counts(0, 1), counts(0, 0));
        s.media = counts(2, 1);
        let view = plan_view(&s);
        assert!(view.can_import);
        assert!(
            view.body.starts_with("This adds 2 media files. "),
            "{}",
            view.body
        );
        s.media = counts(0, 3);
        assert!(!plan_view(&s).can_import);
    }

    #[test]
    fn new_rows_offer_import_and_name_what_is_already_here() {
        let view = plan_view(&summary(counts(12, 0), counts(840, 0), counts(5_210, 0)));
        assert_eq!(
            view,
            PlanView {
                heading: "Import Data?",
                body: "This adds 12 decks, 840 cards and 5,210 reviews. \
                       Your data is backed up first."
                    .to_owned(),
                can_import: true,
            }
        );
        let view = plan_view(&summary(counts(0, 1), counts(1, 0), counts(0, 0)));
        assert_eq!(
            view.body,
            "This adds 1 card. 1 deck is already here and will be skipped. \
             Your data is backed up first."
        );
        let view = plan_view(&summary(counts(1, 2), counts(0, 3), counts(0, 0)));
        assert_eq!(
            view.body,
            "This adds 1 deck. 2 decks and 3 cards are already here and will be skipped. \
             Your data is backed up first."
        );
    }

    #[test]
    fn deleted_rows_brought_back_offer_import() {
        let view = plan_view(&summary(revived(2), revived(30), counts(0, 0)));
        assert_eq!(
            view,
            PlanView {
                heading: "Import Data?",
                body: "This brings back 2 decks and 30 cards you deleted. \
                       Your data is backed up first."
                    .to_owned(),
                can_import: true,
            }
        );
        let mut decks = counts(1, 2);
        decks.revived = 1;
        let view = plan_view(&summary(decks, revived(1), counts(4, 0)));
        assert_eq!(
            view.body,
            "This adds 1 deck and 4 reviews. This brings back 1 deck and 1 card you deleted. \
             2 decks are already here and will be skipped. Your data is backed up first."
        );
    }

    #[test]
    fn nothing_new_offers_no_import() {
        let view = plan_view(&summary(counts(0, 2), counts(0, 5), counts(0, 9)));
        assert_eq!(view.heading, "Nothing to Import");
        assert!(!view.can_import);
        // An empty file too.
        assert!(!plan_view(&ImportSummary::default()).can_import);
    }

    #[test]
    fn conflicts_offer_no_import_whatever_is_new() {
        let mut s = summary(counts(3, 0), counts(9, 0), counts(0, 0));
        s.conflicts = vec![Conflict::DeckDiffers(DECK.to_owned())];
        let view = plan_view(&s);
        assert_eq!(view.heading, "Cannot Import");
        assert!(!view.can_import);
        assert_eq!(
            view.body,
            "This file disagrees with your data in 1 place. Nothing is imported, \
             and your data stays as it is."
        );
    }

    fn names(id: &str) -> Lookup {
        match id {
            DECK => Lookup::Found("Maths".to_owned()),
            OTHER => Lookup::Found("Physics".to_owned()),
            _ => Lookup::Deleted,
        }
    }

    fn fronts(id: &str) -> Lookup {
        if id == CARD {
            Lookup::Found("2 + 2 ?".to_owned())
        } else {
            Lookup::Deleted
        }
    }

    #[test]
    fn each_conflict_names_the_users_own_deck_or_card() {
        let conflicts = [
            Conflict::DeckDiffers(DECK.to_owned()),
            Conflict::DeckDiffers(EVENT.to_owned()),
            Conflict::CardDiffers(CARD.to_owned()),
            Conflict::CardDiffers(EVENT.to_owned()),
            Conflict::EventDiffers(EVENT.to_owned()),
        ];
        assert_eq!(
            conflicts_text(&conflicts, names, fronts),
            "• The deck “Maths” is different in the file\n\
             • A deleted deck is different in the file\n\
             • The card “2 + 2 ?” is different in the file\n\
             • A deleted card is different in the file\n\
             • A review is recorded differently in the file"
        );
        let taken = [Conflict::DeckNameTaken {
            deck_id: DECK.to_owned(),
            existing_id: OTHER.to_owned(),
        }];
        assert_eq!(
            conflicts_text(&taken, names, fronts),
            "• The file has another deck named “Physics”"
        );
    }

    #[test]
    fn unreadable_names_are_not_called_deleted() {
        let conflicts = [
            Conflict::DeckDiffers(DECK.to_owned()),
            Conflict::CardDiffers(CARD.to_owned()),
        ];
        assert_eq!(
            conflicts_text(&conflicts, |_| Lookup::Unknown, |_| Lookup::Unknown),
            "• A deck is different in the file\n\
             • A card is different in the file"
        );
    }

    #[test]
    fn a_cut_front_ends_with_an_ellipsis() {
        assert_eq!(short_front("2 +\n 2 ?"), "2 + 2 ?");
        assert_eq!(short_front("**What** is `x`?"), "What is x?");
        let long = "word ".repeat(200);
        let short = short_front(&long);
        assert!(short.ends_with('…'));
        assert_eq!(short.chars().count(), FRONT_CHARS);
        assert!(!short.contains(" …"));
    }

    #[test]
    fn only_the_first_conflicts_are_listed_then_a_count() {
        let conflicts: Vec<Conflict> = (0..8)
            .map(|_| Conflict::EventDiffers(EVENT.to_owned()))
            .collect();
        let text = conflicts_text(&conflicts, names, fronts);
        assert_eq!(text.lines().count(), SHOWN_CONFLICTS + 1);
        assert_eq!(text.lines().last(), Some("and 3 more"));
        let five = conflicts_text(&conflicts[..5], names, fronts);
        assert!(!five.contains("more"));
    }

    #[test]
    fn the_import_toast_says_what_was_added() {
        let report = ImportReport {
            decks: 1,
            cards: 2,
            tags: 0,
            card_tags: 0,
            review_events: 0,
            media: 0,
            settings_applied: false,
            revived_decks: 0,
            revived_cards: 0,
            backup: None,
        };
        assert_eq!(imported_text(&report), "Imported 1 deck and 2 cards");
        let report = ImportReport {
            revived_cards: 3,
            ..report
        };
        assert_eq!(
            imported_text(&report),
            "Imported 1 deck and 2 cards, and brought back 3 cards"
        );
        let report = ImportReport {
            decks: 0,
            cards: 0,
            revived_decks: 1,
            ..report
        };
        assert_eq!(imported_text(&report), "Brought back 1 deck and 3 cards");
        let report = ImportReport {
            revived_decks: 0,
            revived_cards: 0,
            ..report
        };
        assert_eq!(
            imported_text(&report),
            "Nothing was imported: everything was already here"
        );
        let tags = ImportReport {
            card_tags: 2,
            ..report.clone()
        };
        assert_eq!(imported_text(&tags), "Imported tags");
        let settings = ImportReport {
            settings_applied: true,
            ..report.clone()
        };
        assert_eq!(imported_text(&settings), "Imported settings");
        let both = ImportReport {
            tags: 1,
            ..settings
        };
        assert_eq!(imported_text(&both), "Imported tags and settings");
        let media = ImportReport {
            media: 3,
            ..report.clone()
        };
        assert_eq!(imported_text(&media), "Imported media files");
        let all = ImportReport { media: 3, ..both };
        assert_eq!(
            imported_text(&all),
            "Imported tags, media files and settings"
        );
    }

    #[test]
    fn a_refused_file_says_why_once() {
        assert_eq!(
            refused_text(&ImportProblem::EarlyTestVersion),
            "This file was made by an early test version of Rappel and cannot be imported"
        );
        assert_eq!(
            refused_text(&ImportProblem::NotAFile),
            "This file cannot be imported: it is not a regular file (a folder, a pipe or a device)"
        );
    }

    #[test]
    fn a_changed_file_says_so_and_nothing_was_imported() {
        assert_eq!(
            check_failed_text(&Error::ImportFileChanged),
            "The file changed while it was being checked. Import it again."
        );
        assert_eq!(
            import_failed_text(&Error::ImportFileChanged),
            "The file changed since it was checked. Nothing was imported; import it again."
        );
        assert_eq!(
            import_failed_text(&Error::ImportPlanStale),
            "Your data changed while the file was being checked. \
             Nothing was imported; import it again."
        );
        assert_eq!(
            import_failed_text(&Error::NoBackupDir),
            "The import failed, and nothing was imported: \
             this database cannot be backed up, so nothing can be imported into it"
        );
        assert!(
            check_failed_text(&Error::NoBackupDir).starts_with("The file could not be checked: ")
        );
    }

    #[test]
    fn tags_and_settings_alone_offer_import() {
        let mut s = summary(counts(0, 1), counts(0, 2), counts(0, 0));
        s.tags = counts(2, 0);
        s.card_tags = counts(3, 0);
        let view = plan_view(&s);
        assert!(view.can_import);
        assert_eq!(
            view.body,
            "This adds 2 tags. This puts 3 tags on cards. \
             1 deck and 2 cards are already here and will be skipped. \
             Your data is backed up first."
        );
        let mut s = ImportSummary {
            settings: SettingsImport::Applied,
            ..ImportSummary::default()
        };
        let view = plan_view(&s);
        assert!(view.can_import);
        assert_eq!(
            view.body,
            "Your settings are replaced by the file’s. Your data is backed up first."
        );
        // Kept settings alone are nothing to import.
        s.settings = SettingsImport::Kept;
        assert_eq!(plan_view(&s).heading, "Nothing to Import");
        s.cards = counts(1, 0);
        assert_eq!(
            plan_view(&s).body,
            "This adds 1 card. Your settings stay as they are. Your data is backed up first."
        );
    }

    #[test]
    fn conflicts_of_tags_and_undos_have_their_own_lines() {
        let conflicts = [
            Conflict::TagDiffers(OTHER.to_owned()),
            Conflict::AlreadyUndone {
                event_id: EVENT.to_owned(),
                existing_id: OTHER.to_owned(),
            },
        ];
        assert_eq!(
            conflicts_text(&conflicts, names, fronts),
            "• A tag has another name in the file\n\
             • The file undoes a review or reset that is already undone here"
        );
    }
}
