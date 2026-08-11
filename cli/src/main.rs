//! Bench command line for PhotonProtocol.
//!
//! This binary exists to measure the protocol, not to be pretty. Everything a
//! user sees in the browser has to be reproducible here first, on a desktop,
//! with numbers attached — throughput, frames used and dropped, cell error rate
//! per frame — because those are what settle the open questions in `SPEC.md`
//! §12.
//!
//! Argument parsing is hand-rolled while there is exactly one command. A parser
//! crate arrives with `encode` and `decode` in phase 2, when there is a command
//! surface worth the dependency.

use std::process::ExitCode;

use photon_core::simulate::{Channel, measure};
use photon_core::{PROTOCOL_VERSION, SPEC_VERSION, profile::PROFILES};

/// Confidence threshold the receiver uses to flag a cell as an erasure.
const ERASURE_CONFIDENCE: f32 = 0.08;

/// Pixels per cell used for the simulation sweep.
///
/// A 4K recording of a phone screen that fills most of the frame gives roughly
/// this many camera pixels per cell at `P2-standard`, which is what `SPEC.md`
/// Q1 asks to be measured rather than assumed.
const SWEEP_CELL_PX: u32 = 12;

const USAGE: &str = "\
photon — PhotonProtocol bench command line

USAGE:
    photon <COMMAND>

COMMANDS:
    profiles    List the physical-layer profiles and their derived geometry
    simulate    Sweep the synthetic channel and report cell error rates
    version     Print the implementation and protocol versions
    help        Print this message
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("profiles") => {
            print_profiles();
            ExitCode::SUCCESS
        }
        Some("simulate") => {
            print_simulation();
            ExitCode::SUCCESS
        }
        Some("version") => {
            print_version();
            ExitCode::SUCCESS
        }
        Some("help" | "--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        None => {
            print!("{USAGE}");
            ExitCode::FAILURE
        }
        Some(unknown) => {
            eprintln!("photon: unknown command '{unknown}'\n");
            eprint!("{USAGE}");
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

fn print_version() {
    println!("photon {}", env!("CARGO_PKG_VERSION"));
    println!("protocol version {PROTOCOL_VERSION}");
    println!("specification {SPEC_VERSION} (draft — wire format unstable)");
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
}
