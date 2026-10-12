# TASK-010: What becomes of `.rnote`, and publishing the fork — notes

Status: **notes only, nothing decided for good, nothing implemented (2026-10-07).** Written down after a conversation at the end of TASK-005 phase 2, to be picked up later.

## Full initial request (verbatim)

> Ok now with all this, what role does the old .rnote has? Should we even keep it? I'd say nah. Also, this is a fork, do you think we should rebrand or try to get our changes into upstream? (Likely nearly impossible) Any other options? I really want to share this with others, but I don't really want to maintain it

> Note these for task_010 for later.

## Read first

- `rnote_ocr_search.md` in the repo root: the project rules.
- `docs/tasks/task_005_notes_library_sync_progress.md`: the note folder (`.rnoted`), the packed note (`.rnotez`) and what the single file `.rnote` still does today.

## 1. The single file `.rnote`

The user's leaning: drop it ("I'd say nah"). The proposal made: drop it as a save format, keep it as an import and export format.

| Role today | Proposed |
|---|---|
| Reading existing notes, the user's own and anyone's from upstream Rnote | Keep, always. This is the `fileformats/rnoteformat` upgrade chain; it costs nothing to keep. |
| Saving | Drop. Every save becomes a note folder, in the library or anywhere else. One format to think about; no document that silently misses the merging and the kept Pdfs. |
| A single file to send | Drop. `.rnotez` does that without losing anything. |
| A file other Rnote versions can open | Keep as "Export → Rnote file (.rnote)". Existing export code behind a menu entry, and the only way an upstream user ever opens one of these notes. |

What changes when this is done:

- Save on a document that was opened from a `.rnote` acts like Save As, suggesting `<name>.rnoted` next to it. The old file stays until the user trashes it.
- The Save As dialog offers `.rnoted` only. The Save As code path for `.rnote` (`to_note_folder == false` in `RnCanvas::save_document_to_file`) moves to the export.
- `rnote-cli convert` keeps both directions.
- The index keeps indexing `.rnote` files in the library, so nothing disappears from search.
- `crates/rnote-ocr/README.md` and the task 005 doc say "single file for Save As" in places; to be updated.

Small change, an hour or two with the checks.

## 2. Publishing the fork

The user wants to share it with others and does not want to maintain it.

What the fork adds on top of Rnote 0.15.0: on-device OCR with a 21 MB ONNX runtime and PaddleOCR models compiled into `rnote-cli`, search across a library with a Mandarin focus, the note folder format with merging between devices, kept Pdfs with page export, packed notes.

Options weighed:

| Option | Assessment |
|---|---|
| Upstream all of it | Not realistic: three upstream maintainers with a Linux/GNOME focus, a runtime and models most users do not want, a new file format and a sync model. |
| Upstream pieces | Realistic and cheap. Upstream already uses `hayro`, so keeping the imported Pdf and exporting its pages as the original with notes on top fits them (`engine/pdfpages.rs`, `hayro-write`). So does the selection fix (deselecting no longer reorders strokes, `store/selection_comp.rs`) and the text layer of Pdf pages kept with them (task 003). Small PRs, each useful on its own; visibility without a promise to maintain. |
| Rebrand | Needed to publish binaries, not code. Shipping something called Rnote with the app id `com.github.flxzt.rnote` confuses their users and bug tracker, and Flathub needs a new id anyway. The id is set in `meson.build` (`base_id`, `app_idpath`) but appears in 42 files; count on a few hours plus an icon. |
| Publish as is, without maintaining | Normal. README first lines: built for my own use, offered as is, no support, no release schedule, based on Rnote 0.15. The existing workflows (`.github/workflows/dist.yml`, `release-windows.yml`, `nightly.yml`) build Flatpak bundles and the Windows installer on a tag, so people get binaries without work. Issues off, or left with the notice. Anyone who wants more forks again. |
| Hand it over | One discussion post upstream: what was built, that the Pdf pieces will be offered as PRs, that the rest is not going to be maintained. The one place the right people read; someone may pick it up. |

Recommendation given: rename, publish as is with the notice and automated builds, send the two or three small PRs upstream, post once.

To check before publishing: the OCR models in `crates/rnote-ocr/models/` come with their own `LICENSE`; that notice has to travel with the binaries. Everything else is upstream's GPL-3.

## Open

- Whether `.rnote` goes as a save format (user leaning yes).
- Name and icon of the fork.
- Which pieces go upstream first.
