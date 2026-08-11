//! A synthetic camera, so the codec can be measured before anyone films
//! anything.
//!
//! Every distortion here models something a real recording does to a frame, and
//! the point is to make each one adjustable and repeatable. A single real video
//! answers "did it work"; a severity ladder answers "at what point does it stop
//! working, and which stage gives out first", which is the question the open
//! items in `SPEC.md` §12 need settled.
//!
//! | Distortion | What it stands in for |
//! | ---------- | --------------------- |
//! | perspective | the phone is not held square to the screen |
//! | resampling | how many camera pixels land on a cell |
//! | blur | focus, motion, and the sensor's own optics |
//! | gain and lift | white balance, exposure, and glare washing out black |
//! | noise | sensor noise, worse in dim light |
//! | block quantisation | the recording is a compressed video, not a bitmap |
//!
//! Nothing here is used by the encoder or decoder. It exists to attack them.

// The workspace denies lossy numeric casts, because in the protocol path a
// silent truncation is a correctness bug. This module is the opposite kind of
// code: it converts between pixel coordinates, colour channels and kernel
// indices on nearly every line, and each conversion is bounded by construction —
// channels are clamped to [0, 1] before scaling to a byte, dimensions come from
// an existing image, kernel indices from a radius this file computed. Denying
// them here would mean a `try_from` on every arithmetic line, which would hide
// the model rather than protect it.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use crate::geom::{Homography, Point};
use crate::image::RgbImage;
use crate::symbol::Rgb;

/// A repeatable pseudo-random source.
///
/// Deterministic on purpose: a channel that varies between runs turns a rare
/// decoding failure into something nobody can reproduce.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A sample in `[-1, 1)`.
    fn signed(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        f64::from(u32::try_from(bits & 0xFFFF_FFFF).unwrap_or(0)) / f64::from(u32::MAX) * 2.0 - 1.0
    }

    /// An approximately normal sample, by summing uniforms.
    fn normal(&mut self) -> f32 {
        let sum: f64 = (0..4).map(|_| self.signed()).sum();
        (sum / 2.0) as f32
    }
}

/// What a simulated camera produced.
#[derive(Debug, Clone)]
pub struct Capture {
    /// The distorted image.
    pub image: RgbImage,
    /// Where the code area actually landed, in the distorted image.
    ///
    /// Ground truth. A decoder must find this for itself; handing it over lets a
    /// test separate "the classifier failed" from "the detector failed", which
    /// otherwise look identical from the outside.
    pub transform: Homography,
}

/// A configurable optical channel.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Corner displacement as a fraction of the frame, modelling a phone held
    /// off-square.
    pub perspective: f64,
    /// Output pixels per input pixel. Below one, the frame is being recorded at
    /// fewer pixels per cell than it was drawn with.
    pub scale: f64,
    /// Gaussian blur radius in output pixels.
    pub blur_sigma: f64,
    /// Per-channel gain, modelling white balance and exposure.
    pub gain: [f32; 3],
    /// Added to every channel, modelling glare lifting the blacks.
    pub lift: f32,
    /// Standard deviation of additive sensor noise, on a 0-to-1 scale.
    pub noise: f32,
    /// Block quantisation strength, 0 for none and 1 for heavy. Models the
    /// blocking and ringing of a compressed video.
    pub blocking: f32,
    /// Seed for every random draw.
    pub seed: u64,
}

impl Channel {
    /// A perfect channel: the frame comes back exactly as it was painted.
    #[must_use]
    pub const fn pristine() -> Self {
        Self {
            perspective: 0.0,
            scale: 1.0,
            blur_sigma: 0.0,
            gain: [1.0, 1.0, 1.0],
            lift: 0.0,
            noise: 0.0,
            blocking: 0.0,
            seed: 0x5EED,
        }
    }

