//! The cell alphabet: shapes, colours, and how a measured cell becomes a value.
//!
//! A data cell carries `log2(shapes) + log2(colours)` bits as a shape painted in
//! an ink colour over a black background (`SPEC.md` §4.3). Encoding is trivial.
//! Classification is the hard half, and it is where the format's one structural
//! idea shows up.
//!
//! A decoder must not compare a measured cell against the palette this module
//! declares. A camera applies its own white balance, exposure and colour matrix,
//! all of which drift while it records and none of which are known to the
//! emitter, so the nominal palette describes what was *sent* and never what
//! arrives. Instead every frame carries a calibration ring — a labelled sample
//! of every symbol in the alphabet, exposed through the same optics in the same
//! instant — and the classifier is fitted to that (see [`Classifier`]).

use crate::profile::Profile;

/// Side of a shape mask in sub-cells. Masks are 4x4 (`SPEC.md` §4.3.1).
pub const SHAPE_GRID: u32 = 4;

/// Sub-cells in a shape mask.
pub const SHAPE_SUBCELLS: usize = (SHAPE_GRID * SHAPE_GRID) as usize;

/// Sub-cells painted by every mask. Masks are balanced so that a cell's ink
/// energy does not depend on which shape it carries, which keeps the colour
/// classifier independent of the shape classifier.
pub const SHAPE_INK_SUBCELLS: usize = SHAPE_SUBCELLS / 2;

/// The 4-shape alphabet (`SPEC.md` §4.3.1).
pub const SHAPES_4: [u16; 4] = [0x00FF, 0x3333, 0xCCCC, 0xFF00];

/// The 8-shape alphabet (`SPEC.md` §4.3.1).
pub const SHAPES_8: [u16; 8] = [0x00FF, 0x1DF0, 0x3333, 0x662E, 0x8CCE, 0xC837, 0xF710, 0xFC88];

/// An sRGB colour, written to the framebuffer without gamma adjustment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// A colour from its three channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Pure black, the background of every cell.
    pub const BLACK: Self = Self::new(0x00, 0x00, 0x00);

    /// Pure white, used by the quiet zone, separators and solid cells.
    pub const WHITE: Self = Self::new(0xFF, 0xFF, 0xFF);
}

/// The 4-colour palette (`SPEC.md` §4.3.2).
pub const PALETTE_4: [Rgb; 4] = [
    Rgb::new(0xFF, 0x2A, 0x2A), // red
    Rgb::new(0x2A, 0xFF, 0x2A), // green
    Rgb::new(0x2A, 0x2A, 0xFF), // blue
    Rgb::new(0xFF, 0xFF, 0xFF), // white
];

/// The 8-colour palette (`SPEC.md` §4.3.2).
pub const PALETTE_8: [Rgb; 8] = [
    Rgb::new(0xFF, 0x2A, 0x2A), // red
    Rgb::new(0xFF, 0xFF, 0x2A), // yellow
    Rgb::new(0x2A, 0xFF, 0x2A), // green
    Rgb::new(0x2A, 0xFF, 0xFF), // cyan
    Rgb::new(0x2A, 0x2A, 0xFF), // blue
    Rgb::new(0xFF, 0x2A, 0xFF), // magenta
    Rgb::new(0xFF, 0xFF, 0xFF), // white
    Rgb::new(0x8C, 0x8C, 0x8C), // grey
];

/// A linear colour sample, with channels in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgbf {
    /// Red channel.
    pub r: f32,
    /// Green channel.
    pub g: f32,
    /// Blue channel.
    pub b: f32,
}

impl Rgbf {
    /// All channels zero.
    pub const ZERO: Self = Self { r: 0.0, g: 0.0, b: 0.0 };

    /// A sample from its three channels.
    #[must_use]
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    /// Squared Euclidean distance, which is all a nearest-centroid search needs.
    #[must_use]
    pub fn distance_squared(self, other: Self) -> f32 {
        let (dr, dg, db) = (self.r - other.r, self.g - other.g, self.b - other.b);
        dr.mul_add(dr, dg.mul_add(dg, db * db))
    }

    /// Rec. 601 luma, the perceptual weighting a monochrome shape decision wants.
    #[must_use]
    pub fn luma(self) -> f32 {
        0.299f32.mul_add(self.r, 0.587f32.mul_add(self.g, 0.114 * self.b))
    }

    fn scaled(self, k: f32) -> Self {
        Self::new(self.r * k, self.g * k, self.b * k)
    }

