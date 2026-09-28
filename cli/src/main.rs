//! Bench command line for PhotonProtocol.
//!
//! This binary exists to measure the protocol, not to be pretty. Everything a
//! user sees in the browser has to be reproducible here first, on a desktop,
//! with numbers attached — throughput, frames used and dropped, cell error rate
//! per frame — because those are what settle the open questions in `SPEC.md`
//! §12.

mod bench;
mod decode;
mod encode;
mod film;
mod inspect;
mod media;
mod truth;

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
        /// Write what the decoder saw into this directory: the code area with
        /// its perspective undone, one picture per frame. If the sampling grid
        /// is landing in the wrong place, that is visible here and nowhere else.
        #[arg(long)]
        debug_dir: Option<PathBuf>,
        /// The file that was sent. With it, every cell the decoder read is
        /// compared against what the sender painted there.
        #[arg(long)]
        truth: Option<PathBuf>,
        /// Print the comparison for every frame, not only the summary.
        #[arg(long)]
        verbose: bool,
    },

    /// Film a transfer with a simulated phone camera pointed at a simulated
    /// monitor, and write the pictures a browser would have been handed.
    Film {
        /// The file the simulated sender is sending.
        input: PathBuf,
        /// Where to write the pictures.
        #[arg(short, long)]
        out: PathBuf,
        /// Physical-layer profile.
        #[arg(short, long, value_enum, default_value_t = Profile::P1)]
        profile: Profile,
        /// The monitor, as `WIDTHxHEIGHT`.
        #[arg(long, default_value = "1920x1080")]
        screen: String,
        /// Pixels per cell. Fitted to the screen, as the sending page does,
        /// when not given.
        #[arg(long)]
        cell_px: Option<u32>,
        /// Display refreshes each code is held for.
        #[arg(long, default_value_t = 4)]
        hold: u32,
        /// Display refresh rate.
        #[arg(long, default_value_t = 60.0)]
        refresh: f64,
        /// The camera's picture, as `WIDTHxHEIGHT`.
        #[arg(long, default_value = "1080x1920")]
        camera: String,
        /// Pictures per second.
        #[arg(long, default_value_t = 30.0)]
        fps: f64,
        /// How long to film, in seconds.
        #[arg(long, default_value_t = 4.0)]
        seconds: f64,
        /// How hostile the capture is.
        #[arg(long, value_enum, default_value_t = film::Preset::Typical)]
        preset: film::Preset,
        /// Overrides the preset: side of the code as a fraction of the
        /// picture's shorter side.
        #[arg(long)]
        fill: Option<f64>,
        /// Overrides the preset: lens blur, in sensor pixels.
        #[arg(long)]
        blur: Option<f64>,
        /// Overrides the preset: radial distortion at the corner.
        #[arg(long)]
        distortion: Option<f64>,
        /// Overrides the preset: exposure time in milliseconds.
        #[arg(long)]
        exposure: Option<f64>,
        /// Overrides the preset: sensor readout time in milliseconds.
        #[arg(long)]
        readout: Option<f64>,
        /// Overrides the preset: where white lands relative to saturation.
        #[arg(long)]
        gain: Option<f32>,
        /// Overrides the preset: hand tremor in degrees.
        #[arg(long)]
        shake: Option<f64>,
        /// Which way the shutter rolls across the picture. A phone held
        /// upright rolls sideways, which is what is assumed when not given.
        #[arg(long, value_enum)]
        sweep: Option<film::Sweep>,
        /// Where in a code's time on screen the first picture is taken, 0 to 1.
        #[arg(long)]
        phase: Option<f64>,
        /// Seed for every random draw.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Do not write PNGs.
        #[arg(long)]
        no_png: bool,
        /// Also write `camera.y4m`, which Chrome can play as a camera.
        #[arg(long)]
        y4m: bool,
    },

    /// Say what can be said about pictures nobody has the file for: where the
    /// grid landed, and how well the calibration ring reads itself.
    Inspect {
        /// A picture, or a directory of them.
        input: PathBuf,
        /// Write each picture as the decoder saw it into this directory.
        #[arg(long)]
        debug_dir: Option<PathBuf>,
    },

    /// Time each stage of reading a picture, on one core.
    Bench {
        /// A directory of pictures.
        input: PathBuf,
        /// How many of them to use.
        #[arg(long, default_value_t = 40)]
        frames: usize,
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
    /// `P4-balanced`.
    P4,
}

impl From<Profile> for ProfileId {
    fn from(profile: Profile) -> Self {
        match profile {
            Profile::P1 => Self::P1Conservative,
            Profile::P2 => Self::P2Standard,
            Profile::P3 => Self::P3Dense,
            Profile::P4 => Self::P4Balanced,
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
        Command::Decode { input, out, profile, max_frames, debug_dir, truth, verbose } => {
            decode::run(&decode::Options {
                input: &input,
                output: &out,
                profile: profile.map(Into::into),
                max_frames,
                debug_dir: debug_dir.as_deref(),
                truth: truth.as_deref(),
                verbose,
            })
        }
        Command::Film {
            input,
            out,
            profile,
            screen,
            cell_px,
            hold,
            refresh,
            camera,
            fps,
            seconds,
            preset,
            fill,
            blur,
            distortion,
            exposure,
            readout,
            gain,
            shake,
            sweep,
            phase,
            seed,
            no_png,
            y4m,
        } => (|| {
            let mut model = film::CameraModel::preset(preset);
            if let Some(value) = fill {
                model.fill = value;
            }
            if let Some(value) = blur {
                model.blur = value;
            }
            if let Some(value) = distortion {
                model.distortion = value;
            }
            if let Some(value) = exposure {
                model.exposure_ms = value;
            }
            if let Some(value) = readout {
                model.readout_ms = value;
            }
            if let Some(value) = gain {
                model.gain = value;
            }
            if let Some(value) = shake {
                model.shake = value;
            }
            film::run(&film::Options {
                input,
                out,
                profile: profile.into(),
                screen: parse_size(&screen)?,
                cell_px,
                hold,
                refresh,
                camera: parse_size(&camera)?,
                fps,
                seconds,
                model,
                sweep,
                phase,
                seed,
                png: !no_png,
                y4m,
            })
        })(),
        Command::Inspect { input, debug_dir } => inspect::run(&input, debug_dir.as_deref()),
        Command::Bench { input, frames } => bench::run(&input, frames),
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

/// Reads `1920x1080`.
fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let parsed = text
        .split_once(['x', 'X'])
        .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)));
    match parsed {
        Some((width, height)) if width > 0 && height > 0 => Ok((width, height)),
        _ => Err(format!("'{text}' is not a size; write it as WIDTHxHEIGHT")),
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
