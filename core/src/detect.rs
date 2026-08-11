//! Finding a frame in a photograph.
//!
//! Everything else in this crate assumes the code area's position is known.
//! This module is what supplies it, and it is the only part of the decoder with
//! no help from the format: before a finder pattern is located there is no
//! header to read, no calibration ring to fit against, and no idea which profile
//! drew the frame.
//!
//! The search follows the shape the physical layer was designed around
//! (`SPEC.md` §4.2.1, §9.1):
//!
//! 1. threshold the image locally, because a photographed screen is never
//!    evenly lit;
//! 2. sweep rows for the finder pattern's 1:1:3:1:1 run lengths, and confirm
//!    each hit down its column — a ratio that survives blur and, along any line
//!    through the centre, perspective;
//! 3. take four centres and read a homography straight off them;
//! 4. try each profile and each of the four rotations, and *check* the result
//!    against the centre alignment marker and the orientation tag.
//!
//! Step four is the one that matters. Any four points produce a homography, so
//! a detector without a check does not fail — it returns a confident transform
//! onto nothing, and everything downstream reads noise with full conviction.

use crate::error::{Error, Result};
use crate::frame::{FINDER_PATTERN, FrameLayout};
use crate::geom::{Homography, Point};
use crate::image::RgbImage;
use crate::profile::{PROFILES, ProfileId};

/// Fraction below the local mean at which a pixel counts as dark.
///
/// A margin rather than the mean itself: a flat region of pure white has a mean
/// equal to itself, and without the margin half of it would threshold as dark
/// on rounding alone.
const DARK_MARGIN: f32 = 0.06;

/// Side of the local averaging window, as a fraction of the shorter image edge.
///
/// Large enough to span several cells, so the window's mean is not dragged
/// around by the code's own contents, and small enough to track the lighting
/// gradient across a photographed screen.
const WINDOW_FRACTION: u32 = 12;

/// How far a run length may stray from its expected multiple of the module
/// size, as a fraction of that module.
const RATIO_TOLERANCE: f64 = 0.55;

/// Two finder centres closer than this many module widths are the same one seen
/// from adjacent rows.
const CLUSTER_RADIUS: f64 = 3.0;

/// Hull vertices to consider when selecting the four corners.
///
/// A cap on the brute-force search, not on what may be detected. Real frames
/// leave a handful of hull vertices; this only bites on an image full of
/// look-alikes, where the most-seen ones are the better bet anyway.
const MAX_HULL_POINTS: usize = 24;

/// Smallest module, in pixels, a candidate may claim.
///
/// Below about three pixels per module a finder cannot be read anyway, and
/// fine-grained noise otherwise produces an endless supply of one-pixel
/// "patterns" that satisfy every ratio test by accident.
const MIN_MODULE_PX: f64 = 2.5;

/// Contrast the alignment marker must show before a transform is believed.
///
/// A threshold rather than a mere sign test. Sampling noise averages to roughly
/// the same grey everywhere, so the contrasts come out near zero but land on
/// whichever side chance puts them — and "positive by a hair" would accept a
/// photograph of static as a frame.
const MIN_ALIGNMENT_SCORE: f64 = 0.12;

/// Margin the orientation tag must show over the opposite corner.
const MIN_ORIENTATION_SCORE: f64 = 0.08;

/// A located frame.
#[derive(Debug, Clone)]
pub struct Detection {
    /// The profile whose geometry fits.
    pub profile: ProfileId,
    /// Cell space to image pixels.
    pub transform: Homography,
    /// The four finder centres, in the frame's own order: top-left, top-right,
    /// bottom-right, bottom-left.
    pub corners: [Point; 4],
    /// How strongly the two independent checks corroborated the transform, from
    /// 0 to 1: the alignment marker's contrast times the orientation tag's
    /// margin.
    ///
    /// Not a probability. It is here so a caller looking at several candidate
    /// frames in a video can prefer the one the format itself agreed with most.
    pub confidence: f64,
}

