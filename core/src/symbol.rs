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

/// Numbers in a template: three channels for each sub-cell.
const TEMPLATE_LEN: usize = SHAPE_SUBCELLS * 3;

/// How far a cell's brightness may stray from its template's before the
/// difference counts against the match.
///
/// A photographed screen is not evenly bright: the lens darkens the corners,
/// the backlight is uneven, and a screen seen at an angle is brighter at the
/// near edge. None of that says anything about which symbol a cell holds, so
/// the match allows each cell a gain of its own. The allowance is bounded
/// because brightness is sometimes the whole difference between two symbols —
/// white and grey in the 8-colour palette — and an unbounded gain would make
/// those identical.
const GAIN_RANGE: (f32, f32) = (0.6, 1.5);

/// How alike two templates must be, as the cosine of the angle between them,
/// before only their brightness is taken to tell them apart.
const SAME_PATTERN: f32 = 0.97;

/// How much of the brightness gap between two such templates a cell's own gain
/// may cover, as a power of their ratio.
///
/// A third each way leaves the middle third between them, which is what keeps
/// a dim white from being read as a bright grey.
const GAIN_SHARE: f32 = 0.33;

/// A per-frame classifier, fitted to that frame's calibration ring.
///
/// Fitting per frame rather than per transfer is not an optimisation. Exposure
/// and white balance move continuously while a camera records, so a classifier
/// fitted to frame 1 is already wrong by frame 40. The calibration ring exists
/// so that the references and the payload pass through identical optics at an
/// identical instant.
///
/// What is fitted is a whole picture of each symbol — every sub-cell, in colour
/// — rather than a colour per ink and a shape per mask. The two cannot be
/// decided apart. Shape is a pattern of *this ink* against black, and an ink is
/// only as bright as its colour: blue carries a quarter of the luma white does.
/// Judged by luma, a blue cell's own pattern is fainter than what its bright
/// neighbours spill into it, and a real camera then reads a quarter of all
/// blue cells as the wrong shape while reading every green and white one
/// correctly. Judged in colour, against a template of what that symbol looked
/// like through this lens a moment ago, the spill from a white neighbour is
/// simply not blue enough to matter.
#[derive(Debug, Clone)]
pub struct Classifier {
    alphabet: Alphabet,
    /// What each cell value looks like, black already subtracted, and already
    /// multiplied by the weights.
    templates: Vec<[f32; TEMPLATE_LEN]>,
    /// Weighted squared length of each template.
    energies: Vec<f32>,
    /// How much each of the 48 numbers is trusted.
    weights: [f32; TEMPLATE_LEN],
    /// The gain a cell is allowed against a template: lowest and highest.
    gain: (f32, f32),
    /// What an unpainted sub-cell looks like.
    black: Rgbf,
}

/// Cells of one value a refit needs before it will replace that value's
/// template with what it measured.
const MIN_REFIT_CELLS: u32 = 12;

impl Classifier {
    /// Fits a classifier to labelled samples, which the caller takes from the
    /// calibration ring (`SPEC.md` §4.2.4).
    ///
    /// Falls back to the nominal palette for any symbol the ring did not
    /// exercise. That should not happen for a well-formed frame, but a decoder
    /// working from a partly obscured recording is exactly the case this format
    /// is built for, and losing a whole frame because three cells of the ring
    /// were behind a reflection would be a poor trade.
    #[must_use]
    pub fn fit(alphabet: Alphabet, labelled: &[(u16, CellSample)]) -> Self {
        let values = alphabet.len();

        // Black first: every template is measured relative to it, so that
        // stray light lifting the whole frame does not read as ink.
        let mut black_sum = Rgbf::ZERO;
        let mut black_count = 0u32;
        for (value, sample) in labelled {
            let (_, shape) = alphabet.split(*value);
            let Some(&mask) = alphabet.shapes.get(shape) else { continue };
            for y in 0..SHAPE_GRID {
                for x in 0..SHAPE_GRID {
                    if !mask_bit(mask, y, x) {
                        black_sum = black_sum.added(sample.sub[(y * SHAPE_GRID + x) as usize]);
                        black_count += 1;
                    }
                }
            }
        }
        // The darkest the unpainted sub-cells get is nearer the truth than
        // their mean, which includes whatever the painted half spilt into
        // them. Halving the mean is a cheap stand-in for that.
        let black =
            if black_count == 0 { Rgbf::ZERO } else { black_sum.scaled(0.5 / black_count as f32) };

        let mut sums = vec![[0.0f32; TEMPLATE_LEN]; values];
        let mut counts = vec![0u32; values];
        for (value, sample) in labelled {
            let index = usize::from(*value);
            let Some(slot) = sums.get_mut(index) else { continue };
            for (sub, colour) in sample.sub.iter().enumerate() {
                slot[sub * 3] += colour.r - black.r;
                slot[sub * 3 + 1] += colour.g - black.g;
                slot[sub * 3 + 2] += colour.b - black.b;
            }
            counts[index] += 1;
        }

        let templates: Vec<[f32; TEMPLATE_LEN]> = sums
            .iter()
            .zip(counts.iter())
            .enumerate()
            .map(|(index, (sum, &count))| {
                if count == 0 {
                    nominal_template(alphabet, index)
                } else {
                    core::array::from_fn(|i| sum[i] / count as f32)
                }
            })
            .collect();

        Self::assemble(alphabet, &templates, [1.0; TEMPLATE_LEN], black)
    }

