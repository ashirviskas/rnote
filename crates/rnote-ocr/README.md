# rnote-ocr

Text recognition for rnote documents and the index that makes the recognised text searchable.
Everything runs on the CPU and on the device.

## Overview

```mermaid
graph TD
    A[rnote-ui: file opened or saved, or a note in the library changed on disk] --> B["rnote-cli index FILE or FOLDER (background process)"]
    B --> C[rnote-engine: split the document into units]
    C --> D1[Ink of one page]
    C --> D2[One image or Pdf page]
    C --> D3[One typed text]
    D1 --> E{Unit hash already in the index?}
    D2 --> E
    D3 --> E
    E -->|Yes| K[Keep its lines]
    E -->|No, ink or image| F[Render, then rnote-ocr: Recognizer]
    E -->|No, typed or Pdf page with text| G[Take the text and positions the stroke carries]
    F --> H[(rnote-ocr: Index, SQLite)]
    G --> H
    K --> H
    I[rnote-ui search entry / rnote-cli search] --> J[rnote-ocr: Index::search]
    H --> J
    J --> L[Hits: file, page, bounds, text]
    L --> M[rnote-ui: open file, centre view, highlight]
```

The crate has two parts:

| Module | Feature | Used by | What it does |
|---|---|---|---|
| `index` | always built | `rnote-cli`, `rnote-ui` | SQLite index: store lines per unit, search |
| `query` | always built | `rnote-ui`, `index` | Match a query against a line |
| `recognize` | `recognize` | `rnote-cli` only | Find text lines in an image and read them |

`rnote-ui` depends on this crate without the `recognize` feature. The models and the model runtime are therefore only
part of `rnote-cli`. Recognition runs in that separate process, so its memory never sits in the app and is given back
when the process exits.

## Units

A document is not recognised as a whole. `Engine::extract_text_units` (in `rnote-engine`) splits it into units that are
read on their own:

- **Ink**: the brush and shape strokes of one page.
- **Image**: one bitmap or vector image, for example an imported Pdf page. Images are separate from ink, so handwriting
  on top of a Pdf page and the print below it do not disturb each other. A Pdf page that was imported with its text
  layer carries that text (`Stroke::text_lines`); it is indexed as it is and not recognised.
- **Typed**: one text typed with the typewriter. Its characters and their positions come from the text layout, no
  recognition is involved.

Every unit has a hash of its content. A unit whose hash is already in the index is neither rendered nor recognised
again. Editing one page of a long document therefore costs one page, not the document.

## Recognition

`Recognizer::recognize(image)` returns the text lines of an image, each with its characters:

1. **Detection** (`pp-ocrv5_mobile_det.onnx`): the image is scaled so its long side is at most 960 px and the model
   marks the pixels that belong to text. Connected regions become line boxes.
2. **Recognition** (`pp-ocrv5_mobile_rec.onnx`): each line box is cut from the full-resolution image, scaled to a
   height of 48 px and read. The model emits one frame per horizontal step; a character is the first frame of a run.
3. **Candidates**: for each character the up to five most likely readings are kept with their confidence
   (`CharBox::candidates`), together with the horizontal extent of the character (`x0`, `x1`).

Keeping candidates matters for handwriting and for text without context, where the most likely reading is often
wrong while the right one is among the next few.

The models are PP-OCRv5 mobile (Apache-2.0), one model for Traditional and Simplified Chinese, English and more, print
and handwriting. They are compiled into the binary, see [models](models/README.md). They are run with ONNX Runtime
through the `ort` crate, which downloads a prebuilt runtime at build time.

## Index

One SQLite file, `index.sqlite`, in `$XDG_DATA_HOME/rnote/ocr/` (usually `~/.local/share/rnote/ocr/`).

```
files (id, path, mtime, size, zhuyin)    one row per indexed file
units (id, file_id, hash, page, source)  one row per unit of a file
lines (id, unit_id, x, y, w, h, text, chars)
```

- `chars` is the JSON of the line's `CharBox`es, `text` its most likely reading.
- All positions are in document coordinates, so a hit can be shown on the canvas directly.
- A file whose `mtime`, `size` and zhuyin option match is skipped without being loaded.
- The index has a version (`Index::VERSION`). An index of another version is emptied and fills again.
- Units are written as they are finished. A file's `mtime` and `size` are only set once all of its units are in, so an
  interrupted run continues where it stopped.
- Files that no longer exist are removed on the next `rnote-cli index` run.

## Search

Matching lives in `Query` (`query.rs`), so the app uses the same rules to search the text the open document
carries (typed text, Pdf text) without going through the index.