impl Detection {
    /// Camera pixels the code area spans per cell.
    ///
    /// The single most useful number about a capture, and the one `SPEC.md` Q1
    /// asks to be measured rather than assumed. It decides everything
    /// downstream: below roughly four the shape alphabet stops being separable
    /// whatever else is right, and no amount of error correction substitutes for
    /// pixels that were never recorded.
    ///
    /// Averaged over all four edges of the quadrilateral, so a frame seen at an
    /// angle reports what it has on average rather than at its nearest corner.
    #[must_use]
    pub fn pixels_per_cell(&self) -> f64 {
        // Finder centres sit 3.5 cells in from each corner, so the distance
        // between two of them spans `grid - 7` cells.
        let span = f64::from(self.profile.profile().grid) - f64::from(FINDER_PATTERN);
        if span <= 0.0 {
            return 0.0;
        }

        let mut total = 0.0;
        for index in 0..4 {
            total += self.corners[index].distance(self.corners[(index + 1) % 4]);
        }
        total / 4.0 / span
    }
}

/// Locates frames in images.
#[derive(Debug, Clone)]
pub struct Detector {
    profiles: Vec<ProfileId>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    /// A detector that will try every profile the specification defines.
    #[must_use]
    pub fn new() -> Self {
        Self { profiles: PROFILES.iter().map(|p| p.id).collect() }
    }

    /// A detector restricted to one profile.
    ///
    /// Worth using when the profile is already known — from a previous frame of
    /// the same recording, say — because it removes three quarters of the
    /// verification work and, more importantly, three chances to accept a
    /// plausible fit to the wrong geometry.
    #[must_use]
    pub fn for_profile(profile: ProfileId) -> Self {
        Self { profiles: vec![profile] }
    }

    /// Finds a frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoFinders`] when four corner patterns could not be
    /// found, and [`Error::NoOrientation`] when they were found but no profile
    /// and rotation produced a transform the alignment marker agrees with.
    pub fn detect(&self, image: &RgbImage) -> Result<Detection> {
        let binary = Binary::from_image(image);
        let candidates = find_finders(&binary);
        let corners = select_quad(&candidates).ok_or(Error::NoFinders)?;

        let mut best: Option<Detection> = None;

        for &profile in &self.profiles {
            let layout = FrameLayout::new(profile.profile());
            let ideal = layout.finder_centres();

            // Four identical finders leave the frame's rotation undetermined, so
            // every cyclic shift of the detected corners is a candidate.
            for rotation in 0..4 {
                let rotated = rotate(corners, rotation);
                let Some(transform) = Homography::from_quads(ideal, rotated) else { continue };

                let alignment = score_alignment(image, &layout, &transform);
                if alignment < MIN_ALIGNMENT_SCORE {
                    continue;
                }
                let orientation = score_orientation(image, &layout, &transform);
                if orientation < MIN_ORIENTATION_SCORE {
                    continue;
                }

                let confidence = alignment * orientation;
                if best.as_ref().is_none_or(|b| confidence > b.confidence) {
                    best = Some(Detection { profile, transform, corners: rotated, confidence });
                }
            }
        }

        best.ok_or(Error::NoOrientation)
    }
}

/// Rotates a corner list by `steps` positions.
fn rotate(corners: [Point; 4], steps: usize) -> [Point; 4] {
    core::array::from_fn(|i| corners[(i + steps) % 4])
}

/// How well the centre alignment module corroborates a transform.
///
/// The module has a signature no accidental fit reproduces: a dark cell at the
/// centre, light one cell out, dark two cells out, in both axes. Sampling it is
/// what turns four points that *determine* a homography into four points that
/// have been *checked* (`SPEC.md` §4.2.5).
fn score_alignment(image: &RgbImage, layout: &FrameLayout, transform: &Homography) -> f64 {
    let centre = layout.alignment_centre();
    let Some(dark_centre) = sample(image, transform, centre.x, centre.y) else { return 0.0 };

    let mut light = 0.0f64;
    let mut dark_ring = 0.0f64;
    let mut taps = 0.0f64;

    for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let Some(inner) = sample(image, transform, centre.x + dx, centre.y + dy) else {
            return 0.0;
        };
        let Some(outer) = sample(image, transform, centre.x + 2.0 * dx, centre.y + 2.0 * dy) else {
            return 0.0;
        };
        light += f64::from(inner);
        dark_ring += f64::from(outer);
        taps += 1.0;
    }

    let light = light / taps;
    let dark_ring = dark_ring / taps;
    let dark_centre = f64::from(dark_centre);

    // Both contrasts must be present and in the right direction. Scaling by the
    // measured light level keeps the score meaningful under any exposure.
    let inner_contrast = light - dark_centre;
    let outer_contrast = light - dark_ring;
    if inner_contrast <= 0.0 || outer_contrast <= 0.0 || light <= 0.0 {
        return 0.0;
    }

    ((inner_contrast / light) * (outer_contrast / light)).clamp(0.0, 1.0)
}

