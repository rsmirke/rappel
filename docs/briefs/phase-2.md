# Phase 2 — brief for batch mode (target: 0.3.0-beta.1)
Same standards as Phase 1: GNOME HIG, libadwaita docs checked, every slice gated,
reviewed, screenshotted (light, dark, 1000 px, 360×640 portrait and landscape).

Pre-approved (not stop conditions): migrations for the media table and for the
settings/restore fixes below; media entries in export format v2 (the media list
already exists; any other format change is a stop condition); new dependencies
whose licence is already allowed; the media trash flow in slice 5 (the only
approved deletion of user files).

## Slices, in order
0. Phase-1 questions: damaged stored setting → "Reset This Setting" (after a
   backup) on the could-not-start page; restoring a backup sets a waiting
   restore copy aside instead of replacing it; tag suggestions only offer tags
   used by at least one card.
1. Streaming import (SPEC §4.1): two passes, small notes in pass 1, writes in
   pass 2; memory stays bounded (target < 200 MiB peak) for any accepted file.
   Raise archive limits for media (archive ≤ 2 GiB; images ≤ 10 MB, audio ≤ 20 MB each).
2. Media store (core): content-addressed files in the data dir (sha256),
   `media:<hash>.<ext>` references, export/import of media files (streamed),
   validation by magic bytes and size; media is never fetched from the network.
3. Images: PNG, JPEG, WebP; GIF shown static; no SVG. Decode with glycin
   (sandboxed) if feasible — evaluate and record the decision. Paste, drag-and-drop
   and an "Insert Image…" button in the editor; scaled to the card width; click
   for full size; Markdown alt text as the accessible label.
4. Audio: Ogg/Opus, MP3, M4A, WAV. Inline play control, never autoplay; R
   replays the card's audio in review. Insert by drag-and-drop or button. No recording.
5. Unused media: "Clean Up Media" in Preferences, showing the space to free.
   Unused = referenced by no card (live or deleted). Moved to a trash folder,
   purged after 30 days; a restore that needs a trashed file gets it back.
6. Phone and touch: everything usable at 360×640 portrait and landscape; touch
   targets comfortable; no swipe-to-grade; on-screen keyboard never covers the
   focused field; bottom sheets where the HIG suggests them.
7. Translation: gettext infrastructure, every user string marked, plurals,
   locale-aware numbers and dates ("91 %" in French); full French translation
   (the maintainer will proofread).
8. Desktop integration drafts: .desktop file and AppStream metainfo (description,
   releases, content rating, screenshots from the harness). No Flatpak manifest
   (must be human-written for Flathub). Icon: placeholder only.
9. Anki .apkg import (optional, last): legacy and current Anki formats; each
   Anki card → one Rappel card (both directions of reversed notes; cloze → simple
   front/back); HTML → safe Markdown; \( \) / \[ \] → $ / $$; review history
   kept (Again → Fail; Hard/Good/Easy → Pass), schedule rebuilt; "A::B" decks
   → "A › B"; tags and media imported; a clear report of anything skipped. If
   reliable test fixtures can't be produced, stop and note it in QUESTIONS.md.
10. Code clean-up #3 and hardening round 3 (hostile images and audio, media
    zip bombs, huge archives, streaming import attacks).
11. Version 0.3.0-beta.1 (Cargo.toml + CHANGELOG, do NOT tag); final report
    docs/briefs/phase-2-report.md with decisions, questions, screenshot paths
    and one checklist; draft PR phase-2 → main.
