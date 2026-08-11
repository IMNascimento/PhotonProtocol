//! Frame geometry: which cell is what, how a frame is painted, how it is read.
//!
//! `SPEC.md` §4.2 divides the code area into regions, and every one of them
//! exists to answer a question the decoder cannot answer any other way:
//!
//! | Region | Question it answers |
//! | ------ | ------------------- |
//! | finder patterns | where is the code |
//! | orientation tag | which way up |
//! | timing ring | where exactly are the cell boundaries |
//! | calibration ring | what does each symbol look like *through this camera, now* |
//! | alignment marker | is the transform I just computed actually right |
//! | header bands | what am I looking at |
//!
//! Together they cost between 9% and 17% of the frame. That is the price of a
//! channel where nothing can ever be asked for twice.

use crate::image::RgbImage;
use crate::profile::Profile;
use crate::symbol::{Alphabet, CellSample, Rgb, Rgbf, SHAPE_GRID, mask_bit};
use crate::{Homography, Point};

/// Width of the white margin around the code area, in cells (`SPEC.md` §4.1).
pub const QUIET_ZONE_CELLS: u32 = 4;

/// Side of a finder box, pattern plus separator (`SPEC.md` §4.2.1).
pub const FINDER_BOX: u32 = 8;

/// Side of the finder pattern itself.
pub const FINDER_PATTERN: u32 = 7;

/// Side of the alignment module, pattern plus separator (`SPEC.md` §4.2.5).
pub const ALIGNMENT_MODULE: u32 = 7;

/// Side of the alignment pattern itself.
pub const ALIGNMENT_PATTERN: u32 = 5;

/// Side of the orientation tag (`SPEC.md` §4.2.2).
pub const ORIENTATION_TAG: u32 = 3;

/// The finder pattern, one bit per cell, most significant bit leftmost.
/// The 1:1:3:1:1 scanline ratio survives blur and is invariant along any line
/// through the centre under perspective, which is what makes it findable.
const FINDER_ROWS: [u8; FINDER_PATTERN as usize] =
    [0b111_1111, 0b100_0001, 0b101_1101, 0b101_1101, 0b101_1101, 0b100_0001, 0b111_1111];

/// The alignment pattern, one bit per cell, most significant bit leftmost.
const ALIGNMENT_ROWS: [u8; ALIGNMENT_PATTERN as usize] =
    [0b1_1111, 0b1_0001, 0b1_0101, 0b1_0001, 0b1_1111];

/// What a cell is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    /// Part of a corner finder box: the pattern, or its white separator.
    Finder,
    /// The orientation tag that breaks the four-fold rotational symmetry.
    Orientation,
    /// The alternating outer ring used to recover the cell lattice.
    Timing,
    /// The labelled symbol samples the classifier is fitted to.
    Calibration,
    /// The centre alignment module.
    Alignment,
    /// One of the two header bands.
    Header,
    /// Payload.
    Data,
}

/// The cell map of a frame, plus the orderings the codec reads it in.
///
/// Built once per profile and reused. Every ordering the specification defines —
/// data cells in raster order, calibration cells clockwise, header cells per
/// band — is materialised here rather than recomputed, because each is a place
/// where an encoder and a decoder could disagree silently.
#[derive(Debug, Clone)]
pub struct FrameLayout {
    profile: Profile,
    alphabet: Alphabet,
    regions: Vec<Region>,
    data_cells: Vec<u32>,
    calibration_cells: Vec<u32>,
    header_bands: [Vec<u32>; 2],
}

impl FrameLayout {
    /// Builds the layout for a profile.
    #[must_use]
    pub fn new(profile: &Profile) -> Self {
        let grid = profile.grid;
        let header_rows = profile.header_rows();
        let mut regions = vec![Region::Data; (grid * grid) as usize];

        for row in 0..grid {
            for col in 0..grid {
                regions[(row * grid + col) as usize] =
                    classify_cell(profile, header_rows, row, col);
            }
        }

        let mut data_cells = Vec::new();
        let mut header_bands = [Vec::new(), Vec::new()];
        for row in 0..grid {
            for col in 0..grid {
                let index = row * grid + col;
                match regions[index as usize] {
                    Region::Data => data_cells.push(index),
                    Region::Header => {
                        let band = usize::from(row >= grid / 2);
                        header_bands[band].push(index);
                    }
                    _ => {}
                }
            }
        }

        Self {
            profile: *profile,
            alphabet: Alphabet::for_profile(profile),
            regions,
            data_cells,
            calibration_cells: calibration_order(grid),
            header_bands,
        }
    }

