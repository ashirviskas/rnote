// Imports
use rnote_ocr::Index;

pub(crate) fn run_search(query: &str) -> anyhow::Result<()> {
    for hit in Index::open()?.search(query)? {
        println!(
            "{}: page {}: {}",
            hit.path.display(),
            hit.page + 1,
            hit.text
        );
    }
    Ok(())
}
