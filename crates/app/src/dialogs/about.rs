//! About Rappel (primary menu, `win.about`): an `AdwAboutDialog` with the
//! version, the licence, the repository and the credits for the bundled
//! `KaTeX` fonts and the main libraries. Built in code: everything in it is
//! a constant, and the texts are tested below.

use adw::prelude::*;

use crate::i18n::gettext;

/// The repository, from the workspace's `[package] repository`.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The person shown as the developer and copyright holder.
const DEVELOPER: &str = "Robert Smirke";

/// The libraries Rappel is built on, credited by name.
const LIBRARIES: [&str; 9] = [
    "GTK",
    "libadwaita",
    "GtkSourceView",
    "gtk-rs",
    "RaTeX",
    "SQLite",
    "rusqlite",
    "FSRS (fsrs-rs)",
    "pulldown-cmark",
];

/// A legal section of the dialog: a title, a copyright and a licence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Legal {
    title: String,
    copyright: &'static str,
    /// Pango markup (`AdwAboutDialog` shows a custom licence as markup).
    license: String,
}

/// The `KaTeX` fonts drawn by the math renderer (ADR 0021, 0023). The
/// copyright is the one in the fonts' own name table; the OFL asks for it
/// to be kept with the fonts. Both strings are shown as markup
/// (`adw-about-dialog.c`, `append_legal_section`), so neither may hold a
/// bare `&` or `<`.
fn katex_fonts() -> Legal {
    Legal {
        title: gettext("KaTeX Fonts"),
        copyright: "© 2009–2010 Design Science, Inc.\n© 2014–2018 Khan Academy",
        license: gettext(
            // Translators: Pango markup: keep the <a href="…"> link and
            // the licence's name as they are, and write & as &amp;.
            "These fonts are licensed under the \
             <a href=\"https://openfontlicense.org\">SIL Open Font License, Version 1.1</a>. \
             They are data shipped with Rappel, not part of its code.",
        ),
    }
}

/// The issue tracker, from the repository.
fn issue_url() -> String {
    format!("{REPOSITORY}/issues")
}

/// Opens About Rappel over `parent`.
pub fn show(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::AboutDialog::new();
    dialog.set_application_name(rappel_core::APP_NAME);
    // No icon is installed yet; libadwaita then shows a placeholder.
    dialog.set_application_icon(rappel_core::APP_ID);
    dialog.set_version(env!("CARGO_PKG_VERSION"));
    dialog.set_developer_name(DEVELOPER);
    dialog.set_developers(&[DEVELOPER]);
    dialog.set_copyright(&format!("© 2026 {DEVELOPER}"));
    // GPL-3.0-or-later; GTK_LICENSE_GPL_3_0 is the "or later" one.
    dialog.set_license_type(gtk::License::Gpl30);
    // Opened in the browser only when clicked; Rappel fetches nothing.
    dialog.set_website(REPOSITORY);
    dialog.set_issue_url(&issue_url());
    // Translators: put your name here (one per line, with an e-mail
    // address in <> if you like); libadwaita lists it under Translated by.
    dialog.set_translator_credits(&gettext("translator-credits"));
    dialog.add_credit_section(Some(&gettext("Built With")), &LIBRARIES);
    let fonts = katex_fonts();
    dialog.add_legal_section(
        &fonts.title,
        Some(fonts.copyright),
        gtk::License::Custom,
        Some(&fonts.license),
    );
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_point_at_the_repository() {
        assert_eq!(REPOSITORY, "https://github.com/rsmirke/rappel");
        assert_eq!(issue_url(), "https://github.com/rsmirke/rappel/issues");
    }

    #[test]
    fn the_main_libraries_are_credited() {
        for name in [
            "GTK",
            "libadwaita",
            "GtkSourceView",
            "RaTeX",
            "SQLite",
            "FSRS",
        ] {
            assert!(
                LIBRARIES.iter().any(|l| l.contains(name)),
                "{name} is not credited"
            );
        }
    }

    #[test]
    fn the_fonts_keep_their_copyright_and_licence() {
        let fonts = katex_fonts();
        assert!(fonts.copyright.contains("Design Science"));
        assert!(fonts.copyright.contains("Khan Academy"));
        assert!(fonts.license.contains("SIL Open Font License, Version 1.1"));
        let french = crate::i18n::tests::in_french(katex_fonts);
        assert!(
            french
                .license
                .contains("SIL Open Font License, Version 1.1")
        );
        assert!(
            french
                .license
                .contains("<a href=\"https://openfontlicense.org\">")
        );
    }

    #[test]
    fn the_font_licence_is_label_markup() {
        // AdwAboutDialog shows a custom licence in a GtkLabel with markup,
        // which adds `<a href>` to Pango's tags (GtkLabel docs, "Links").
        let english = katex_fonts().license;
        let french = crate::i18n::tests::in_french(|| katex_fonts().license);
        crate::testing::with_gtk(move || {
            let label = gtk::Label::new(None);
            label.set_markup(&english);
            assert_eq!(
                label.text(),
                "These fonts are licensed under the SIL Open Font License, Version 1.1. \
                 They are data shipped with Rappel, not part of its code."
            );
            // The translation is markup too: GTK would show a broken one
            // as its raw text.
            label.set_markup(&french);
            assert!(!label.text().contains('<'), "{}", label.text());
            assert!(label.text().contains("SIL Open Font License"));
        });
    }
}
