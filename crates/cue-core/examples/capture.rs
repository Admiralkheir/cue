//! Captures the frontmost window the way the hotkey does and saves the JPEG.
//!
//! cargo run -p cue-core --example capture -- [output.jpg] [delay-seconds]

use std::time::{Duration, Instant};
use std::{env, fs, thread};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let output = args.next().unwrap_or_else(|| "capture.jpg".into());
    let delay: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(0);
    // Time to switch to the game before the capture.
    thread::sleep(Duration::from_secs(delay));

    let started = Instant::now();
    let capture = cue_core::capture_focused_window(std::process::id(), None)?;
    let elapsed = started.elapsed();

    fs::write(&output, &capture.frame.jpeg)?;
    println!("{capture:?} in {elapsed:.0?} -> {output}");
    Ok(())
}
