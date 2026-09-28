//! `photon inspect`: what can be said about a picture nobody has the file for.
//!
//! A picture a phone could not read arrives without the file that was being
//! sent, so the cells in it cannot be checked against what was painted. But a
//! frame carries cells whose values are known regardless: the calibration ring.
//! Fitting the classifier to half of the ring and reading the other half with
//! it measures the classifier on cells it has not seen, through the very camera
//! that took the picture.
//!
//! It flatters the payload a little. The ring's cells have solid neighbours on
//! two sides, and the payload's have payload. But it separates the two
//! questions that matter: whether the grid is landing on the cells, and whether
//! the cells can be told apart once it does.

use std::path::Path;

use photon_core::detect::Detector;
use photon_core::session::Receiver;
use photon_core::{Classifier, FrameLayout, Mesh};

use crate::media;

/// Reports on every picture in `input`.
///
/// # Errors
///
/// Returns a message suitable for printing.
pub(crate) fn run(input: &Path, debug_dir: Option<&Path>) -> Result<(), String> {
    let paths = if input.is_dir() {
        media::frame_paths(input).map_err(|e| e.to_string())?
    } else {
        vec![input.to_path_buf()]
    };
    if paths.is_empty() {
        return Err(format!("{} contains no image files", input.display()));
    }
    if let Some(directory) = debug_dir {
        std::fs::create_dir_all(directory)
            .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;
    }

    println!(
        "{:<38} {:>13} {:>7} {:>7} {:>6} {:>6} {:>9} {:>9}",
        "PICTURE", "PROFILE", "SEQ", "PX/CELL", "BENT", "RING", "RING WRONG", "OUTCOME"
    );

    let detector = Detector::new();
    let receiver = Receiver::new();

    for path in &paths {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let Ok(image) = media::read_image(path) else { continue };

        let Ok(detection) = detector.detect(&image) else {
            println!("{name:<38} {:>13}", "not found");
            continue;
        };
        let opening = receiver.open(&image);
        let profile = opening.as_ref().map_or(detection.profile, |o| o.profile);
        let layout = FrameLayout::new(profile.profile());
        let mesh = Mesh::fit(&layout, &image, &detection.transform);

        // Half the ring to fit, the other half to read.
        let ring: Vec<_> = layout
            .calibration_cells()
            .iter()
            .enumerate()
            .map(|(index, &cell)| {
                (layout.calibration_value(index), layout.sample_cell_in(&image, &mesh, cell))
            })
            .collect();
        // Every symbol comes round once per alphabet, so taking alternate
        // laps keeps every symbol in both halves.
        let lap = layout.alphabet().len();
        let (fitted, held): (Vec<_>, Vec<_>) =
            ring.iter().enumerate().partition(|(index, _)| (index / lap).is_multiple_of(2));
        let fitted: Vec<_> = fitted.into_iter().map(|(_, entry)| *entry).collect();
        let classifier = Classifier::fit(layout.alphabet(), &fitted);
        let wrong = held
            .iter()
            .filter(|(_, (value, sample))| classifier.classify(sample).value != *value)
            .count();

        let reading = opening.as_ref().map(|o| receiver.read(&image, o));
        let outcome = reading.as_ref().map_or("no header", |r| match r.outcome {
            photon_core::session::FrameOutcome::Decoded => "decoded",
            photon_core::session::FrameOutcome::Straddled => "two codes",
            photon_core::session::FrameOutcome::HeaderUnreadable => "no header",
            photon_core::session::FrameOutcome::PayloadUnrecoverable => "cells",
            _ => "other",
        });
        let sequence = opening
            .as_ref()
            .and_then(|o| o.header)
            .map_or_else(|| "-".to_owned(), |h| h.frame_seq.to_string());

        println!(
            "{name:<38} {:>13} {sequence:>7} {:>7.2} {:>6.2} {:>5.0}% {:>9.1}% {outcome:>9}",
            profile.profile().name,
            detection.pixels_per_cell(),
            mesh.largest_correction(),
            mesh.coverage() * 100.0,
            wrong as f64 / held.len().max(1) as f64 * 100.0,
        );

        if let Some(directory) = debug_dir {
            let target = directory.join(format!("{name}.rectified.png"));
            media::write_png(&target, &layout.rectify(&image, &mesh, 8))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
