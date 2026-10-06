// Imports
use anyhow::Context;
use rnote_ocr::Index;
use std::path::Path;

pub(crate) fn run_search(query: &str, folder: Option<&Path>) -> anyhow::Result<()> {
    // The index identifies files by their canonical path
    let folder = folder
        .map(|folder| {
            folder
                .canonicalize()
                .with_context(|| format!("Path \"{}\" not found.", folder.display()))
        })
        .transpose()?;
    for hit in Index::open()?.search(query, folder.as_deref())? {
        println!(
            "{}: page {}: {}",
            hit.path.display(),
            hit.page + 1,
            hit.text
        );
    }
    Ok(())
}
