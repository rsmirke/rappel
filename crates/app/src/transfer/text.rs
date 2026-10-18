//! The texts of export and import: pure functions, unit-tested below.

use std::path::Path;

use rappel_core::interchange::{
    Conflict, FILE_EXTENSION, ImportProblem, ImportReport, ImportSummary, SettingsImport,
};

use rappel_core::{Error, content};

use super::names::Lookup;
use crate::i18n::{fill, gettext, ngettext};
use crate::ui::text::{and_list, counted, decks_cards_reviews, group_digits};
use crate::ui::toast::titled;

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
    // Toast titles are short (`ui::toast::toast_text`): a long text starts
    // with a title, and the rest is behind Details.
    match missing_media {
        0 => titled(
            &gettext("Export done"),
            // Translators: {name} is the name of the file written.
            fill(&gettext("Exported to “{name}”"), &[("name", &name)]),
        ),
        n => fill(
            // Translators: a toast: its first sentence is its title, the
            // rest is behind Details. {name} is the file's name, {count} a
            // number of media files (images, sounds).
            &ngettext(
                "Media file left out. Exported to “{name}”, without {count} media file that is \
                 missing or damaged",
                "Media files left out. Exported to “{name}”, without {count} media files that are \
                 missing or damaged",
                n,
            ),
            &[("name", &name), ("count", &group_digits(n))],
        ),
    }
}

/// The toast for a refused file. A file from an early test version says
/// so in its own words: its message already says it cannot be imported.
pub(super) fn refused_text(problem: &ImportProblem) -> String {
    match problem {
        ImportProblem::EarlyTestVersion => gettext(
            "Early test version. This file was made by an early test version of Rappel and \
             cannot be imported",
        ),
        other => fill(
            // Translators: a toast: what is before ":" is its title, the
            // rest is behind Details. {reason} is why, in English.
            &gettext("Cannot import this file: {reason}"),
            &[("reason", &other.to_string())],
        ),
    }
}

/// The toast when checking a file failed for a reason other than the
/// file being refused ([`refused_text`]).
pub(super) fn check_failed_text(err: &Error) -> String {
    match err {
        Error::ImportFileChanged => {
            gettext("File changed. It changed while it was being checked; import it again.")
        }
        other => fill(
            // Translators: a toast: what is before ":" is its title, the
            // rest is behind Details. {error} is why, in English.
            &gettext("Cannot check the file: {error}"),
            &[("error", &other.to_string())],
        ),
    }
}

/// The toast when an import the user confirmed failed. Nothing was
/// imported in every case (ADR 0018, ADR 0053).
pub(super) fn import_failed_text(err: &Error) -> String {
    match err {
        Error::ImportPlanStale => gettext(
            "Nothing was imported. Your data changed while the file was being checked; import it \
             again.",
        ),
        Error::ImportFileChanged => {
            gettext("Nothing was imported. The file changed since it was checked; import it again.")
        }
        other => fill(
            // Translators: a toast: what is before ":" is its title, the
            // rest is behind Details. {error} is why, in English.
            &gettext("The import failed: nothing was imported; {error}"),
            &[("error", &other.to_string())],
        ),
    }
}

/// "reviews" here counts every entry of the log, resets and undos too
/// (ADR 0028). Those are rare beside reviews, and the counts only tell
/// the user roughly how much a file holds, so they are not named apart.
pub(super) fn imported_text(report: &ImportReport) -> String {
    titled(&gettext("Import done"), imported_body(report))
}

/// What an import added and brought back, in a sentence.
fn imported_body(report: &ImportReport) -> String {
    let added = decks_cards_reviews(report.decks, report.cards, report.review_events);
    let revived = decks_cards_reviews(report.revived_decks, report.revived_cards, 0);
    match (added, revived) {
        (Some(added), Some(revived)) => fill(
            // Translators: {added} and {revived} are lists of counts ("1
            // deck and 2 cards").
            &gettext("Imported {added}, and brought back {revived}"),
            &[("added", &added), ("revived", &revived)],
        ),
        // Translators: {added} is a list of counts ("1 deck and 2 cards").
        (Some(added), None) => fill(&gettext("Imported {added}"), &[("added", &added)]),
        (None, Some(revived)) => fill(
            // Translators: {revived} is a list of counts ("1 deck and 2
            // cards") of things deleted earlier and back now.
            &gettext("Brought back {revived}"),
            &[("revived", &revived)],
        ),
        (None, None) => {
            let tags = report.tags + report.card_tags > 0;
            let mut parts = Vec::new();
            if tags {
                parts.push(gettext("tags"));
            }
            if report.media > 0 {
                parts.push(gettext("media files"));
            }
            if report.settings_applied {
                parts.push(gettext("settings"));
            }
            if parts.is_empty() {
                gettext("Nothing was imported: everything was already here")
            } else {
                // Translators: {added} is a list of "tags", "media files"
                // and "settings".
                fill(
                    &gettext("Imported {added}"),
                    &[("added", &and_list(&parts))],
                )
            }
        }
    }
}

/// What the import dialog says.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PlanView {
    pub(super) heading: String,
    pub(super) body: String,
    /// Whether Import is offered.
    pub(super) can_import: bool,
}

