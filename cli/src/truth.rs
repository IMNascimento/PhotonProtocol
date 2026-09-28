//! Counting the cells a decoder read wrongly, given the file that was sent.
//!
//! Whether a frame decoded is one bit. It says the damage was under or over
//! what the parity could repair and nothing about how far, where in the frame,
//! or which half of a cell's value went wrong — and those are the things that
//! say what to change.
//!
//! The sender is deterministic: the same file, name, profile and session
//! produce the same frames. So a bench that is given the file can rebuild what
//! every frame carried and compare it, cell by cell, against what the decoder
//! made of the picture.

use std::collections::HashMap;
use std::path::Path;

use photon_core::FrameLayout;
use photon_core::session::{FrameContents, FrameOutcome, FrameReading, Transmitter};

/// What went wrong in one frame.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Damage {
    cells: usize,
    wrong: usize,
    wrong_shape: usize,
    wrong_colour: usize,
    /// Wrong cells by ninth of the frame, row-major.
    zones: [usize; 9],
    zone_cells: [usize; 9],
    /// Wrong cells, and all cells, by the colour that was painted.
    by_colour: [usize; 8],
    colour_cells: [usize; 8],
}

impl Damage {
    fn rate(&self) -> f64 {
        if self.cells == 0 { 0.0 } else { self.wrong as f64 / self.cells as f64 }
    }
}

/// Rebuilds what the sender painted, frame by frame.
pub(crate) struct Truth {
    transmitter: Transmitter,
    layout: FrameLayout,
    frames: HashMap<u32, FrameContents>,
    generated: u32,
}

/// Frames the sender is allowed to have got through before the search for one
/// is abandoned as a mistake rather than a long recording.
const FURTHEST_FRAME: u32 = 200_000;

impl Truth {
    /// Prepares to rebuild the frames of `file` as sent in `session`.
    ///
    /// # Errors
    ///
    /// Returns a message if the file cannot be read or sent.
    pub(crate) fn new(
        file: &Path,
        profile: photon_core::ProfileId,
        session: u32,
    ) -> Result<Self, String> {
        let bytes =
            std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "the truth file has no usable name".to_owned())?;
        let transmitter =
            Transmitter::new(name, &bytes, profile, session).map_err(|e| e.to_string())?;
        Ok(Self {
            transmitter,
            layout: FrameLayout::new(profile.profile()),
            frames: HashMap::new(),
            generated: 0,
        })
    }

    fn contents(&mut self, frame_seq: u32) -> Option<&FrameContents> {
        if frame_seq > FURTHEST_FRAME {
            return None;
        }
        while self.generated <= frame_seq {
            let contents = self.transmitter.next_contents().ok()?;
            self.frames.insert(self.generated, contents);
            self.generated += 1;
        }
        self.frames.get(&frame_seq)
    }

    /// Compares a reading against what its frame carried.
    pub(crate) fn damage(&mut self, reading: &FrameReading) -> Option<Damage> {
        let header = reading.header?;
        if reading.cells.is_empty() {
            return None;
        }
        let layout = self.layout.clone();
        let contents = self.contents(header.frame_seq)?;
        if contents.cells.len() != reading.cells.len() {
            return None;
        }

        let alphabet = layout.alphabet();
        let grid = layout.grid();
        let mut damage = Damage { cells: reading.cells.len(), ..Damage::default() };

        for ((&cell, &want), &got) in
            layout.data_cells().iter().zip(contents.cells.iter()).zip(reading.cells.iter())
        {
            let (row, col) = layout.coordinates(cell);
            let zone = ((row * 3 / grid) * 3 + col * 3 / grid) as usize;
            damage.zone_cells[zone] += 1;

            let (want_colour, want_shape) = alphabet.split(want);
            damage.colour_cells[want_colour.min(7)] += 1;

            if want == got {
                continue;
            }
            damage.wrong += 1;
            damage.zones[zone] += 1;
            damage.by_colour[want_colour.min(7)] += 1;

            let (got_colour, got_shape) = alphabet.split(got);
            if want_shape != got_shape {
                damage.wrong_shape += 1;
            }
            if want_colour != got_colour {
                damage.wrong_colour += 1;
            }
        }
        Some(damage)
    }
}