    fn assemble(
        alphabet: Alphabet,
        templates: &[[f32; TEMPLATE_LEN]],
        weights: [f32; TEMPLATE_LEN],
        black: Rgbf,
    ) -> Self {
        let energies = templates
            .iter()
            .map(|t| t.iter().zip(weights.iter()).map(|(v, w)| v * v * w).sum::<f32>().max(1e-9))
            .collect();
        let weighted =
            templates.iter().map(|t| core::array::from_fn(|i| t[i] * weights[i])).collect();
        let gain = gain_range(templates);
        Self { alphabet, templates: weighted, energies, weights, gain, black }
    }

    /// Fits again, to the frame's own payload.
    ///
    /// The calibration ring is a few hundred cells at the edge of the frame,
    /// with the timing ring on one side of them and a header band on the other.
    /// The payload is thousands of cells everywhere else, each surrounded by
    /// other payload. Through a lens that blurs, a cell's appearance depends on
    /// its neighbours, so the ring describes cells in a neighbourhood the
    /// payload does not have.
    ///
    /// Once the payload has been read once, most of it is known — nine cells in
    /// ten even on a capture too poor to decode. Averaging the cells read as
    /// each value gives templates measured where they will be used, from twenty
    /// times the evidence. How much the cells of one value differ among
    /// themselves, number by number, says which parts of a cell to trust: the
    /// rim, where the neighbours spill in, varies more than the middle and
    /// counts for less.
    ///
    /// `samples` and `calls` are the payload cells and what this classifier
    /// made of them, in the same order.
    #[must_use]
    pub fn refit(&self, samples: &[CellSample], calls: &[Classification]) -> Self {
        let values = self.alphabet.len();
        let mut sums = vec![[0.0f64; TEMPLATE_LEN]; values];
        let mut counts = vec![0u32; values];

        let centred = |sample: &CellSample| -> [f32; TEMPLATE_LEN] {
            let mut cell = [0.0f32; TEMPLATE_LEN];
            for (sub, colour) in sample.sub.iter().enumerate() {
                cell[sub * 3] = colour.r - self.black.r;
                cell[sub * 3 + 1] = colour.g - self.black.g;
                cell[sub * 3 + 2] = colour.b - self.black.b;
            }
            cell
        };

        for (sample, call) in samples.iter().zip(calls.iter()) {
            let Some(slot) = sums.get_mut(usize::from(call.value)) else { continue };
            for (sum, value) in slot.iter_mut().zip(centred(sample).iter()) {
                *sum += f64::from(*value);
            }
            counts[usize::from(call.value)] += 1;
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "a mean of values in [-1, 1] fits an f32"
        )]
        let templates: Vec<[f32; TEMPLATE_LEN]> = (0..values)
            .map(|index| {
                if counts[index] < MIN_REFIT_CELLS {
                    // Too few to measure. Keep what the ring said, undoing the
                    // weights it was stored with.
                    core::array::from_fn(|i| self.templates[index][i] / self.weights[i].max(1e-9))
                } else {
                    core::array::from_fn(|i| (sums[index][i] / f64::from(counts[index])) as f32)
                }
            })
            .collect();

