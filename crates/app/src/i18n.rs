//! Translation (ADR 0059): the gettext domain, the lookups every user
//! string goes through, and named placeholders.
//!
//! Every text a user can see is a whole sentence marked with [`gettext`],
//! [`pgettext`] (with a context, for a word that could be read several
//! ways), [`ngettext`] (a count, which picks the plural form of the
//! language) or [`npgettext`] (a count with a context). Their first arguments are string literals, so
//! the tests can extract them into `po/rappel.pot` (`i18n::extract`). A
//! number or a name goes into a translated sentence through a named
//! placeholder, `{count}`, filled by [`fill`], so a translator can move it.
//! Blueprint files mark their strings with `_("…")` and `C_("…", "…")`,
//! which `GtkBuilder` looks up in the same domain.
//!
//! Translations come from `<locale dir>/<lang>/LC_MESSAGES/rappel.mo`,
//! which `build.rs` compiles from `po/<lang>.po` (`i18n::po`). In the
//! tests nothing is bound, so every lookup gives the English text, unless
//! a test asks for French ([`tests::in_french`]).

#[cfg(test)]
mod extract;
// The app reads catalogs through the C library; `build.rs` includes this
// module itself to compile them, and the tests use it to check them.
#[cfg(test)]
mod po;
#[cfg(test)]
pub mod tests;

use std::ffi::OsString;
use std::path::PathBuf;

/// The gettext domain: `rappel.mo`, and `po/rappel.pot`.
pub const DOMAIN: &str = "rappel";

/// The environment variable that names the locale directory at run time.
pub const LOCALEDIR_VARIABLE: &str = "RAPPEL_LOCALEDIR";

/// Where the installed `.mo` files are when a release build was not told
/// otherwise at compile time: the system's locale directory.
const SYSTEM_LOCALEDIR: &str = "/usr/share/locale";

/// Where the translations are: `RAPPEL_LOCALEDIR` at run time when set
/// (not empty); else, in a debug build, the catalogs `build.rs` compiled
/// into `OUT_DIR`, so `cargo run` is translated without installing
/// anything; else `RAPPEL_LOCALEDIR` as it was at compile time (packaging
/// sets it), else `/usr/share/locale`.
fn locale_dir(
    at_run_time: Option<OsString>,
    debug: bool,
    at_compile_time: Option<&str>,
) -> PathBuf {
    if let Some(dir) = at_run_time.filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    if debug && let Some(dir) = build_locale_dir() {
        return dir;
    }
    PathBuf::from(at_compile_time.unwrap_or(SYSTEM_LOCALEDIR))
}

/// The catalogs `build.rs` compiled, which only a debug build reads: a
/// release build does not carry the build folder's path.
#[cfg(debug_assertions)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the release build's twin has no folder to give"
)]
fn build_locale_dir() -> Option<PathBuf> {
    Some(PathBuf::from(env!("OUT_DIR")).join("locale"))
}

#[cfg(not(debug_assertions))]
fn build_locale_dir() -> Option<PathBuf> {
    None
}

/// Binds the domain to the locale directory and makes it the default one,
/// in UTF-8, before GTK starts. GTK sets the locale from the environment
/// (`setlocale (LC_ALL, "")`) in `gtk_init`, unless told not to: "Prevents
/// `gtk_init()` … from automatically calling `setlocale (LC_ALL, "")`. You
/// would want to use this function if you wanted to set the locale for
/// your program to something other than the user's locale" (GTK docs,
/// `gtk_disable_setlocale`). So Rappel does not call `setlocale` itself,
/// which the bindings only offer as `unsafe`. No text is translated before
/// GTK starts: what `main` does before is only logged. `GtkBuilder`
/// translates with `gettext()`, the default domain, when a builder has no
/// domain of its own ("If the translation domain is `NULL`, `GtkBuilder` uses
/// `gettext()`, otherwise `g_dgettext()`", GTK docs,
/// `gtk_builder_set_translation_domain`); templates and the layouts
/// `ui::dialog` loads set none, so `textdomain` covers them all.
pub fn init() {
    let dir = locale_dir(
        std::env::var_os(LOCALEDIR_VARIABLE),
        cfg!(debug_assertions),
        option_env!("RAPPEL_LOCALEDIR"),
    );
    bind(&dir);
}

/// [`init`] with the locale directory given.
pub fn bind(dir: &std::path::Path) {
    tracing::debug!("translations from {}", dir.display());
    if let Err(err) = gettextrs::bindtextdomain(DOMAIN, dir) {
        tracing::warn!("could not bind the translations: {err}");
    }
    if let Err(err) = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8") {
        tracing::warn!("could not ask for translations in UTF-8: {err}");
    }
    if let Err(err) = gettextrs::textdomain(DOMAIN) {
        tracing::warn!("could not set the translation domain: {err}");
    }
}