    /// A channel on a severity ladder from 0 (pristine) to 1 (barely a
    /// recording).
    ///
    /// The ladder moves everything at once, on purpose: real conditions do not
    /// degrade one axis at a time, and a codec tuned against single-axis tests
    /// tends to fail the first time two of them coincide.
    ///
    /// Severity 0.5 is meant to sit near a plausible hand-held 4K capture:
    /// slightly off-square, a couple of pixels of blur, a warm white balance and
    /// visible compression.
    #[must_use]
    pub fn severity(level: f64) -> Self {
        let s = level.clamp(0.0, 1.0);
        let sf = s as f32;
        Self {
            perspective: 0.10 * s,
            scale: 1.0 - 0.55 * s,
            blur_sigma: 2.2 * s,
            // Warm, because indoor light and phone auto-white-balance usually
            // are, and because it puts the burden on the calibration ring.
            gain: [1.0 + 0.30 * sf, 1.0 - 0.06 * sf, 1.0 - 0.28 * sf],
            lift: 0.10 * sf,
            noise: 0.055 * sf,
            blocking: sf,
            seed: 0x5EED,
        }
    }

    /// Runs a painted frame through the channel.
    ///
    /// `source_transform` is where the code area sits in the input, and the
    /// returned capture says where it ended up.
    #[must_use]
    pub fn apply(&self, source: &RgbImage, source_transform: &Homography) -> Capture {
        let mut rng = Rng::new(self.seed);

        let width = ((f64::from(source.width()) * self.scale).round() as u32).max(1);
        let height = ((f64::from(source.height()) * self.scale).round() as u32).max(1);

        let projection = self.projection(source, width, height, &mut rng);
        let inverse = projection.inverse().unwrap_or_else(Homography::identity);

        let mut out = RgbImage::filled(width, height, Rgb::BLACK);
        for y in 0..height {
            for x in 0..width {
                let target = Point::new(f64::from(x), f64::from(y));
                let colour = match inverse.map(target) {
                    Some(p) => source.sample_bilinear(p.x, p.y),
                    None => continue,
                };
                out.set(x, y, to_rgb8(colour.r, colour.g, colour.b));
            }
        }

        if self.blur_sigma > 0.05 {
            out = gaussian_blur(&out, self.blur_sigma);
        }
        if self.blocking > 0.01 {
            out = block_quantise(&out, self.blocking);
        }
        self.apply_response(&mut out, &mut rng);

        Capture { image: out, transform: projection.compose(source_transform) }
    }

    /// The projective map from the painted image into the captured one.
    fn projection(&self, source: &RgbImage, width: u32, height: u32, rng: &mut Rng) -> Homography {
        let sw = f64::from(source.width() - 1);
        let sh = f64::from(source.height() - 1);
        let src =
            [Point::new(0.0, 0.0), Point::new(sw, 0.0), Point::new(sw, sh), Point::new(0.0, sh)];

        // Inset slightly so a tilted frame stays inside the capture rather than
        // running off the edge, which would be a framing mistake rather than a
        // channel effect.
        let margin = 0.04;
        let w = f64::from(width - 1);
        let h = f64::from(height - 1);
        let (x0, x1) = (w * margin, w * (1.0 - margin));
        let (y0, y1) = (h * margin, h * (1.0 - margin));

        let jitter = self.perspective * w;
        let mut nudge =
            |x: f64, y: f64| Point::new(x + rng.signed() * jitter, y + rng.signed() * jitter);

        let dst = [nudge(x0, y0), nudge(x1, y0), nudge(x1, y1), nudge(x0, y1)];
        Homography::from_quads(src, dst).unwrap_or_else(Homography::identity)
    }

    /// Gain, lift and noise: everything the sensor and its processing add after
    /// the light has arrived.
    fn apply_response(&self, image: &mut RgbImage, rng: &mut Rng) {
        for y in 0..image.height() {
            for x in 0..image.width() {
                let pixel = image.get(x, y);
                let mut channels = [
                    f32::from(pixel.r) / 255.0,
                    f32::from(pixel.g) / 255.0,
                    f32::from(pixel.b) / 255.0,
                ];
                for (value, gain) in channels.iter_mut().zip(self.gain.iter()) {
                    *value = value.mul_add(*gain, self.lift);
                    if self.noise > 0.0 {
                        *value += rng.normal() * self.noise;
                    }
                }
                image.set(x, y, to_rgb8(channels[0], channels[1], channels[2]));
            }
        }
    }
}

