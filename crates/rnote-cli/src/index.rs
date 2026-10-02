// Imports
use crate::{cli, validators};
use anyhow::Context;
use image::RgbImage;
use rnote_compose::SplitOrder;
use rnote_compose::shapes::Shapeable;
use rnote_engine::Engine;
use rnote_engine::engine::EngineSnapshot;
use rnote_engine::engine::export::{TextLayer, TextUnit};
use rnote_engine::strokes::Stroke;
use rnote_ocr::{Bounds, Candidate, CharBox, FileStamp, Index, Line, Recognizer, Source, Unit};
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
        let stamp = FileStamp::of(&rnote_file, zhuyin)?;
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
            Ok(n_read) => {
                let finish_msg = format!("Indexed \"{file_disp}\", {n_read} changed unit(s) read.");
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

/// The rnote files among the paths. Folders are searched recursively.
fn rnote_files(paths: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
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
        if path.is_dir() {
            collect(&path, &mut files)?;
        } else {
            validators::file_has_ext(&path, "rnote")?;
            files.push(path);
        }
    }
    Ok(files)
}

/// Brings the index up to date with the file. Returns how many units were read.
async fn index_file(
    index: &mut Index,
    recognizer: &mut Option<Recognizer>,
    rnote_file: &Path,
    stamp: FileStamp,
    progressbar: &indicatif::ProgressBar,
) -> anyhow::Result<usize> {
    let rnote_bytes = cli::read_bytes_from_file(rnote_file).await?;
    let engine_snapshot = EngineSnapshot::load_from_rnote_bytes(rnote_bytes).await?;
    let mut engine = Engine::default();
    let _ = engine.load_snapshot(engine_snapshot);

    let text_units = engine.extract_text_units(SplitOrder::default());
    let indexed = index.unit_hashes(rnote_file)?;
    let mut units = Vec::with_capacity(text_units.len());
    let mut hashes = HashSet::new();
    let mut n_read = 0;
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
            let lines = read_unit(recognizer, text_unit, stamp.zhuyin)?;
            index.insert_unit(rnote_file, unit, &lines)?;
            n_read += 1;
        }
        units.push(unit);
    }
    index.finish_file(rnote_file, stamp, &units)?;
    Ok(n_read)
}

/// Reads the text lines of the unit, positioned on the document.
fn read_unit(
    recognizer: &mut Option<Recognizer>,
    text_unit: &TextUnit,
    zhuyin: bool,
) -> anyhow::Result<Vec<Line>> {
    if text_unit.layer == TextLayer::Typed {
        let mut lines = Vec::new();
        for stroke in text_unit.content.strokes.iter() {
            if let Stroke::TextStroke(textstroke) = stroke.as_ref() {
                lines.extend(textstroke.lines()?.into_iter().map(|line| {
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
                            .map(|c| CharBox {
                                x0: c.bounds.mins.x,
                                x1: c.bounds.maxs.x,
                                candidates: vec![Candidate {
                                    ch: c.ch,
                                    confidence: 1.0,
                                }],
                            })
                            .collect(),
                    }
                }));
            }
        }
        return Ok(lines);
    }

    let Some(size) = text_unit.content.size() else {
        return Ok(Vec::new());
    };
    let scale = RENDER_SCALE.min(RENDER_MAX_SIDE / size.max_element());
    let Some(image) = text_unit
        .content
        .gen_image(true, false, false, 0.0, scale)?
    else {
        return Ok(Vec::new());
    };
    let origin = image.rectangle.bounds().mins;
    let recognizer = match recognizer {
        Some(recognizer) => recognizer,
        None => recognizer.insert(Recognizer::new(zhuyin)?),
    };
    Ok(recognizer
        .recognize(&flatten(image)?)?
        .into_iter()
        .map(|line| line.onto_document((origin.x, origin.y), scale))
        .collect())
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