    fn added(self, other: Self) -> Self {
        Self::new(self.r + other.r, self.g + other.g, self.b + other.b)
    }
}

impl From<Rgb> for Rgbf {
    fn from(c: Rgb) -> Self {
        Self::new(f32::from(c.r) / 255.0, f32::from(c.g) / 255.0, f32::from(c.b) / 255.0)
    }
}

/// The shape and colour alphabets a profile uses together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alphabet {
    /// Shape masks, indexed by shape index.
    pub shapes: &'static [u16],
    /// Ink colours, indexed by colour index.
    pub colours: &'static [Rgb],
}

impl Alphabet {
    /// The alphabet a profile uses.
    ///
    /// # Panics
    ///
    /// Panics if the profile declares alphabet sizes the specification does not
    /// define. That is unreachable for the profiles in [`crate::profile`], and a
    /// panic is the right response to a profile table that has been edited into
    /// an inconsistent state.
    #[must_use]
    pub fn for_profile(profile: &Profile) -> Self {
        let shapes: &'static [u16] = match profile.num_shapes {
            4 => &SHAPES_4,
            8 => &SHAPES_8,
            n => panic!("no shape alphabet of size {n} is defined"),
        };
        let colours: &'static [Rgb] = match profile.num_colours {
            4 => &PALETTE_4,
            8 => &PALETTE_8,
            n => panic!("no colour palette of size {n} is defined"),
        };
        Self { shapes, colours }
    }

    /// Bits carried by one cell.
    #[must_use]
    pub fn bits_per_cell(&self) -> u32 {
        self.shapes.len().ilog2() + self.colours.len().ilog2()
    }

    /// Number of distinct cell values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shapes.len() * self.colours.len()
    }

    /// Whether the alphabet is empty. Never true for a valid profile.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Packs a colour and shape index into a cell value (`SPEC.md` §4.3).
    ///
    /// # Panics
    ///
    /// Panics if the indices do not belong to this alphabet. Every defined
    /// alphabet packs into 6 bits, so this can only fire on an index the caller
    /// invented.
    #[must_use]
    pub fn join(&self, colour: usize, shape: usize) -> u16 {
        let shape_bits = self.shapes.len().ilog2();
        let value = (colour << shape_bits) | shape;
        u16::try_from(value).expect("cell value fits in 16 bits for every defined alphabet")
    }

    /// Splits a cell value into its colour and shape indices.
    #[must_use]
    pub fn split(&self, value: u16) -> (usize, usize) {
        let shape_bits = self.shapes.len().ilog2();
        let value = usize::from(value);
        (value >> shape_bits, value & ((1 << shape_bits) - 1))
    }
}

/// Whether sub-cell `(y, x)` of a mask is painted (`SPEC.md` §4.3.1).
#[must_use]
pub fn mask_bit(mask: u16, y: u32, x: u32) -> bool {
    debug_assert!(y < SHAPE_GRID && x < SHAPE_GRID);
    (mask >> (y * SHAPE_GRID + x)) & 1 == 1
}

/// A cell as the decoder measured it: mean colour of each of the 16 sub-cells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellSample {
    /// Sub-cell means in raster order, `4y + x`.
    pub sub: [Rgbf; SHAPE_SUBCELLS],
}

impl CellSample {
    /// A sample with every sub-cell black.
    #[must_use]
    pub const fn zeroed() -> Self {
        Self { sub: [Rgbf::ZERO; SHAPE_SUBCELLS] }
    }

    /// Per-sub-cell luma.
    #[must_use]
    pub fn lumas(&self) -> [f32; SHAPE_SUBCELLS] {
        core::array::from_fn(|i| self.sub[i].luma())
    }

    /// Mean colour of the sub-cells a mask paints.
    #[must_use]
    pub fn ink_colour(&self, mask: u16) -> Rgbf {
        let mut sum = Rgbf::ZERO;
        let mut count = 0u32;
        for y in 0..SHAPE_GRID {
            for x in 0..SHAPE_GRID {
                if mask_bit(mask, y, x) {
                    sum = sum.added(self.sub[(y * SHAPE_GRID + x) as usize]);
                    count += 1;
                }
            }
        }
        if count == 0 { Rgbf::ZERO } else { sum.scaled(1.0 / count as f32) }
    }
}

