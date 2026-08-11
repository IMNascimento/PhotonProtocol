//! `photon decode`: a recording in, the file back out, with the numbers.
//!
//! The numbers are the point. Anyone can report "it worked"; what phase 2 needs
//! is how many frames were located, how many survived, how many were duplicates
//! of frames already seen, and how fast the file actually moved — because those
//! are what say whether a profile is right, whether the parity is sized
//! correctly, and where the next improvement should go.

use std::path::Path;

use photon_core::ProfileId;
use photon_core::session::{FrameOutcome, FrameReading, Receiver};
use rayon::prelude::*;

use crate::media;

/// Tally of what a recording contained.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    seen: usize,
    not_located: usize,
    header_unreadable: usize,
    payload_lost: usize,
    wrong_session: usize,
    straddled: usize,
    duplicate: usize,
    decoded: usize,
    new_symbols: usize,
    doubtful_cells: usize,
    total_cells: usize,
    pixels_per_cell: f64,
    located: usize,
}

impl Tally {
    /// Mean camera pixels per cell across the frames that were located.
    ///
    /// The measurement `SPEC.md` Q1 asks for, and the one that explains most
    /// failed captures on its own.
    fn pixels_per_cell(&self) -> f64 {
        if self.located == 0 { 0.0 } else { self.pixels_per_cell / self.located as f64 }
    }

    fn doubtful_rate(&self) -> f64 {
        if self.total_cells == 0 {
            0.0
        } else {
            self.doubtful_cells as f64 / self.total_cells as f64
        }
    }
}

/// Runs the decoder.
///
/// # Errors
///
/// Returns a message suitable for printing. A failed decode is still a
/// successful *run*: the report explains which stage gave up and how close it
/// came, as `SPEC.md` §9.2 requires.
pub(crate) fn run(
    input: &Path,
    output: &Path,
    profile: Option<ProfileId>,
    max_frames: Option<usize>,
    debug_dir: Option<&Path>,
) -> Result<(), String> {
    let scratch = std::env::temp_dir().join(format!("photon-decode-{}", std::process::id()));
    let (frames, timing) = gather_frames(input, &scratch, max_frames)?;

    println!("Frames          {}", frames.len());
    if let Some(info) = timing {
        println!("Recording       {:.2} s at {:.2} fps", info.duration, info.fps);
    }
    match profile {
        Some(id) => println!("Profile         {}", id.profile().name),
        None => println!("Profile         detect from the frames"),
    }
    println!();

    let receiver = profile.map_or_else(Receiver::new, Receiver::for_profile);

    if let Some(directory) = debug_dir {
        write_rectified(&frames, directory)?;
    }

    // Reading a frame is the expensive half and depends on nothing else, so it
    // runs across every core. Folding the readings in has to happen in order,
    // and is cheap.
    let readings: Vec<FrameReading> = frames
        .par_iter()
        .map(|path| match media::read_image(path) {
            Ok(image) => receiver.examine(&image),
            Err(_) => FrameReading {
                outcome: FrameOutcome::NotLocated,
                header: None,
                units: Vec::new(),
                units_rejected: 0,
                doubtful_cells: 0,
                total_cells: 0,
                pixels_per_cell: None,
            },
        })
        .collect();

    let mut receiver = receiver;
    let mut tally = Tally::default();
    let mut frames_to_completion = None;

    for (index, reading) in readings.into_iter().enumerate() {
        let report = receiver.absorb(reading);
        tally.seen += 1;
        tally.doubtful_cells += report.doubtful_cells;
        tally.total_cells += report.total_cells;
        tally.new_symbols += report.new_symbols;
        if let Some(per_cell) = report.pixels_per_cell {
            tally.pixels_per_cell += per_cell;
            tally.located += 1;
        }

        match report.outcome {
            FrameOutcome::NotLocated => tally.not_located += 1,
            FrameOutcome::Straddled => tally.straddled += 1,
            FrameOutcome::HeaderUnreadable => tally.header_unreadable += 1,
            FrameOutcome::PayloadUnrecoverable => tally.payload_lost += 1,
            FrameOutcome::WrongSession => tally.wrong_session += 1,
            FrameOutcome::Duplicate => tally.duplicate += 1,
            FrameOutcome::Decoded => tally.decoded += 1,
        }

        if frames_to_completion.is_none() && receiver.is_complete() {
            frames_to_completion = Some(index + 1);
        }
    }

    let result = receiver.finish();
    if let Some(id) = receiver.profile() {
        println!("Read as         {}", id.profile().name);
    }
    print_frame_report(&tally);
    print_progress(&receiver);

    let _ = std::fs::remove_dir_all(&scratch);

    match result {
        Ok(file) => {
            let target = output.join(&file.name);
            std::fs::create_dir_all(output)
                .map_err(|e| format!("cannot create {}: {e}", output.display()))?;
            std::fs::write(&target, &file.bytes)
                .map_err(|e| format!("cannot write {}: {e}", target.display()))?;

            println!();
            println!("Recovered       {} ({} bytes)", file.name, file.bytes.len());
            println!("Digest          verified");
            println!("Written to      {}", target.display());
            print_throughput(file.bytes.len(), frames_to_completion, timing, frames.len());
            Ok(())
        }
        Err(error) => {
            println!();
            println!("Failed          {}", error.code());
            println!("                {error}");
            if error.is_recoverable_by_more_capture() {
                println!();
                print_advice(&tally);
            }
            Err(format!("decode failed: {error}"))
        }
    }
}

