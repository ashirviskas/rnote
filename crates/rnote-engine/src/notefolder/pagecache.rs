//! The pages of Pdfs as the canvas draws them, kept on the device.
//!
//! A note folder holds a Pdf as it was imported. What the canvas draws is the `svg_data` of each page, which
//! takes about half a second a page to make. It is made once per device and kept here. Everything in the cache
//! can be made again, so it is trimmed to a size and may be deleted at any time.

// Imports
use crate::strokes::vectorimage::PdfPageRef;
use anyhow::Context;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::SystemTime;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct PageCache {
    pub dir: PathBuf,
    /// The size in bytes the cache is trimmed to.
    pub limit: u64,
}

impl Default for PageCache {
    fn default() -> Self {
        Self {
            dir: glib::user_cache_dir().join("rnote").join("pages"),
            limit: Self::LIMIT_DEFAULT,
        }
    }
}

impl PageCache {
    pub const LIMIT_DEFAULT: u64 = 1024 * 1024 * 1024;

    fn file(&self, page: &PdfPageRef) -> PathBuf {
        let name = format!("{}-{}.svg.gz", page.file.stem(), page.page);
        self.dir.join(name)
    }

    pub(crate) fn contains(&self, page: &PdfPageRef) -> bool {
        self.file(page).is_file()
    }

    /// The `svg_data` of the page, when it is in the cache.
    pub(crate) fn get(&self, page: &PdfPageRef) -> Option<String> {
        let file = std::fs::File::open(self.file(page)).ok()?;
        let mut svg_data = String::new();
        flate2::read::GzDecoder::new(&file)
            .read_to_string(&mut svg_data)
            .ok()?;
        // What is trimmed first is what was not used for the longest time
        if let Err(e) = file.set_modified(SystemTime::now()) {
            warn!("Marking a cached page as used failed, Err: {e:?}");
        }
        Some(svg_data)
    }

    pub(crate) fn put(&self, page: &PdfPageRef, svg_data: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("Creating the page cache {:?} failed.", self.dir))?;
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::<u8>::new(), flate2::Compression::fast());
        encoder.write_all(svg_data.as_bytes())?;
        crate::utils::atomic_save_to_file(self.file(page), encoder.finish()?)
    }

    /// Deletes the pages that were not used for the longest time, until the cache is within its limit.
    pub(crate) fn trim(&self) -> anyhow::Result<()> {
        struct Cached {
            file: PathBuf,
            size: u64,
            used: SystemTime,
        }

        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            // Nothing was cached yet
            return Ok(());
        };
        let mut cached = Vec::new();
        for entry in entries {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                cached.push(Cached {
                    file: entry.path(),
                    size: metadata.len(),
                    used: metadata.modified()?,
                });
            }
        }
        cached.sort_by_key(|cached| cached.used);

        let mut size = cached.iter().map(|cached| cached.size).sum::<u64>();
        for cached in cached {
            if size <= self.limit {
                break;
            }
            std::fs::remove_file(&cached.file)?;
            size -= cached.size;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notefolder::FileName;

    fn page(page: u32) -> PdfPageRef {
        PdfPageRef {
            file: FileName::of(b"a pdf", "pdf"),
            page,
        }
    }

    #[test]
    fn the_pages_that_were_not_used_for_the_longest_time_go_first() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = PageCache {
            dir: tmp.path().join("pages"),
            limit: u64::MAX,
        };
        // Nothing to trim, and nothing to find
        cache.trim().unwrap();
        assert_eq!(cache.get(&page(0)), None);

        for i in 0..3 {
            cache.put(&page(i), &format!("<svg>{i}</svg>")).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        cache.trim().unwrap();
        assert!((0..3).all(|i| cache.contains(&page(i))));

        // Room for two pages. The first one was used last.
        assert_eq!(cache.get(&page(0)).as_deref(), Some("<svg>0</svg>"));
        let size = std::fs::metadata(cache.file(&page(0))).unwrap().len();
        cache.limit = 2 * size;
        cache.trim().unwrap();
        assert!(cache.contains(&page(0)) && !cache.contains(&page(1)) && cache.contains(&page(2)));

        cache.limit = 0;
        cache.trim().unwrap();
        assert!((0..3).all(|i| !cache.contains(&page(i))));
    }
}
