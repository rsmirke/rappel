//! About Rappel (primary menu, `win.about`): an `AdwAboutDialog` with the
//! version, the licence, the repository and the credits for the bundled
//! `KaTeX` fonts and the main libraries. Built in code: everything in it is
//! a constant, and the texts are tested below.

use adw::prelude::*;

/// The repository, from the workspace's `[package] repository`.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The person shown as the developer and copyright holder.
const DEVELOPER: &str = "Robert Smirke";

/// The libraries Rappel is built on, credited by name.
const LIBRARIES: [&str; 8] = [
    "GTK",
    "libadwaita",
    "GtkSourceView",
    "gtk-rs",
    "RaTeX",
    "SQLite and rusqlite",
    "FSRS (fsrs-rs)",
    "pulldown-cmark",
];

/// A legal section of the dialog: a title, a copyright and a licence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Legal {
    title: &'static str,
    copyright: &'static str,
    /// Pango markup (`AdwAboutDialog` shows a custom licence as markup).
    license: &'static str,
}

/// The `KaTeX` fonts drawn by the math renderer (ADR 0021, 0023). The
/// copyright is the one in the fonts' own name table; the OFL asks for it
/// to be kept with the fonts. Both strings are shown as markup
/// (`adw-about-dialog.c`, `append_legal_section`), so neither may hold a
/// bare `&` or `<`.
const KATEX_FONTS: Legal = Legal {
    title: "KaTeX Fonts",
    copyright: "© 2009–2010 Design Science, Inc.\n© 2014–2018 Khan Academy",
    license: "These fonts are licensed under the \
              <a href=\"https://openfontlicense.org\">SIL Open Font License, Version 1.1</a>. \
              They are data shipped with Rappel, not part of its code.",
};

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
    dialog.add_credit_section(Some("Built With"), &LIBRARIES);
    dialog.add_legal_section(
        KATEX_FONTS.title,
        Some(KATEX_FONTS.copyright),
        gtk::License::Custom,
        Some(KATEX_FONTS.license),
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
        assert!(KATEX_FONTS.copyright.contains("Design Science"));
        assert!(KATEX_FONTS.copyright.contains("Khan Academy"));
        assert!(
            KATEX_FONTS
                .license
                .contains("SIL Open Font License, Version 1.1")
        );
    }

    #[test]
    fn the_font_licence_is_label_markup() {
        // AdwAboutDialog shows a custom licence in a GtkLabel with markup,
        // which adds `<a href>` to Pango's tags (GtkLabel docs, "Links").
        crate::testing::with_gtk(|| {
            let label = gtk::Label::new(None);
            label.set_markup(KATEX_FONTS.license);
            assert_eq!(
                label.text(),
                "These fonts are licensed under the SIL Open Font License, Version 1.1. \
                 They are data shipped with Rappel, not part of its code."
            );
        });
    }
}
