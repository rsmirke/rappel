# 55. Images: decoded by glycin in a sandbox, drawn in the card, added in the editor

- Status: Accepted
- Date: 2026-10-18
- Amends: ADR 0019 (a `media:` image at the top level of a paragraph is drawn as a picture; elsewhere it is its alt text), ADR 0054 (the `[image: …]` placeholders are gone; `Store::add_image`), ADR 0025 and ADR 0050 (rows 42 to 44 of the untrusted-input table), ADR 0021 (`deny.toml` ignores four more "unmaintained" advisories)
- Refers to: SPEC §4.3, §4.4, principle 5; `docs/briefs/phase-2.md` (slice 3)

## Context

Slice 2 (ADR 0054) gave the media store, `media:` references in the `content` tree and `Store::add_media`; the app showed `[image: …]` placeholders. This slice draws PNG, JPEG, WebP and GIF (still) images on the review page and in the editor's preview, opens one at full size, and adds images in the editor by a button, paste and drag and drop. The brief asks for glycin (sandboxed decoding) if feasible, with the decision recorded.

Image decoders are a classic attack surface, and the files come from imports and the clipboard. The window must never freeze on a large image, and a bad file must never crash or hang the app.

## Decision

### 1. The decoder: glycin 3.1, sandboxed, with SVG refused

**Evaluated** (time-boxed spike, `target/tmp/glycin-spike`):

- **Option A, the `glycin` crate** (GNOME's image loading library, MPL-2.0 OR LGPL-2.1-or-later). It sends the bytes over a private socket to a loader process (`glycin-loaders`, the image-rs loader for our four formats) that runs under bubblewrap with a seccomp filter and a memory limit, and returns a `GdkTexture`. Locally: a 40 × 30 PNG decodes in about 20 ms; a header claiming 100,000 × 100,000 makes the loader process fail its own allocation ("memory allocation of 30000000000 bytes failed") while Rappel's process stays at 35 MB; a decompression bomb and damaged files are errors. **Where bubblewrap cannot create namespaces** (simulated with an outer `bwrap --disable-userns`, which is what an unprivileged Docker container such as CI gives), glycin detects it (`SandboxMechanism::detect`, `check_bwrap_syscalls_blocked`) and runs the loader as a plain separate process, printing "Glycin running without sandbox": decoding still works, out of Rappel's process. Without bubblewrap installed at all, every load fails (an error, so a placeholder; not a crash). **glycin decodes SVG** when its SVG loader is installed (it is here), so SVG must be refused before glycin sees the bytes.
- **Option B, `gdk::Texture::from_bytes`.** GTK 4.24 decodes PNG, JPEG and TIFF in-process with libpng, libjpeg and libtiff, and other formats through gdk-pixbuf (which on this system goes to glycin itself). In-process for the two most common formats, no sandbox, and blocking.

**Chosen: A.** It works here with the sandbox, and in CI without it, so the tests run everywhere. glycin 3.1 is the newest release for the workspace's Rust 1.92 floor (4.0 needs 1.93 and has the same dependency tree) and talks to the loaders distributions ship (`glycin-loaders` 2.x). Feature `gdk4` for `Frame::texture`. It brings zbus (its private socket speaks D-Bus; never the session or system bus), lcms2, libseccomp and fontconfig bindings (system libraries: `liblcms2-dev libseccomp-dev libfontconfig-dev` to build; `glycin-loaders bubblewrap` at run time; README and CI updated). All licences were already allowed. **cargo-deny reports four "unmaintained" advisories** in that tree: `paste` (RUSTSEC-2024-0436, a build-time macro) and `zvariant`, `zvariant_derive`, `zbus_names` (RUSTSEC-2026-0370, -0372, -0371: merged into zbus 6). None is a vulnerability and none has a fixed version; glycin 4.0.3 has the same ones. They are ignored in `deny.toml` with a reason and a review date (2027-01-16), as ADR 0021 did for `ttf-parser`, and are to be removed once glycin moves to zbus 6. This is a policy decision the maintainer should confirm.

**How a file reaches the decoder** (`render/images.rs`, `render/picture.rs`):

1. Only an `ImageTarget::Media` is ever read (`images::pieces`): a checked hash and an image kind; the path is the media folder joined with the name the core builds. `http:`, `file:`, paths and anything else are never an image piece, so no IO can follow from them (a unit test lists them).
2. `read_image_file`, on a worker thread (`gio::spawn_blocking`): a regular file (`symlink_metadata`, so never through a link), not empty, at most 10 MB, read whole (capped at 10 MB + 1), and its first bytes of the reference's kind (`MediaKind::detect`). SVG text under a `.png` name stops here. Those bytes, not the path, go to glycin (`Loader::new_vec`), so the file cannot change between the check and the decode.
3. glycin's own reading of the type must agree (`accepted_mime`: `image/png` or `image/apng` for a PNG, and exactly the kind's type for the others), so a polyglot that gio takes for another format is refused before its frame is asked for.
4. The header's size is checked before the pixels are decoded (`within_limits`: no side over 16,384 px, at most 64 MP, 256 MiB as a texture), and again on the frame.
5. The whole decode is bounded by `DECODE_TIMEOUT` (10 s); past it the `GCancellable` stops the loader and the image is a placeholder.
6. A GIF is shown still: its first frame (`Image::next_frame` once). An animated PNG likewise.

