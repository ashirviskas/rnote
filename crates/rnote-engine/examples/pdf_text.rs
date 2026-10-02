//! Prints the text that would be kept with each page of a Pdf when it is imported.
//!
//! Usage: `cargo run -p rnote-engine --example pdf_text -- <file.pdf>`

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use rnote_engine::strokes::imagetext;
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .expect("Usage: pdf_text <file.pdf>");
    let pdf = Pdf::new(Arc::new(std::fs::read(path)?))
        .map_err(|e| anyhow::anyhow!("Reading the Pdf failed, Err: {e:?}"))?;
    let settings = InterpreterSettings::default();

    for (i, page) in pdf.pages().iter().enumerate() {
        let lines = imagetext::pdf_page_text(page, &settings);
        println!("page {}: {} lines", i + 1, lines.len());
        for line in lines {
            println!(
                "    [{:.3}..{:.3}, {:.3}..{:.3}] {}",
                line.spans[0][0],
                line.spans[line.spans.len() - 1][1],
                line.top,
                line.bottom,
                line.text
            );
        }
    }
    Ok(())
}