/// The outcome of classifying one cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Classification {
    /// The cell value the classifier settled on.
    pub value: u16,
    /// How clearly it won, in `[0, 1]`.
    ///
    /// This is a margin, not a probability: the gap between the best and second
    /// best candidate, relative to the best. It exists so a decoder can mark
    /// doubtful cells as erasures, which doubles what Reed-Solomon can repair
    /// (`SPEC.md` §5.2.3).
    pub confidence: f32,
}

/// A per-frame classifier, fitted to that frame's calibration ring.
///
/// Fitting per frame rather than per transfer is not an optimisation. Exposure
/// and white balance move continuously while a camera records, so a classifier
/// fitted to frame 1 is already wrong by frame 40. The calibration ring exists
/// so that the references and the payload pass through identical optics at an
/// identical instant.
#[derive(Debug, Clone)]
pub struct Classifier {
    alphabet: Alphabet,
    /// Measured mean ink colour per colour index.
    colour_refs: Vec<Rgbf>,
    /// Measured mean ink luma, used to normalise the shape decision.
    ink_level: f32,
}

impl Classifier {
    /// Fits a classifier to labelled samples, which the caller takes from the
    /// calibration ring (`SPEC.md` §4.2.4).
    ///
    /// Falls back to the nominal palette for any colour the ring did not
    /// exercise. That should not happen for a well-formed frame, but a decoder
    /// working from a partly obscured recording is exactly the case this format
    /// is built for, and losing a whole frame because three cells of the ring
    /// were behind a reflection would be a poor trade.
    #[must_use]
    pub fn fit(alphabet: Alphabet, labelled: &[(u16, CellSample)]) -> Self {
        let mut sums = vec![Rgbf::ZERO; alphabet.colours.len()];
        let mut counts = vec![0u32; alphabet.colours.len()];
        let mut ink_sum = 0.0f32;
        let mut ink_count = 0u32;

        for (value, sample) in labelled {
            let (colour, shape) = alphabet.split(*value);
            let Some(mask) = alphabet.shapes.get(shape) else { continue };
            let Some(slot) = sums.get_mut(colour) else { continue };
            let ink = sample.ink_colour(*mask);
            *slot = slot.added(ink);
            counts[colour] += 1;
            ink_sum += ink.luma();
            ink_count += 1;
        }

        let colour_refs =
            sums.iter()
                .zip(counts.iter())
                .enumerate()
                .map(|(i, (sum, &n))| {
                    if n == 0 {
                        Rgbf::from(alphabet.colours[i])
                    } else {
                        sum.scaled(1.0 / n as f32)
                    }
                })
                .collect();

        let ink_level =
            if ink_count == 0 { 0.5 } else { (ink_sum / ink_count as f32).max(f32::EPSILON) };

        Self { alphabet, colour_refs, ink_level }
    }

    /// The alphabet this classifier was fitted for.
    #[must_use]
    pub fn alphabet(&self) -> Alphabet {
        self.alphabet
    }

    /// Classifies one measured cell.
    ///
    /// Shape first, then colour. The order matters: knowing the shape says which
    /// sub-cells hold ink, and averaging only those keeps the black background
    /// out of the colour estimate. Averaging the whole cell instead would drag
    /// every colour halfway to black and collapse the white/grey pair, which is
    /// already the weakest distinction in the 8-colour palette.
    #[must_use]
    pub fn classify(&self, sample: &CellSample) -> Classification {
        let (shape, shape_margin) = self.classify_shape(sample);
        let mask = self.alphabet.shapes[shape];
        let (colour, colour_margin) = self.classify_colour(sample, mask);

        Classification {
            value: self.alphabet.join(colour, shape),
            // A cell is only as trustworthy as its weaker half.
            confidence: shape_margin.min(colour_margin),
        }
    }