/// How strongly the orientation tag says this rotation is the right one.
///
/// The tag is a solid white 3x3 and nothing else in the frame can match it,
/// because every data, calibration and timing cell is at most half ink
/// (`SPEC.md` §4.2.2). Comparing it against the diagonally opposite corner —
/// which is ordinary payload — makes the test independent of exposure.
fn score_orientation(image: &RgbImage, layout: &FrameLayout, transform: &Homography) -> f64 {
    let tag = layout.orientation_centre();
    let grid = f64::from(layout.grid());
    let opposite = Point::new(grid - tag.x, grid - tag.y);

    let Some(here) = sample(image, transform, tag.x, tag.y) else { return 0.0 };
    let Some(there) = sample(image, transform, opposite.x, opposite.y) else { return 0.0 };

    let difference = f64::from(here) - f64::from(there);
    if difference <= 0.0 { 0.0 } else { difference.min(1.0) }
}

/// Mean luma of a small patch of cell space, or `None` if it maps off the image.
fn sample(image: &RgbImage, transform: &Homography, x: f64, y: f64) -> Option<f32> {
    let mut total = 0.0f32;
    let mut taps = 0.0f32;
    for (dx, dy) in [(-0.2, -0.2), (0.2, -0.2), (-0.2, 0.2), (0.2, 0.2), (0.0, 0.0)] {
        let mapped = transform.map(Point::new(x + dx, y + dy))?;
        if mapped.x < -1.0
            || mapped.y < -1.0
            || mapped.x > f64::from(image.width())
            || mapped.y > f64::from(image.height())
        {
            return None;
        }
        total += image.sample_bilinear(mapped.x, mapped.y).luma();
        taps += 1.0;
    }
    Some(total / taps)
}

// --- thresholding -------------------------------------------------------------

/// A locally thresholded copy of the image.
struct Binary {
    width: u32,
    height: u32,
    dark: Vec<bool>,
}

impl Binary {
    /// Thresholds against a local mean rather than a global one.
    ///
    /// A photographed screen is never evenly lit — it is brighter where the lamp
    /// is and darker at the edges — so a single threshold either loses the dark
    /// corner or floods the bright one. The local mean follows the gradient.
    fn from_image(image: &RgbImage) -> Self {
        let (width, height) = (image.width(), image.height());
        let mut luma = vec![0.0f32; (width as usize) * (height as usize)];
        for y in 0..height {
            for x in 0..width {
                let value = crate::symbol::Rgbf::from(image.get(x, y)).luma();
                luma[(y as usize) * (width as usize) + x as usize] = value;
            }
        }

        // Summed-area table, so a window mean is four lookups whatever its size.
        let (iw, ih) = (width as usize + 1, height as usize + 1);
        let mut integral = vec![0.0f64; iw * ih];
        for y in 0..height as usize {
            let mut row_sum = 0.0f64;
            for x in 0..width as usize {
                row_sum += f64::from(luma[y * width as usize + x]);
                integral[(y + 1) * iw + x + 1] = integral[y * iw + x + 1] + row_sum;
            }
        }

        let radius = (width.min(height) / WINDOW_FRACTION).max(4);
        let mut dark = vec![false; (width as usize) * (height as usize)];

        for y in 0..height {
            let y0 = y.saturating_sub(radius);
            let y1 = (y + radius).min(height - 1);
            for x in 0..width {
                let x0 = x.saturating_sub(radius);
                let x1 = (x + radius).min(width - 1);

                let area = f64::from((x1 - x0 + 1) * (y1 - y0 + 1));
                let sum = integral[(y1 as usize + 1) * iw + x1 as usize + 1]
                    - integral[(y0 as usize) * iw + x1 as usize + 1]
                    - integral[(y1 as usize + 1) * iw + x0 as usize]
                    + integral[(y0 as usize) * iw + x0 as usize];
                let mean = sum / area;

                let value = f64::from(luma[(y as usize) * (width as usize) + x as usize]);
                dark[(y as usize) * (width as usize) + x as usize] =
                    value < mean - f64::from(DARK_MARGIN);
            }
        }

        Self { width, height, dark }
    }

    fn is_dark(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        self.dark[(y as usize) * (self.width as usize) + x as usize]
    }
}

// --- finder search -------------------------------------------------------------

