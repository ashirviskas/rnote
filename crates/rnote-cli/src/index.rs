// Imports
use crate::{cli, validators};
use anyhow::Context;
use image::RgbImage;
use rnote_compose::SplitOrder;
use rnote_compose::shapes::Shapeable;
use rnote_engine::Engine;
use rnote_engine::engine::export::{TextLayer, TextUnit};
use rnote_engine::notefolder::NoteFolder;
use rnote_ocr::{Bounds, CharBox, FileStamp, Index, Line, Recognizer, Source, Unit, textfiles};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The image scale-factor the units are rendered with. Text is read worse from smaller images.
const RENDER_SCALE: f64 = 2.0;
/// Units that would render larger than this are rendered with a smaller scale-factor, to bound the memory.
const RENDER_MAX_SIDE: f64 = 4096.0;

pub(crate) async fn run_index(paths: &[PathBuf], zhuyin: bool) -> anyhow::Result<()> {
    let mut index = Index::open()?;
    let removed = index.remove_missing()?;
    if removed > 0 {
        println!("Removed {removed} file(s) that no longer exist from the index.");
    }

    // The models are only loaded once a unit needs to be recognised
    let mut recognizer = None;
    let mut failed = 0;
    for rnote_file in rnote_files(paths)? {
        let file_disp = rnote_file.display().to_string();
        let stamp = FileStamp::of(&stamp_path(&rnote_file), zhuyin)?;
        if index.is_current(&rnote_file, stamp)? {
            continue;
        }
        let progressbar = cli::new_progressbar(format!("Indexing \"{file_disp}\""));

        match index_file(
            &mut index,
            &mut recognizer,
            &rnote_file,
            stamp,
            &progressbar,
        )
        .await
        {
            Ok(done) => {
                let finish_msg = format!(
                    "Indexed \"{file_disp}\", {} changed unit(s) read, {} taken from the note.",
                    done.recognized, done.from_note
                );
                if progressbar.is_hidden() {
                    println!("{finish_msg}");
                }
                progressbar.finish_with_message(finish_msg);
            }
            Err(e) => {
                let abandon_msg = format!("Indexing \"{file_disp}\" failed, Err: {e:?}");
                if progressbar.is_hidden() {
                    println!("{abandon_msg}");
                }
                progressbar.abandon_with_message(abandon_msg);
                failed += 1;
            }
        }
    }

    if failed > 0 {
        return Err(anyhow::anyhow!("Indexing failed for {failed} file(s)."));
    }
    Ok(())
}

/// The file whose state tells whether a note changed: the rnote file itself, or of a note folder the directory of
/// its batches, which changes with every batch that is written or removed.
fn stamp_path(note: &Path) -> PathBuf {
    if note.is_dir() {
        note.join("ink")
    } else {
        note.to_path_buf()
    }
}

