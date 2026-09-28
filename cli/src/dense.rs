//! `photon decode`, for pictures of dense codes.
//!
//! A picture of a dense code is not read or lost: it yields some of its tiles.
//! So what is counted here is tiles, and what is reported is how many symbols a
//! second they carried, which is the only figure that says how fast a file
//! moves.

// Counts become rates on nearly every line here.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use photon_core::dense::{
    DenseLayout, DenseProfile, DenseReader, DenseReading, DenseReceiver, DenseTransmitter,
};
use rayon::prelude::*;

use crate::media;

/// What to decode and how much to say about it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options<'a> {
    /// A directory of pictures, or one picture.
    pub(crate) input: &'a Path,
    /// Where the recovered file goes.
    pub(crate) output: &'a Path,
    /// Stop after this many pictures.
    pub(crate) max_frames: Option<usize>,
    /// The file that was sent, to count wrong modules against.
    pub(crate) truth: Option<&'a Path>,
    /// Report every picture.
    pub(crate) verbose: bool,
}

/// The pictures to read.
fn pictures(input: &Path, limit: Option<usize>) -> Result<Vec<std::path::PathBuf>, String> {
    if input.is_dir() {
        let mut frames = media::frame_paths(input).map_err(|e| e.to_string())?;
        if let Some(limit) = limit {
            frames.truncate(limit);
        }
        if frames.is_empty() {
            return Err(format!("{} contains no image files", input.display()));
        }
        return Ok(frames);
    }
    if media::looks_like_an_image(input) {
        return Ok(vec![input.to_path_buf()]);
    }
    Err("dense codes are read from pictures; extract the recording's frames first".to_owned())
}

/// Whether these are pictures of dense codes.
///
/// What was asked for settles it. Failing that, the first few pictures are
/// read as dense codes, and if a single tile comes out whole they were.
pub(crate) fn is_dense(input: &Path, asked: Option<bool>) -> bool {
    if let Some(asked) = asked {
        return asked;
    }
    let Ok(frames) = pictures(input, Some(6)) else { return false };
    let reader = DenseReader::new();
    frames.par_iter().any(|path| {
        media::read_image(path).is_ok_and(|image| !reader.read(&image).tiles.is_empty())
    })
}

/// How long the pictures took to take, when `photon film` left a note of it.
fn filmed_for(input: &Path) -> Option<(f64, f64)> {
    let text = std::fs::read_to_string(input.join("filmed.txt")).ok()?;
    let line = text.lines().find(|line| line.starts_with("camera"))?;
    let mut words = line.split_whitespace().skip_while(|word| *word != "at");
    let fps = words.nth(1)?.parse().ok()?;
    let seconds = words.nth(2)?.parse().ok()?;
    Some((fps, seconds))
}

/// What the pictures came to.
#[derive(Default)]
struct Tally {
    whole: usize,
    partial: usize,
    empty: usize,
    not_located: usize,
    tiles_read: usize,
    tiles_lost: usize,
    read_again: usize,
    new_symbols: usize,
    repaired: usize,
    bytes: usize,
    pixels: Vec<f64>,
    codes: BTreeSet<u32>,
}

impl Tally {
    fn add(&mut self, reading: &DenseReading, report: &photon_core::dense::DenseReport) {
        self.tiles_read += report.tiles_read;
        self.tiles_lost += report.tiles_lost - report.tiles_read_again.min(report.tiles_lost);
        self.read_again += report.tiles_read_again;
        self.new_symbols += report.new_symbols;
        self.repaired += reading.bytes_repaired;
        self.bytes += reading.bytes_read;
        self.codes.extend(reading.tiles.iter().map(|tile| tile.code));
        self.pixels.extend(reading.pixels_per_module);

        match (reading.located, report.tiles_read, report.tiles_lost) {
            (false, ..) => self.not_located += 1,
            (true, 0, _) => self.empty += 1,
            (true, _, 0) => self.whole += 1,
            (true, ..) => self.partial += 1,
        }
    }