    /// The profile this layout describes.
    #[must_use]
    pub const fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The alphabet this layout's cells are drawn from.
    #[must_use]
    pub const fn alphabet(&self) -> Alphabet {
        self.alphabet
    }

    /// Side of the code area in cells.
    #[must_use]
    pub const fn grid(&self) -> u32 {
        self.profile.grid
    }

    /// What the cell at `(row, col)` is for.
    ///
    /// # Panics
    ///
    /// Panics if the coordinates fall outside the code area.
    #[must_use]
    pub fn region(&self, row: u32, col: u32) -> Region {
        let grid = self.grid();
        assert!(row < grid && col < grid, "cell ({row}, {col}) is outside a {grid}x{grid} grid");
        self.regions[(row * grid + col) as usize]
    }

    /// Linear indices of the payload cells, in the raster order of `SPEC.md`
    /// §4.4.
    #[must_use]
    pub fn data_cells(&self) -> &[u32] {
        &self.data_cells
    }

    /// Linear indices of the calibration ring, clockwise from `(1, 8)`
    /// (`SPEC.md` §4.2.4).
    #[must_use]
    pub fn calibration_cells(&self) -> &[u32] {
        &self.calibration_cells
    }

    /// Linear indices of one header band, in raster order. Band 0 is the top.
    ///
    /// # Panics
    ///
    /// Panics if `band` is not 0 or 1.
    #[must_use]
    pub fn header_band(&self, band: usize) -> &[u32] {
        assert!(band < 2, "a frame has two header bands, not {band}");
        &self.header_bands[band]
    }

    /// The value the calibration cell at ring position `index` must carry.
    #[must_use]
    pub fn calibration_value(&self, index: usize) -> u16 {
        u16::try_from(index % self.alphabet.len()).unwrap_or(0)
    }

    /// Splits a linear cell index into row and column.
    #[must_use]
    pub const fn coordinates(&self, index: u32) -> (u32, u32) {
        (index / self.profile.grid, index % self.profile.grid)
    }

    /// Side of a rendered frame in pixels, quiet zone included.
    #[must_use]
    pub const fn image_side(&self, cell_px: u32) -> u32 {
        (self.profile.grid + 2 * QUIET_ZONE_CELLS) * cell_px
    }

    /// Paints a frame.
    ///
    /// `header_codeword` is the error-corrected header, written into both bands.
    /// `data_values` supplies one cell value per payload cell, in the order of
    /// [`FrameLayout::data_cells`].
    ///
    /// # Panics
    ///
    /// Panics if `data_values` is not exactly one value per payload cell, or if
    /// the codeword does not fit a header band. Both are internal contract
    /// violations rather than anything a recording could cause.
    #[must_use]
    pub fn render(&self, header_codeword: &[u8], data_values: &[u16], cell_px: u32) -> RgbImage {
        assert_eq!(data_values.len(), self.data_cells.len(), "expected one value per payload cell");
        assert!(
            header_codeword.len() * 8 <= self.header_bands[0].len(),
            "header codeword does not fit its band"
        );

        let grid = self.grid();
        let side = self.image_side(cell_px);
        let mut image = RgbImage::filled(side, side, Rgb::WHITE);

        // The quiet zone is already white; paint the code area over it.
        let origin = QUIET_ZONE_CELLS * cell_px;
        image.fill_rect(origin, origin, grid * cell_px, grid * cell_px, Rgb::BLACK);

        for row in 0..grid {
            for col in 0..grid {
                let x = (col + QUIET_ZONE_CELLS) * cell_px;
                let y = (row + QUIET_ZONE_CELLS) * cell_px;
                match self.region(row, col) {
                    Region::Finder => {
                        image.fill_rect(x, y, cell_px, cell_px, finder_colour(grid, row, col));
                    }
                    Region::Orientation => image.fill_rect(x, y, cell_px, cell_px, Rgb::WHITE),
                    Region::Timing => {
                        image.fill_rect(x, y, cell_px, cell_px, timing_colour(grid, row, col));
                    }
                    Region::Alignment => {
                        image.fill_rect(x, y, cell_px, cell_px, alignment_colour(grid, row, col));
                    }
                    Region::Header | Region::Calibration | Region::Data => {}
                }
            }
        }

        for (band_index, band) in self.header_bands.iter().enumerate() {
            debug_assert!(band_index < 2);
            for (bit_index, &cell) in band.iter().enumerate() {
                let bit = bit_at(header_codeword, bit_index);
                let (row, col) = self.coordinates(cell);
                let x = (col + QUIET_ZONE_CELLS) * cell_px;
                let y = (row + QUIET_ZONE_CELLS) * cell_px;
                let colour = if bit { Rgb::WHITE } else { Rgb::BLACK };
                image.fill_rect(x, y, cell_px, cell_px, colour);
            }
        }

        for (index, &cell) in self.calibration_cells.iter().enumerate() {
            self.paint_symbol(&mut image, cell, self.calibration_value(index), cell_px);
        }

        for (&cell, &value) in self.data_cells.iter().zip(data_values.iter()) {
            self.paint_symbol(&mut image, cell, value, cell_px);
        }

        image
    }

