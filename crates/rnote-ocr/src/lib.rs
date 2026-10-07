//! rnote-ocr
//!
//! Text recognition for rnote documents and the index that makes the recognised text searchable.
//!
//! The index is always available. The recogniser loads the model runtime and sits behind the `recognize` feature,
//! which only the cli enables.

// Modules
pub mod index;
pub mod query;
#[cfg(feature = "recognize")]
pub mod recognize;
pub mod select;
pub mod textfiles;

// Re-exports
pub use index::{FileStamp, Index};
pub use query::{Match, Query};
#[cfg(feature = "recognize")]
pub use recognize::Recognizer;

// Imports
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The directory that holds the index.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set.")?)
            .join(".local/share"),
    };
    Ok(base.join("rnote").join("ocr"))
}

/// An axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What a line of text was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Handwriting and shapes.
    Ink = 0,
    /// Imported images and Pdf pages.
    Image = 1,
    /// Text typed with the typewriter, which needs no recognition.
    Typed = 2,
}

/// One reading of a character.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub ch: char,
    pub confidence: f32,
}

/// A character of a line: its horizontal extent and its possible readings, the most likely first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharBox {
    pub x0: f64,
    pub x1: f64,
    pub candidates: Vec<Candidate>,
}

impl CharBox {
    /// A character that is known, not recognised.
    pub fn exact(ch: char, x0: f64, x1: f64) -> Self {
        Self {
            x0,
            x1,
            candidates: vec![Candidate {
                ch,
                confidence: 1.0,
            }],
        }
    }

    /// The most likely reading.
    pub fn top(&self) -> Option<char> {
        self.candidates.first().map(|c| c.ch)
    }
}

/// A line of text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub bounds: Bounds,
    pub chars: Vec<CharBox>,
}

impl Line {
    /// The most likely reading of the line.
    pub fn text(&self) -> String {
        self.chars.iter().filter_map(CharBox::top).collect()
    }

    /// Moves the line from the pixels of an image onto the document.
    ///
    /// `origin` is the document position of the image's top-left pixel, `scale` the pixels per document unit.
    pub fn onto_document(mut self, origin: (f64, f64), scale: f64) -> Self {
        self.bounds = Bounds {
            x: origin.0 + self.bounds.x / scale,
            y: origin.1 + self.bounds.y / scale,
            w: self.bounds.w / scale,
            h: self.bounds.h / scale,
        };
        for c in self.chars.iter_mut() {
            c.x0 = origin.0 + c.x0 / scale;
            c.x1 = origin.0 + c.x1 / scale;
        }
        self
    }
}

/// A piece of a document that is recognised on its own: the ink of one page, one image or one typed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    /// Identifies the content. A unit whose hash is indexed already does not need to be recognised again.
    pub hash: u64,
    /// The page the unit is on.
    pub page: u32,
    pub source: Source,
}

/// A search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    pub page: u32,
    /// The bounds of the matched characters on the document.
    pub bounds: Bounds,
    /// The most likely reading of the line containing the match.
    pub text: String,
    /// Higher is better. Matches on the most likely readings rank above matches on other candidates.
    pub score: f32,
}