/// Says which of the failures this was, and what changes it.
///
/// The counters make the difference obvious and a person reading them should
/// not have to work it out. Each branch has a different fix and three of the
/// four are not "film for longer", which is what a generic message would have
/// sent everyone off to do.
fn print_advice(tally: &Tally) {
    if tally.straddled > tally.decoded {
        println!(
            "{} frames caught two codes at once. The sending screen is changing",
            tally.straddled
        );
        println!("faster than this camera can capture a whole one. Hold each code for");
        println!("longer on the sending device and film it again.");
        return;
    }

    if tally.payload_lost > tally.decoded {
        let doubtful = tally.doubtful_rate() * 100.0;
        println!(
            "{} frames were found and their headers read, but their cells did not",
            tally.payload_lost
        );
        println!("decode. {doubtful:.2}% of cells were read uncertainly.");
        if tally.doubtful_rate() < 0.02 {
            println!("That is low, which means the cells are being read confidently and");
            println!("wrongly rather than uncertainly — a systematic fault rather than a");
            println!("marginal capture. Keep one of these frames; it is worth looking at.");
        } else {
            println!("Steady the camera, light the screen evenly, and let it focus.");
        }
        return;
    }

    if tally.header_unreadable > tally.decoded {
        println!(
            "{} frames were found but their headers would not read. The header is",
            tally.header_unreadable
        );
        println!("the sturdiest part of a frame — solid black and white cells — so this");
        println!("usually means the code is being scaled or clipped on the sending screen.");
        return;
    }

    if tally.located > 0 && tally.pixels_per_cell() < 5.0 {
        println!(
            "At {:.1} pixels per cell the screen was too small in the shot for the",
            tally.pixels_per_cell()
        );
        println!("cell alphabet to be separable. Get closer before anything else.");
        return;
    }

    println!("This is the kind of failure more filming fixes. Fill more of the");
    println!("frame with the screen, hold steadier, and keep recording longer.");
}

/// Writes each frame as the decoder sees it: located, de-warped, one cell at a
/// fixed size.
///
/// Counters describe a failure; this shows it. A transform that is off by a
/// fraction of a cell, or by a whole one, or that has locked onto something
/// that is not the code, all produce numbers that look broadly similar and
/// pictures that do not.
fn write_rectified(frames: &[std::path::PathBuf], directory: &Path) -> Result<(), String> {
    use photon_core::FrameLayout;
    use photon_core::detect::Detector;

    std::fs::create_dir_all(directory)
        .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;

    let detector = Detector::new();
    let mut written = 0usize;

    for (index, path) in frames.iter().enumerate().take(24) {
        let Ok(image) = media::read_image(path) else { continue };
        let Ok(detection) = detector.detect(&image) else { continue };

        let layout = FrameLayout::new(detection.profile.profile());
        let rectified = layout.rectify(&image, &detection.transform, 8);
        let target = directory.join(format!("rectified-{index:03}.png"));
        media::write_png(&target, &rectified).map_err(|e| e.to_string())?;
        written += 1;
    }

    println!("Debug           {written} rectified frames in {}", directory.display());
    println!("                Each should look like the code that was sent. If it does");
    println!("                not, the fault is in locating the frame, not in reading it.");
    println!();
    Ok(())
}

