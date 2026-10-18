//! The texts of the Anki import (ADR 0061): the question before it, the
//! report of what is left out or changed, and the toasts. Pure, unit-tested
//! below.

use rappel_core::interchange::{AnkiProblem, AnkiReport, AnkiSummary, Skip, Skipped};

use crate::i18n::{fill, gettext, ngettext, pgettext};
use crate::ui::text::{counted, decks_cards_reviews, group_digits, isolate, list_separator};
use crate::ui::toast::titled;

/// The most tags a card keeps from its note (core's limit).
const MAX_TAGS: usize = 100;

/// What the Anki import dialog says.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AnkiView {
    pub(super) heading: String,
    pub(super) body: String,
    /// The report, below the body (plain text, a line per reason), if
    /// anything is left out or changed.
    pub(super) report: Option<String>,
    /// Whether Import is offered.
    pub(super) can_import: bool,
}

/// Whether a reason changes how something is kept rather than leaving it
/// out.
fn is_change(reason: Skip) -> bool {
    matches!(
        reason,
        Skip::UnknownFilter
            | Skip::UnknownField
            | Skip::MathAsText
            | Skip::RemoteImage
            | Skip::DeckNameShortened
    )
}

/// One reason with its count, as a phrase.
fn reason_text(reason: Skip, n: usize) -> String {
    if reason == Skip::TooManyTags {
        return fill(
            // Translators: {count} is a number of notes (Anki's), {limit}
            // the most tags a card keeps.
            &ngettext(
                "{count} note with more than {limit} tags, the rest left out",
                "{count} notes with more than {limit} tags, the rest left out",
                n,
            ),
            &[
                ("count", &group_digits(n)),
                ("limit", &group_digits(MAX_TAGS)),
            ],
        );
    }
    counted(
        &card_template(reason, n).unwrap_or_else(|| other_template(reason, n)),
        n,
    )
}

/// The template of a reason about cards, if `reason` is one.
fn card_template(reason: Skip, n: usize) -> Option<String> {
    Some(match reason {
        // Translators: {count} is a number of cards; a line of the Anki import's report.
        Skip::AlreadyInRappel => ngettext(
            "{count} card already in Rappel",
            "{count} cards already in Rappel",
            n,
        ),
        // Translators: {count} is a number of cards.
        Skip::EmptyFront => ngettext(
            "{count} card with an empty front",
            "{count} cards with an empty front",
            n,
        ),
        // Translators: a note is what Anki makes cards from.
        Skip::BrokenNote => ngettext(
            "{count} card whose note is damaged",
            "{count} cards whose notes are damaged",
            n,
        ),
        // Translators: a template is how Anki lays out a card.
        Skip::BadTemplate => ngettext(
            "{count} card whose template cannot be read",
            "{count} cards whose templates cannot be read",
            n,
        ),
        // Translators: {count} is a number of cards.
        Skip::TooLong => ngettext(
            "{count} card too long for Rappel",
            "{count} cards too long for Rappel",
            n,
        ),
        // Translators: a filter is a part of an Anki template ({{filter:Field}}).
        Skip::UnknownFilter => ngettext(
            "{count} card with a template filter Rappel does not know, shown as it is",
            "{count} cards with a template filter Rappel does not know, shown as they are",
            n,
        ),
        // Translators: {count} is a number of cards.
        Skip::UnknownField => ngettext(
            "{count} card whose template names a missing field",
            "{count} cards whose templates name a missing field",
            n,
        ),
        // Translators: {count} is a number of formulas.
        Skip::MathAsText => ngettext(
            "{count} formula Rappel cannot draw, kept as text",
            "{count} formulas Rappel cannot draw, kept as text",
            n,
        ),
        // Translators: {count} is a number of images.
        Skip::RemoteImage => ngettext(
            "{count} image from the web, kept as its address and never downloaded",
            "{count} images from the web, kept as their addresses and never downloaded",
            n,
        ),
        _ => return None,
    })
}