/// Prints the damage in every frame that got as far as reading cells.
pub(crate) fn report(truth: &mut Truth, readings: &[FrameReading], budget: f64, verbose: bool) {
    let mut rates: Vec<f64> = Vec::new();
    let mut total = Damage::default();

    println!("Cell errors against the file that was sent (budget {:.1}%)", budget * 100.0);
    if verbose {
        println!(
            "  {:>6} {:>8} {:>9} {:>9} {:>9}  OUTCOME",
            "FRAME", "SEQ", "WRONG", "SHAPE", "COLOUR"
        );
    }

    for (index, reading) in readings.iter().enumerate() {
        let Some(damage) = truth.damage(reading) else { continue };
        rates.push(damage.rate());

        total.cells += damage.cells;
        total.wrong += damage.wrong;
        total.wrong_shape += damage.wrong_shape;
        total.wrong_colour += damage.wrong_colour;
        for zone in 0..9 {
            total.zones[zone] += damage.zones[zone];
            total.zone_cells[zone] += damage.zone_cells[zone];
        }
        for colour in 0..8 {
            total.by_colour[colour] += damage.by_colour[colour];
            total.colour_cells[colour] += damage.colour_cells[colour];
        }

        if verbose {
            let percent = |n: usize| n as f64 / damage.cells.max(1) as f64 * 100.0;
            println!(
                "  {:>6} {:>8} {:>8.2}% {:>8.2}% {:>8.2}%  {}",
                index,
                reading.header.map_or(0, |h| h.frame_seq),
                percent(damage.wrong),
                percent(damage.wrong_shape),
                percent(damage.wrong_colour),
                match reading.outcome {
                    FrameOutcome::Decoded => "decoded",
                    FrameOutcome::PayloadUnrecoverable => "lost",
                    _ => "other",
                }
            );
        }
    }

    if rates.is_empty() {
        println!("  no frame got as far as reading cells");
        println!();
        return;
    }

    rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    // Quarters of the way through the sorted list, in whole positions.
    let at = |quarters: usize| rates[(rates.len() - 1) * quarters / 4] * 100.0;
    let under = rates.iter().filter(|&&rate| rate <= budget).count();

    println!("  frames measured   {}", rates.len());
    println!(
        "  wrong cells       best {:.2}%  quarter {:.2}%  median {:.2}%  worst {:.2}%",
        at(0),
        at(1),
        at(2),
        at(4)
    );
    println!("  within budget     {under} of {}", rates.len());

    let percent = |n: usize| n as f64 / total.cells.max(1) as f64 * 100.0;
    println!(
        "  of all cells      {:.2}% wrong: shape {:.2}%, colour {:.2}%",
        percent(total.wrong),
        percent(total.wrong_shape),
        percent(total.wrong_colour)
    );
    let colours: Vec<String> = (0..8)
        .filter(|&colour| total.colour_cells[colour] > 0)
        .map(|colour| {
            format!(
                "{colour}: {:.2}%",
                total.by_colour[colour] as f64 / total.colour_cells[colour] as f64 * 100.0
            )
        })
        .collect();
    println!("  by colour painted {}", colours.join("  "));
    println!("  by ninth of the frame, wrong cells:");
    for row in 0..3 {
        let cells: Vec<String> = (0..3)
            .map(|col| {
                let zone = row * 3 + col;
                let rate = total.zones[zone] as f64 / total.zone_cells[zone].max(1) as f64;
                format!("{:>6.2}%", rate * 100.0)
            })
            .collect();
        println!("    {}", cells.join(" "));
    }
    println!();
}