/// A finder pattern the search believes it has seen.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    centre: Point,
    module: f64,
    hits: f64,
}

/// One run of equal-valued pixels along a scan line.
#[derive(Debug, Clone, Copy)]
struct Run {
    dark: bool,
    start: u32,
    len: u32,
}

/// Whether five consecutive runs have the finder pattern's 1:1:3:1:1 shape.
fn matches_ratio(runs: &[Run; 5]) -> Option<f64> {
    if !(runs[0].dark && !runs[1].dark && runs[2].dark && !runs[3].dark && runs[4].dark) {
        return None;
    }
    let total: u32 = runs.iter().map(|r| r.len).sum();
    if total < 7 {
        return None;
    }

    let module = f64::from(total) / 7.0;
    let tolerance = module * RATIO_TOLERANCE;
    let expected = [1.0, 1.0, 3.0, 1.0, 1.0];

    for (run, multiple) in runs.iter().zip(expected.iter()) {
        let want = module * multiple;
        if (f64::from(run.len) - want).abs() > tolerance * multiple.max(1.0) {
            return None;
        }
    }
    Some(module)
}

/// Rounds a coordinate to a pixel index, or `None` if it is outside any image.
///
/// The bounds check is not ceremony. A ray walked from a finder near the edge
/// runs off the image within a few steps, and a bare cast turns a coordinate of
/// `-1.0` into four billion — which then reads whichever pixel that happens to
/// land on rather than the nothing that is actually there.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "range is checked immediately above the conversion"
)]
fn to_pixel(value: f64) -> Option<u32> {
    let rounded = value.round();
    if rounded < 0.0 || rounded > f64::from(u32::MAX) { None } else { Some(rounded as u32) }
}

/// Whether a continuous coordinate falls on a dark pixel. Outside is not dark.
fn is_dark_at(binary: &Binary, x: f64, y: f64) -> bool {
    match (to_pixel(x), to_pixel(y)) {
        (Some(px), Some(py)) => binary.is_dark(px, py),
        _ => false,
    }
}

/// The centre of a run, in pixel-centre coordinates.
///
/// A run covering pixel indices `[start, start + len)` has its first pixel at
/// coordinate `start` and its last at `start + len - 1`, so the middle sits at
/// `start + (len - 1) / 2`. Using `len / 2` instead puts every measurement half
/// a pixel late — which sounds harmless and is not: at six or seven pixels per
/// module that is a third of a sub-cell, applied uniformly to all four corners,
/// and it drags every sample in the frame towards its neighbour.
fn run_centre(run: Run) -> f64 {
    f64::from(run.start) + f64::from(run.len - 1) / 2.0
}

/// Runs along a row, or down a column when `vertical`.
fn runs_along(binary: &Binary, index: u32, vertical: bool) -> Vec<Run> {
    let length = if vertical { binary.height } else { binary.width };
    let mut runs: Vec<Run> = Vec::new();

    for position in 0..length {
        let dark = if vertical {
            binary.is_dark(index, position)
        } else {
            binary.is_dark(position, index)
        };
        match runs.last_mut() {
            Some(run) if run.dark == dark => run.len += 1,
            _ => runs.push(Run { dark, start: position, len: 1 }),
        }
    }
    runs
}

/// Directions the radial check walks: the four axes and the four diagonals.
const RAYS: [(f64, f64); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (0.707, 0.707),
    (0.707, -0.707),
    (-0.707, 0.707),
    (-0.707, -0.707),
];

/// Rays that must agree before a candidate is believed.
///
/// Six of eight rather than all eight: a corner of the frame can be clipped, a
/// reflection can sit across one side, and losing a finder to a single bad ray
/// would cost the whole frame.
const RAYS_REQUIRED: usize = 6;

/// Run lengths along a ray from a point, in whole steps.
fn ray_runs(binary: &Binary, from: Point, direction: (f64, f64), limit: u32) -> Vec<u32> {
    let mut runs: Vec<u32> = Vec::new();
    let mut current = is_dark_at(binary, from.x, from.y);
    if !current {
        return runs;
    }
    let mut length = 0u32;

    for step in 0..limit {
        let x = from.x + direction.0 * f64::from(step);
        let y = from.y + direction.1 * f64::from(step);
        if x < 0.0 || y < 0.0 {
            break;
        }
        let dark = is_dark_at(binary, x, y);
        if dark == current {
            length += 1;
        } else {
            runs.push(length);
            current = dark;
            length = 1;
            if runs.len() >= 4 {
                break;
            }
        }
    }
    if runs.len() < 4 {
        runs.push(length);
    }
    runs
}

