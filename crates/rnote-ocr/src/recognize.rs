//! Reads text from an image with the PP-OCRv5 mobile models: one finds the text lines, the other reads each line.

// Imports
use crate::{Bounds, Candidate, CharBox, Line};
use anyhow::Context;
use image::RgbImage;
use image::imageops::{self, FilterType};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

/// Finds and reads the text lines of an image.
#[derive(Debug)]
pub struct Recognizer {
    det: Session,
    rec: Session,
    /// The character of each recognition class. Class 0 is the blank.
    classes: Vec<char>,
}

impl Recognizer {
    pub const DET_MODEL: &'static str = "pp-ocrv5_mobile_det.onnx";
    pub const REC_MODEL: &'static str = "pp-ocrv5_mobile_rec.onnx";
    pub const DICT: &'static str = "ppocrv5_dict.txt";

    /// Larger images are scaled down for detection. The peak memory grows with the pixels detection runs on:
    /// about 110 MB at this size, 280 MB at 1600.
    const DET_MAX_SIDE: u32 = 960;
    /// Pixels of the detection output above this value belong to text.
    const DET_PIXEL_THRESHOLD: f32 = 0.3;
    /// Regions with a lower mean value are not text.
    const DET_REGION_THRESHOLD: f32 = 0.6;
    /// The detected regions are shrunk text lines. This grows them back.
    const DET_UNCLIP_RATIO: f64 = 1.5;
    /// The height the recognition model expects.
    const REC_HEIGHT: u32 = 48;
    /// How many readings are kept per character.
    const MAX_CANDIDATES: usize = 5;
    /// Readings other than the most likely one are dropped below this confidence.
    const MIN_CANDIDATE_CONFIDENCE: f32 = 0.01;

    /// Loads the models from the given directory.
    pub fn new(model_dir: &Path) -> anyhow::Result<Self> {
        for name in [Self::DET_MODEL, Self::REC_MODEL, Self::DICT] {
            if !model_dir.join(name).is_file() {
                anyhow::bail!(
                    "Recognition model file \"{}\" is missing. Run `just ocr-models` to download the models.",
                    model_dir.join(name).display()
                );
            }
        }
        let dict = std::fs::read_to_string(model_dir.join(Self::DICT))
            .context("Reading the dictionary failed.")?;
        // Blank, then one class per dictionary line, then the space.
        let classes = std::iter::once('\0')
            .chain(dict.lines().filter_map(|l| l.chars().next()))
            .chain(std::iter::once(' '))
            .collect();

        Ok(Self {
            det: load_session(&model_dir.join(Self::DET_MODEL))
                .context("Loading the detection model failed.")?,
            rec: load_session(&model_dir.join(Self::REC_MODEL))
                .context("Loading the recognition model failed.")?,
            classes,
        })
    }

    /// Reads the text lines of the image, ordered top to bottom. All positions are in pixels of the image.
    pub fn recognize(&mut self, image: &RgbImage) -> anyhow::Result<Vec<Line>> {
        let mut lines = Vec::new();
        for bounds in self.detect(image)? {
            let line = self.read_line(image, bounds)?;
            if !line.chars.is_empty() {
                lines.push(line);
            }
        }
        Ok(lines)
    }

    /// Finds the bounds of the text lines.
    fn detect(&mut self, image: &RgbImage) -> anyhow::Result<Vec<Bounds>> {
        let (width, height) = image.dimensions();
        let shrink = (Self::DET_MAX_SIDE as f64 / width.max(height) as f64).min(1.0);
        // The model needs sides that are multiples of 32
        let side = |s: u32| ((s as f64 * shrink / 32.0).round() as u32).max(1) * 32;
        let (det_width, det_height) = (side(width), side(height));
        let resized = imageops::resize(image, det_width, det_height, FilterType::Triangle);

        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let input = bgr_planes(&resized, |value, channel| {
            (value / 255.0 - MEAN[channel]) / STD[channel]
        })?;
        let outputs = self.det.run(ort::inputs![input])?;
        let (_, text_map) = outputs[0].try_extract_tensor::<f32>()?;

        let (scale_x, scale_y) = (
            width as f64 / det_width as f64,
            height as f64 / det_height as f64,
        );
        let mut lines = text_regions(text_map, det_width as usize, det_height as usize)
            .into_iter()
            .filter(|r| r.mean >= Self::DET_REGION_THRESHOLD && r.w >= 3.0 && r.h >= 3.0)
            .map(|r| {
                let grow = r.w * r.h * Self::DET_UNCLIP_RATIO / (2.0 * (r.w + r.h));
                let x0 = ((r.x - grow) * scale_x).max(0.0);
                let y0 = ((r.y - grow) * scale_y).max(0.0);
                let x1 = ((r.x + r.w + grow) * scale_x).min(width as f64);
                let y1 = ((r.y + r.h + grow) * scale_y).min(height as f64);
                Bounds {
                    x: x0,
                    y: y0,
                    w: x1 - x0,
                    h: y1 - y0,
                }
            })
            .collect::<Vec<Bounds>>();
        lines.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
        Ok(lines)
    }

