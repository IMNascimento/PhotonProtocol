//! Bench command line for PhotonProtocol.
//!
//! This binary exists to measure the protocol, not to be pretty. Everything a
//! user sees in the browser has to be reproducible here first, on a desktop,
//! with numbers attached — throughput, frames used and dropped, cell error rate
//! per frame — because those are what settle the open questions in `SPEC.md`
//! §12.

mod decode;
mod encode;
mod media;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use photon_core::simulate::{Channel, measure};
use photon_core::{PROTOCOL_VERSION, ProfileId, SPEC_VERSION, profile::PROFILES};

/// Confidence threshold the receiver uses to flag a cell as an erasure.
const ERASURE_CONFIDENCE: f32 = 0.08;

/// Pixels per cell used for the simulation sweep.
///
/// A 4K recording of a phone screen that fills most of the frame gives roughly
/// this many camera pixels per cell at `P2-standard`, which is what `SPEC.md`
/// Q1 asks to be measured rather than assumed.
const SWEEP_CELL_PX: u32 = 12;

#[derive(Parser)]
#[command(
    name = "photon",
    about = "PhotonProtocol bench command line",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Turn a file into a sequence of frames to display.
    Encode {
        /// The file to send.
        input: PathBuf,
        /// Where to write the frames. Defaults to `<name>-frames`.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Physical-layer profile.
        #[arg(short, long, value_enum, default_value_t = Profile::P2)]
        profile: Profile,
        /// Pixels per cell. Larger frames are easier to film and need a bigger
        /// screen.
        #[arg(long, default_value_t = 8)]
        cell_px: u32,
        /// How many times to repeat the full set of source symbols. Above one,
        /// the extra frames are repair symbols.
        #[arg(long, default_value_t = 1.5)]
        passes: f64,
        /// Frame rate to assemble the video at.
        #[arg(long, default_value_t = 30)]
        fps: u32,
        /// Also assemble a lossless video with ffmpeg.
        #[arg(long)]
        video: bool,
    },

    /// Read a recording, or a directory of frames, back into the file.
    Decode {
        /// The recording, or a directory of extracted frames.
        input: PathBuf,
        /// Where to write the recovered file.
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        /// Profile the frames were drawn with. Worked out from the frames
        /// themselves when not given.
        #[arg(short, long, value_enum)]
        profile: Option<Profile>,
        /// Stop after this many frames.
        #[arg(long)]
        max_frames: Option<usize>,
    },

    /// List the physical-layer profiles and their derived geometry.
    Profiles,

    /// Sweep the synthetic channel and report cell error rates.
    Simulate,
}

/// Profile names as the command line spells them.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Profile {
    /// `P1-conservative`.
    P1,
    /// `P2-standard`.
    P2,
    /// `P3-dense`.
    P3,
}

impl From<Profile> for ProfileId {
    fn from(profile: Profile) -> Self {
        match profile {
            Profile::P1 => Self::P1Conservative,
            Profile::P2 => Self::P2Standard,
            Profile::P3 => Self::P3Dense,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let outcome = match cli.command {
        Command::Encode { input, out, profile, cell_px, passes, fps, video } => {
            let target = out.unwrap_or_else(|| encode::default_output(&input));
            encode::run(&input, &target, profile.into(), cell_px, passes, fps, video)
        }
        Command::Decode { input, out, profile, max_frames } => {
            decode::run(&input, &out, profile.map(Into::into), max_frames)
        }
        Command::Profiles => {
            print_profiles();
            Ok(())
        }
        Command::Simulate => {
            print_simulation();
            Ok(())
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("photon: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Sweeps the synthetic channel and reports what it costs the classifier.
///
/// This is the phase 1 measurement. The columns that matter are the cell error
/// rate against the profile's correction budget — which is what decides whether
/// a frame is recoverable at all — and how much of that error the confidence
/// margin managed to flag, since erasure decoding is only worth its complexity
/// if the margin tracks error.
fn print_simulation() {
    println!("Synthetic channel sweep at {SWEEP_CELL_PX} pixels per cell.");
    println!("Budget is the profile's error-correction radius; above it a frame is lost.\n");

    for profile in &PROFILES {
        let budget = profile.rs_parity_rate() / 2.0;
        println!(
            "{} — {}x{} cells, {} bits/cell, correction budget {:.2}%",
            profile.name,
            profile.grid,
            profile.grid,
            profile.bits_per_cell(),
            budget * 100.0
        );
        println!(
            "  {:>8} {:>12} {:>12} {:>12} {:>10}",
            "SEVERITY", "CELL ERRORS", "DOUBTFUL", "CAUGHT", "VERDICT"
        );

        let mut breaking = None;
        for step in 0..=10 {
            let severity = f64::from(step) / 10.0;
            let channel = Channel::severity(severity);
            let report = measure(profile, &channel, SWEEP_CELL_PX, ERASURE_CONFIDENCE);

            let rate = report.cell_error_rate();
            let over = rate > budget;
            if over && breaking.is_none() {
                breaking = Some(severity);
            }

            println!(
                "  {:>8.1} {:>11.3}% {:>11.3}% {:>11.1}% {:>10}",
                severity,
                rate * 100.0,
                report.doubtful_rate() * 100.0,
                report.error_detection_rate() * 100.0,
                if over { "over" } else { "ok" }
            );
        }

        match breaking {
            Some(severity) => println!("  first uncorrectable severity: {severity:.1}"),
            None => println!("  correctable across the whole ladder"),
        }
        println!();
    }

    println!("Caveat: the severity ladder is a model, not a measurement of any real");
    println!("camera. Phase 2 replaces it with parameters fitted to actual footage,");
    println!("and the numbers above should be re-read then (SPEC.md Q1, Q3).");
}

fn print_profiles() {
    println!(
        "{:<16} {:>4} {:>9} {:>5} {:>10} {:>15} {:>10} {:>9}",
        "PROFILE", "ID", "GRID", "BITS", "HEADER", "PAYLOAD RS", "CAPACITY", "OVERHEAD"
    );

    for profile in &PROFILES {
        let partition = profile.rs_partition();
        let grid = format!("{0}x{0}", profile.grid);
        let header = format!("RS({},20)", profile.header_codeword_len());
        let payload_rs = format!("{}xRS(255,{})", partition.full_codewords, profile.rs_data_len);

        println!(
            "{:<16} {:>#04x} {:>9} {:>5} {:>10} {:>15} {:>8} B {:>8.1}%",
            profile.name,
            profile.id.as_u8(),
            grid,
            profile.bits_per_cell(),
            header,
            payload_rs,
            profile.payload_capacity(),
            profile.link_overhead() * 100.0,
        );

        // The shortened codeword is the tail of the partition, so it hangs off
        // the payload column rather than claiming a column of its own.
        if let Some((n, k)) = partition.shortened {
            let shortened = format!("+ RS({n},{k})");
            println!("{:<16} {:>4} {:>9} {:>5} {:>10} {:>15}", "", "", "", "", "", shortened);
        }
    }

    println!();
    println!(
        "photon {} — protocol {PROTOCOL_VERSION}, spec {SPEC_VERSION} (draft)",
        env!("CARGO_PKG_VERSION")
    );
}