    /// Correlates the cell's luma pattern against every mask.
    ///
    /// Correlation rather than distance, because the absolute brightness of a
    /// cell depends on the local illumination, on the ink colour (blue carries
    /// far less luma than white) and on the camera's exposure, none of which say
    /// anything about which shape was painted.
    fn classify_shape(&self, sample: &CellSample) -> (usize, f32) {
        let lumas = sample.lumas();
        let mean = lumas.iter().sum::<f32>() / SHAPE_SUBCELLS as f32;
        let centred: [f32; SHAPE_SUBCELLS] = core::array::from_fn(|i| lumas[i] - mean);
        let energy = centred.iter().map(|v| v * v).sum::<f32>().sqrt();

        // A flat cell carries no shape information at all. Rather than let
        // floating-point noise pick a winner, report shape 0 with no confidence
        // so the cell becomes an erasure candidate.
        if energy < f32::EPSILON {
            return (0, 0.0);
        }

        let mut best = (0usize, f32::NEG_INFINITY);
        let mut second = f32::NEG_INFINITY;

        for (index, &mask) in self.alphabet.shapes.iter().enumerate() {
            // Masks are balanced, so the centred template is +/-0.5 everywhere
            // and its norm is the same for every shape; the correlation is
            // therefore directly comparable across shapes.
            let mut score = 0.0f32;
            for y in 0..SHAPE_GRID {
                for x in 0..SHAPE_GRID {
                    let template = if mask_bit(mask, y, x) { 0.5 } else { -0.5 };
                    score = centred[(y * SHAPE_GRID + x) as usize].mul_add(template, score);
                }
            }

            if score > best.1 {
                second = best.1;
                best = (index, score);
            } else if score > second {
                second = score;
            }
        }

        let margin = margin_of(best.1, second, energy);
        (best.0, margin)
    }

    /// Nearest measured centroid in raw RGB.
    ///
    /// Raw rather than chromaticity-normalised, because white and grey share a
    /// chromaticity and differ only in luminance (`SPEC.md` Q2). Normalising
    /// would make them indistinguishable by construction.
    fn classify_colour(&self, sample: &CellSample, mask: u16) -> (usize, f32) {
        let ink = sample.ink_colour(mask);

        let mut best = (0usize, f32::INFINITY);
        let mut second = f32::INFINITY;
        for (index, reference) in self.colour_refs.iter().enumerate() {
            let d = ink.distance_squared(*reference);
            if d < best.1 {
                second = best.1;
                best = (index, d);
            } else if d < second {
                second = d;
            }
        }

        // Distances are squared, so compare in the linear domain; scale by the
        // measured ink level so the margin means the same thing under any
        // exposure.
        let margin = margin_of(second.sqrt(), best.1.sqrt(), self.ink_level);
        (best.0, margin)
    }
}

