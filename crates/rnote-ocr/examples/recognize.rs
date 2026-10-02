//! Recognises one image and prints its lines with their character boxes.
//!
//! Usage: `cargo run -p rnote-ocr --features recognize --example recognize -- <image> [--zhuyin]`

use rnote_ocr::Recognizer;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let image_path = PathBuf::from(args.next().expect("Usage: recognize <image> [--zhuyin]"));
    let zhuyin = args.next().is_some_and(|arg| arg == "--zhuyin");

    let image = image::open(&image_path)?.into_rgb8();
    let mut recognizer = Recognizer::new(zhuyin)?;
    let start = Instant::now();
    let lines = recognizer.recognize(&image)?;
    let elapsed = start.elapsed();

    for line in lines.iter() {
        let b = line.bounds;
        println!(
            "[{:.0}, {:.0}, {:.0}x{:.0}] {}",
            b.x,
            b.y,
            b.w,
            b.h,
            line.text()
        );
        for c in line.chars.iter() {
            let candidates = c
                .candidates
                .iter()
                .map(|c| format!("{} {:.2}", c.ch, c.confidence))
                .collect::<Vec<String>>()
                .join(", ");
            println!("    {:.0}..{:.0}: {candidates}", c.x0, c.x1);
        }
    }
    println!(
        "{} lines, {}x{} px, {elapsed:.2?}",
        lines.len(),
        image.width(),
        image.height()
    );
    Ok(())
}