/// `msgid` in the user's language.
pub fn gettext(msgid: &str) -> String {
    #[cfg(test)]
    if let Some(text) = tests::translated(None, msgid, None, 0) {
        return text;
    }
    gettextrs::gettext(msgid)
}

/// `msgid` in the user's language, in `context`: the same English word can
/// be two words in another language ("Due" a heading, "due" a count).
pub fn pgettext(context: &str, msgid: &str) -> String {
    #[cfg(test)]
    if let Some(text) = tests::translated(Some(context), msgid, None, 0) {
        return text;
    }
    gettextrs::pgettext(context, msgid)
}

/// The form of `singular` / `plural` that `n` takes in the user's
/// language (English: `singular` for exactly one).
pub fn ngettext(singular: &str, plural: &str, n: usize) -> String {
    #[cfg(test)]
    if let Some(text) = tests::translated(None, singular, Some(plural), n) {
        return text;
    }
    gettextrs::ngettext(singular, plural, plural_n(n))
}

/// [`ngettext`] in `context`: the same English words counted another way
/// ("{count} days" of a decimal number, which every language counts by its
/// own rule for decimals).
pub fn npgettext(context: &str, singular: &str, plural: &str, n: usize) -> String {
    #[cfg(test)]
    if let Some(text) = tests::translated(Some(context), singular, Some(plural), n) {
        return text;
    }
    gettextrs::npgettext(context, singular, plural, plural_n(n))
}

/// `n` as the C library's `ngettext` takes it (an `unsigned long`, which
/// the bindings narrow to 32 bits). A count past `u32::MAX` keeps what
/// plural rules look at: its last six digits, and that it is more than
/// one (rules test `n % 10`, `n % 100`, `n > 1`, `n == 0`…).
fn plural_n(n: usize) -> u32 {
    u32::try_from(n).unwrap_or_else(|_| {
        let last = u32::try_from(n % 1_000_000).unwrap_or(0);
        1_000_000 + last
    })
}

/// `template` with each `{name}` of `values` replaced by its value, in one
/// pass (a value is never searched for placeholders, so a deck named
/// "{count}" stays as typed). A placeholder without a value is left as it
/// is, so a mistake shows instead of vanishing; the tests check that each
/// translation has exactly the placeholders of its English text.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('}').and_then(|close| {
            let name = &after[..close];
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value, close))
        });
        if let Some((value, close)) = value {
            out.push_str(value);
            rest = &after[close + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The names of the placeholders in `text`, in order: `{count}` gives
/// "count". Only names made of ASCII letters, digits and `_`.
#[cfg(test)]
pub fn placeholders(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close)
                if close > 0
                    && after[..close]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                names.push(&after[..close]);
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    names
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn placeholders_are_filled_once_by_name() {
        assert_eq!(
            fill(
                "{count} cards in {deck}",
                &[("deck", "Maths"), ("count", "3")]
            ),
            "3 cards in Maths"
        );
        // A value is never searched again.
        assert_eq!(fill("“{name}”", &[("name", "{name}")]), "“{name}”");
        // An unknown or broken placeholder stays as it is.
        assert_eq!(fill("{nope} {", &[("count", "3")]), "{nope} {");
        assert_eq!(fill("a } b", &[]), "a } b");
        assert_eq!(
            placeholders("{count} of {total}, {} {x y}"),
            ["count", "total"]
        );
    }

    #[test]
    fn large_counts_keep_their_plural_form() {
        assert_eq!(plural_n(0), 0);
        assert_eq!(plural_n(1), 1);
        assert_eq!(plural_n(4_000_000_000), 4_000_000_000);
        let past = usize::try_from(u64::from(u32::MAX) + 2).unwrap_or(usize::MAX);
        assert!(plural_n(past) > 1);
        assert_eq!(
            plural_n(past) % 1_000_000,
            u32::try_from(past % 1_000_000).unwrap()
        );
    }

    #[test]
    fn the_locale_dir_is_the_variable_then_the_build_dir_then_the_system() {
        let set = Some(OsString::from("/opt/rappel/locale"));
        assert_eq!(
            locale_dir(set.clone(), true, None),
            PathBuf::from("/opt/rappel/locale")
        );
        assert_eq!(
            locale_dir(set, false, Some("/x")),
            PathBuf::from("/opt/rappel/locale")
        );
        let empty = Some(OsString::new());
        assert_eq!(
            locale_dir(empty, true, None),
            build_locale_dir().unwrap_or_else(|| PathBuf::from(SYSTEM_LOCALEDIR))
        );
        assert_eq!(
            locale_dir(None, false, None),
            PathBuf::from(SYSTEM_LOCALEDIR)
        );
        assert_eq!(
            locale_dir(None, false, Some("/app/share/locale")),
            PathBuf::from("/app/share/locale")
        );
    }
}