    fn paint_symbol(&self, image: &mut RgbImage, cell: u32, value: u16, cell_px: u32) {
        let (colour_index, shape_index) = self.alphabet.split(value);
        let Some(&mask) = self.alphabet.shapes.get(shape_index) else { return };
        let Some(&ink) = self.alphabet.colours.get(colour_index) else { return };

        let (row, col) = self.coordinates(cell);
        let x0 = (col + QUIET_ZONE_CELLS) * cell_px;
        let y0 = (row + QUIET_ZONE_CELLS) * cell_px;
        let sub_px = cell_px / SHAPE_GRID;

        for y in 0..SHAPE_GRID {
            for x in 0..SHAPE_GRID {
                if mask_bit(mask, y, x) {
                    image.fill_rect(x0 + x * sub_px, y0 + y * sub_px, sub_px, sub_px, ink);
                }
            }
        }
    }

    /// Measures one cell through a homography mapping cell space to pixels.
    ///
    /// Cell space places the code area over `[0, G] x [0, G]`, so the quiet zone
    /// sits at negative coordinates. Each of the 16 sub-cells is averaged from a
    /// 2x2 sample grid: a single centre tap would sit exactly on the boundary
    /// between two painted sub-cells whenever the grid registration is half a
    /// sub-cell out, which is the error the shape alphabet was chosen to
    /// tolerate rather than one it should be handed.
    #[must_use]
    pub fn sample_cell(&self, image: &RgbImage, transform: &Homography, cell: u32) -> CellSample {
        let (row, col) = self.coordinates(cell);
        let mut sample = CellSample::zeroed();
        let step = 1.0 / f64::from(SHAPE_GRID);

        for y in 0..SHAPE_GRID {
            for x in 0..SHAPE_GRID {
                let mut sum = Rgbf::ZERO;
                let mut taps = 0.0f32;
                for (dy, dx) in [(0.25, 0.25), (0.25, 0.75), (0.75, 0.25), (0.75, 0.75)] {
                    let u = f64::from(col) + (f64::from(x) + dx) * step;
                    let v = f64::from(row) + (f64::from(y) + dy) * step;
                    if let Some(p) = transform.map(Point::new(u, v)) {
                        let c = image.sample_bilinear(p.x, p.y);
                        sum = Rgbf::new(sum.r + c.r, sum.g + c.g, sum.b + c.b);
                        taps += 1.0;
                    }
                }
                if taps > 0.0 {
                    sample.sub[(y * SHAPE_GRID + x) as usize] =
                        Rgbf::new(sum.r / taps, sum.g / taps, sum.b / taps);
                }
            }
        }

        sample
    }

    /// Mean colour of a cell, for the regions painted as one solid block.
    #[must_use]
    pub fn sample_cell_mean(&self, image: &RgbImage, transform: &Homography, cell: u32) -> Rgbf {
        let sample = self.sample_cell(image, transform, cell);
        let mut sum = Rgbf::ZERO;
        for sub in &sample.sub {
            sum = Rgbf::new(sum.r + sub.r, sum.g + sub.g, sum.b + sub.b);
        }
        let n = sample.sub.len() as f32;
        Rgbf::new(sum.r / n, sum.g / n, sum.b / n)
    }