        // Spread of the cells about their own template, pooled over every
        // value: one number per sub-cell and channel.
        let mut spread = [0.0f64; TEMPLATE_LEN];
        let mut measured = 0u64;
        for (sample, call) in samples.iter().zip(calls.iter()) {
            let Some(template) = templates.get(usize::from(call.value)) else { continue };
            for ((slot, value), expected) in
                spread.iter_mut().zip(centred(sample).iter()).zip(template.iter())
            {
                let difference = f64::from(value - expected);
                *slot += difference * difference;
            }
            measured += 1;
        }

        let mut weights = [1.0f32; TEMPLATE_LEN];
        if measured > 0 {
            let mean: f64 = spread.iter().sum::<f64>() / (TEMPLATE_LEN as f64 * measured as f64);
            // A floor on the spread, so that one number that happens to vary
            // little cannot be given the whole decision.
            let floor = (mean * 0.25).max(1e-6);
            for (weight, total) in weights.iter_mut().zip(spread.iter()) {
                let variance = (total / measured as f64).max(floor);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a ratio of two variances of the same order"
                )]
                let value = (mean.max(floor) / variance) as f32;
                *weight = value;
            }
        }

        Self::assemble(self.alphabet, &templates, weights, self.black)
    }

    /// The alphabet this classifier was fitted for.
    #[must_use]
    pub fn alphabet(&self) -> Alphabet {
        self.alphabet
    }

    /// Classifies one measured cell.
    ///
    /// The cell is compared against every symbol's template and the nearest
    /// wins, with each comparison allowed a gain of its own inside
    /// [`GAIN_RANGE`].
    #[must_use]
    #[expect(clippy::suboptimal_flops, reason = "see the note on the inner loop")]
    pub fn classify(&self, sample: &CellSample) -> Classification {
        let mut cell = [0.0f32; TEMPLATE_LEN];
        for (sub, colour) in sample.sub.iter().enumerate() {
            cell[sub * 3] = colour.r - self.black.r;
            cell[sub * 3 + 1] = colour.g - self.black.g;
            cell[sub * 3 + 2] = colour.b - self.black.b;
        }
        let weighted: [f32; TEMPLATE_LEN] =
            core::array::from_fn(|i| cell[i] * cell[i] * self.weights[i]);
        let energy = sum_lanes(&weighted);

        let mut best = (0usize, f32::INFINITY);
        let mut second = f32::INFINITY;

        for (index, (template, &template_energy)) in
            self.templates.iter().zip(self.energies.iter()).enumerate()
        {
            // Written out rather than fused: WebAssembly has no fused
            // multiply-add and emulates one in software, in the one loop of
            // the decoder that runs a million times a frame.
            let dot = dot_lanes(&cell, template);
            let gain = (dot / template_energy).clamp(self.gain.0, self.gain.1);
            // |cell - gain * template|^2, expanded so the products above are
            // the only pass over the 48 numbers.
            let distance = gain * gain * template_energy + energy - 2.0 * gain * dot;

            if distance < best.1 {
                second = best.1;
                best = (index, distance);
            } else if distance < second {
                second = distance;
            }
        }

        // The gap between the two nearest templates, measured against how much
        // signal the cell held. A flat cell is equally far from everything and
        // reports no confidence at all.
        let confidence = if energy <= f32::EPSILON || !second.is_finite() {
            0.0
        } else {
            ((second.max(0.0).sqrt() - best.1.max(0.0).sqrt()) / energy.sqrt()).clamp(0.0, 1.0)
        };

        Classification { value: u16::try_from(best.0).unwrap_or(0), confidence }
    }
}