/// Collects the frames to decode, extracting them if the input is a video.
fn gather_frames(
    input: &Path,
    scratch: &Path,
    max_frames: Option<usize>,
) -> Result<(Vec<std::path::PathBuf>, Option<media::VideoInfo>), String> {
    if input.is_dir() {
        let mut frames = media::frame_paths(input).map_err(|e| e.to_string())?;
        if let Some(limit) = max_frames {
            frames.truncate(limit);
        }
        if frames.is_empty() {
            return Err(format!("{} contains no image files", input.display()));
        }
        return Ok((frames, None));
    }

    // A single picture is a perfectly good input, and the most useful one when
    // something is going wrong: it is what the receiving page hands over for
    // diagnosis. Sending it to ffprobe because it is not a directory would fail
    // for a reason that has nothing to do with the picture.
    if media::looks_like_an_image(input) {
        return Ok((vec![input.to_path_buf()], None));
    }

    let info = media::probe(input).map_err(|e| e.to_string())?;
    let frames = media::extract_frames(input, scratch, max_frames).map_err(|e| e.to_string())?;
    Ok((frames, Some(info)))
}

fn print_frame_report(tally: &Tally) {
    println!("{:<16}{:>8}", "OUTCOME", "FRAMES");
    println!("{:<16}{:>8}", "decoded", tally.decoded);
    println!("{:<16}{:>8}", "duplicate", tally.duplicate);
    println!("{:<16}{:>8}", "not located", tally.not_located);
    println!("{:<16}{:>8}", "header unreadable", tally.header_unreadable);
    println!("{:<16}{:>8}", "cells unreadable", tally.payload_lost);
    println!("{:<16}{:>8}", "two at once", tally.straddled);
    println!("{:<16}{:>8}", "other session", tally.wrong_session);
    println!();
    if tally.located > 0 {
        println!("Pixels per cell {:.2}", tally.pixels_per_cell());
    }
    println!("Doubtful cells  {:.3}%", tally.doubtful_rate() * 100.0);
    println!("New symbols     {}", tally.new_symbols);
}

fn print_progress(receiver: &Receiver) {
    if let Some((accepted, needed)) = receiver.progress() {
        println!("Symbols         {accepted} of about {needed}");
    }
    if let Some(manifest) = receiver.manifest() {
        println!("Declared        {} ({} bytes)", manifest.name, manifest.original_size);
    }
}

/// Reports how fast the file actually moved.
///
/// Two figures, because they answer different questions. The wall figure is what
/// a user experienced, including the seconds spent pointing the camera. The
/// effective figure divides by the frames it actually took, and is the one that
/// says how fast the protocol is.
fn print_throughput(
    bytes: usize,
    frames_to_completion: Option<usize>,
    timing: Option<media::VideoInfo>,
    total_frames: usize,
) {
    let Some(info) = timing else { return };
    if info.duration <= 0.0 || info.fps <= 0.0 {
        return;
    }

    println!();
    let wall = bytes as f64 / info.duration;
    println!("Throughput      {:.1} KB/s over the whole recording", wall / 1024.0);

    if let Some(frames) = frames_to_completion {
        let seconds = frames as f64 / info.fps;
        if seconds > 0.0 {
            println!(
                "                {:.1} KB/s over the {frames} frames it needed ({:.2} s)",
                bytes as f64 / seconds / 1024.0,
                seconds
            );
        }
        if frames < total_frames {
            println!("                {} further frames were not needed", total_frames - frames);
        }
    }
}