/// Whether a ray leaving the centre of a finder crosses what a finder is made
/// of: half the dark centre, a light ring, a dark ring, then light.
///
/// This is the check that separates a finder from a stripe. A row of alternating
/// cells in the header band reproduces the 1:1:3:1:1 run lengths along one axis
/// perfectly well — it simply has no such structure along the diagonals, and a
/// finder has it along all eight.
fn ray_matches(binary: &Binary, centre: Point, direction: (f64, f64), module: f64) -> bool {
    let limit = to_pixel((module * 6.0).ceil()).unwrap_or(0).saturating_add(4);
    let runs = ray_runs(binary, centre, direction, limit);
    if runs.len() < 4 {
        return false;
    }

    let (dark_centre, light_ring, dark_ring, beyond) =
        (f64::from(runs[0]), f64::from(runs[1]), f64::from(runs[2]), f64::from(runs[3]));

    // Estimate the module from the ray itself: along a diagonal each step covers
    // more ground, so an absolute comparison against the horizontal module would
    // reject every diagonal.
    let unit = (dark_centre / 1.5 + light_ring + dark_ring) / 3.0;
    if unit < 1.0 {
        return false;
    }
    let tolerance = unit * 0.35;

    // The centre must be visibly wider than the rings around it. Without this
    // the check accepts 1:1:1, which is what a field of identical cells looks
    // like — and a frame whose payload is mostly padding is exactly that over
    // most of its area, so the clutter would arrive by the thousand.
    if dark_centre < light_ring * 1.2 || dark_centre < dark_ring * 1.2 {
        return false;
    }

    (dark_centre - unit * 1.5).abs() <= tolerance * 1.5
        && (light_ring - unit).abs() <= tolerance
        && (dark_ring - unit).abs() <= tolerance
        // The separator, or the quiet zone. A finder is never adjacent to ink.
        && beyond >= unit * 0.4
}

/// Whether a candidate really looks like a finder pattern from every side.
fn is_finder(binary: &Binary, centre: Point, module: f64) -> bool {
    let agreeing =
        RAYS.iter().filter(|&&direction| ray_matches(binary, centre, direction, module)).count();
    agreeing >= RAYS_REQUIRED
}

/// Confirms a horizontal hit by looking for the same ratio down its column.
fn confirm_vertically(binary: &Binary, x: u32, y: u32) -> Option<(f64, f64)> {
    let runs = runs_along(binary, x, true);
    for window in runs.windows(5) {
        let five: [Run; 5] = [window[0], window[1], window[2], window[3], window[4]];
        let Some(module) = matches_ratio(&five) else { continue };

        let middle = five[2];
        let centre = run_centre(middle);
        if (centre - f64::from(y)).abs() <= module * 2.0 {
            return Some((centre, module));
        }
    }
    None
}

/// Sweeps the image for finder patterns.
fn find_finders(binary: &Binary) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = Vec::new();

    // Every second row. A finder is at least seven modules tall, so nothing is
    // missed and the sweep costs half as much.
    let mut y = 0;
    while y < binary.height {
        let runs = runs_along(binary, y, false);
        for window in runs.windows(5) {
            let five: [Run; 5] = [window[0], window[1], window[2], window[3], window[4]];
            let Some(module) = matches_ratio(&five) else { continue };

            let middle = five[2];
            let x = run_centre(middle);

            let Some(column) = to_pixel(x) else { continue };
            let Some((cy, vertical_module)) = confirm_vertically(binary, column, y) else {
                continue;
            };

            // A finder is square, so the two module estimates must agree.
            if (module - vertical_module).abs() > module * 0.6 {
                continue;
            }

            let centre = Point::new(x, cy);
            let module = f64::midpoint(module, vertical_module);
            if module < MIN_MODULE_PX {
                continue;
            }

            // The row sweep alone is not selective enough: the header band is
            // made of alternating solid cells, and a row through it reproduces
            // the 1:1:3:1:1 ratio exactly. Only the radial check tells the two
            // apart, and without it a frame's own header supplies dozens of
            // candidates that crowd out its real corners.
            if !is_finder(binary, centre, module) {
                continue;
            }

            add_candidate(&mut candidates, Candidate { centre, module, hits: 1.0 });
        }
        y += 2;
    }

    candidates
}