/// Normalises the gap between a winner and a runner-up into `[0, 1]`.
fn margin_of(best: f32, second: f32, scale: f32) -> f32 {
    if scale <= f32::EPSILON {
        return 0.0;
    }
    ((best - second) / scale).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::PROFILES;

    fn render_ideal(alphabet: Alphabet, value: u16) -> CellSample {
        let (colour, shape) = alphabet.split(value);
        let ink = Rgbf::from(alphabet.colours[colour]);
        let mask = alphabet.shapes[shape];
        let mut sample = CellSample::zeroed();
        for y in 0..SHAPE_GRID {
            for x in 0..SHAPE_GRID {
                sample.sub[(y * SHAPE_GRID + x) as usize] =
                    if mask_bit(mask, y, x) { ink } else { Rgbf::ZERO };
            }
        }
        sample
    }

    fn calibration_for(alphabet: Alphabet) -> Vec<(u16, CellSample)> {
        (0..alphabet.len())
            .map(|v| {
                let value = u16::try_from(v).unwrap();
                (value, render_ideal(alphabet, value))
            })
            .collect()
    }

    #[test]
    fn every_mask_is_balanced() {
        // Unbalanced ink would make a cell's colour energy depend on its shape,
        // coupling the two classifiers that the format keeps apart.
        for mask in SHAPES_4.iter().chain(SHAPES_8.iter()) {
            assert_eq!(
                mask.count_ones() as usize,
                SHAPE_INK_SUBCELLS,
                "mask {mask:#06X} paints {} sub-cells",
                mask.count_ones()
            );
        }
    }

    #[test]
    fn masks_are_distinct_within_an_alphabet() {
        for alphabet in [&SHAPES_4[..], &SHAPES_8[..]] {
            let mut seen = alphabet.to_vec();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), alphabet.len(), "duplicate mask in {alphabet:04X?}");
        }
    }

    #[test]
    fn masks_match_the_patterns_printed_in_the_specification() {
        // Spot-checks the bit layout of SPEC.md 4.3.1: sub-cell (y, x) is bit
        // 4y + x. If this convention were ever transposed, every table in the
        // specification would still look right while the format silently
        // mirrored itself.
        let rows = |mask: u16| -> Vec<String> {
            (0..SHAPE_GRID)
                .map(|y| {
                    (0..SHAPE_GRID).map(|x| if mask_bit(mask, y, x) { '#' } else { '.' }).collect()
                })
                .collect()
        };

        assert_eq!(rows(0x00FF), ["####", "####", "....", "...."]);
        assert_eq!(rows(0x3333), ["##..", "##..", "##..", "##.."]);
        assert_eq!(rows(0xCCCC), ["..##", "..##", "..##", "..##"]);
        assert_eq!(rows(0xFF00), ["....", "....", "####", "####"]);
        assert_eq!(rows(0x1DF0), ["....", "####", "#.##", "#..."]);
        assert_eq!(rows(0x662E), [".###", ".#..", ".##.", ".##."]);
        assert_eq!(rows(0x8CCE), [".###", "..##", "..##", "...#"]);
        assert_eq!(rows(0xC837), ["###.", "##..", "...#", "..##"]);
        assert_eq!(rows(0xF710), ["....", "#...", "###.", "####"]);
        assert_eq!(rows(0xFC88), ["...#", "...#", "..##", "####"]);
    }

    #[test]
    fn cell_values_round_trip_through_the_packing() {
        for profile in &PROFILES {
            let alphabet = Alphabet::for_profile(profile);
            assert_eq!(alphabet.bits_per_cell(), profile.bits_per_cell());
            for value in 0..u16::try_from(alphabet.len()).unwrap() {
                let (colour, shape) = alphabet.split(value);
                assert!(colour < alphabet.colours.len());
                assert!(shape < alphabet.shapes.len());
                assert_eq!(alphabet.join(colour, shape), value);
            }
        }
    }

    #[test]
    fn a_clean_cell_classifies_exactly() {
        for profile in &PROFILES {
            let alphabet = Alphabet::for_profile(profile);
            let classifier = Classifier::fit(alphabet, &calibration_for(alphabet));
            for v in 0..alphabet.len() {
                let value = u16::try_from(v).unwrap();
                let got = classifier.classify(&render_ideal(alphabet, value));
                assert_eq!(got.value, value, "{} value {value}", profile.name);
                assert!(got.confidence > 0.0, "{} value {value} had no margin", profile.name);
            }
        }
    }

    #[test]
    fn classification_survives_a_global_exposure_shift() {
        // The camera decides the exposure, not the emitter. A classifier fitted
        // to the same frame's calibration ring must not care what it chose.
        for profile in &PROFILES {
            let alphabet = Alphabet::for_profile(profile);
            for gain in [0.35f32, 0.6, 1.4] {
                let dim = |mut s: CellSample| {
                    for sub in &mut s.sub {
                        *sub = sub.scaled(gain);
                    }
                    s
                };
                let calibration: Vec<_> =
                    calibration_for(alphabet).into_iter().map(|(v, s)| (v, dim(s))).collect();
                let classifier = Classifier::fit(alphabet, &calibration);

                for v in 0..alphabet.len() {
                    let value = u16::try_from(v).unwrap();
                    let got = classifier.classify(&dim(render_ideal(alphabet, value)));
                    assert_eq!(got.value, value, "{} gain {gain} value {value}", profile.name);
                }
            }
        }
    }

    #[test]
    fn a_featureless_cell_reports_no_confidence() {
        // A cell washed out to a flat patch carries no shape information. The
        // classifier must say so rather than let rounding noise elect a winner,
        // because the Reed-Solomon layer can repair an erasure twice as cheaply
        // as an error.
        let alphabet = Alphabet::for_profile(&PROFILES[1]);
        let classifier = Classifier::fit(alphabet, &calibration_for(alphabet));
        let flat = CellSample { sub: [Rgbf::new(0.5, 0.5, 0.5); SHAPE_SUBCELLS] };
        assert!(classifier.classify(&flat).confidence < f32::EPSILON);
    }

    #[test]
    fn palettes_are_separated_in_raw_rgb() {
        // The classifier searches nearest centroid in raw RGB, so two palette
        // entries that sit close together would be a permanent error source no
        // amount of calibration can fix.
        for palette in [&PALETTE_4[..], &PALETTE_8[..]] {
            let mut worst = f32::INFINITY;
            for (i, a) in palette.iter().enumerate() {
                for b in palette.iter().skip(i + 1) {
                    worst = worst.min(Rgbf::from(*a).distance_squared(Rgbf::from(*b)).sqrt());
                }
            }
            assert!(worst > 0.25, "closest palette pair is only {worst:.3} apart");
        }
    }
}