    /// The homography mapping cell space onto a frame this layout rendered.
    ///
    /// Useful on its own for tests and for the channel simulator, which needs to
    /// know where the ideal frame sits before it distorts it.
    ///
    /// The half-pixel term is not a fudge. A rendered pixel `k` covers the
    /// continuous interval `[k, k+1)`, but a sampler addresses it by its centre,
    /// which sits at `k + 0.5` in that continuous space — so pixel-centre
    /// coordinates are half a pixel behind continuous ones. Omitting the shift
    /// puts every sample half a pixel late, which at eight pixels per cell drags
    /// the outer tap of each sub-cell into its neighbour and mixes the two.
    /// A detector measuring finder centres from an image works in pixel-centre
    /// coordinates already, so this is the transform it converges to as well.
    #[must_use]
    pub fn identity_transform(&self, cell_px: u32) -> Homography {
        let origin = f64::from(QUIET_ZONE_CELLS * cell_px) - 0.5;
        let scale = f64::from(cell_px);
        Homography::from_coefficients([scale, 0.0, origin, 0.0, scale, origin, 0.0, 0.0, 1.0])
    }

    /// The four finder pattern centres, in cell space, corner-ordered.
    ///
    /// These are the correspondences a detector has to find in the image; having
    /// the ideal side written once keeps the detector and the renderer from
    /// disagreeing about where a centre is.
    #[must_use]
    pub fn finder_centres(&self) -> [Point; 4] {
        let grid = f64::from(self.grid());
        let near = f64::from(FINDER_PATTERN) / 2.0;
        let far = grid - near;
        [Point::new(near, near), Point::new(far, near), Point::new(far, far), Point::new(near, far)]
    }

    /// Centre of the alignment module, in cell space.
    ///
    /// The module spans the seven cells `[G/2-3, G/2+4)`, so its middle cell is
    /// `G/2` and the middle of *that cell* — which is what a sampler wants — is
    /// half a cell further on.
    #[must_use]
    pub fn alignment_centre(&self) -> Point {
        let centre = f64::from(self.grid() / 2) + 0.5;
        Point::new(centre, centre)
    }

    /// Centre of the orientation tag, in cell space.
    #[must_use]
    pub fn orientation_centre(&self) -> Point {
        let grid = f64::from(self.grid());
        let offset = grid - 12.0 + f64::from(ORIENTATION_TAG) / 2.0;
        Point::new(offset, offset)
    }
}

/// Which region a cell belongs to, in the precedence order of `SPEC.md` §4.2.
fn classify_cell(profile: &Profile, header_rows: u32, row: u32, col: u32) -> Region {
    let grid = profile.grid;
    let in_band = (FINDER_BOX..grid - FINDER_BOX).contains(&col);
    let in_side_band = (FINDER_BOX..grid - FINDER_BOX).contains(&row);

    // Finder boxes claim their corners outright; nothing else may overlap them.
    let near_edge = |v: u32| v < FINDER_BOX || v >= grid - FINDER_BOX;
    if near_edge(row) && near_edge(col) {
        return Region::Finder;
    }

    if (row == 0 || row == grid - 1) && in_band {
        return Region::Timing;
    }
    if (col == 0 || col == grid - 1) && in_side_band {
        return Region::Timing;
    }
    if (row == 1 || row == grid - 2) && in_band {
        return Region::Calibration;
    }
    if (col == 1 || col == grid - 2) && in_side_band {
        return Region::Calibration;
    }

    let top_band = 2..2 + header_rows;
    let bottom_band = grid - 2 - header_rows..grid - 2;
    if (top_band.contains(&row) || bottom_band.contains(&row)) && in_band {
        return Region::Header;
    }

    let align = alignment_start(grid)..alignment_start(grid) + ALIGNMENT_MODULE;
    if align.contains(&row) && align.contains(&col) {
        return Region::Alignment;
    }

    let tag_start = grid - 12;
    let tag = tag_start..tag_start + ORIENTATION_TAG;
    if tag.contains(&row) && tag.contains(&col) {
        return Region::Orientation;
    }

    Region::Data
}

