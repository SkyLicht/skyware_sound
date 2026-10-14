// SPDX-License-Identifier: MIT OR Apache-2.0
//! Renders one asset of a Skyware Sound export to a WAV file, offline.
//!
//! ```text
//! cargo run -p sky-runtime --example render_manifest -- <manifest dir> <asset name> <seconds> <out.wav> [sample rate]
//! ```
//!
//! Loops play for the whole duration; an SFX container is triggered once.
//! The output is 32-bit float stereo at 48 kHz unless a rate is given.

use std::error::Error;
use std::process::ExitCode;

use sky_runtime::{PlayParams, Runtime};

const USAGE: &str =
    "usage: render_manifest <manifest dir> <asset name> <seconds> <out.wav> [sample rate]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !(4..=5).contains(&args.len()) {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    let (dir, name, out_path) = (&args[0], &args[1], &args[3]);
    let seconds: f64 = args[2]
        .parse()
        .map_err(|_| format!("seconds must be a number, got {:?}", args[2]))?;
    if !(seconds.is_finite() && seconds >= 0.0) {
        return Err(format!("seconds must be zero or more, got {seconds}").into());
    }
    let sample_rate: u32 = match args.get(4) {
        Some(rate) => rate
            .parse()
            .map_err(|_| format!("sample rate must be a whole number, got {rate:?}"))?,
        None => 48_000,
    };

    let mut runtime = Runtime::new(sample_rate);
    runtime.load_manifest(dir)?;
    for warning in runtime.warnings() {
        eprintln!("warning: {warning}");
    }
    runtime.play_by_name(name, PlayParams::default())?;

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(out_path, spec)?;
    const BLOCK: usize = 1024;
    let mut buffer = vec![0.0f32; BLOCK * 2];
    let mut remaining = (seconds * f64::from(sample_rate)).round() as usize;
    while remaining > 0 {
        let frames = remaining.min(BLOCK);
        let block = &mut buffer[..frames * 2];
        runtime.render(block);
        for &sample in block.iter() {
            writer.write_sample(sample)?;
        }
        remaining -= frames;
    }
    writer.finalize()?;
    println!("rendered {seconds} s of {name:?} at {sample_rate} Hz to {out_path}");
    Ok(())
}
