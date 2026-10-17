# 2. GTK stack versions and Blueprint build pipeline

- Status: Accepted; the libadwaita floor is raised to 1.8 (GTK 4.20) by ADR 0039; `build.rs` runs `glib-compile-resources` itself, without `glib-build-tools`, since ADR 0047
- Date: 2026-10-15

## Context

SPEC.md prescribes Rust, GTK4, libadwaita and Blueprint. We need to fix which versions we target and how Blueprint files reach the running app, since both affect every UI slice and what users need installed to build.

## Decision

- Rust bindings: `gtk4` 0.11 and `libadwaita` 0.9 (the matching gtk-rs 0.22 release set), Rust ≥ 1.92 (their MSRV).
- Minimum system libraries: **GTK 4.14** and **libadwaita 1.6** (cargo features `v4_14`, `v1_6`). libadwaita 1.6 gives `AdwToolbarView`, `AdwDialog`, `AdwAboutDialog`, `AdwPreferencesDialog` and `AdwSpinner`, which SPEC §4.8 needs; GTK 4.14 is what that libadwaita requires. These are available on current GNOME distributions and in the GNOME 47+ Flatpak runtimes. We raise the floor only with a new ADR.
- Blueprint is compiled at **build time** by `crates/app/build.rs`: `blueprint-compiler batch-compile` turns `data/*.blp` into `.ui` files in `OUT_DIR`, then `glib-build-tools` runs `glib-compile-resources` to produce a gresource that is embedded in the binary with `gio::resources_register_include!`. Templates are loaded by resource path (`/io/github/rsmirke/Rappel/…`).
- We do not use gtk4-macros' `blueprint` feature (inline Blueprint in Rust): it cannot load templates from resources, and keeping layout in `.blp` files matches CLAUDE.md.

## Consequences

- Building needs `blueprint-compiler` and `glib-compile-resources` on `PATH` (documented in README). The future Meson/Flatpak build (SPEC §5) can reuse the same `.blp` files and gresource XML.
- The binary is self-contained: no `.ui` files to install.
- Using a widget newer than libadwaita 1.6 / GTK 4.14 needs a cargo feature bump and a new ADR.