pub(super) fn plan_view(summary: &ImportSummary) -> PlanView {
    let conflicts = summary.conflicts.len();
    if conflicts > 0 {
        return PlanView {
            heading: gettext("Cannot Import"),
            body: counted(
                // Translators: {count} is the number of differences
                // between the file and the user's data.
                &ngettext(
                    "This file disagrees with your data in {count} place. Nothing is imported, \
                     and your data stays as it is.",
                    "This file disagrees with your data in {count} places. Nothing is imported, \
                     and your data stays as it is.",
                    conflicts,
                ),
                conflicts,
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
            heading: gettext("Nothing to Import"),
            body: gettext("Everything in this file is already here."),
            can_import: false,
        };
    }
    let mut sentences = Vec::new();
    if let Some(new) = new {
        // Translators: {new} is a list of counts ("12 decks, 840 cards
        // and 5,210 reviews").
        sentences.push(fill(&gettext("This adds {new}."), &[("new", &new)]));
    }
    let tags = summary.tags.new;
    if tags > 0 {
        // Translators: {count} is a number of new tags.
        sentences.push(counted(
            &ngettext("This adds {count} tag.", "This adds {count} tags.", tags),
            tags,
        ));
    }
    let card_tags = summary.card_tags.new;
    if card_tags > 0 {
        sentences.push(counted(
            // Translators: {count} is how many tags the import puts on
            // cards.
            &ngettext(
                "This puts {count} tag on cards.",
                "This puts {count} tags on cards.",
                card_tags,
            ),
            card_tags,
        ));
    }
    let media = summary.media.new;
    if media > 0 {
        sentences.push(counted(
            // Translators: {count} is a number of media files (images,
            // sounds).
            &ngettext(
                "This adds {count} media file.",
                "This adds {count} media files.",
                media,
            ),
            media,
        ));
    }
    if let Some(revived) = revived {
        sentences.push(fill(
            // Translators: {revived} is a list of counts ("2 decks and 30
            // cards") of things the user deleted.
            &gettext("This brings back {revived} you deleted."),
            &[("revived", &revived)],
        ));
    }
    let (decks, cards, reviews) = (
        summary.decks.present,
        summary.cards.present,
        summary.review_events.present,
    );
    sentences.extend(already_here(decks, cards, reviews));
    match summary.settings {
        SettingsImport::Applied => {
            sentences.push(gettext("Your settings are replaced by the file’s."));
        }
        SettingsImport::Kept => {
            sentences.push(gettext("Your settings stay as they are."));
        }
        SettingsImport::Unchanged => {}
    }
    sentences.push(gettext("Your data is backed up first."));
    // Translators: what separates two sentences in a dialog's text.
    let body = sentences.join(&crate::i18n::pgettext("sentence separator", " "));
    PlanView {
        heading: gettext("Import Data?"),
        body,
        can_import: true,
    }
}

/// The sentence about the rows the database already has, if any.
fn already_here(decks: usize, cards: usize, reviews: usize) -> Option<String> {
    let present = decks_cards_reviews(decks, cards, reviews)?;
    let total = decks.saturating_add(cards).saturating_add(reviews);
    Some(fill(
        // Translators: {present} is a list of counts ("2 decks and 3
        // cards"); the plural follows their total.
        &ngettext(
            "{present} is already here and will be skipped.",
            "{present} are already here and will be skipped.",
            total,
        ),
        &[("present", &present)],
    ))
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
                    Lookup::Found(name) => fill(
                        // Translators: {name} is the name of one of the
                        // user's decks.
                        &gettext("The deck “{name}” is different in the file"),
                        &[("name", &name)],
                    ),
                    Lookup::Deleted => gettext("A deleted deck is different in the file"),
                    Lookup::Unknown => gettext("A deck is different in the file"),
                },
                Conflict::CardDiffers(id) => match card_front(id) {
                    Lookup::Found(front) => fill(
                        // Translators: {front} is the start of the front of
                        // one of the user's cards.
                        &gettext("The card “{front}” is different in the file"),
                        &[("front", &front)],
                    ),
                    Lookup::Deleted => gettext("A deleted card is different in the file"),
                    Lookup::Unknown => gettext("A card is different in the file"),
                },
                Conflict::EventDiffers(_) => {
                    gettext("A review is recorded differently in the file")
                }
                Conflict::DeckNameTaken { existing_id, .. } => match deck_name(existing_id) {
                    Lookup::Found(name) => fill(
                        // Translators: {name} is the name of one of the
                        // user's decks.
                        &gettext("The file has another deck named “{name}”"),
                        &[("name", &name)],
                    ),
                    Lookup::Deleted | Lookup::Unknown => {
                        gettext("The file has another deck with the name of one of yours")
                    }
                },
                Conflict::TagDiffers(_) => gettext("A tag has another name in the file"),
                Conflict::MediaDiffers(_) => gettext("A media file is different in the file"),
                Conflict::AlreadyUndone { .. } => {
                    gettext("The file undoes a review or reset that is already undone here")
                }
            };
            format!("• {line}")
        })
        .collect();
    let more = conflicts.len().saturating_sub(SHOWN_CONFLICTS);
    if more > 0 {
        // Translators: after a list of differences, one per line; {count}
        // is how many more there are.
        lines.push(counted(
            &ngettext("and {count} more", "and {count} more", more),
            more,
        ));
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

    /// `text` fits a toast whole, or has a first clause short enough to be
    /// its title (`ui::toast::toast_text`): never the generic one.
    fn assert_has_a_title(text: &str) {
        use crate::ui::toast::{TOAST_FITS_WITH_BUTTON, ToastText, fallback_title, toast_text};
        if let ToastText::Detailed { title, .. } = toast_text(text) {
            assert_ne!(title, fallback_title(), "{text}");
            assert!(title.chars().count() <= TOAST_FITS_WITH_BUTTON, "{title}");
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
            exported_text(Path::new("/tmp/a.rappel"), 0),
            "Exported to “a.rappel”"
        );
        assert_eq!(
            exported_text(path, 0),
            "Export done. Exported to “rappel-2026-10-16.rappel”"
        );
        assert_eq!(
            exported_text(path, 1),
            "Media file left out. Exported to “rappel-2026-10-16.rappel”, without 1 media file that is missing or damaged"
        );
        assert_eq!(
            exported_text(path, 1_200),
            "Media files left out. Exported to “rappel-2026-10-16.rappel”, without 1,200 media files that are missing or damaged"
        );
        for missing in [0, 1, 1_200] {
            assert_has_a_title(&exported_text(path, missing));
        }
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
                heading: "Import Data?".to_owned(),
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
                heading: "Import Data?".to_owned(),
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
            "Import done. Imported 1 deck and 2 cards, and brought back 3 cards"
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
            "Import done. Nothing was imported: everything was already here"
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
            "Import done. Imported tags, media files and settings"
        );
        let many = ImportReport {
            decks: 12,
            cards: 840,
            review_events: 5_210,
            ..all
        };
        assert_has_a_title(&imported_text(&many));
    }

    #[test]
    fn a_refused_file_says_why_once() {
        assert_eq!(
            refused_text(&ImportProblem::EarlyTestVersion),
            "Early test version. This file was made by an early test version of Rappel and cannot be imported"
        );
        assert_eq!(
            refused_text(&ImportProblem::NotAFile),
            "Cannot import this file: it is not a regular file (a folder, a pipe or a device)"
        );
        assert_has_a_title(&refused_text(&ImportProblem::EarlyTestVersion));
        assert_has_a_title(&refused_text(&ImportProblem::NotAFile));
    }

    #[test]
    fn a_changed_file_says_so_and_nothing_was_imported() {
        assert_eq!(
            check_failed_text(&Error::ImportFileChanged),
            "File changed. It changed while it was being checked; import it again."
        );
        assert_eq!(
            import_failed_text(&Error::ImportFileChanged),
            "Nothing was imported. The file changed since it was checked; import it again."
        );
        assert_eq!(
            import_failed_text(&Error::ImportPlanStale),
            "Nothing was imported. Your data changed while the file \
             was being checked; import it again."
        );
        assert_eq!(
            import_failed_text(&Error::NoBackupDir),
            "The import failed: nothing was imported; \
             this database cannot be backed up, so nothing can be imported into it"
        );
        assert!(check_failed_text(&Error::NoBackupDir).starts_with("Cannot check the file: "));
        for err in [
            Error::ImportFileChanged,
            Error::NoBackupDir,
            Error::ImportPlanStale,
        ] {
            assert_has_a_title(&check_failed_text(&err));
            assert_has_a_title(&import_failed_text(&err));
        }
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

    /// Every toast of an export or an import has its own short title in
    /// French too, and the dialog's texts are French.
    #[test]
    fn the_texts_are_translated_and_keep_their_titles_in_french() {
        crate::i18n::tests::in_french(|| {
            let path = Path::new("/home/me/Documents/rappel-2026-10-16.rappel");
            for missing in [0, 1, 99_999] {
                assert_has_a_title(&exported_text(path, missing));
            }
            for problem in [ImportProblem::EarlyTestVersion, ImportProblem::NotAFile] {
                assert_has_a_title(&refused_text(&problem));
            }
            for err in [
                Error::ImportFileChanged,
                Error::NoBackupDir,
                Error::ImportPlanStale,
            ] {
                assert_has_a_title(&check_failed_text(&err));
                assert_has_a_title(&import_failed_text(&err));
            }
            let report = ImportReport {
                decks: 12,
                cards: 840,
                tags: 1,
                card_tags: 0,
                review_events: 5_210,
                media: 3,
                settings_applied: true,
                revived_decks: 1,
                revived_cards: 3,
                backup: None,
            };
            assert_has_a_title(&imported_text(&report));
            let view = plan_view(&summary(counts(1, 2), counts(0, 3), counts(0, 0)));
            assert_eq!(view.heading, "Importer les données\u{202f}?");
            assert!(
                view.body.starts_with("Ceci ajoute 1 paquet. "),
                "{}",
                view.body
            );
        });
    }
}