/// The template of a reason about media, tags, decks or the history.
fn other_template(reason: Skip, n: usize) -> String {
    match reason {
        // Translators: media files are images and sounds.
        Skip::MissingMedia => ngettext(
            "{count} media file missing from the package",
            "{count} media files missing from the package",
            n,
        ),
        // Translators: {count} is a number of media files.
        Skip::UnsupportedMedia => ngettext(
            "{count} media file of a kind Rappel does not show",
            "{count} media files of kinds Rappel does not show",
            n,
        ),
        // Translators: {count} is a number of media files.
        Skip::MediaTooLarge => ngettext(
            "{count} media file too large for Rappel",
            "{count} media files too large for Rappel",
            n,
        ),
        // Translators: {count} is a number of media files.
        Skip::DamagedMedia => ngettext(
            "{count} damaged media file",
            "{count} damaged media files",
            n,
        ),
        // Translators: {count} is a number of media files.
        Skip::BadMediaName => ngettext(
            "{count} media file with a name that cannot be used",
            "{count} media files with names that cannot be used",
            n,
        ),
        // Translators: {count} is a number of media files.
        Skip::UnusedMedia => ngettext(
            "{count} media file no card uses",
            "{count} media files no card uses",
            n,
        ),
        // Translators: {count} is a number of tags (Anki's).
        Skip::InvalidTag => ngettext(
            "{count} tag Rappel cannot have",
            "{count} tags Rappel cannot have",
            n,
        ),
        // Translators: {count} is a number of deck names.
        Skip::DeckNameShortened => ngettext(
            "{count} deck name shortened to fit",
            "{count} deck names shortened to fit",
            n,
        ),
        // Translators: Anki's “Set Due Date” and “Forget”.
        Skip::ManualEntry => ngettext(
            "{count} schedule change made by hand, not a review",
            "{count} schedule changes made by hand, not reviews",
            n,
        ),
        // Translators: {count} is a number of reviews.
        Skip::FilteredReview => ngettext(
            "{count} review in a filtered deck that did not count",
            "{count} reviews in filtered decks that did not count",
            n,
        ),
        // Translators: {count} is a number of reviews.
        Skip::BadReview => ngettext("{count} damaged review", "{count} damaged reviews", n),
        // Card reasons are `card_template`'s, and TooManyTags is
        // `reason_text`'s.
        // Translators: {count} is a number of things in a package.
        _ => ngettext("{count} item", "{count} items", n),
    }
}

/// One line of the report: the reason, then its examples in quotes.
fn report_line(skipped: &Skipped) -> String {
    let reason = reason_text(skipped.reason, skipped.count);
    if skipped.examples.is_empty() {
        return format!("• {reason}");
    }
    let examples: Vec<String> = skipped
        .examples
        .iter()
        .map(|e| {
            fill(
                // Translators: one example in the Anki import's report (a
                // card's text, a file or tag name), in quotes.
                &gettext("“{example}”"),
                &[("example", &isolate(e))],
            )
        })
        .collect();
    format!(
        "• {}",
        fill(
            // Translators: {reason} is a line of the Anki import's report
            // ("2 cards with an empty front"), {examples} a few of them in
            // quotes.
            &gettext("{reason} ({examples})"),
            &[
                ("reason", &reason),
                ("examples", &examples.join(&list_separator()))
            ],
        )
    )
}