/// The gain a cell may be given against these templates.
///
/// As wide as [`GAIN_RANGE`] when every symbol differs from every other in
/// pattern or in hue, which is every alphabet built on the 4-colour palette.
/// Where two differ in brightness alone, the range is drawn in until it covers
/// only part of the gap between them.
fn gain_range(templates: &[[f32; TEMPLATE_LEN]]) -> (f32, f32) {
    let lengths: Vec<f32> = templates.iter().map(|t| dot_lanes(t, t).sqrt()).collect();
    let mut closest = f32::INFINITY;

    for (i, a) in templates.iter().enumerate() {
        for (j, b) in templates.iter().enumerate().skip(i + 1) {
            let (la, lb) = (lengths[i], lengths[j]);
            if la <= f32::EPSILON || lb <= f32::EPSILON {
                continue;
            }
            if dot_lanes(a, b) / (la * lb) < SAME_PATTERN {
                continue;
            }
            closest = closest.min(la.max(lb) / la.min(lb));
        }
    }

    if !closest.is_finite() {
        return GAIN_RANGE;
    }
    let reach = closest.max(1.0).powf(GAIN_SHARE);
    ((1.0 / reach).max(GAIN_RANGE.0), reach.min(GAIN_RANGE.1))
}

/// Lanes the sums below are kept in.
const LANES: usize = 8;

/// The sum of 48 numbers, kept in eight running totals.
///
/// Floating-point addition is not associative, so a compiler may not reorder a
/// plain running sum and therefore cannot do several additions at once. Eight
/// totals state the order outright, which lets it.
#[inline]
fn sum_lanes(values: &[f32; TEMPLATE_LEN]) -> f32 {
    let mut lanes = [0.0f32; LANES];
    for chunk in values.as_chunks::<LANES>().0 {
        for (lane, value) in lanes.iter_mut().zip(chunk.iter()) {
            *lane += value;
        }
    }
    lanes.iter().sum()
}

/// The inner product of two templates' worth of numbers.
#[inline]
fn dot_lanes(a: &[f32; TEMPLATE_LEN], b: &[f32; TEMPLATE_LEN]) -> f32 {
    let mut lanes = [0.0f32; LANES];
    for (left, right) in a.as_chunks::<LANES>().0.iter().zip(b.as_chunks::<LANES>().0) {
        for ((lane, p), q) in lanes.iter_mut().zip(left.iter()).zip(right.iter()) {
            *lane += p * q;
        }
    }
    lanes.iter().sum()
}