    fn print(&self) {
        println!("{:<16}{:>8}", "OUTCOME", "FRAMES");
        println!("{:<16}{:>8}", "every tile", self.whole);
        println!("{:<16}{:>8}", "some tiles", self.partial);
        println!("{:<16}{:>8}", "no tile", self.empty);
        println!("{:<16}{:>8}", "not located", self.not_located);
        println!();

        let all = (self.tiles_read + self.tiles_lost).max(1);
        println!(
            "Tiles read      {} of {} ({:.1}%)",
            self.tiles_read,
            self.tiles_read + self.tiles_lost,
            self.tiles_read as f64 / all as f64 * 100.0
        );
        println!(
            "Read again      {} of them, with a code that was known taken out",
            self.read_again
        );
        println!("Codes seen      {}", self.codes.len());
        if !self.pixels.is_empty() {
            println!(
                "Pixels per module {:.2}",
                self.pixels.iter().sum::<f64>() / self.pixels.len() as f64
            );
        }
        if self.bytes > 0 {
            println!("Bytes repaired  {:.3}%", self.repaired as f64 / self.bytes as f64 * 100.0);
        }
        println!("New symbols     {}", self.new_symbols);
    }
}

/// Runs the decoder.
///
/// # Errors
///
/// Returns a message suitable for printing.
pub(crate) fn run(options: &Options<'_>) -> Result<(), String> {
    let frames = pictures(options.input, options.max_frames)?;
    let timing = filmed_for(options.input);
    println!("Frames          {}", frames.len());

    let reader = if options.truth.is_some() {
        DenseReader::new().keeping_modules()
    } else {
        DenseReader::new()
    };
    let readings: Vec<DenseReading> = frames
        .par_iter()
        .map(|path| {
            media::read_image(path)
                .map_or_else(|_| DenseReading::default(), |image| reader.read(&image))
        })
        .collect();

    // The profile is what the pictures that yielded a tile say it is, or
    // failing that what most of the pictures looked like.
    let mut votes: BTreeMap<u8, usize> = BTreeMap::new();
    for reading in &readings {
        if let Some(id) = reading.profile {
            *votes.entry(id).or_default() += 1 + 1000 * reading.tiles.len();
        }
    }
    let profile = votes
        .iter()
        .max_by_key(|(_, count)| **count)
        .and_then(|(&id, _)| DenseProfile::from_id(id));
    match profile {
        Some(profile) => println!("Read as         {}", profile.name),
        None => println!("Read as         nothing: no code was found"),
    }
    println!();

    if let (Some(file), Some(profile)) = (options.truth, profile) {
        report_truth(file, profile, &readings, options.verbose)?;
    }

    let mut receiver = DenseReceiver::new();
    let mut tally = Tally::default();
    let mut completed_at = None;
    for (index, reading) in readings.iter().enumerate() {
        tally.add(reading, &receiver.absorb(reading));
        if completed_at.is_none() && receiver.is_complete() {
            completed_at = Some(index + 1);
        }
    }
    tally.print();
    let new_symbols = tally.new_symbols;
    if let Some((accepted, needed)) = receiver.progress() {
        println!("Symbols         {accepted} of about {needed}");
    }

    let symbol = profile.map_or(0, |profile| usize::from(DenseLayout::new(profile).symbol_size()));
    if let Some((fps, seconds)) = timing {
        let taken = completed_at.map_or(seconds, |count| count as f64 / fps);
        // Symbols that arrived after the file was whole carried nothing.
        let carried = match (completed_at, receiver.manifest()) {
            (Some(_), Some(manifest)) => manifest.original_size as f64,
            _ => (new_symbols * symbol) as f64,
        };
        if taken > 0.0 {
            println!(
                "Rate            {:.1} KB/s ({:.0} bytes in {taken:.2} s)",
                carried / taken / 1024.0,
                carried
            );
        }
    }

    match receiver.finish() {
        Ok((name, file)) => {
            let target = options.output.join(&name);
            std::fs::create_dir_all(options.output)
                .map_err(|e| format!("cannot create {}: {e}", options.output.display()))?;
            std::fs::write(&target, &file)
                .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
            println!();
            println!("Recovered       {name} ({} bytes)", file.len());
            println!("Digest          verified");
            if let Some(count) = completed_at {
                println!("Needed          {count} pictures");
            }
            println!("Written to      {}", target.display());
            Ok(())
        }
        Err(error) => {
            println!();
            println!("Failed          {}", error.code());
            println!("                {error}");
            Err(format!("decode failed: {error}"))
        }
    }
}