fn to_rgb8(r: f32, g: f32, b: f32) -> Rgb {
    let clamp = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb::new(clamp(r), clamp(g), clamp(b))
}

/// Separable Gaussian blur.
fn gaussian_blur(image: &RgbImage, sigma: f64) -> RgbImage {
    let radius = (sigma * 3.0).ceil().max(1.0) as i32;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| {
            let x = f64::from(i);
            (-(x * x) / (2.0 * sigma * sigma)).exp() as f32
        })
        .collect();
    let total: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();

    let horizontal = convolve(image, &kernel, radius, true);
    convolve(&horizontal, &kernel, radius, false)
}

fn convolve(image: &RgbImage, kernel: &[f32], radius: i32, horizontal: bool) -> RgbImage {
    let (w, h) = (image.width(), image.height());
    let mut out = RgbImage::filled(w, h, Rgb::BLACK);

    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for (index, weight) in kernel.iter().enumerate() {
                let offset = index as i32 - radius;
                let (sx, sy) = if horizontal {
                    (clamp_coord(x, offset, w), y)
                } else {
                    (x, clamp_coord(y, offset, h))
                };
                let pixel = image.get(sx, sy);
                acc[0] = f32::from(pixel.r).mul_add(*weight, acc[0]);
                acc[1] = f32::from(pixel.g).mul_add(*weight, acc[1]);
                acc[2] = f32::from(pixel.b).mul_add(*weight, acc[2]);
            }
            out.set(x, y, to_rgb8(acc[0] / 255.0, acc[1] / 255.0, acc[2] / 255.0));
        }
    }
    out
}

fn clamp_coord(base: u32, offset: i32, limit: u32) -> u32 {
    let value = i64::from(base) + i64::from(offset);
    u32::try_from(value.clamp(0, i64::from(limit) - 1)).unwrap_or(0)
}

/// The two things video compression does that a cell classifier cares about.
///
/// Not a codec, and not trying to be. Two effects matter and both are modelled
/// directly rather than emerging from a transform nobody would inspect.
///
/// **Chroma subsampling** is unconditional, because 4:2:0 is what every phone
/// records: colour is averaged over 2x2 pixels while brightness is not. It is
/// the reason the format cannot lean on hue alone at small cell sizes.
///
/// **Transform quantisation** pulls each 8x8 block towards its own mean, chroma
/// harder than luma. The strength is squared so that the low end of the severity
/// ladder stays mild — a 4K phone recording runs at a high enough bitrate that
/// blocking is barely visible, and modelling it as though it were a low-bitrate
/// stream would make every conclusion drawn from this ladder pessimistic.
fn block_quantise(image: &RgbImage, strength: f32) -> RgbImage {
    let level = strength.clamp(0.0, 1.0);
    let subsampled = subsample_chroma(image);

    let (width, height) = (image.width(), image.height());
    let mut out = subsampled.clone();
    let luma_mix = 0.35 * level * level;
    let chroma_mix = 0.60 * level * level;

    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            let bw = 8.min(width - x);
            let bh = 8.min(height - y);

            let mut mean = [0.0f32; 3];
            for by in 0..bh {
                for bx in 0..bw {
                    let pixel = subsampled.get(x + bx, y + by);
                    mean[0] += f32::from(pixel.r);
                    mean[1] += f32::from(pixel.g);
                    mean[2] += f32::from(pixel.b);
                }
            }
            let count = (bw * bh) as f32;
            for channel in &mut mean {
                *channel /= count * 255.0;
            }
            let block_luma = luma_of(mean);

            for by in 0..bh {
                for bx in 0..bw {
                    let pixel = subsampled.get(x + bx, y + by);
                    let mut channels = [
                        f32::from(pixel.r) / 255.0,
                        f32::from(pixel.g) / 255.0,
                        f32::from(pixel.b) / 255.0,
                    ];
                    let luma = luma_of(channels);
                    let target_luma = luma.mul_add(1.0 - luma_mix, block_luma * luma_mix);

                    for (value, block_mean) in channels.iter_mut().zip(mean.iter()) {
                        let chroma = *value - luma;
                        let block_chroma = block_mean - block_luma;
                        let mixed = chroma.mul_add(1.0 - chroma_mix, block_chroma * chroma_mix);
                        *value = target_luma + mixed;
                    }

                    out.set(x + bx, y + by, to_rgb8(channels[0], channels[1], channels[2]));
                }
            }
            x += 8;
        }
        y += 8;
    }
    out
}