/// The report: what is left out, then what is changed, a line each.
/// `None` if nothing is.
pub(super) fn report_text(skipped: &[Skipped]) -> Option<String> {
    let (changes, left_out): (Vec<&Skipped>, Vec<&Skipped>) = skipped
        .iter()
        .filter(|s| s.count > 0)
        .partition(|s| is_change(s.reason));
    let mut lines = Vec::new();
    if !left_out.is_empty() {
        // Translators: the heading of the list of what an Anki import
        // leaves out.
        lines.push(gettext("Left out:"));
        lines.extend(left_out.into_iter().map(report_line));
    }
    if !changes.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        // Translators: the heading of the list of what an Anki import
        // keeps differently.
        lines.push(gettext("Changed:"));
        lines.extend(changes.into_iter().map(report_line));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// What the dialog says before an Anki import.
pub(super) fn anki_view(summary: &AnkiSummary) -> AnkiView {
    let report = report_text(&summary.skipped);
    if summary.is_noop() {
        let present = summary
            .skipped
            .iter()
            .any(|s| s.reason == Skip::AlreadyInRappel);
        let only_present = summary.skipped.iter().all(|s| {
            s.reason == Skip::AlreadyInRappel
                || !matches!(
                    s.reason,
                    Skip::EmptyFront | Skip::BrokenNote | Skip::BadTemplate | Skip::TooLong
                )
        });
        let body = if present && only_present {
            gettext("Every card of this package is already in Rappel.")
        } else {
            gettext("No card of this package can be imported.")
        };
        return AnkiView {
            heading: gettext("Nothing to Import"),
            body,
            report,
            can_import: false,
        };
    }
    let mut sentences = Vec::new();
    if let Some(new) = decks_cards_reviews(summary.new_decks, summary.cards, summary.reviews) {
        // Translators: {new} is a list of counts ("12 decks, 840 cards
        // and 5,210 reviews").
        sentences.push(fill(&gettext("This adds {new}."), &[("new", &new)]));
    }
    let existing = summary.existing_decks;
    if existing > 0 {
        sentences.push(counted(
            // Translators: {count} is a number of the user's decks that
            // have the name of a deck of the Anki package.
            &ngettext(
                "Cards also go into {count} deck you already have.",
                "Cards also go into {count} decks you already have.",
                existing,
            ),
            existing,
        ));
    }
    let media = summary.new_media;
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
    let suspended = summary.suspended;
    if suspended > 0 {
        sentences.push(counted(
            // Translators: {count} is a number of cards suspended in Anki.
            &ngettext(
                "{count} card stays suspended, as in Anki.",
                "{count} cards stay suspended, as in Anki.",
                suspended,
            ),
            suspended,
        ));
    }
    if report.is_some() {
        sentences.push(gettext(
            "Some of the package is left out or changed, as listed below.",
        ));
    }
    sentences.push(gettext("Your data is backed up first."));
    AnkiView {
        // Translators: the question before an import from Anki.
        heading: gettext("Import from Anki?"),
        body: sentences.join(&pgettext("sentence separator", " ")),
        report,
        can_import: true,
    }
}

/// The toast after an Anki import: what was added, then the report
/// behind Details.
pub(super) fn anki_imported_text(report: &AnkiReport) -> String {
    let added = decks_cards_reviews(report.new_decks, report.cards, report.reviews).map_or_else(
        || gettext("Nothing was imported: everything was already here"),
        |added| fill(&gettext("Imported {added}"), &[("added", &added)]),
    );
    match report_text(&report.skipped) {
        None => titled(&gettext("Import done"), added),
        Some(lines) => titled(&gettext("Import done"), format!("{added}.\n\n{lines}")),
    }
}

/// The toast for a package Rappel cannot read.
pub(super) fn anki_refused_text(problem: &AnkiProblem) -> String {
    match problem {
        AnkiProblem::NewerVersion(_) => gettext(
            "Newer Anki package. Export it again in Anki with “Support older Anki versions” \
             checked, then import that file",
        ),
        other => fill(
            // Translators: a toast: what is before ":" is its title, the
            // rest is behind Details. {reason} is why, in English.
            &gettext("Cannot import this file: {reason}"),
            &[("reason", &other.to_string())],
        ),
    }
}

/// The toast titles of an Anki import's progress (with Cancel).
pub(super) fn reading_text() -> String {
    // Translators: a toast with a Cancel button while an Anki package is
    // read; keep it short (it must fit a phone's width).
    gettext("Reading Anki package…")
}

pub(super) fn importing_text() -> String {
    // Translators: a toast with a Cancel button while an Anki package is
    // imported; keep it short (it must fit a phone's width).
    gettext("Importing from Anki…")
}

pub(super) fn cancelled_text() -> String {
    gettext("Import cancelled")
}

#[cfg(test)]
mod tests {
    use rappel_core::interchange::AnkiFormat;

    use super::*;

    fn skipped(reason: Skip, count: usize, examples: &[&str]) -> Skipped {
        Skipped {
            reason,
            count,
            examples: examples.iter().map(|e| (*e).to_owned()).collect(),
        }
    }

    fn summary(cards: usize, skipped: Vec<Skipped>) -> AnkiSummary {
        AnkiSummary {
            format: AnkiFormat::Current,
            new_decks: 2,
            existing_decks: 0,
            cards,
            suspended: 0,
            reviews: 30,
            new_media: 0,
            present_media: 0,
            tags: 0,
            skipped,
        }
    }

    #[test]
    fn the_question_counts_what_is_added() {
        let mut s = summary(16, Vec::new());
        s.existing_decks = 1;
        s.new_media = 3;
        s.suspended = 1;
        let view = anki_view(&s);
        assert_eq!(view.heading, "Import from Anki?");
        assert_eq!(
            view.body,
            "This adds 2 decks, 16 cards and 30 reviews. Cards also go into 1 deck you already \
             have. This adds 3 media files. 1 card stays suspended, as in Anki. Your data is \
             backed up first."
        );
        assert_eq!(view.report, None);
        assert!(view.can_import);
    }

    #[test]
    fn the_report_lists_what_is_left_out_then_what_is_changed() {
        let s = summary(
            5,
            vec![
                skipped(Skip::EmptyFront, 1, &["poisson"]),
                skipped(Skip::UnknownFilter, 2, &["myaddon"]),
                skipped(Skip::MissingMedia, 1_200, &["a.png", "b.png"]),
                skipped(Skip::ManualEntry, 2, &[]),
            ],
        );
        let view = anki_view(&s);
        assert!(view.body.ends_with(
            "Some of the package is left out or changed, as listed below. Your data is backed up first."
        ));
        assert_eq!(
            view.report.unwrap(),
            "Left out:\n\
             • 1 card with an empty front (“\u{2068}poisson\u{2069}”)\n\
             • 1,200 media files missing from the package (“\u{2068}a.png\u{2069}”, “\u{2068}b.png\u{2069}”)\n\
             • 2 schedule changes made by hand, not reviews\n\
             \n\
             Changed:\n\
             • 2 cards with a template filter Rappel does not know, shown as they are (“\u{2068}myaddon\u{2069}”)"
        );
    }

    #[test]
    fn every_reason_has_a_text() {
        for reason in [
            Skip::AlreadyInRappel,
            Skip::EmptyFront,
            Skip::BrokenNote,
            Skip::BadTemplate,
            Skip::TooLong,
            Skip::UnknownFilter,
            Skip::UnknownField,
            Skip::MathAsText,
            Skip::RemoteImage,
            Skip::MissingMedia,
            Skip::UnsupportedMedia,
            Skip::MediaTooLarge,
            Skip::DamagedMedia,
            Skip::BadMediaName,
            Skip::UnusedMedia,
            Skip::InvalidTag,
            Skip::TooManyTags,
            Skip::DeckNameShortened,
            Skip::ManualEntry,
            Skip::FilteredReview,
            Skip::BadReview,
        ] {
            for n in [1, 2] {
                let text = reason_text(reason, n);
                assert!(text.starts_with(&n.to_string()), "{text}");
            }
        }
        assert_eq!(
            reason_text(Skip::TooManyTags, 3),
            "3 notes with more than 100 tags, the rest left out"
        );
    }

    #[test]
    fn nothing_to_import_says_why() {
        let view = anki_view(&summary(
            0,
            vec![skipped(Skip::AlreadyInRappel, 16, &["x"])],
        ));
        assert_eq!(view.heading, "Nothing to Import");
        assert_eq!(
            view.body,
            "Every card of this package is already in Rappel."
        );
        assert!(!view.can_import);
        let view = anki_view(&summary(0, vec![skipped(Skip::BrokenNote, 3, &[])]));
        assert_eq!(view.body, "No card of this package can be imported.");
        assert!(view.report.is_some());
        let view = anki_view(&summary(0, Vec::new()));
        assert_eq!(view.body, "No card of this package can be imported.");
    }

    #[test]
    fn the_done_toast_has_a_short_title_and_the_report_behind_details() {
        use crate::ui::toast::{TOAST_FITS_WITH_BUTTON, ToastText, toast_text};
        let report = AnkiReport {
            new_decks: 5,
            cards: 16,
            suspended: 1,
            reviews: 16,
            new_media: 3,
            skipped: vec![skipped(Skip::EmptyFront, 1, &["poisson"])],
            backup: None,
        };
        let text = anki_imported_text(&report);
        let ToastText::Detailed { title, details } = toast_text(&text) else {
            panic!("{text}");
        };
        assert_eq!(title, "Import done");
        assert!(title.chars().count() <= TOAST_FITS_WITH_BUTTON);
        assert_eq!(
            details,
            "Imported 5 decks, 16 cards and 16 reviews.\n\nLeft out:\n• 1 card with an empty front (“\u{2068}poisson\u{2069}”)"
        );
        let plain = AnkiReport {
            skipped: Vec::new(),
            ..report
        };
        let text = anki_imported_text(&plain);
        assert_eq!(
            text,
            "Import done. Imported 5 decks, 16 cards and 16 reviews"
        );
    }

    #[test]
    fn progress_toasts_fit_beside_their_button() {
        use crate::ui::toast::TOAST_FITS_WITH_BUTTON;
        for text in [reading_text(), importing_text(), cancelled_text()] {
            assert!(text.chars().count() <= TOAST_FITS_WITH_BUTTON, "{text}");
        }
    }

    #[test]
    fn refusals_have_a_title() {
        use crate::ui::toast::{TOAST_FITS_WITH_BUTTON, ToastText, fallback_title, toast_text};
        for problem in [
            AnkiProblem::NewerVersion(4),
            AnkiProblem::NoCollection,
            AnkiProblem::Damaged("x".to_owned()),
        ] {
            let text = anki_refused_text(&problem);
            if let ToastText::Detailed { title, .. } = toast_text(&text) {
                assert_ne!(title, fallback_title(), "{text}");
                assert!(title.chars().count() <= TOAST_FITS_WITH_BUTTON, "{title}");
            }
        }
    }
}