/// The calibration ring, clockwise from `(1, 8)` (`SPEC.md` §4.2.4).
fn calibration_order(grid: u32) -> Vec<u32> {
    let mut cells = Vec::new();
    let last = grid - FINDER_BOX;

    // Ring 1: row 1 and row G-2 across the top and bottom, columns 1 and G-2
    // down the sides, always skipping the cells a finder box has claimed.
    for col in FINDER_BOX..last {
        cells.push(grid + col);
    }
    for row in FINDER_BOX..last {
        cells.push(row * grid + (grid - 2));
    }
    for col in (FINDER_BOX..last).rev() {
        cells.push((grid - 2) * grid + col);
    }
    for row in (FINDER_BOX..last).rev() {
        cells.push(row * grid + 1);
    }

    cells
}

fn finder_colour(grid: u32, row: u32, col: u32) -> Rgb {
    // The pattern hugs the outer corner; the separator is the inner strip.
    let pattern_row = if row < FINDER_BOX { row } else { row.wrapping_sub(grid - FINDER_PATTERN) };
    let pattern_col = if col < FINDER_BOX { col } else { col.wrapping_sub(grid - FINDER_PATTERN) };

    if pattern_row < FINDER_PATTERN && pattern_col < FINDER_PATTERN {
        let bit = (FINDER_ROWS[pattern_row as usize] >> (FINDER_PATTERN - 1 - pattern_col)) & 1;
        if bit == 1 { Rgb::BLACK } else { Rgb::WHITE }
    } else {
        Rgb::WHITE
    }
}

fn timing_colour(grid: u32, row: u32, col: u32) -> Rgb {
    let varying = if row == 0 || row == grid - 1 { col } else { row };
    if varying % 2 == 0 { Rgb::BLACK } else { Rgb::WHITE }
}

/// First row and column of the alignment module (`SPEC.md` §4.2.5).
///
/// Written once and shared by the region map, the renderer and the detector.
/// Three places computing "the middle, minus half a module" independently is
/// three chances to be one cell out — and a decoder that samples one cell away
/// from where the encoder painted sees plausible values in the wrong place.
const fn alignment_start(grid: u32) -> u32 {
    grid / 2 - 3
}

fn alignment_colour(grid: u32, row: u32, col: u32) -> Rgb {
    let start = alignment_start(grid);
    let local_row = row - start;
    let local_col = col - start;

    // Local ring 0 is the separator; the pattern sits one cell inside.
    if local_row == 0
        || local_col == 0
        || local_row == ALIGNMENT_MODULE - 1
        || local_col == ALIGNMENT_MODULE - 1
    {
        return Rgb::WHITE;
    }

    let bit = (ALIGNMENT_ROWS[(local_row - 1) as usize] >> (ALIGNMENT_PATTERN - local_col)) & 1;
    if bit == 1 { Rgb::BLACK } else { Rgb::WHITE }
}

/// Bit `index` of a byte slice, most significant bit first (`SPEC.md` §2).
fn bit_at(bytes: &[u8], index: usize) -> bool {
    match bytes.get(index / 8) {
        Some(byte) => (byte >> (7 - index % 8)) & 1 == 1,
        None => false,
    }
}

/// Spreads a byte stream across cell values, `bits` bits each (`SPEC.md` §4.4).
///
/// Values are taken most significant bit first from a bit stream that is itself
/// filled most significant bit first, so the packing is the same convention end
/// to end. Cells past the end of the byte stream carry zero, which is what the
/// specification requires of the frame's unused tail.
#[must_use]
pub fn bytes_to_cells(bytes: &[u8], bits: u32, cell_count: usize) -> Vec<u16> {
    let mut cells = Vec::with_capacity(cell_count);
    for index in 0..cell_count {
        let start = index * bits as usize;
        let mut value = 0u16;
        for offset in 0..bits as usize {
            value = (value << 1) | u16::from(bit_at(bytes, start + offset));
        }
        cells.push(value);
    }
    cells
}

/// Gathers cell values back into a byte stream, the inverse of
/// [`bytes_to_cells`].
#[must_use]
pub fn cells_to_bytes(cells: &[u16], bits: u32, byte_len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; byte_len];
    for (index, &value) in cells.iter().enumerate() {
        let start = index * bits as usize;
        for offset in 0..bits as usize {
            let bit = (value >> (bits as usize - 1 - offset)) & 1;
            if bit == 0 {
                continue;
            }
            let position = start + offset;
            if let Some(byte) = bytes.get_mut(position / 8) {
                *byte |= 1 << (7 - position % 8);
            }
        }
    }
    bytes
}