fn luma_of(rgb: [f32; 3]) -> f32 {
    0.299f32.mul_add(rgb[0], 0.587f32.mul_add(rgb[1], 0.114 * rgb[2]))
}

/// 4:2:0 chroma subsampling: colour averaged over 2x2, brightness untouched.
fn subsample_chroma(image: &RgbImage) -> RgbImage {
    let (w, h) = (image.width(), image.height());
    let mut out = image.clone();

    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let bw = 2.min(w - x);
            let bh = 2.min(h - y);

            let mut mean = [0.0f32; 3];
            for by in 0..bh {
                for bx in 0..bw {
                    let pixel = image.get(x + bx, y + by);
                    mean[0] += f32::from(pixel.r) / 255.0;
                    mean[1] += f32::from(pixel.g) / 255.0;
                    mean[2] += f32::from(pixel.b) / 255.0;
                }
            }
            let count = (bw * bh) as f32;
            for channel in &mut mean {
                *channel /= count;
            }
            let mean_luma = luma_of(mean);

            for by in 0..bh {
                for bx in 0..bw {
                    let pixel = image.get(x + bx, y + by);
                    let channels = [
                        f32::from(pixel.r) / 255.0,
                        f32::from(pixel.g) / 255.0,
                        f32::from(pixel.b) / 255.0,
                    ];
                    // Keep this pixel's own brightness, take the block's colour.
                    let luma = luma_of(channels);
                    out.set(
                        x + bx,
                        y + by,
                        to_rgb8(
                            luma + (mean[0] - mean_luma),
                            luma + (mean[1] - mean_luma),
                            luma + (mean[2] - mean_luma),
                        ),
                    );
                }
            }
            x += 2;
        }
        y += 2;
    }
    out
}

/// What a channel did to one frame's payload cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelReport {
    /// Payload cells in the frame.
    pub cells: usize,
    /// Cells the classifier read as the wrong value.
    pub wrong: usize,
    /// Cells the classifier flagged as doubtful, whether or not it was right.
    pub doubtful: usize,
    /// Doubtful cells that really were wrong.
    ///
    /// Erasure decoding only pays off when the confidence margin actually tracks
    /// error, so this is the number that says whether the margin is worth
    /// anything (`SPEC.md` Q5).
    pub doubtful_and_wrong: usize,
}

impl ChannelReport {
    /// Fraction of cells read wrongly.
    #[must_use]
    pub fn cell_error_rate(&self) -> f64 {
        if self.cells == 0 { 0.0 } else { self.wrong as f64 / self.cells as f64 }
    }

    /// Fraction of cells flagged as doubtful.
    #[must_use]
    pub fn doubtful_rate(&self) -> f64 {
        if self.cells == 0 { 0.0 } else { self.doubtful as f64 / self.cells as f64 }
    }

    /// Share of wrong cells the confidence margin caught.
    #[must_use]
    pub fn error_detection_rate(&self) -> f64 {
        if self.wrong == 0 { 1.0 } else { self.doubtful_and_wrong as f64 / self.wrong as f64 }
    }
}