/// What a symbol would look like with no camera in the way.
fn nominal_template(alphabet: Alphabet, value: usize) -> [f32; TEMPLATE_LEN] {
    let (colour, shape) = alphabet.split(u16::try_from(value).unwrap_or(0));
    let ink = alphabet.colours.get(colour).copied().map_or(Rgbf::ZERO, Rgbf::from);
    let mask = alphabet.shapes.get(shape).copied().unwrap_or(0);

    let mut template = [0.0f32; TEMPLATE_LEN];
    for y in 0..SHAPE_GRID {
        for x in 0..SHAPE_GRID {
            if mask_bit(mask, y, x) {
                let sub = (y * SHAPE_GRID + x) as usize;
                template[sub * 3] = ink.r;
                template[sub * 3 + 1] = ink.g;
                template[sub * 3 + 2] = ink.b;
            }
        }
    }
    template
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

    /// A cell with some of its neighbours' light spilt into it.
    fn spilt_into(mut sample: CellSample, spill: Rgbf) -> CellSample {
        for sub in &mut sample.sub {
            *sub = sub.added(spill);
        }
        sample
    }

    #[test]
    fn a_dim_ink_beside_bright_neighbours_keeps_its_shape() {
        // The fault that stopped every real transfer. The palette's blue has a
        // quarter of the luma its white does, so a blue cell's own pattern is
        // fainter in luma than what bright neighbours spill into its unpainted
        // half. Judged by luma a quarter of all blue cells came out as the
        // wrong shape, which put every frame over what its parity could repair.
        //
        // Here white neighbours have spilt three tenths of themselves into the
        // bottom half of a cell whose top half is blue. By luma the bottom is
        // now the brighter half: the blue is 0.26, and the spill is 0.30.
        let alphabet = Alphabet::for_profile(&PROFILES[0]);
        let classifier = Classifier::fit(alphabet, &calibration_for(alphabet));

        let blue = 2;
        let top = 0;
        let value = alphabet.join(blue, top);
        let mut sample = render_ideal(alphabet, value);
        for index in SHAPE_SUBCELLS / 2..SHAPE_SUBCELLS {
            sample.sub[index] = Rgbf::new(0.3, 0.3, 0.3);
        }

        let lumas = sample.lumas();
        let upper: f32 = lumas[..SHAPE_SUBCELLS / 2].iter().sum();
        let lower: f32 = lumas[SHAPE_SUBCELLS / 2..].iter().sum();
        assert!(lower > upper, "the test does not set up what it claims to");

        assert_eq!(classifier.classify(&sample).value, value);
    }

    #[test]
    fn every_symbol_survives_light_spilt_evenly_across_it() {
        // Stray light inside a lens lifts everything by about the same amount.
        // It is not ink, and must not be read as any.
        for profile in &PROFILES[..2] {
            let alphabet = Alphabet::for_profile(profile);
            let classifier = Classifier::fit(alphabet, &calibration_for(alphabet));
            for v in 0..alphabet.len() {
                let value = u16::try_from(v).unwrap();
                let sample = spilt_into(render_ideal(alphabet, value), Rgbf::new(0.1, 0.1, 0.1));
                assert_eq!(
                    classifier.classify(&sample).value,
                    value,
                    "{} value {value}",
                    profile.name
                );
            }
        }
    }

    #[test]
    fn white_and_grey_are_told_apart_by_brightness_alone() {
        // The one pair in any palette that differs in nothing else. A cell is
        // allowed a gain of its own so that uneven lighting costs nothing, and
        // left unbounded that gain would make these two the same symbol.
        let alphabet = Alphabet::for_profile(&PROFILES[2]);
        let classifier = Classifier::fit(alphabet, &calibration_for(alphabet));
        let (white, grey) = (6, 7);

        for shape in 0..alphabet.shapes.len() {
            for colour in [white, grey] {
                let value = alphabet.join(colour, shape);
                // A tenth dimmer and a tenth brighter than it was painted.
                for gain in [0.9f32, 1.1] {
                    let mut sample = render_ideal(alphabet, value);
                    for sub in &mut sample.sub {
                        *sub = sub.scaled(gain);
                    }
                    assert_eq!(
                        classifier.classify(&sample).value,
                        value,
                        "colour {colour} shape {shape} at gain {gain}"
                    );
                }
            }
        }
    }

    #[test]
    fn refitting_to_the_payload_learns_what_the_ring_could_not() {
        // The ring's cells have the timing ring and a header band for
        // neighbours; the payload's have each other. Through a lens that
        // blurs, the same symbol looks different in the two places. Here the
        // payload's cells all carry a spill the ring's never saw, large enough
        // that templates from the ring misread some of them.
        let alphabet = Alphabet::for_profile(&PROFILES[1]);
        let from_ring = Classifier::fit(alphabet, &calibration_for(alphabet));

        let spill = Rgbf::new(0.30, 0.22, 0.05);
        let wanted: Vec<u16> =
            (0..640).map(|i| u16::try_from(i % alphabet.len()).unwrap()).collect();
        let samples: Vec<CellSample> =
            wanted.iter().map(|&v| spilt_into(render_ideal(alphabet, v), spill)).collect();

        let wrong = |classifier: &Classifier| {
            samples
                .iter()
                .zip(wanted.iter())
                .filter(|(sample, want)| classifier.classify(sample).value != **want)
                .count()
        };

        let first: Vec<Classification> = samples.iter().map(|s| from_ring.classify(s)).collect();
        let refitted = from_ring.refit(&samples, &first);

        let before = wrong(&from_ring);
        let after = wrong(&refitted);
        assert!(after <= before, "refitting made it worse: {before} wrong became {after}");
        assert_eq!(after, 0, "refitting left {after} of {} wrong, from {before}", samples.len());
    }

    #[test]
    fn refitting_to_too_few_cells_keeps_what_the_ring_said() {
        // A value the first reading found three times has not been measured.
        let alphabet = Alphabet::for_profile(&PROFILES[0]);
        let from_ring = Classifier::fit(alphabet, &calibration_for(alphabet));

        let samples: Vec<CellSample> = (0..3).map(|_| render_ideal(alphabet, 5)).collect();
        let first: Vec<Classification> = samples.iter().map(|s| from_ring.classify(s)).collect();
        let refitted = from_ring.refit(&samples, &first);

        for v in 0..alphabet.len() {
            let value = u16::try_from(v).unwrap();
            assert_eq!(refitted.classify(&render_ideal(alphabet, value)).value, value);
        }
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