`Index::search(query, scope)` goes through the lines of all files, or with a scope through those of one file or of
the files below a folder (`rnote-cli search --in FOLDER`). In the app the scope is chosen above the results:
the open document, the folder the files list shows with the folders in it, or all notes. A line matches where the characters of the query appear in a row, each
among the candidates of its position. Whitespace, letter case and zhuyin tone marks are ignored. A hit covers exactly the matched
characters. Hits on the most likely readings rank first, then by confidence.

## Copying text

The selector style "Select Text to Copy" keeps a rectangle on the page, and `Ctrl+C` copies the text inside it. The
app puts the lines together: the ones the open document carries (typed text, Pdf text) as they are right now, and
the ones `Index::lines(path)` has for the file (handwriting, images), without those the document gave already.

`select::text_in(lines, area)` cuts them. A line takes part when its vertical middle is inside the area, and gives
the characters whose horizontal middle is inside it: over `ABC` above `DEF`, a rectangle around the right two
columns gives `BC\nEF`. Lines are put top to bottom; lines on one row (two columns, or the pieces a row was
recognised in) left to right with a space between them. Of each character the most likely reading is taken.

- Handwriting and images give text once the document is saved and indexed, at the place they had when it was last
  saved.
- Text that does not run left to right in an upright box (rotated text or pages) is cut by its bounding box only.
- Across columns the text is read row by row, not column by column.

## Zhuyin (experimental)

The recognition model file has a second output with 41 extra classes for the zhuyin symbols and tone marks.
`rnote-cli index --zhuyin`, or "Read Zhuyin (experimental)" in the app's settings, reads that output; it is off by
default, and the default reads exactly what the published model reads.

With it on, horizontal zhuyin is read, handwritten and printed. The cost: a zhuyin symbol can become the first
reading of a character that looks like it (about 0.4% of the characters of ordinary text in unfamiliar fonts). The
character nearly always stays as a second reading, so it is still found. Search ignores tone marks, and a query
without zhuyin also matches across zhuyin symbols: ruby zhuyin beside printed characters comes out as such symbols
between them. Switching the
option makes files be read again when they are next indexed. Details and measurements:
[misc/zhuyin](../../misc/zhuyin/README.md).

## Memory

Measured with a debug build on a 4-core laptop, two threads used.

| What | Additional memory |
|---|---|
| The app (`rnote`) | None for recognition: it never loads the models. Searching opens the SQLite index, a few MB |
| `rnote-cli search` | 22 MB in total for the process |
| `rnote-cli index`, models loaded | 80 MB |
| `rnote-cli index`, while reading a page | about 110 MB more, at the 960 px detection size. About 200 MB in total for recognition |
| `rnote-cli index`, loading the document | Depends on the file. A small note: 275 MB peak for the whole process. A 50 MB note with a 112-page Pdf: 800 MB peak, of which about 630 MB is the loaded document |
| Size of `rnote-cli` on disk | 21 MB more, the compiled-in models |

A note folder (`.rnoted`) keeps the text that was read from its units in its `text` directory
(`rnote_ocr::textfiles`), named by the unit hash and `TEXT_VERSION`. Another device that has the note takes the text
from there instead of recognising again. The unit hash is the same on every device: a Pdf page is hashed by which
page of which Pdf it is, not by what is drawn of it.

The indexer is a separate process that the app starts for one file, or for the library folder, at a time; all of the
above is given back when it exits. The library is the folder set as "Notes Library" in the settings: it is indexed
when the app starts and again when a note in it changes on disk, so notes that were never opened are found too. A file that did not change is skipped without being loaded (20 MB, 0.04 s).

## Limits

- Text on an arc or at a steep angle is not found, line boxes are axis-aligned.
- Zhuyin (bopomofo) is not in the model's dictionary and is read as look-alike characters, unless the experimental
  zhuyin option is on (see below). Vertical zhuyin is not read either way.
- Search does not cross line breaks.
- A moved or renamed file is read again in full.
- Pdf pages imported before text was kept, and scanned Pdfs, have no text layer and are recognised like any image.

## Trying it

```bash
# Lines, character boxes and candidates of one image
cargo run -p rnote-ocr --features recognize --example recognize -- image.png

# The unit images of a document, as recognition gets to see them
cargo run -p rnote-engine --example text_units -- file.rnote out-dir/

cargo run -p rnote-cli -- index notes-folder/
cargo run -p rnote-cli -- search 注音
cargo run -p rnote-cli -- search 注音 --in notes-folder/mandarin/
```