/// Merges a hit into an existing candidate, or records a new one.
fn add_candidate(candidates: &mut Vec<Candidate>, found: Candidate) {
    for existing in candidates.iter_mut() {
        if existing.centre.distance(found.centre) <= existing.module * CLUSTER_RADIUS {
            let total = existing.hits + found.hits;
            existing.centre = Point::new(
                (existing.centre.x * existing.hits + found.centre.x * found.hits) / total,
                (existing.centre.y * existing.hits + found.centre.y * found.hits) / total,
            );
            existing.module = (existing.module * existing.hits + found.module * found.hits) / total;
            existing.hits = total;
            return;
        }
    }
    candidates.push(found);
}

/// Picks four candidates and orders them clockwise.
fn select_quad(candidates: &[Candidate]) -> Option<[Point; 4]> {
    if candidates.len() < 4 {
        return None;
    }

    // The search runs over the convex hull rather than over every candidate.
    // The four finders sit at the extreme corners of the code area, so they are
    // always hull vertices, while anything the payload throws up is interior by
    // construction. Ranking candidates by how many scan lines saw them and
    // keeping the top few does the opposite: a frame whose payload produces
    // hundreds of look-alikes crowds its own corners out of the list.
    let mut ranked = convex_hull(candidates);
    if ranked.len() > MAX_HULL_POINTS {
        ranked.sort_by(|a, b| b.hits.partial_cmp(&a.hits).unwrap_or(core::cmp::Ordering::Equal));
        ranked.truncate(MAX_HULL_POINTS);
    }
    if ranked.len() < 4 {
        return None;
    }

    // Among hull vertices the four finders enclose the largest area and share a
    // module size. Brute force is cheap at this many points and avoids a greedy
    // choice that one stray detection could mislead.
    let mut best: Option<([Point; 4], f64)> = None;
    for a in 0..ranked.len() {
        for b in a + 1..ranked.len() {
            for c in b + 1..ranked.len() {
                for d in c + 1..ranked.len() {
                    let group = [ranked[a], ranked[b], ranked[c], ranked[d]];

                    let smallest = group.iter().map(|g| g.module).fold(f64::INFINITY, f64::min);
                    let largest = group.iter().map(|g| g.module).fold(0.0f64, f64::max);
                    if largest > smallest * 2.0 {
                        continue;
                    }

                    let points = order_clockwise(group.map(|g| g.centre));
                    let area = quad_area(&points);
                    if best.as_ref().is_none_or(|(_, best_area)| area > *best_area) {
                        best = Some((points, area));
                    }
                }
            }
        }
    }

    best.map(|(points, _)| points)
}