/// Paints a frame with known contents, runs it through a channel, and reports
/// how the classifier fared.
///
/// This is the measurement the whole module exists for: it isolates the
/// physical layer, with ground truth on both sides, so that a result can be
/// attributed to the classifier rather than to error correction hiding the
/// damage or the detector never finding the frame.
///
/// # Panics
///
/// Panics if the profile's alphabet cannot be indexed, which no defined profile
/// allows.
#[must_use]
pub fn measure(
    profile: &crate::profile::Profile,
    channel: &Channel,
    cell_px: u32,
    erasure_confidence: f32,
) -> ChannelReport {
    use crate::frame::FrameLayout;
    use crate::symbol::Classifier;

    let layout = FrameLayout::new(profile);
    let alphabet = layout.alphabet();

    // A stride co-prime with the alphabet, so every symbol appears about
    // equally often and no symbol can hide in a corner of the frame.
    let expected: Vec<u16> = (0..layout.data_cells().len())
        .map(|i| u16::try_from((i * 7) % alphabet.len()).unwrap_or(0))
        .collect();
    let codeword = vec![0x5Au8; profile.header_codeword_len() as usize];
    let image = layout.render(&codeword, &expected, cell_px);

    let capture = channel.apply(&image, &layout.identity_transform(cell_px));

    let calibration: Vec<_> = layout
        .calibration_cells()
        .iter()
        .enumerate()
        .map(|(i, &cell)| {
            (
                layout.calibration_value(i),
                layout.sample_cell(&capture.image, &capture.transform, cell),
            )
        })
        .collect();
    let classifier = Classifier::fit(alphabet, &calibration);

    let mut report =
        ChannelReport { cells: expected.len(), wrong: 0, doubtful: 0, doubtful_and_wrong: 0 };

    for (&cell, &want) in layout.data_cells().iter().zip(expected.iter()) {
        let sample = layout.sample_cell(&capture.image, &capture.transform, cell);
        let call = classifier.classify(&sample);
        let wrong = call.value != want;
        let doubtful = call.confidence < erasure_confidence;

        report.wrong += usize::from(wrong);
        report.doubtful += usize::from(doubtful);
        report.doubtful_and_wrong += usize::from(wrong && doubtful);
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameLayout;
    use crate::profile::PROFILES;

    fn painted(profile: &crate::profile::Profile, cell_px: u32) -> (FrameLayout, RgbImage) {
        let layout = FrameLayout::new(profile);
        let alphabet = layout.alphabet();
        let values: Vec<u16> = (0..layout.data_cells().len())
            .map(|i| u16::try_from((i * 7) % alphabet.len()).unwrap())
            .collect();
        let codeword = vec![0x5Au8; profile.header_codeword_len() as usize];
        let image = layout.render(&codeword, &values, cell_px);
        (layout, image)
    }

    fn cell_error_rate(profile: &crate::profile::Profile, channel: &Channel, cell_px: u32) -> f64 {
        measure(profile, channel, cell_px, 0.08).cell_error_rate()
    }

    #[test]
    fn a_pristine_channel_changes_nothing() {
        let (layout, image) = painted(&PROFILES[1], 8);
        let capture = Channel::pristine().apply(&image, &layout.identity_transform(8));
        assert_eq!(capture.image.width(), image.width());
        assert!(cell_error_rate(&PROFILES[1], &Channel::pristine(), 8) < 1e-12);
    }

    #[test]
    fn the_channel_is_repeatable() {
        // A channel that varies between runs turns a rare failure into
        // something nobody can reproduce, which is the worst kind of bug to
        // have in a measurement harness.
        let (layout, image) = painted(&PROFILES[1], 8);
        let transform = layout.identity_transform(8);
        let channel = Channel::severity(0.6);
        let first = channel.apply(&image, &transform);
        let second = channel.apply(&image, &transform);
        assert_eq!(first.image.as_raw(), second.image.as_raw());
    }

    #[test]
    fn cell_errors_rise_with_severity() {
        // The curve this produces is what SPEC.md Q3 needs in order to set the
        // parity rates by measurement instead of by guess.
        let profile = &PROFILES[1];
        let rates: Vec<f64> = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0]
            .iter()
            .map(|&s| cell_error_rate(profile, &Channel::severity(s), 8))
            .collect();

        assert!(rates[0] < 1e-12, "a pristine channel must be lossless");
        assert!(
            rates.last().copied().unwrap_or(0.0) > rates[1],
            "severity did not degrade classification: {rates:?}"
        );
        for pair in rates.windows(2) {
            assert!(pair[1] >= pair[0] - 0.02, "the curve is not monotone: {rates:?}");
        }
    }

    /// The lowest severity at which a profile's cell error rate outruns what its
    /// parity can repair.
    ///
    /// Reported rather than asserted against a fixed number. Where that point
    /// falls is exactly what `SPEC.md` Q3 leaves open, and a test that pinned it
    /// to a value invented today would be measuring this file's opinion rather
    /// than the codec.
    fn breaking_severity(profile: &crate::profile::Profile, cell_px: u32) -> f64 {
        let budget = profile.rs_parity_rate() / 2.0;
        let mut level = 0.0;
        while level <= 1.0 {
            if cell_error_rate(profile, &Channel::severity(level), cell_px) > budget {
                return level;
            }
            level += 0.1;
        }
        f64::INFINITY
    }

    #[test]
    fn the_profile_ladder_is_a_robustness_ladder() {
        // The entire justification for having profiles: the conservative one has
        // to survive conditions the dense one cannot. If this ever inverts, P1
        // is costing capacity and buying nothing, and the ladder should be
        // rebuilt rather than defended.
        let conservative = breaking_severity(&PROFILES[0], 8);
        let standard = breaking_severity(&PROFILES[1], 8);
        let dense = breaking_severity(&PROFILES[2], 8);

        assert!(
            conservative >= standard && standard >= dense,
            "breaking severities are out of order: P1 {conservative:.2}, \
             P2 {standard:.2}, P3 {dense:.2}"
        );
        assert!(dense > 0.0, "P3-dense fails even a pristine channel");
    }

    #[test]
    fn each_distortion_on_its_own_degrades_classification() {
        // The severity ladder moves everything together, which would hide an
        // axis that quietly does nothing.
        let profile = &PROFILES[1];
        let base = Channel::pristine();

        let mut blurred = base.clone();
        blurred.blur_sigma = 3.0;
        assert!(cell_error_rate(profile, &blurred, 8) > 0.0, "blur had no effect");

        // Noise has to be driven hard before it bites. Each sub-cell is the mean
        // of four taps and each colour decision the mean of eight sub-cells, so
        // the classifier averages away most of it -- a useful thing to know, and
        // the reason this level looks extreme.
        let mut noisy = base.clone();
        noisy.noise = 0.60;
        assert!(cell_error_rate(profile, &noisy, 8) > 0.0, "noise had no effect");

        let mut blocky = base.clone();
        blocky.blocking = 1.0;
        assert!(cell_error_rate(profile, &blocky, 8) > 0.0, "block quantisation had no effect");

        let mut shrunk = base.clone();
        shrunk.scale = 0.30;
        assert!(cell_error_rate(profile, &shrunk, 8) > 0.0, "resampling had no effect");
    }

    #[test]
    fn white_balance_alone_is_absorbed_by_calibration() {
        // A strong colour cast with nothing else wrong must cost nothing at
        // all. This is the property the calibration ring exists for, and it is
        // worth isolating from the rest of the ladder.
        let profile = &PROFILES[2];
        let mut channel = Channel::pristine();
        channel.gain = [1.35, 0.92, 0.62];
        channel.lift = 0.08;
        let rate = cell_error_rate(profile, &channel, 8);
        assert!(rate < 1e-12, "a colour cast defeated the per-frame calibration: {rate:.4}");
    }
}
