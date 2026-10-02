//! Writes the text units of a rnote file as Png images, to inspect what text recognition gets to see.
//!
//! Usage: `cargo run -p rnote-engine --example text_units -- <file.rnote> <output-dir>`

use rnote_compose::SplitOrder;
use rnote_engine::Engine;
use rnote_engine::engine::EngineSnapshot;
use rnote_engine::engine::export::TextLayer;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let usage = "Usage: text_units <file.rnote> <output-dir>";
    let rnote_file = PathBuf::from(args.next().expect(usage));
    let output_dir = PathBuf::from(args.next().expect(usage));
    std::fs::create_dir_all(&output_dir)?;

    let mut engine = Engine::default();
    let bytes = std::fs::read(rnote_file)?;
    let snapshot = futures::executor::block_on(EngineSnapshot::load_from_rnote_bytes(bytes))?;
    let _ = engine.load_snapshot(snapshot);

    for (i, unit) in engine
        .extract_text_units(SplitOrder::default())
        .into_iter()
        .enumerate()
    {
        let name = format!("{i:03}_page{:03}_{:?}", unit.page, unit.layer);
        let mut carried = Vec::new();
        for stroke in unit.content.strokes.iter() {
            carried.extend(stroke.text_lines()?);
        }
        if !carried.is_empty() || unit.layer == TextLayer::Typed {
            println!("{name}: carries {} lines of text", carried.len());
            for line in carried {
                let text = line.chars.iter().map(|c| c.ch).collect::<String>();
                println!("    {:?}: {text}", line.bounds);
            }
            continue;
        }
        let start = Instant::now();
        let Some(image) = unit.content.gen_image(true, false, false, 0.0, 2.0)? else {
            continue;
        };
        println!(
            "{name}: {}x{} px, rendered in {:.2?}",
            image.pixel_width,
            image.pixel_height,
            start.elapsed()
        );
        let png = image.into_encoded_bytes(image::ImageFormat::Png, None)?;
        std::fs::write(output_dir.join(name + ".png"), png)?;
    }
    Ok(())
}