    /// Reads the text line inside the bounds.
    fn read_line(&mut self, image: &RgbImage, bounds: Bounds) -> anyhow::Result<Line> {
        let crop = imageops::crop_imm(
            image,
            bounds.x as u32,
            bounds.y as u32,
            (bounds.w as u32).max(1),
            (bounds.h as u32).max(1),
        )
        .to_image();
        let rec_width = (Self::REC_HEIGHT as f64 * bounds.w / bounds.h)
            .round()
            .max(1.0) as u32;
        let resized = imageops::resize(&crop, rec_width, Self::REC_HEIGHT, FilterType::Triangle);

        let input = bgr_planes(&resized, |value, _| (value / 255.0 - 0.5) / 0.5)?;
        let outputs = self.rec.run(ort::inputs![input])?;
        let (shape, probabilities) = outputs[0].try_extract_tensor::<f32>()?;
        let n_classes = shape[2] as usize;
        if n_classes != self.classes.len() {
            anyhow::bail!(
                "The recognition model has {n_classes} classes, but the dictionary gives {}.",
                self.classes.len()
            );
        }

        // The model emits one frame per horizontal step. A character is the first frame of a run of the same class.
        let n_frames = shape[1] as usize;
        let mut centers = Vec::new();
        let mut readings = Vec::new();
        let mut previous = 0;
        for (i, frame) in probabilities.chunks_exact(n_classes).enumerate() {
            let class = (0..n_classes)
                .max_by(|&a, &b| frame[a].total_cmp(&frame[b]))
                .unwrap_or(0);
            if class != 0 && class != previous {
                centers.push(bounds.x + (i as f64 + 0.5) / n_frames as f64 * bounds.w);
                readings.push(candidates(frame, &self.classes));
            }
            previous = class;
        }

        // A character reaches halfway to its neighbours
        let chars = readings
            .into_iter()
            .enumerate()
            .map(|(i, candidates)| {
                let left = i.checked_sub(1).map(|j| (centers[i] - centers[j]) * 0.5);
                let right = centers.get(i + 1).map(|next| (next - centers[i]) * 0.5);
                let reach = |side: Option<f64>| side.or(left).or(right).unwrap_or(bounds.w);
                CharBox {
                    x0: (centers[i] - reach(left)).max(bounds.x),
                    x1: (centers[i] + reach(right)).min(bounds.x + bounds.w),
                    candidates,
                }
            })
            .collect();

        Ok(Line { bounds, chars })
    }
}

/// The most likely readings of a recognition frame.
fn candidates(frame: &[f32], classes: &[char]) -> Vec<Candidate> {
    const MAX: usize = Recognizer::MAX_CANDIDATES;
    let mut best: Vec<(usize, f32)> = Vec::with_capacity(MAX + 1);
    // Skips the blank
    for (class, &confidence) in frame.iter().enumerate().skip(1) {
        if best.len() == MAX && confidence <= best[MAX - 1].1 {
            continue;
        }
        let position = best.partition_point(|&(_, c)| c >= confidence);
        best.insert(position, (class, confidence));
        best.truncate(MAX);
    }
    best.into_iter()
        .enumerate()
        .filter(|&(i, (_, confidence))| {
            i == 0 || confidence >= Recognizer::MIN_CANDIDATE_CONFIDENCE
        })
        .map(|(_, (class, confidence))| Candidate {
            ch: classes[class],
            confidence,
        })
        .collect()
}

/// Loads a model. The memory arena and the memory pattern are off, so that memory is not held on to between images
/// of different sizes.
fn load_session(model: &Path) -> ort::Result<Session> {
    Session::builder()?
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])?
        .with_memory_pattern(false)?
        .with_intra_threads(2)?
        .with_inter_threads(1)?
        .commit_from_file(model)
}

/// Converts an image into the model input: one plane per channel in blue, green, red order.
fn bgr_planes(image: &RgbImage, normalize: impl Fn(f32, usize) -> f32) -> ort::Result<Tensor<f32>> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let plane = width * height;
    let mut data = vec![0.0; 3 * plane];
    for (i, pixel) in image.pixels().enumerate() {
        for channel in 0..3 {
            data[channel * plane + i] = normalize(pixel[2 - channel] as f32, channel);
        }
    }
    Tensor::from_array(([1, 3, height, width], data))
}

/// A connected region of text pixels in the detection output.
struct TextRegion {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    /// The mean detection value of the region's pixels.
    mean: f32,
}

/// Collects the connected regions of text pixels.
fn text_regions(text_map: &[f32], width: usize, height: usize) -> Vec<TextRegion> {
    let is_text = |i: usize| text_map[i] > Recognizer::DET_PIXEL_THRESHOLD;
    let mut visited = vec![false; width * height];
    let mut pending = Vec::new();
    let mut regions = Vec::new();

    for start in 0..width * height {
        if visited[start] || !is_text(start) {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        let (mut sum, mut count) = (0.0, 0);
        visited[start] = true;
        pending.push(start);
        while let Some(i) = pending.pop() {
            let (x, y) = (i % width, i / width);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            sum += text_map[i];
            count += 1;
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < width).then(|| i + 1),
                (y > 0).then(|| i - width),
                (y + 1 < height).then(|| i + width),
            ];
            for n in neighbours.into_iter().flatten() {
                if !visited[n] && is_text(n) {
                    visited[n] = true;
                    pending.push(n);
                }
            }
        }
        regions.push(TextRegion {
            x: x0 as f64,
            y: y0 as f64,
            w: (x1 - x0 + 1) as f64,
            h: (y1 - y0 + 1) as f64,
            mean: sum / count as f32,
        });
    }
    regions
}