/// The notes among the paths: rnote files and note folders. Folders are searched recursively.
fn rnote_files(paths: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if NoteFolder::folder_of(&path).is_some() {
                files.push(path);
            } else if path.is_dir() {
                collect(&path, files)?;
            } else if path.extension().is_some_and(|ext| ext == "rnote") {
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    for path in paths {
        // The index identifies files by their canonical path
        let path = path
            .canonicalize()
            .with_context(|| format!("Path \"{}\" not found.", path.display()))?;
        if let Some(note_folder) = NoteFolder::folder_of(&path) {
            files.push(note_folder);
        } else if path.is_dir() {
            collect(&path, &mut files)?;
        } else {
            validators::file_has_ext(&path, "rnote")?;
            files.push(path);
        }
    }
    Ok(files)
}

/// What indexing a note did.
#[derive(Debug, Clone, Copy, Default)]
struct Indexed {
    /// How many units were recognised.
    recognized: usize,
    /// How many units got their text from the text files of the note folder, read by another device.
    from_note: usize,
}

/// Brings the index up to date with the file.
async fn index_file(
    index: &mut Index,
    recognizer: &mut Option<Recognizer>,
    rnote_file: &Path,
    stamp: FileStamp,
    progressbar: &indicatif::ProgressBar,
) -> anyhow::Result<Indexed> {
    let engine_snapshot = cli::load_note(rnote_file).await?;
    let mut engine = Engine::default();
    let _ = engine.load_snapshot(engine_snapshot);
    // A note folder keeps the text that was read from it, for the other devices that have the note
    let text_dir = rnote_file
        .is_dir()
        .then(|| rnote_file.join(NoteFolder::TEXT_DIR_NAME));

    let text_units = engine.extract_text_units(SplitOrder::default());
    let indexed = index.unit_hashes(rnote_file)?;
    let mut units = Vec::with_capacity(text_units.len());
    let mut hashes = HashSet::new();
    let mut done = Indexed::default();
    for (i, text_unit) in text_units.iter().enumerate() {
        let unit = Unit {
            // What is read from a unit depends on whether zhuyin is read
            hash: text_unit.content_hash()?.wrapping_add(stamp.zhuyin as u64),
            page: text_unit.page as u32,
            source: match text_unit.layer {
                TextLayer::Ink => Source::Ink,
                TextLayer::Image => Source::Image,
                TextLayer::Typed => Source::Typed,
            },
        };
        // Identical content at the same position is the same unit
        if !hashes.insert(unit.hash) {
            continue;
        }
        if !indexed.contains(&unit.hash) {
            progressbar.set_message(format!(
                "Indexing \"{}\", unit {} of {}",
                rnote_file.display(),
                i + 1,
                text_units.len()
            ));
            let kept = text_dir
                .as_ref()
                .and_then(|dir| textfiles::read(dir, unit.hash));
            let lines = match kept {
                Some(lines) => {
                    done.from_note += 1;
                    lines
                }
                None => {
                    let read = read_unit(recognizer, text_unit, stamp.zhuyin)?;
                    if read.recognized {
                        done.recognized += 1;
                        if let Some(dir) = &text_dir {
                            textfiles::write(dir, unit.hash, &read.lines)?;
                        }
                    }
                    read.lines
                }
            };
            index.insert_unit(rnote_file, unit, &lines)?;
        }
        units.push(unit);
    }
    index.finish_file(rnote_file, stamp, &units)?;
    if let Some(dir) = &text_dir {
        // The text of units the note no longer has. Both readings of a unit are kept, with and without zhuyin.
        let kept = hashes
            .iter()
            .flat_map(|hash| [hash.wrapping_sub(1), *hash, hash.wrapping_add(1)])
            .collect::<HashSet<u64>>();
        textfiles::retain(dir, &kept)?;
    }
    Ok(done)
}

/// The text of a unit, and whether it had to be recognised.
#[derive(Debug, Clone)]
struct UnitText {
    lines: Vec<Line>,
    recognized: bool,
}

/// Reads the text lines of the unit, positioned on the document.
fn read_unit(
    recognizer: &mut Option<Recognizer>,
    text_unit: &TextUnit,
    zhuyin: bool,
) -> anyhow::Result<UnitText> {
    let carried = |lines| UnitText {
        lines,
        recognized: false,
    };
    // Text that a stroke carries needs no recognition: typed text, and the text layer of an imported Pdf page
    let mut lines = Vec::new();
    for stroke in text_unit.content.strokes.iter() {
        lines.extend(stroke.text_lines()?.into_iter().map(|line| {
            let extents = line.bounds.extents();
            Line {
                bounds: Bounds {
                    x: line.bounds.mins.x,
                    y: line.bounds.mins.y,
                    w: extents.x,
                    h: extents.y,
                },
                chars: line
                    .chars
                    .into_iter()
                    .map(|c| CharBox::exact(c.ch, c.bounds.mins.x, c.bounds.maxs.x))
                    .collect(),
            }
        }));
    }
    if !lines.is_empty() || text_unit.layer == TextLayer::Typed {
        return Ok(carried(lines));
    }

    let Some(size) = text_unit.content.size() else {
        return Ok(carried(Vec::new()));
    };
    let scale = RENDER_SCALE.min(RENDER_MAX_SIDE / size.max_element());
    let Some(image) = text_unit
        .content
        .gen_image(true, false, false, 0.0, scale)?
    else {
        return Ok(carried(Vec::new()));
    };
    let origin = image.rectangle.bounds().mins;
    let recognizer = match recognizer {
        Some(recognizer) => recognizer,
        None => recognizer.insert(Recognizer::new(zhuyin)?),
    };
    let lines = recognizer
        .recognize(&flatten(image)?)?
        .into_iter()
        .map(|line| line.onto_document((origin.x, origin.y), scale))
        .collect();
    Ok(UnitText {
        lines,
        recognized: true,
    })
}

/// Lays the image over white. The recogniser needs an opaque image.
fn flatten(image: rnote_engine::Image) -> anyhow::Result<RgbImage> {
    // The colors are premultiplied with the alpha
    let rgb = image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| {
            let white = 255 - pixel[3];
            [
                pixel[0].saturating_add(white),
                pixel[1].saturating_add(white),
                pixel[2].saturating_add(white),
            ]
        })
        .collect::<Vec<u8>>();
    RgbImage::from_raw(image.pixel_width, image.pixel_height, rgb)
        .context("The rendered image has an invalid size.")
}
