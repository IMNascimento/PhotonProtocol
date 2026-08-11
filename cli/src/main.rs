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

use photon_core::{PROTOCOL_VERSION, SPEC_VERSION, profile::PROFILES};

const USAGE: &str = "\
photon — PhotonProtocol bench command line

USAGE:
    photon <COMMAND>

COMMANDS:
    profiles    List the physical-layer profiles and their derived geometry
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
