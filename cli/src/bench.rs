//! `photon bench`: how long each stage of reading a picture takes.
//!
//! On a phone the decoder shares a second with thirty camera frames, and how
//! many of them it gets through is the throughput of the whole protocol. This
//! times the stages one at a time, on one core, so that the slow one can be
//! named rather than guessed at.

use std::path::Path;
use std::time::{Duration, Instant};

use photon_core::detect::Detector;
use photon_core::session::Receiver;
use photon_core::{Classifier, FrameLayout, Mesh};

use crate::media;

/// Times the decoder over the pictures in `input`.
///
/// # Errors
///
/// Returns a message suitable for printing.
pub(crate) fn run(input: &Path, limit: usize) -> Result<(), String> {
    let mut paths = media::frame_paths(input).map_err(|e| e.to_string())?;
    paths.truncate(limit);
    if paths.is_empty() {
        return Err(format!("{} contains no image files", input.display()));
    }

    let images: Vec<_> = paths.iter().filter_map(|path| media::read_image(path).ok()).collect();
    let Some(first) = images.first() else {
        return Err("none of the pictures could be read".to_owned());
    };
    println!("Pictures        {} of {}x{}", images.len(), first.width(), first.height());

    let dense = photon_core::dense::DenseReader::new();
    if images.iter().take(6).any(|image| !dense.read(image).tiles.is_empty()) {
        let (mut whole, mut tiles) = (Duration::ZERO, 0usize);
        for image in &images {
            let started = Instant::now();
            let reading = dense.read(image);
            whole += started.elapsed();
            tiles += reading.tiles.len();
        }
        let each = whole.as_secs_f64() * 1000.0 / images.len() as f64;
        println!("Tiles           {tiles}");
        println!("Everything      {each:>7.2} ms a picture");
        return Ok(());
    }

    let detector = Detector::new();
    let mut detect = Duration::ZERO;
    let mut fit = Duration::ZERO;
    let mut whole = Duration::ZERO;
    let mut sampling = Duration::ZERO;
    let mut classifying = Duration::ZERO;
    let mut located = 0usize;

    for image in &images {
        let started = Instant::now();
        let detection = detector.detect(image);
        detect += started.elapsed();

        if let Ok(detection) = detection {
            located += 1;
            let layout = FrameLayout::new(detection.profile.profile());
            let started = Instant::now();
            let mesh = Mesh::fit(&layout, image, &detection.transform);
            fit += started.elapsed();

            let started = Instant::now();
            let samples: Vec<_> = layout
                .data_cells()
                .iter()
                .map(|&cell| layout.sample_cell_in(image, &mesh, cell))
                .collect();
            sampling += started.elapsed();

            let labelled: Vec<_> = layout
                .calibration_cells()
                .iter()
                .enumerate()
                .map(|(i, &cell)| {
                    (layout.calibration_value(i), layout.sample_cell_in(image, &mesh, cell))
                })
                .collect();
            let started = Instant::now();
            let classifier = Classifier::fit(layout.alphabet(), &labelled);
            let calls: Vec<_> = samples.iter().map(|s| classifier.classify(s)).collect();
            classifying += started.elapsed();
            std::hint::black_box(calls);
        }
    }

    let receiver = Receiver::new();
    for image in &images {
        let started = Instant::now();
        let reading = receiver.examine(image);
        whole += started.elapsed();
        std::hint::black_box(reading);
    }

    let per = |total: Duration, count: usize| total.as_secs_f64() * 1000.0 / count.max(1) as f64;
    println!("Located         {located}");
    println!("Finding         {:>7.2} ms a picture", per(detect, images.len()));
    println!("Fitting         {:>7.2} ms a picture", per(fit, located));
    println!("Sampling        {:>7.2} ms a picture", per(sampling, located));
    println!("Classifying     {:>7.2} ms a picture, once through", per(classifying, located));
    println!("Everything      {:>7.2} ms a picture", per(whole, images.len()));
    println!(
        "Reading         {:>7.2} ms a picture, which is everything less finding and fitting",
        per(whole, images.len()) - per(detect, images.len()) - per(fit, located)
    );
    Ok(())
}