/// The convex hull of the candidate centres, by Andrew's monotone chain.
fn convex_hull(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut sorted = candidates.to_vec();
    sorted.sort_by(|a, b| {
        a.centre
            .x
            .partial_cmp(&b.centre.x)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then(a.centre.y.partial_cmp(&b.centre.y).unwrap_or(core::cmp::Ordering::Equal))
    });
    sorted.dedup_by(|a, b| a.centre.distance(b.centre) < 1e-9);

    if sorted.len() < 3 {
        return sorted;
    }

    let cross = |o: &Candidate, a: &Candidate, b: &Candidate| {
        (a.centre.x - o.centre.x) * (b.centre.y - o.centre.y)
            - (a.centre.y - o.centre.y) * (b.centre.x - o.centre.x)
    };

    let mut lower: Vec<Candidate> = Vec::new();
    for point in &sorted {
        while lower.len() >= 2
            && cross(&lower[lower.len() - 2], &lower[lower.len() - 1], point) <= 0.0
        {
            lower.pop();
        }
        lower.push(*point);
    }

    let mut upper: Vec<Candidate> = Vec::new();
    for point in sorted.iter().rev() {
        while upper.len() >= 2
            && cross(&upper[upper.len() - 2], &upper[upper.len() - 1], point) <= 0.0
        {
            upper.pop();
        }
        upper.push(*point);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Sorts four points into clockwise order in image coordinates.
fn order_clockwise(points: [Point; 4]) -> [Point; 4] {
    let centre = Point::new(
        points.iter().map(|p| p.x).sum::<f64>() / 4.0,
        points.iter().map(|p| p.y).sum::<f64>() / 4.0,
    );

    let mut sorted = points;
    sorted.sort_by(|a, b| {
        let angle_a = (a.y - centre.y).atan2(a.x - centre.x);
        let angle_b = (b.y - centre.y).atan2(b.x - centre.x);
        angle_a.partial_cmp(&angle_b).unwrap_or(core::cmp::Ordering::Equal)
    });

    // Image coordinates put y downwards, so ascending angle already runs
    // clockwise on screen — the same winding the cell-space corner order uses.
    sorted
}

fn quad_area(points: &[Point; 4]) -> f64 {
    let mut sum = 0.0;
    for i in 0..4 {
        let p = points[i];
        let q = points[(i + 1) % 4];
        sum += p.x * q.y - q.x * p.y;
    }
    sum.abs() / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::PROFILES;
    use crate::simulate::Channel;
    use crate::symbol::Rgb;

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

    /// Worst distance, in cells, between where the detector thinks a reference
    /// point is and where it actually is.
    fn worst_error(layout: &FrameLayout, found: &Homography, truth: &Homography) -> f64 {
        let grid = f64::from(layout.grid());
        let probes = [
            Point::new(3.5, 3.5),
            Point::new(grid - 3.5, 3.5),
            Point::new(grid - 3.5, grid - 3.5),
            Point::new(3.5, grid - 3.5),
            layout.alignment_centre(),
        ];

        let mut worst: f64 = 0.0;
        for probe in probes {
            let (Some(a), Some(b)) = (found.map(probe), truth.map(probe)) else {
                return f64::INFINITY;
            };
            // Convert the pixel miss back into cells, so the number means the
            // same thing at any capture size.
            let scale = truth
                .map(Point::new(probe.x + 1.0, probe.y))
                .map_or(1.0, |next| next.distance(b).max(1e-9));
            worst = worst.max(a.distance(b) / scale);
        }
        worst
    }

    #[test]
    fn an_undistorted_frame_is_located_exactly() {
        for profile in &PROFILES {
            let (layout, image) = painted(profile, 8);
            let truth = layout.identity_transform(8);

            let detection = Detector::new().detect(&image).unwrap_or_else(|e| {
                panic!("{} was not detected: {e}", profile.name);
            });

            assert_eq!(detection.profile, profile.id, "{} matched another profile", profile.name);
            let error = worst_error(&layout, &detection.transform, &truth);

            // Tight on purpose. A uniform half-pixel bias in the run-centre
            // arithmetic lands at about 0.06 cells here, which is invisible to a
            // loose bound and still enough to drag every sample a third of a
            // sub-cell towards its neighbour.
            assert!(error < 0.03, "{} located {error:.4} cells out", profile.name);
        }
    }

    #[test]
    fn a_distorted_frame_is_located_within_a_fraction_of_a_cell() {
        // Perspective, resampling, blur, a colour cast and compression at once.
        // Sub-cell accuracy is the requirement: half a cell out and every
        // sample straddles two symbols.
        let profile = &PROFILES[1];
        for severity in [0.1, 0.2, 0.3, 0.4] {
            let (layout, image) = painted(profile, 10);
            let capture = Channel::severity(severity).apply(&image, &layout.identity_transform(10));

            let detection = Detector::new()
                .detect(&capture.image)
                .unwrap_or_else(|e| panic!("severity {severity} was not detected: {e}"));

            let error = worst_error(&layout, &detection.transform, &capture.transform);
            assert!(error < 0.35, "severity {severity} located {error:.3} cells out");
        }
    }

    #[test]
    fn a_rotated_frame_is_turned_the_right_way_up() {
        // Four identical finders leave the rotation undetermined; the
        // orientation tag is what resolves it. A frame read upside down decodes
        // into noise with complete confidence.
        let profile = &PROFILES[1];
        let (layout, image) = painted(profile, 8);
        let truth = layout.identity_transform(8);

        for quarter_turns in 0..4 {
            let rotated = rotate_image(&image, quarter_turns);
            let detection = Detector::new()
                .detect(&rotated)
                .unwrap_or_else(|e| panic!("{quarter_turns} quarter turns: {e}"));

            // The detector should map cell space onto the rotated image, so
            // sampling the orientation tag must find it bright.
            let tag = layout.orientation_centre();
            let luma = sample(&rotated, &detection.transform, tag.x, tag.y).unwrap_or(0.0);
            assert!(luma > 0.8, "{quarter_turns} quarter turns left the tag at {luma:.3}");

            // And the four corners must be a genuine quad, not a collapsed one.
            let area = quad_area(&detection.corners);
            let expected = quad_area(&[
                truth.map(Point::new(3.5, 3.5)).unwrap(),
                truth.map(Point::new(124.5, 3.5)).unwrap(),
                truth.map(Point::new(124.5, 124.5)).unwrap(),
                truth.map(Point::new(3.5, 124.5)).unwrap(),
            ]);
            assert!(
                (area - expected).abs() < expected * 0.1,
                "{quarter_turns} quarter turns gave area {area:.0}, expected {expected:.0}"
            );
        }
    }

    #[test]
    fn an_image_with_no_frame_in_it_is_refused() {
        // The failure that matters. Four points always produce a homography, so
        // a detector without a check returns a confident transform onto nothing
        // and every layer above it reads noise.
        let blank = RgbImage::filled(400, 400, Rgb::WHITE);
        assert!(matches!(Detector::new().detect(&blank), Err(Error::NoFinders)));

        let mut noisy = RgbImage::filled(400, 400, Rgb::BLACK);
        let mut state = 0x1234_5678u32;
        for y in 0..400 {
            for x in 0..400 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let shade = u8::try_from(state >> 24).unwrap_or(0);
                noisy.set(x, y, Rgb::new(shade, shade, shade));
            }
        }
        assert!(Detector::new().detect(&noisy).is_err(), "random noise was accepted as a frame");
    }

    #[test]
    fn a_frame_with_its_centre_marker_defaced_is_refused() {
        // Exactly the case the alignment check exists for: the finders are
        // intact, so the corners are found and a homography is produced, but the
        // frame is not what it claims. Without the check this decodes.
        let profile = &PROFILES[1];
        let (layout, mut image) = painted(profile, 8);

        let centre = layout.alignment_centre();
        let transform = layout.identity_transform(8);
        let at = transform.map(centre).unwrap();
        let (cx, cy) = (to_pixel(at.x).unwrap(), to_pixel(at.y).unwrap());
        image.fill_rect(cx - 40, cy - 40, 80, 80, Rgb::WHITE);

        assert!(Detector::new().detect(&image).is_err(), "a defaced alignment marker was accepted");
    }

    #[test]
    fn pixels_per_cell_matches_what_was_painted() {
        // The number a user is shown while aiming a camera, and the one Q1 asks
        // for. If it disagreed with reality the guidance would send people the
        // wrong way.
        for profile in &PROFILES {
            for cell_px in [6u32, 8, 12] {
                let (_, image) = painted(profile, cell_px);
                let detection = Detector::new().detect(&image).expect("detected");
                let measured = detection.pixels_per_cell();
                assert!(
                    (measured - f64::from(cell_px)).abs() < 0.1,
                    "{} at {cell_px} px/cell measured {measured:.3}",
                    profile.name
                );
            }
        }
    }

    #[test]
    fn pixels_per_cell_falls_when_the_capture_shrinks() {
        // A camera further away is the same code across fewer pixels, which is
        // the condition the guidance exists to warn about.
        let profile = &PROFILES[1];
        let (layout, image) = painted(profile, 12);

        let mut channel = Channel::pristine();
        channel.scale = 0.5;
        let capture = channel.apply(&image, &layout.identity_transform(12));

        let detection = Detector::new().detect(&capture.image).expect("detected");
        let measured = detection.pixels_per_cell();
        assert!((measured - 5.5).abs() < 1.0, "expected about 5.5, measured {measured:.3}");
    }

    #[test]
    fn restricting_the_detector_to_one_profile_agrees_with_the_open_search() {
        let profile = &PROFILES[2];
        let (layout, image) = painted(profile, 8);

        let open = Detector::new().detect(&image).unwrap();
        let restricted = Detector::for_profile(profile.id).detect(&image).unwrap();

        assert_eq!(open.profile, restricted.profile);
        let truth = layout.identity_transform(8);
        assert!(worst_error(&layout, &restricted.transform, &truth) < 0.25);
    }

    fn rotate_image(image: &RgbImage, quarter_turns: u32) -> RgbImage {
        let mut out = image.clone();
        for _ in 0..quarter_turns % 4 {
            let (w, h) = (out.width(), out.height());
            let mut turned = RgbImage::filled(h, w, Rgb::BLACK);
            for y in 0..h {
                for x in 0..w {
                    turned.set(h - 1 - y, x, out.get(x, y));
                }
            }
            out = turned;
        }
        out
    }
}