/// Compares every module that was read with the module that was painted.
fn report_truth(
    file: &Path,
    profile: &'static DenseProfile,
    readings: &[DenseReading],
    verbose: bool,
) -> Result<(), String> {
    let bytes = std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "the file has no usable name".to_owned())?;
    // With no tile to say which session this was, the one `photon film` gives
    // every transfer it films.
    let session = readings
        .iter()
        .find_map(|r| r.tiles.first())
        .map_or(crate::film::SESSION, |tile| tile.session);

    let last = readings
        .iter()
        .flat_map(|r| r.tiles.iter())
        .map(|tile| tile.code)
        .max()
        .unwrap_or(readings.len() as u32 * 2);
    let mut sender =
        DenseTransmitter::new(name, &bytes, profile, session).map_err(|e| e.to_string())?;
    let layout = sender.layout().clone();
    let painted: Vec<Vec<bool>> =
        (0..=last.saturating_add(2)).map(|_| sender.next_modules()).collect();

    let wrong_in = |reading: &DenseReading, tile: u32, code: u32| -> Option<f64> {
        let truth = painted.get(code as usize)?;
        let modules = layout.tile_modules(tile);
        let wrong = modules
            .iter()
            .filter(|&&module| reading.modules.get(module as usize) != truth.get(module as usize))
            .count();
        Some(wrong as f64 / modules.len().max(1) as f64)
    };

    let (mut of_read, mut of_lost) = (Vec::new(), Vec::new());
    let mut by_tile: BTreeMap<u32, (usize, usize)> = BTreeMap::new();

    for (index, reading) in readings.iter().enumerate() {
        if !reading.located || reading.modules.is_empty() || reading.profile != Some(profile.id) {
            continue;
        }
        let read: BTreeMap<u32, u32> =
            reading.tiles.iter().map(|tile| (u32::from(tile.index), tile.code)).collect();
        let mut seen: BTreeSet<u32> =
            read.values().flat_map(|&code| [code.saturating_sub(1), code, code + 1]).collect();
        if seen.is_empty() {
            seen.extend(0..painted.len() as u32);
        }

        let mut line = String::new();
        for tile in 0..profile.tiles() {
            let entry = by_tile.entry(tile).or_default();
            entry.1 += 1;
            if let Some(&code) = read.get(&tile) {
                entry.0 += 1;
                if let Some(rate) = wrong_in(reading, tile, code) {
                    of_read.push(rate);
                }
                line.push('#');
            } else {
                // Nothing says which code a tile that would not read was from,
                // so it is held against each it might have been, and the one
                // it is nearest to is taken.
                let nearest = seen
                    .iter()
                    .filter_map(|&code| wrong_in(reading, tile, code))
                    .fold(f64::INFINITY, f64::min);
                if nearest.is_finite() {
                    of_lost.push(nearest);
                }
                line.push(if nearest < 0.1 { '+' } else { '.' });
            }
            if tile % profile.tile_cols == profile.tile_cols - 1 {
                line.push(' ');
            }
        }
        if verbose {
            println!("  picture {index:>4}  {line} bent {:.2}", reading.correction);
        }
    }

    let median = |values: &mut Vec<f64>| -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        Some(values[values.len() / 2] * 100.0)
    };
    let budget = f64::from(profile.parity) / 2.0 / 221.0 * 100.0;

    println!("Against the file that was sent:");
    if let Some(rate) = median(&mut of_read) {
        println!("  tiles that read     {:>6}   wrong modules, median {rate:.2}%", of_read.len());
    }
    if let Some(rate) = median(&mut of_lost) {
        println!("  tiles that did not  {:>6}   wrong modules, median {rate:.2}%", of_lost.len());
    }
    println!("  a tile is lost above about {budget:.1}% of its bytes wrong, which a");
    println!("  little under {:.1}% of its modules wrong is enough for", budget / 8.0 * 1.2);

    println!("  read, of the times seen, by where in the frame the tile is:");
    for row in 0..profile.tile_rows {
        let cells: Vec<String> = (0..profile.tile_cols)
            .map(|col| {
                let (read, seen) =
                    by_tile.get(&(row * profile.tile_cols + col)).copied().unwrap_or((0, 0));
                format!("{:>4.0}%", read as f64 / seen.max(1) as f64 * 100.0)
            })
            .collect();
        println!("    {}", cells.join(" "));
    }
    println!();
    Ok(())
}