Every failure is a placeholder (`Failure::Missing` for no file or no media folder; `CannotShow` for the rest), never a panic.

### 2. Rendering

- **Where an image is drawn.** A `media:` image at the top level of a paragraph is a picture between the paragraph's runs of text (`images::pieces`: blank runs between images are dropped; the paragraph becomes a small vertical box). Inside emphasis or a link, in a heading or a table cell, it is its alt text in italics (or nothing): a picture there would need a layout of its own, and such cards are rare. A non-media image is the Markdown typed, as before.
- **Size** (`display_size`, `max_height`, pure): scaled down to the card's width and to a height cap, aspect kept, never scaled up beyond its natural size, at the start of the line like the text. The cap is 60 % of the review page's (or the editor's) height when the side is rendered, at least 120 px: on a phone held sideways (640 × 360) an image is about 190 px tall and the text around it stays in view. The grade buttons are in the page's bottom bar, outside the scrolled card, so no image can push them off screen (checked at 640 × 360). `CardImage` is a small widget whose `measure` gives exactly that size (height-for-width), so GTK lays it out without a picture larger than it is drawn. Rounded corners: the picture is the child of a flat button with `overflow: hidden`, so it takes the button's own rounded corners (`button.card-image { padding: 0 }` in `style.css`); no colour of ours.
- **Budget.** At most 20 images per side (`MAX_IMAGES_PER_SIDE`); later ones are placeholders ("Image not shown: too many images on this side") and are never read. Each costs widgets in the side's 2,000-widget budget (`style::widget_cost`: a column box and three per image), and the 32 KiB shown part still applies.
- **Cache.** Decoded textures are kept per path in a least-recently-used cache bounded by 256 MiB of texture (4 bytes a pixel) and 256 entries (`TextureCache`, pure); a lasting failure is cached too (so a hostile file is not decoded at every card), a missing file is not (it may come with an import). An image shown again appears at once, without a layout jump; one already decoding is not decoded twice (`WAITING`).
- **While decoding** the image takes no room; it appears when done (about 20 ms for a small file). No spinner: it would only flash.
- **Placeholder**: a dimmed line, the `image-missing-symbolic` icon and "Image not available" or "Image could not be shown", then ": " and the alt text if any (plain text, with the long-run breaks of ADR 0025). For screen readers it is one Img with the same words.

### 3. Opening at full size, focus and keys

- A click, or Enter / Space on the focused image, opens an `AdwDialog` (`data/image_dialog.blp`) titled with the alt text (or "Image"), with the picture scaled down to fit (never up). The dialog asks for the image's size; libadwaita keeps it inside the window (a bottom sheet on a narrow one). Escape closes it, as every `AdwDialog`. No 100 % / zoom toggle: a flashcard image that needs one is not a flashcard image.
- **Keyboard on the review page.** The image is a button: Tab from the card area reaches it, and its own Enter and Space open it. The page's keys run in the bubble phase, so a focused button handles its keys first (the same rule the page already follows for Show Answer, `setup_keys`); 1, 2, E, I, S, Delete and Ctrl+Z are not a button's keys and keep working with the image focused. The button does not take the focus on click (`focus-on-click: false`), so after a click the card area keeps it and Space still reveals and grades. After the dialog closes, `AdwDialog` gives the focus back to the image, so Space opens it again until Tab or Shift+Tab moves on; 2 still grades. No new review key was added.
- **Accessible names.** The button is named by the alt text, "Image" without one, and described "Opens the image at full size"; the picture inside is role Img with the same name (`alternative-text`). The card area's own label (`plain_text`) already holds the alt text.
- **The card lists** (`plain_summary`) show the alt text, or nothing for an image without one; no "[Image]" marker.

### 4. The editor

- **Insert Image…**: an icon button (`insert-image-symbolic`, tooltip "Insert Image…", accessible label "Insert Image in Front" / "in Back") in each side's group header opens a `GtkFileDialog` filtered to PNG, JPEG, WebP and GIF (several files may be chosen). The filter is a convenience: the file's first bytes decide.
- **Paste**: `paste-clipboard` (Ctrl+V, Shift+Insert, the context menu's Paste) is a run-last action signal (gtktextview.c, GTK 4.24), so our handler runs first: if the clipboard offers files (`GdkFileList`) they are added; else if it offers a picture (`GdkTexture`, a screenshot or a copied image) it is encoded with `gdk::Texture::save_to_png_bytes` and added; only then is the default paste stopped. Text alone pastes as before. Files come before a picture because a copied image file often offers both and the file is the original.
- **Drag and drop**: a `GtkDropTarget` for `GdkFileList` and `GdkTexture` on each side's frame, in the capture phase, so it answers before the text view's own target (which takes `G_TYPE_STRING`): drop events go through capture and bubble (`gtk_propagate_event`), the first target to answer a motion sets the drop's status (`gtk_drop_status`), and a target that takes a drop stops it (`gtk_drop_target_handle_event`). A drag of text offers neither type, so the text view still takes it. The references go where the files were dropped (`window_to_buffer_coords`, `iter_at_location`), else at the cursor.
- **Adding** runs on a worker thread (`ui::worker`), on its own connection (`AppState::add_image_on_worker`, reading the `GFile` as a stream, so a portal's file works too): `Store::add_image` (new in core) refuses a file of an audio kind as `MediaProblem::NotImage` before anything is written, then is `add_media`. Where the text goes is a left-gravity `GtkTextMark` made at the start, so typing meanwhile does not move it.
- **What is inserted** (`media::insertion`, pure): `![](media:<sha256>.<ext>)`, several separated by a blank line, a space added on a side that touches a word, as **one user action** (one Ctrl+Z takes it back), with the **cursor between `[` and `]`**, so the alt text, the image's accessible name, can be typed at once.
- **Refusals** are a toast in the editor's overlay (an `ErrorSlot`, so a second replaces the first): "This file is not a PNG, JPEG, WebP or GIF image", "This image is larger than 10 MB", "This file is empty", "The file could not be read" (`media::add_failed_text`).
- **A file added and not used** (the card is not saved, or the reference is deleted) stays in the media folder: it is content-addressed, harmless, and slice 5's clean-up removes unused files. Nothing in this slice deletes a file.
- The preview renders with the same renderer and the same images.

### Untrusted input (continues ADR 0054)

| # | Where it comes in | Risk | Status | Test |
|---|---|---|---|---|
| 42 | **Decoding a media image** (review page, preview). | A decoder exploit; a decompression bomb or a lying header filling memory; a hang; SVG with scripts or external references; a file swapped under its name. | **New.** Decoded by glycin in a separate process, under bubblewrap and seccomp with a memory limit where the system allows (else a plain separate process); only the media folder's regular files, read whole within 10 MB and of their kind before decoding; glycin's own type must agree (no SVG); header within 16,384 px a side and 64 MP; 10 s timeout; any failure a placeholder. Measured: a 100,000 × 100,000 header, a 768 MiB bomb, a truncated PNG and JPEG, a broken WebP and SVG behind a PNG signature each end as a placeholder in about 15 to 20 ms, Rappel's process unaffected; a GIF of 20,000 frames shows its first in about 25 ms. | `render::picture::tests::hostile_images_are_placeholders_in_time`, `a_gif_with_many_frames_shows_its_first`, `render::images::tests::a_media_file_is_read_only_when_it_is_what_it_names`, `the_decoder_must_agree_with_the_first_bytes`, `decoded_sizes_are_limited` |
| 43 | **Image targets in card text.** | A URL fetched, a path read. | **New.** Only `media:` references become pictures; every other target is text, so the renderer has no path to read. | `render::images::tests::other_image_targets_are_never_read`, `render::tests::media_images_are_drawn_up_to_the_side_s_share` |
| 44 | **Images pasted, dropped or chosen in the editor.** | A non-image (audio, SVG, HTML) stored as an image; a huge file; the main thread blocked. | **New.** `Store::add_image` (row 39's checks, and audio refused before writing) on a worker thread; refusals said in words. | `media::tests::add_image_takes_only_images`, `dialogs::card_editor::images::tests::*`, `dialogs::card_editor::media::tests::*` |

## Consequences

- New dependency `glycin` 3.1 (and its tree); system packages to build (`liblcms2-dev libseccomp-dev libfontconfig-dev`) and to show images (`glycin-loaders bubblewrap`). Without the loaders, every image is "Image could not be shown"; nothing else changes.
- `deny.toml` ignores four "unmaintained" advisories (above), to review by 2027-01-16.
- glycin turns on newer GLib API features in the tree (`v2_68`), which deprecates `glib::TimeZone::new`: three tests in `clock.rs` use `TimeZone::from_identifier` instead.
- An image changes size only when its side is rendered again: rotating a phone keeps the cap of the moment the card was shown until the next card.
- A focused image takes Space and Enter for itself; Shift+Tab goes back to the card.
- No new migration, no change to the export format.