/// The byte positions a cell's bits land in.
///
/// A cell of 4, 5 or 6 bits rarely aligns to a byte, so a single unreliable cell
/// can taint two bytes. Erasure decoding needs every byte the cell touched, not
/// just the first: leaving the second unmarked would point the corrector at the
/// wrong half of the damage.
#[must_use]
pub fn cell_byte_span(index: usize, bits: u32) -> (usize, usize) {
    let first_bit = index * bits as usize;
    let last_bit = first_bit + bits as usize - 1;
    (first_bit / 8, last_bit / 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::PROFILES;
    use std::collections::HashMap;

    #[test]
    fn every_cell_has_exactly_one_region() {
        for profile in &PROFILES {
            let layout = FrameLayout::new(profile);
            let grid = layout.grid();
            let mut tally: HashMap<Region, u32> = HashMap::new();
            for row in 0..grid {
                for col in 0..grid {
                    *tally.entry(layout.region(row, col)).or_default() += 1;
                }
            }
            let total: u32 = tally.values().sum();
            assert_eq!(total, grid * grid, "{}", profile.name);
        }
    }

    #[test]
    fn region_counts_match_the_specification_budget() {
        // SPEC.md 4.2.7 gives the reserved budget as a closed formula. If the
        // cell map and the formula disagree, one of them is wrong, and every
        // capacity in the specification is computed from the formula.
        for profile in &PROFILES {
            let layout = FrameLayout::new(profile);
            let grid = layout.grid();
            let mut tally: HashMap<Region, u32> = HashMap::new();
            for row in 0..grid {
                for col in 0..grid {
                    *tally.entry(layout.region(row, col)).or_default() += 1;
                }
            }

            let name = profile.name;
            assert_eq!(tally[&Region::Finder], 4 * FINDER_BOX * FINDER_BOX, "{name} finders");
            assert_eq!(
                tally[&Region::Orientation],
                ORIENTATION_TAG * ORIENTATION_TAG,
                "{name} tag"
            );
            assert_eq!(
                tally[&Region::Alignment],
                ALIGNMENT_MODULE * ALIGNMENT_MODULE,
                "{name} alignment"
            );
            assert_eq!(tally[&Region::Timing], 4 * profile.band_width(), "{name} timing");
            assert_eq!(tally[&Region::Calibration], 4 * profile.band_width(), "{name} calibration");
            assert_eq!(
                tally[&Region::Header],
                2 * profile.header_rows() * profile.band_width(),
                "{name} header"
            );
            assert_eq!(tally[&Region::Data], profile.data_cells(), "{name} data");
            assert_eq!(
                grid * grid - tally[&Region::Data],
                profile.reserved_cells(),
                "{name} reserved"
            );
        }
    }

    #[test]
    fn orderings_are_complete_and_free_of_duplicates() {
        for profile in &PROFILES {
            let layout = FrameLayout::new(profile);

            let mut data = layout.data_cells().to_vec();
            let count = data.len();
            data.sort_unstable();
            data.dedup();
            assert_eq!(data.len(), count, "{} duplicate data cell", profile.name);
            assert_eq!(count, profile.data_cells() as usize);

            let mut ring = layout.calibration_cells().to_vec();
            let ring_count = ring.len();
            assert_eq!(ring_count, 4 * profile.band_width() as usize);
            ring.sort_unstable();
            ring.dedup();
            assert_eq!(ring.len(), ring_count, "{} duplicate ring cell", profile.name);

            for &cell in layout.calibration_cells() {
                let (row, col) = layout.coordinates(cell);
                assert_eq!(layout.region(row, col), Region::Calibration);
            }

            for band in 0..2 {
                assert_eq!(
                    layout.header_band(band).len(),
                    (profile.header_rows() * profile.band_width()) as usize
                );
            }
        }
    }

    #[test]
    fn the_calibration_ring_covers_every_symbol_evenly() {
        // The classifier fits one centroid per colour from this ring. A ring
        // that under-samples a symbol would leave that centroid noisier than
        // the rest, and the format would fail on one colour only.
        for profile in &PROFILES {
            let layout = FrameLayout::new(profile);
            let alphabet = layout.alphabet();
            let mut tally = vec![0u32; alphabet.len()];
            for index in 0..layout.calibration_cells().len() {
                tally[layout.calibration_value(index) as usize] += 1;
            }
            let min = tally.iter().min().copied().unwrap_or(0);
            let max = tally.iter().max().copied().unwrap_or(0);
            assert!(min > 0, "{} leaves a symbol unsampled", profile.name);
            assert!(max - min <= 1, "{} samples symbols unevenly: {tally:?}", profile.name);
        }
    }

    #[test]
    fn rendering_produces_the_expected_canvas() {
        let profile = &PROFILES[1];
        let layout = FrameLayout::new(profile);
        let cell_px = 8;
        let values = vec![0u16; layout.data_cells().len()];
        let codeword = vec![0xA5u8; profile.header_codeword_len() as usize];

        let image = layout.render(&codeword, &values, cell_px);
        let side = (profile.grid + 2 * QUIET_ZONE_CELLS) * cell_px;
        assert_eq!(image.width(), side);
        assert_eq!(image.height(), side);

        // The quiet zone must be white all the way round, or a detector will
        // find the screen bezel instead of the finder pattern.
        assert_eq!(image.get(0, 0), Rgb::WHITE);
        assert_eq!(image.get(side - 1, side - 1), Rgb::WHITE);
        assert_eq!(image.get(side / 2, 1), Rgb::WHITE);
    }

    #[test]
    fn finder_patterns_render_with_the_scanline_ratio() {
        // A horizontal cut through a finder centre must read 1:1:3:1:1 in cells.
        // That ratio is what a detector searches for, so if the renderer paints
        // anything else the two halves can never meet.
        let profile = &PROFILES[1];
        let layout = FrameLayout::new(profile);
        let cell_px = 4;
        let values = vec![0u16; layout.data_cells().len()];
        let image = layout.render(&[0; 42], &values, cell_px);

        let centre_row = QUIET_ZONE_CELLS + 3;
        let y = centre_row * cell_px + cell_px / 2;

        let mut runs: Vec<(bool, u32)> = Vec::new();
        for col in 0..FINDER_PATTERN {
            let x = (QUIET_ZONE_CELLS + col) * cell_px + cell_px / 2;
            let dark = image.get(x, y) == Rgb::BLACK;
            match runs.last_mut() {
                Some((last, n)) if *last == dark => *n += 1,
                _ => runs.push((dark, 1)),
            }
        }

        let widths: Vec<u32> = runs.iter().map(|(_, n)| *n).collect();
        assert_eq!(widths, vec![1, 1, 3, 1, 1], "runs were {runs:?}");
        assert!(runs[0].0, "the finder pattern must start dark");
    }

    #[test]
    fn a_rendered_frame_reads_back_exactly() {
        // The round trip that matters most: paint every payload cell with a
        // distinct value, sample it back through the identity transform, and
        // require the classifier to recover all of it. Any disagreement between
        // the renderer's cell map and the sampler's shows up here.
        for profile in &PROFILES {
            let layout = FrameLayout::new(profile);
            let alphabet = layout.alphabet();
            let cell_px = 8;

            let values: Vec<u16> = (0..layout.data_cells().len())
                .map(|i| u16::try_from(i % alphabet.len()).unwrap())
                .collect();
            let codeword = vec![0u8; profile.header_codeword_len() as usize];
            let image = layout.render(&codeword, &values, cell_px);
            let transform = layout.identity_transform(cell_px);

            let calibration: Vec<_> = layout
                .calibration_cells()
                .iter()
                .enumerate()
                .map(|(i, &cell)| {
                    (layout.calibration_value(i), layout.sample_cell(&image, &transform, cell))
                })
                .collect();
            let classifier = crate::symbol::Classifier::fit(alphabet, &calibration);

            let mut wrong = Vec::new();
            for (&cell, &expected) in layout.data_cells().iter().zip(values.iter()) {
                let sample = layout.sample_cell(&image, &transform, cell);
                let got = classifier.classify(&sample);
                if got.value != expected {
                    wrong.push((layout.coordinates(cell), expected, got.value, sample));
                }
            }
            assert!(wrong.is_empty(), "{} misread: {:?}", profile.name, wrong.first());
        }
    }

    #[test]
    fn the_alignment_module_sits_where_the_specification_says() {
        // SPEC.md 4.2.5 places the module at [G/2-3, G/2+4). The renderer and
        // the region map used to derive that independently and agreed with each
        // other while both being one cell out, which no round-trip test can
        // catch: the encoder and decoder share the mistake. A third-party
        // implementation following the specification would not.
        for profile in &PROFILES {
            let layout = FrameLayout::new(profile);
            let grid = layout.grid();
            let first = grid / 2 - 3;
            let last = grid / 2 + 3;

            assert_eq!(layout.region(first, first), Region::Alignment, "{}", profile.name);
            assert_eq!(layout.region(last, last), Region::Alignment, "{}", profile.name);
            assert_ne!(layout.region(first - 1, first - 1), Region::Alignment, "{}", profile.name);
            assert_ne!(layout.region(last + 1, last + 1), Region::Alignment, "{}", profile.name);

            // And the sampling centre must land on the middle cell of the
            // pattern, which carries the single dark dot a detector looks for.
            let centre = layout.alignment_centre();
            let middle = f64::from(grid / 2);
            assert!((centre.x - middle - 0.5).abs() < 1e-9, "{}", profile.name);
            assert!((centre.y - middle - 0.5).abs() < 1e-9, "{}", profile.name);
        }
    }

    #[test]
    fn bit_packing_round_trips_at_every_width() {
        for bits in [4u32, 5, 6] {
            let byte_len = 97usize;
            let bytes: Vec<u8> = (0..byte_len).map(|i| u8::try_from(i % 251).unwrap()).collect();
            let cell_count = byte_len * 8 / bits as usize;

            let cells = bytes_to_cells(&bytes, bits, cell_count);
            assert_eq!(cells.len(), cell_count);
            assert!(cells.iter().all(|&v| v < (1 << bits)), "{bits} bits produced a wide value");

            let back = cells_to_bytes(&cells, bits, byte_len);
            // The trailing bits of the last byte may fall outside the cells, so
            // compare only the bytes the cells actually cover.
            let covered = cell_count * bits as usize / 8;
            assert_eq!(&back[..covered], &bytes[..covered], "{bits} bits did not round trip");
        }
    }

    #[test]
    fn a_cell_that_straddles_a_byte_boundary_taints_both() {
        // Erasure decoding needs every byte a doubtful cell touched. Marking
        // only the first would point the corrector at half the damage and leave
        // the rest, which is worse than not marking it at all.
        assert_eq!(cell_byte_span(0, 5), (0, 0));
        assert_eq!(cell_byte_span(1, 5), (0, 1));
        assert_eq!(cell_byte_span(2, 5), (1, 1));
        assert_eq!(cell_byte_span(3, 5), (1, 2));

        // Four bits always fit inside one byte; six bits straddle two.
        assert_eq!(cell_byte_span(1, 4), (0, 0));
        assert_eq!(cell_byte_span(1, 6), (0, 1));
    }

    #[test]
    fn the_orientation_tag_is_the_brightest_thing_in_the_frame() {
        // Orientation is resolved by sampling this one position under four
        // rotations and taking the brightest. That only works if nothing else
        // can be as bright, which the balanced masks guarantee -- but only for
        // as long as the masks stay balanced.
        for profile in &PROFILES {
            let layout = FrameLayout::new(profile);
            let alphabet = layout.alphabet();
            let cell_px = 8;
            let values: Vec<u16> = (0..layout.data_cells().len())
                .map(|i| u16::try_from(i % alphabet.len()).unwrap())
                .collect();
            let codeword = vec![0xFFu8; profile.header_codeword_len() as usize];
            let image = layout.render(&codeword, &values, cell_px);
            let transform = layout.identity_transform(cell_px);

            let tag = transform.map(layout.orientation_centre()).unwrap();
            let tag_luma = image.sample_bilinear(tag.x, tag.y).luma();
            assert!(tag_luma > 0.99, "{} tag luma {tag_luma}", profile.name);

            let mut worst = 0.0f32;
            for &cell in layout.data_cells() {
                worst = worst.max(layout.sample_cell_mean(&image, &transform, cell).luma());
            }
            assert!(worst < tag_luma, "{} has a data cell as bright as the tag", profile.name);
        }
    }
}
