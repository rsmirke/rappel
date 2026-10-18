//! Compiles Blueprint files to `GtkBuilder` XML, then bundles them into a
//! gresource that `main.rs` embeds in the binary; and compiles each
//! language of `po/LINGUAS` to `<OUT_DIR>/locale/<lang>/LC_MESSAGES/rappel.mo`,
//! where a debug build finds its translations (ADR 0059).
//!
//! Every input is a tracked build input (#62): each `data/*.blp`, the
//! `data` directory itself (so a new or deleted file counts), and every file
//! `data/resources.gresource.xml` bundles from `data` (`style.css`, …),
//! with the manifest itself. Cargo reruns this script when one of them is
//! newer than its last run, and the crate is then compiled again, because
//! `gio::resources_register_include!` includes the gresource with
//! `include_bytes!`, which rustc tracks. The `.ui` files this script writes
//! to `OUT_DIR` are not listed: they are newer than the run that wrote them,
//! so listing them would make every change to a `.blp` cost two builds
//! (ADR 0047).

#![expect(
    clippy::expect_used,
    reason = "a build script reports failure by panicking: cargo shows the message and \
              stops the build, so `expect()` is the correct error handling here"
)]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

// The `.po` reader and `.mo` writer, shared with the app's tests (ADR 0059).
#[path = "src/i18n/po.rs"]
#[expect(
    dead_code,
    reason = "the build only compiles catalogs; the comment fields and the .po writer serve \
              the tests"
)]
mod po;

const DATA_DIR: &str = "data";
const MANIFEST: &str = "data/resources.gresource.xml";
const TARGET: &str = "rappel.gresource";
/// The translations, at the repository's root (GNOME's layout).
const PO_DIR: &str = "../../po";
/// The gettext domain (`i18n::DOMAIN`).
const DOMAIN: &str = "rappel";

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let ui_dir = out_dir.join("ui");

    let mut blueprints: Vec<PathBuf> = fs::read_dir(DATA_DIR)
        .expect("crates/app/data is part of the source tree")
        .map(|entry| {
            entry
                .expect("entries of crates/app/data are readable")
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "blp"))
        .collect();
    blueprints.sort();

    // A directory is scanned as a whole (Cargo book, build scripts,
    // `rerun-if-changed`), so a file added to `data` reruns the script.
    println!("cargo:rerun-if-changed={DATA_DIR}");
    for blueprint in &blueprints {
        println!("cargo:rerun-if-changed={}", blueprint.display());
    }

    let status = Command::new("blueprint-compiler")
        .arg("batch-compile")
        .arg(&ui_dir)
        .arg(DATA_DIR)
        .args(&blueprints)
        .status()
        .unwrap_or_else(|err| {
            panic!("could not run blueprint-compiler ({err}); install it, see README.md")
        });
    assert!(status.success(), "blueprint-compiler failed: {status}");

    let source_dirs = [Path::new(DATA_DIR), ui_dir.as_path()];
    compile_resources(&source_dirs, &out_dir.join(TARGET));
    println!("cargo:rerun-if-changed={MANIFEST}");
    for input in resource_inputs(&source_dirs) {
        if !input.starts_with(&out_dir) {
            println!("cargo:rerun-if-changed={}", input.display());
        }
    }

    compile_catalogs(&out_dir.join("locale"));
}

/// Compiles `po/<lang>.po` for each language of `po/LINGUAS` into
/// `<locale_dir>/<lang>/LC_MESSAGES/rappel.mo`, as `msgfmt` would (without
/// GNU gettext's tools, which are not needed to build Rappel).
fn compile_catalogs(locale_dir: &Path) {
    let po_dir = Path::new(PO_DIR);
    println!("cargo:rerun-if-changed={PO_DIR}");
    let linguas = po_dir.join("LINGUAS");
    println!("cargo:rerun-if-changed={}", linguas.display());
    let languages = fs::read_to_string(&linguas).expect("po/LINGUAS is part of the source tree");
    for language in languages
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let source = po_dir.join(format!("{language}.po"));
        println!("cargo:rerun-if-changed={}", source.display());
        let text =
            fs::read_to_string(&source).unwrap_or_else(|err| panic!("{}: {err}", source.display()));
        let entries = po::parse(&text).unwrap_or_else(|err| panic!("{}: {err}", source.display()));
        let dir = locale_dir.join(language).join("LC_MESSAGES");
        fs::create_dir_all(&dir).expect("OUT_DIR is writable");
        fs::write(dir.join(format!("{DOMAIN}.mo")), po::mo(&entries)).expect("OUT_DIR is writable");
    }
}

/// `glib-compile-resources` with `source_dirs` searched in order.
fn glib_compile_resources(source_dirs: &[&Path]) -> Command {
    let mut command = Command::new("glib-compile-resources");
    for dir in source_dirs {
        command.arg("--sourcedir").arg(dir);
    }
    command
}

/// Bundles the files `MANIFEST` lists into `target`.
fn compile_resources(source_dirs: &[&Path], target: &Path) {
    let output = glib_compile_resources(source_dirs)
        .arg("--target")
        .arg(target)
        .arg(MANIFEST)
        .output()
        .unwrap_or_else(|err| {
            panic!("could not run glib-compile-resources ({err}); install it, see README.md")
        });
    assert!(
        output.status.success(),
        "glib-compile-resources failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The file behind each entry of `MANIFEST`, as `glib-compile-resources`
/// finds it in `source_dirs`.
fn resource_inputs(source_dirs: &[&Path]) -> Vec<PathBuf> {
    let output = glib_compile_resources(source_dirs)
        .arg("--generate-dependencies")
        .arg(MANIFEST)
        .output()
        .expect("glib-compile-resources ran a moment ago");
    assert!(
        output.status.success(),
        "glib-compile-resources --generate-dependencies failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("glib-compile-resources prints the paths as UTF-8")
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}
