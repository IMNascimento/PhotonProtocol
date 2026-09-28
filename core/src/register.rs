//! Bending the sampling grid to fit what the camera actually saw.
//!
//! Four finder centres fix a homography exactly, and a homography is the right
//! map between a flat screen and an ideal pinhole. A phone is not an ideal
//! pinhole. Through a real one the middle of a code sits up to a third of a cell
//! from where the homography puts it, which is enough to read every cell there
//! from its neighbour.
//!
//! The first answer to that measured the timing ring, which is round the
//! perimeter, and blended what it found inwards. Against a simulated lens it
//! worked. Against a real one it made things worse, because the departure is
//! small at the perimeter and large inside: there was nothing at the perimeter
//! to measure.
//!
//! So the measurement is made inside the code, from the payload itself. A
//! payload is thousands of cells on a lattice, and every edge in it — between
//! one cell and the next, or between the painted and unpainted halves of one —
//! falls on a line of that lattice. Where the edges of a patch of the picture
//! fall, modulo the spacing of the lattice, is therefore how far that patch is
//! from where the homography put it. It is a phase, so it is only known to
//! within one spacing; the timing ring, which repeats every two cells and can be
//! located outright, says which, and the answer is carried inwards from there.
//!
//! No marks are added to the format for this. The payload is its own ruler.

// The workspace denies lossy numeric casts, because in the protocol path a
// silent truncation is a correctness bug. This module is geometry: it moves
// between indices of patches, positions in cells and positions in pixels on
// nearly every line, and every conversion is bounded by the size of a grid or
// of a picture. A `try_from` on each would bury the arithmetic it is there to
// protect.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

use crate::frame::{FINDER_BOX, FrameLayout};
use crate::geom::{Homography, Point};
use crate::image::RgbImage;

/// Anything that says where a point of cell space landed in the picture.
///
/// Cell space places the code area over `[0, G] x [0, G]`, with `x` the column
/// and `y` the row.
pub trait CellMap {
    /// The picture position of a cell-space point, or `None` if it has none.
    fn locate(&self, point: Point) -> Option<Point>;
}

impl CellMap for Homography {
    fn locate(&self, point: Point) -> Option<Point> {
        self.map(point)
    }
}

/// Samples along a cell when looking for an edge.
const STEPS_PER_CELL: u32 = 8;

/// How far outside the code area the search for a timing cell's outer edge
/// starts, and how far inside it ends, in samples.
const OUTWARD_STEPS: u32 = 10;
const INWARD_STEPS: u32 = 5;

/// How far from where the homography puts it an edge is looked for, in cells.
///
/// Wide enough for the distortion of a poor lens at the middle of an edge,
/// and narrower than the two cells over which the timing ring repeats, past
/// which one cell could be mistaken for the next but one.
const SEARCH: f64 = 0.8;

/// Measurements an edge needs before its correction is believed.
const MIN_MEASUREMENTS: usize = 8;

/// Degree of the polynomial fitted along each edge.
const DEGREE: usize = 4;

/// Side of the patches the payload's lattice is measured in, in cells.
///
/// Large enough to hold several hundred edges, which is what makes the phase
/// of a patch precise to a few hundredths of a cell; small enough that the
/// grid moves little across one.
const BLOCK: u32 = 8;

/// A sampling grid that has been fitted to the picture: the position of every
/// cell corner, with everything between them interpolated.
#[derive(Debug, Clone)]
pub struct Mesh {
    grid: u32,
    /// `(grid + 1)^2` corners, row-major. Corner `(row, col)` is the cell-space
    /// point `(col, row)`.
    nodes: Vec<Point>,
    /// How much of the perimeter was measured, from 0 to 1.
    coverage: f64,
    /// The largest correction applied, in cells.
    largest: f64,
}

impl Mesh {
    /// The grid a homography alone implies, with no correction.
    #[must_use]
    pub fn from_homography(layout: &FrameLayout, transform: &Homography) -> Self {
        let grid = layout.grid();
        let mut nodes = Vec::with_capacity(((grid + 1) * (grid + 1)) as usize);
        for row in 0..=grid {
            for col in 0..=grid {
                let point = Point::new(f64::from(col), f64::from(row));
                nodes.push(transform.map(point).unwrap_or(point));
            }
        }
        Self { grid, nodes, coverage: 0.0, largest: 0.0 }
    }

    /// Fits the grid to the timing ring alone, blending what it measures round
    /// the perimeter inwards.
    ///
    /// The first way this was done, and through a real lens the worse one: what
    /// a lens does to a picture is small at the perimeter of a code and large
    /// inside it, so there is little at the perimeter to measure. It is kept
    /// because it needs nothing from the payload. When the payload is too
    /// blurred to say where its own lattice is, the ring, whose cells are solid
    /// and twice the size, often still can.
    #[must_use]
    pub fn fit_to_ring(layout: &FrameLayout, image: &RgbImage, transform: &Homography) -> Self {
        let grid = layout.grid();
        let side = f64::from(grid);

        let edges = [
            measure_edge(image, transform, grid, Edge::Top),
            measure_edge(image, transform, grid, Edge::Bottom),
            measure_edge(image, transform, grid, Edge::Left),
            measure_edge(image, transform, grid, Edge::Right),
        ];
        let measured: usize = edges.iter().map(|e| e.used).sum();
        let possible = 4 * (grid - 2 * FINDER_BOX) as usize;
        let coverage = measured as f64 / possible.max(1) as f64;

        // Where two edges meet they have to agree, or the blend tears at the
        // corner. Take their average there and tilt each edge to meet it.
        let [top, bottom, left, right] = &edges;
        let corner =
            |a: (f64, f64), b: (f64, f64)| (f64::midpoint(a.0, b.0), f64::midpoint(a.1, b.1));
        let corners = [
            corner(top.at(0.0), left.at(0.0)),
            corner(top.at(1.0), right.at(0.0)),
            corner(bottom.at(0.0), left.at(1.0)),
            corner(bottom.at(1.0), right.at(1.0)),
        ];
        let along = |edge: &EdgeFit, t: f64, start: (f64, f64), end: (f64, f64)| {
            let (a, b) = (edge.at(0.0), edge.at(1.0));
            let value = edge.at(t);
            (
                value.0 + (1.0 - t) * (start.0 - a.0) + t * (end.0 - b.0),
                value.1 + (1.0 - t) * (start.1 - a.1) + t * (end.1 - b.1),
            )
        };

        let mut nodes = Vec::with_capacity(((grid + 1) * (grid + 1)) as usize);
        let mut largest = 0.0f64;
        for row in 0..=grid {
            for col in 0..=grid {
                let (x, y) = (f64::from(col), f64::from(row));
                let (u, v) = (x / side, y / side);

                let t = along(top, u, corners[0], corners[1]);
                let b = along(bottom, u, corners[2], corners[3]);
                let l = along(left, v, corners[0], corners[2]);
                let r = along(right, v, corners[1], corners[3]);
                let blend = |t: f64, b: f64, l: f64, r: f64, k: [f64; 4]| {
                    (1.0 - v) * t + v * b + (1.0 - u) * l + u * r
                        - ((1.0 - u) * (1.0 - v) * k[0]
                            + u * (1.0 - v) * k[1]
                            + (1.0 - u) * v * k[2]
                            + u * v * k[3])
                };
                let dx = blend(t.0, b.0, l.0, r.0, corners.map(|c| c.0));
                let dy = blend(t.1, b.1, l.1, r.1, corners.map(|c| c.1));

                largest = largest.max(dx.hypot(dy));
                let moved = Point::new(x + dx, y + dy);
                nodes.push(transform.map(moved).unwrap_or(moved));
            }
        }

        Self { grid, nodes, coverage, largest }
    }

    /// Fits the grid to a picture, starting from the homography its finder
    /// patterns gave.
    #[must_use]
    pub fn fit(layout: &FrameLayout, image: &RgbImage, transform: &Homography) -> Self {
        let grid = layout.grid();
        let side = f64::from(grid);

        let edges = [
            measure_edge(image, transform, grid, Edge::Top),
            measure_edge(image, transform, grid, Edge::Bottom),
            measure_edge(image, transform, grid, Edge::Left),
            measure_edge(image, transform, grid, Edge::Right),
        ];

        let measured: usize = edges.iter().map(|e| e.used).sum();
        let possible = 4 * (grid - 2 * FINDER_BOX) as usize;
        let coverage = measured as f64 / possible.max(1) as f64;

        // Half a cell for every alphabet. The eight shapes have edges on the
        // quarters as well, but a camera that can resolve a quarter of a cell
        // is looking at cells so large that registration is not in doubt, and
        // one that cannot sees only the halves.
        let period = 0.5;
        let patches = measure_lattice(image, transform, grid, period, &edges);
        let surface = Surface::through(&patches, grid.div_ceil(BLOCK), grid);

        let mut nodes = Vec::with_capacity(((grid + 1) * (grid + 1)) as usize);
        let mut largest = 0.0f64;
        for row in 0..=grid {
            for col in 0..=grid {
                let (x, y) = (f64::from(col), f64::from(row));
                let (dx, dy) = surface.at(x / side, y / side);
                largest = largest.max(dx.hypot(dy));
                let moved = Point::new(x + dx, y + dy);
                nodes.push(transform.map(moved).unwrap_or(moved));
            }
        }

        Self { grid, nodes, coverage, largest }
    }

    /// Share of the timing ring that was located, from 0 to 1.
    #[must_use]
    pub const fn coverage(&self) -> f64 {
        self.coverage
    }

    /// The largest correction the fit applied, in cells.
    ///
    /// How far the picture departed from a plane seen through a pinhole. Near
    /// zero for a good lens square on; approaching one for a poor lens, a curved
    /// screen, or a hand that moved while the shutter rolled.
    #[must_use]
    pub const fn largest_correction(&self) -> f64 {
        self.largest
    }

    /// The four corners of a cell: top-left, top-right, bottom-left,
    /// bottom-right.
    #[must_use]
    pub fn corners(&self, row: u32, col: u32) -> [Point; 4] {
        let stride = (self.grid + 1) as usize;
        let (r, c) = (row.min(self.grid - 1) as usize, col.min(self.grid - 1) as usize);
        let at = r * stride + c;
        [self.nodes[at], self.nodes[at + 1], self.nodes[at + stride], self.nodes[at + stride + 1]]
    }
}

impl CellMap for Mesh {
    fn locate(&self, point: Point) -> Option<Point> {
        let last = f64::from(self.grid - 1);
        // Outside the code area the nearest cell's own patch is extended, which
        // is what lets the quiet zone be sampled.
        let col = point.x.floor().clamp(0.0, last);
        let row = point.y.floor().clamp(0.0, last);
        let (fx, fy) = (point.x - col, point.y - row);
        let [top_left, top_right, bottom_left, bottom_right] = self.corners(row as u32, col as u32);

        let along = |from: Point, to: Point, t: f64| {
            Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)
        };
        let top = along(top_left, top_right, fx);
        let bottom = along(bottom_left, bottom_right, fx);
        let found = along(top, bottom, fy);
        (found.x.is_finite() && found.y.is_finite()).then_some(found)
    }
}

// --- measuring an edge ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    /// The cell-space point `along` cells down the edge and `across` cells in
    /// from the outside of the code area.
    fn point(self, grid: u32, along: f64, across: f64) -> Point {
        let side = f64::from(grid);
        match self {
            Self::Top => Point::new(along, across),
            Self::Bottom => Point::new(along, side - across),
            Self::Left => Point::new(across, along),
            Self::Right => Point::new(side - across, along),
        }
    }

    /// A displacement measured along and across the edge, as one along the
    /// cell-space axes.
    const fn to_cell_axes(self, along: f64, across: f64) -> (f64, f64) {
        match self {
            Self::Top => (along, across),
            Self::Bottom => (along, -across),
            Self::Left => (across, along),
            Self::Right => (-across, along),
        }
    }
}

/// How far one edge of the frame is from where the homography puts it, in
/// cells, as a function of how far along it.
#[derive(Debug, Clone)]
struct EdgeFit {
    x: [f64; DEGREE + 1],
    y: [f64; DEGREE + 1],
    used: usize,
}

impl EdgeFit {
    const fn none() -> Self {
        Self { x: [0.0; DEGREE + 1], y: [0.0; DEGREE + 1], used: 0 }
    }

    /// The correction at `t`, from 0 at one end of the edge to 1 at the other.
    fn at(&self, t: f64) -> (f64, f64) {
        // Centred on the middle of the edge, which keeps the powers small.
        let s = 2.0 * t - 1.0;
        let eval = |k: &[f64; DEGREE + 1]| k.iter().rev().fold(0.0, |acc, c| acc * s + c);
        (eval(&self.x), eval(&self.y))
    }
}

fn luma_at(image: &RgbImage, transform: &Homography, point: Point) -> Option<f64> {
    let mapped = transform.map(point)?;
    if mapped.x < 0.0
        || mapped.y < 0.0
        || mapped.x > f64::from(image.width() - 1)
        || mapped.y > f64::from(image.height() - 1)
    {
        return None;
    }
    let c = image.sample_bilinear(mapped.x, mapped.y);
    Some(f64::from(c.r) * 0.299 + f64::from(c.g) * 0.587 + f64::from(c.b) * 0.114)
}

/// Where a run of samples crosses `level`, between `from` and `to`, going the
/// way `rising` says. Returns the position in samples, interpolated.
fn crossing(profile: &[f64], level: f64, rising: bool, nearest: f64, reach: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    for index in 0..profile.len().saturating_sub(1) {
        let (a, b) = (profile[index], profile[index + 1]);
        let crosses = if rising { a < level && b >= level } else { a >= level && b < level };
        if !crosses {
            continue;
        }
        let position = index as f64 + (level - a) / (b - a);
        if (position - nearest).abs() > reach {
            continue;
        }
        if best.is_none_or(|found| (position - nearest).abs() < (found - nearest).abs()) {
            best = Some(position);
        }
    }
    best
}

/// One located timing cell.
struct Located {
    /// Position along the edge, from 0 to 1.
    t: f64,
    /// Displacement in cells, along the axes of cell space.
    dx: f64,
    dy: f64,
}

fn measure_edge(image: &RgbImage, transform: &Homography, grid: u32, edge: Edge) -> EdgeFit {
    let first = FINDER_BOX;
    let last = grid - FINDER_BOX;
    let steps = f64::from(STEPS_PER_CELL);

    // The ring, read along its middle, from a cell before the first timing cell
    // to a cell after the last.
    let start = f64::from(first) - 1.0;
    let count = (last - first + 2) * STEPS_PER_CELL + 1;
    let mut profile = Vec::with_capacity(count as usize);
    for index in 0..count {
        let along = start + f64::from(index) / steps;
        match luma_at(image, transform, edge.point(grid, along, 0.5)) {
            Some(value) => profile.push(value),
            // Part of the ring is out of shot. What is measured is still
            // better than nothing, but not from a profile with a hole in it.
            None => return EdgeFit::none(),
        }
    }

    let mut sorted = profile.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    let dark = sorted[sorted.len() / 10];
    let light = sorted[sorted.len() * 9 / 10];
    if light - dark < 0.08 {
        return EdgeFit::none();
    }
    let level = f64::midpoint(dark, light);

    let to_samples = |along: f64| (along - start) * steps;
    let reach = SEARCH * steps;

    // Every timing cell has an edge either side, of opposite sense. Bright
    // areas spread in a photograph, so each edge sits a little towards the dark
    // side of where it was painted — but the two edges of one cell move in
    // opposite directions, and the middle between them does not move at all.
    let mut found: Vec<(u32, f64, f64)> = Vec::new();
    for cell in first..last {
        let dark_cell = cell % 2 == 0;
        let near = crossing(&profile, level, !dark_cell, to_samples(f64::from(cell)), reach);
        let far = crossing(&profile, level, dark_cell, to_samples(f64::from(cell) + 1.0), reach);
        let (Some(near), Some(far)) = (near, far) else { continue };

        let width = (far - near) / steps;
        if !(0.45..=1.55).contains(&width) {
            continue;
        }
        let centre = start + f64::midpoint(near, far) / steps;
        found.push((cell, centre - (f64::from(cell) + 0.5), width));
    }

    if found.len() < MIN_MEASUREMENTS {
        return EdgeFit::none();
    }

    // How far the spreading moves an edge: dark cells come out narrower than
    // they were painted by twice that much.
    let dark_widths: Vec<f64> =
        found.iter().filter(|(cell, _, _)| cell % 2 == 0).map(|&(_, _, w)| w).collect();
    let spread = if dark_widths.is_empty() {
        0.0
    } else {
        ((1.0 - dark_widths.iter().sum::<f64>() / dark_widths.len() as f64) / 2.0).clamp(-0.3, 0.3)
    };

    let mut located = Vec::with_capacity(found.len());
    let mut last_across = 0.0;
    for &(cell, along, _) in &found {
        // Only a dark cell has an outer edge: a light one runs straight into
        // the quiet zone. Between dark cells, carry the last one forward.
        if cell % 2 == 0 {
            let centre = f64::from(cell) + 0.5 + along;
            let outward = f64::from(OUTWARD_STEPS) / steps;
            let across_profile: Option<Vec<f64>> = (0..=OUTWARD_STEPS + INWARD_STEPS)
                .map(|index| {
                    let across = f64::from(index) / steps - outward;
                    luma_at(image, transform, edge.point(grid, centre, across))
                })
                .collect();
            if let Some(across_profile) = across_profile
                && let Some(position) =
                    crossing(&across_profile, level, false, f64::from(OUTWARD_STEPS), reach)
            {
                last_across = position / steps - outward - spread;
            }
        }

        let (dx, dy) = edge.to_cell_axes(along, last_across);
        located.push(Located { t: (f64::from(cell) + 0.5) / f64::from(grid), dx, dy });
    }

    fit_edge(&located)
}

/// Fits a polynomial through the located cells, discarding the ones that
/// disagree with the rest.
fn fit_edge(located: &[Located]) -> EdgeFit {
    let mut keep: Vec<bool> = vec![true; located.len()];

    let mut fit = EdgeFit::none();
    for _ in 0..3 {
        let kept: Vec<&Located> =
            located.iter().zip(keep.iter()).filter(|(_, k)| **k).map(|(l, _)| l).collect();
        if kept.len() < MIN_MEASUREMENTS {
            return EdgeFit::none();
        }

        let ts: Vec<f64> = kept.iter().map(|l| 2.0 * l.t - 1.0).collect();
        let xs: Vec<f64> = kept.iter().map(|l| l.dx).collect();
        let ys: Vec<f64> = kept.iter().map(|l| l.dy).collect();
        let (Some(x), Some(y)) = (polyfit(&ts, &xs), polyfit(&ts, &ys)) else {
            return EdgeFit::none();
        };
        fit = EdgeFit { x, y, used: kept.len() };

        let residuals: Vec<f64> = located
            .iter()
            .map(|l| {
                let (fx, fy) = fit.at(l.t);
                (fx - l.dx).hypot(fy - l.dy)
            })
            .collect();
        let mut sorted: Vec<f64> =
            residuals.iter().zip(keep.iter()).filter(|(_, k)| **k).map(|(r, _)| *r).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];
        let limit = (median * 4.0).max(0.05);

        let mut changed = false;
        for (slot, residual) in keep.iter_mut().zip(residuals.iter()) {
            let inside = *residual <= limit;
            if *slot != inside {
                *slot = inside;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    fit
}

/// Least-squares polynomial of degree [`DEGREE`], by the normal equations.
///
/// The abscissa is already in `[-1, 1]` and the degree is small, so the normal
/// equations are well enough conditioned to solve directly.
fn polyfit(ts: &[f64], values: &[f64]) -> Option<[f64; DEGREE + 1]> {
    const N: usize = DEGREE + 1;
    let mut matrix = [[0.0f64; N + 1]; N];

    for (&t, &value) in ts.iter().zip(values.iter()) {
        let mut powers = [1.0f64; 2 * N];
        for index in 1..2 * N {
            powers[index] = powers[index - 1] * t;
        }
        for row in 0..N {
            for col in 0..N {
                matrix[row][col] += powers[row + col];
            }
            matrix[row][N] += powers[row] * value;
        }
    }

    // A little weight towards zero on the higher terms. Without it, an edge
    // measured over only part of its length is free to swing wildly over the
    // part that was not.
    for (index, row) in matrix.iter_mut().enumerate().skip(2) {
        row[index] += 2e-4 * ts.len() as f64;
    }

    for pivot in 0..N {
        let mut largest = pivot;
        for row in pivot + 1..N {
            if matrix[row][pivot].abs() > matrix[largest][pivot].abs() {
                largest = row;
            }
        }
        if matrix[largest][pivot].abs() < 1e-12 {
            return None;
        }
        matrix.swap(pivot, largest);

        let pivot_row = matrix[pivot];
        for (row, entries) in matrix.iter_mut().enumerate() {
            if row == pivot {
                continue;
            }
            let factor = entries[pivot] / pivot_row[pivot];
            for (entry, above) in entries.iter_mut().zip(pivot_row.iter()).skip(pivot) {
                *entry -= factor * above;
            }
        }
    }

    let mut out = [0.0f64; N];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = matrix[index][N] / matrix[index][index];
    }
    out.iter().all(|v| v.is_finite()).then_some(out)
}

// --- measuring the payload's own lattice ------------------------------------------

/// One patch of the code, and how far it is from where the homography puts it.
#[derive(Debug, Clone, Copy)]
struct Patch {
    /// Middle of the patch, from 0 to 1 across the code each way.
    u: f64,
    v: f64,
    /// Displacement in cells.
    dx: f64,
    dy: f64,
    /// How much the edges of the patch agreed about it.
    weight: f64,
}

/// A running sum of unit vectors, each the phase of one edge.
#[derive(Debug, Clone, Copy, Default)]
struct Phase {
    re: f64,
    im: f64,
    total: f64,
}

impl Phase {
    fn add(&mut self, position: f64, period: f64, strength: f64) {
        let angle = core::f64::consts::TAU * position / period;
        self.re += strength * angle.cos();
        self.im += strength * angle.sin();
        self.total += strength;
    }

    /// How far the edges agreed with one another, from 0 to 1.
    ///
    /// Edges that all fall on the lattice add up; edges that fall anywhere,
    /// as in a patch out of focus or caught while the screen changed, cancel.
    fn agreement(self) -> f64 {
        if self.total <= 0.0 { 0.0 } else { self.strength() / self.total }
    }

    /// Where the edges fall, modulo the period, nearest to `expected`.
    fn displacement(self, period: f64, expected: f64) -> f64 {
        let measured = self.im.atan2(self.re) * period / core::f64::consts::TAU;
        // Known to within a whole number of periods. Take the one nearest to
        // what the patches already placed say it should be.
        measured + ((expected - measured) / period).round() * period
    }

    fn strength(self) -> f64 {
        self.re.hypot(self.im)
    }
}

/// Measures how far each patch of the code is from where the homography puts
/// it, from where the edges in the patch fall.
///
/// The picture is read along lines of cell space, through the middles of the
/// rows and columns of sub-cells, and wherever the brightness changes between
/// two samples there is an edge between them. Every edge the format paints is
/// on a line of the lattice. In a photograph an edge is not where it was
/// painted — light spreads into dark — but the two edges of anything painted
/// move apart by the same amount, so their phases move in opposite directions
/// and their sum does not move at all.
fn measure_lattice(
    image: &RgbImage,
    transform: &Homography,
    grid: u32,
    period: f64,
    edges: &[EdgeFit; 4],
) -> Vec<Patch> {
    let per_cell: u32 = if period < 0.5 { 16 } else { 8 };
    let step = 1.0 / f64::from(per_cell);
    let blocks = grid.div_ceil(BLOCK);
    let count = (blocks * blocks) as usize;
    let side = f64::from(grid);

    // Lines through the middles of two of a cell's rows of sub-cells. On a
    // boundary between rows a line would read a blend of both.
    let offsets: [f64; 2] = if period < 0.5 { [0.125, 0.625] } else { [0.25, 0.75] };

    let mut across = vec![Phase::default(); count];
    let mut down = vec![Phase::default(); count];

    let luma = |x: f64, y: f64| -> Option<f64> {
        let at = transform.map(Point::new(x, y))?;
        if at.x < 0.0
            || at.y < 0.0
            || at.x > f64::from(image.width() - 1)
            || at.y > f64::from(image.height() - 1)
        {
            return None;
        }
        let colour = image.sample(at.x, at.y);
        Some(
            f64::from(colour[0]) * 0.299
                + f64::from(colour[1]) * 0.587
                + f64::from(colour[2]) * 0.114,
        )
    };

    let samples = grid * per_cell;
    for line in 0..grid {
        for offset in offsets {
            let fixed = f64::from(line) + offset;
            let block_of_line = line / BLOCK;

            let mut previous: [Option<f64>; 2] = [None, None];
            for index in 0..=samples {
                let moving = f64::from(index) * step;
                let here = [luma(moving, fixed), luma(fixed, moving)];

                if index > 0 {
                    // The edge, if there is one, is between this sample and
                    // the last.
                    let position = moving - step / 2.0;
                    let block_of_position = ((index - 1) / per_cell / BLOCK).min(blocks - 1);

                    if let (Some(a), Some(b)) = (previous[0], here[0]) {
                        let at = (block_of_line * blocks + block_of_position) as usize;
                        across[at].add(position, period, (a - b).abs());
                    }
                    if let (Some(a), Some(b)) = (previous[1], here[1]) {
                        let at = (block_of_position * blocks + block_of_line) as usize;
                        down[at].add(position, period, (a - b).abs());
                    }
                }
                previous = here;
            }
        }
    }

    // A phase says where a patch is to within a whole number of periods, and
    // something else has to say which.
    //
    // Round the perimeter the timing ring does. Each of its cells can be
    // located outright, which gives how far every patch on an edge has moved
    // *along* that edge. How far it has moved *across* the edge the ring does
    // not say reliably — the one edge a timing cell has there is moved by the
    // spreading of light — so that is carried along the edge from the corners,
    // where the homography is exact because it was drawn through them.
    //
    // Inside, each ring of patches takes its periods from the ring outside it,
    // since the grid moves little from one patch to the next.
    let agreement: Vec<f64> =
        (0..count).map(|at| across[at].agreement().min(down[at].agreement())).collect();
    let mut placed: Vec<Option<(f64, f64)>> = vec![None; count];
    let index = |row: u32, col: u32| (row * blocks + col) as usize;
    let middle_of = |at: u32| ((f64::from(at) + 0.5) * f64::from(BLOCK) / side).min(1.0);

    let [top, bottom, left, right] = edges;
    let last = blocks - 1;

    // The four corners, then along each edge from both ends towards its
    // middle.
    for (row, col) in [(0, 0), (0, last), (last, 0), (last, last)] {
        let at = index(row, col);
        placed[at] =
            Some((across[at].displacement(period, 0.0), down[at].displacement(period, 0.0)));
    }
    for step in 1..blocks.div_ceil(2).max(1) {
        for from_start in [true, false] {
            let along = if from_start { step } else { last - step };
            let before = if from_start { along - 1 } else { along + 1 };
            if along == 0 || along >= last {
                continue;
            }

            for (edge, horizontal, fixed) in
                [(top, true, 0), (bottom, true, last), (left, false, 0), (right, false, last)]
            {
                let (at, previous) = if horizontal {
                    (index(fixed, along), index(fixed, before))
                } else {
                    (index(along, fixed), index(before, fixed))
                };
                if placed[at].is_some() {
                    continue;
                }
                let carried = placed[previous].unwrap_or((0.0, 0.0));
                let located = (edge.used > 0).then(|| edge.at(middle_of(along)));

                // Along the edge from the ring where it was located, across
                // it from the patch before.
                let expected = match (located, horizontal) {
                    (Some(ring), true) => (ring.0, carried.1),
                    (Some(ring), false) => (carried.0, ring.1),
                    (None, _) => carried,
                };
                placed[at] = Some((
                    across[at].displacement(period, expected.0),
                    down[at].displacement(period, expected.1),
                ));
            }
        }
    }

    // Inwards, a ring at a time.
    for depth in 1..blocks.div_ceil(2) {
        let ring: Vec<(u32, u32)> = (depth..blocks - depth)
            .flat_map(|row| (depth..blocks - depth).map(move |col| (row, col)))
            .filter(|&(row, col)| {
                row == depth || col == depth || row == last - depth || col == last - depth
            })
            .collect();

        let mut solved = Vec::with_capacity(ring.len());
        for &(row, col) in &ring {
            let (mut dx, mut dy, mut weight) = (0.0, 0.0, 0.0);
            for r in row - 1..=row + 1 {
                for c in col - 1..=col + 1 {
                    // Only the ring outside this one, which is settled.
                    let outside = r.min(c).min(last - r.min(last)).min(last - c.min(last)) < depth;
                    if !outside || r > last || c > last {
                        continue;
                    }
                    if let Some((x, y)) = placed[index(r, c)] {
                        let trust = agreement[index(r, c)] + 1e-3;
                        dx += x * trust;
                        dy += y * trust;
                        weight += trust;
                    }
                }
            }
            let expected = if weight > 0.0 { (dx / weight, dy / weight) } else { (0.0, 0.0) };
            let at = index(row, col);
            solved.push((
                at,
                (
                    across[at].displacement(period, expected.0),
                    down[at].displacement(period, expected.1),
                ),
            ));
        }
        for (at, value) in solved {
            placed[at] = Some(value);
        }
    }

    (0..count)
        .map(|at| {
            let (row, col) = (at as u32 / blocks, at as u32 % blocks);
            let (dx, dy) = placed[at].unwrap_or((0.0, 0.0));
            Patch {
                u: ((f64::from(col) + 0.5) * f64::from(BLOCK) / side).min(1.0),
                v: ((f64::from(row) + 0.5) * f64::from(BLOCK) / side).min(1.0),
                dx,
                dy,
                weight: agreement[at],
            }
        })
        .collect()
}

/// How far any point of the code is from where the homography puts it:
/// the patches, with the odd ones out replaced, and everything between them
/// interpolated.
///
/// Not a fitted curve. What a real lens does to a picture is smooth, but it is
/// not a polynomial of any degree worth fitting — a quartic through one real
/// capture was half a cell out at the corners — and the patches are measured
/// closely enough to be believed as they are.
#[derive(Debug, Clone)]
struct Surface {
    /// Patches each way.
    blocks: usize,
    /// Patch middles are this far apart, as a fraction of the code.
    pitch: f64,
    dx: Vec<f64>,
    dy: Vec<f64>,
}

impl Surface {
    fn through(patches: &[Patch], blocks: u32, grid: u32) -> Self {
        let blocks = blocks as usize;
        let pitch = f64::from(BLOCK) / f64::from(grid);

        // In rows, whatever order they were measured in.
        let mut measured = vec![(0.0, 0.0, 0.0); blocks * blocks];
        for patch in patches {
            let (row, col) = ((patch.v / pitch) as usize, (patch.u / pitch) as usize);
            measured[row.min(blocks - 1) * blocks + col.min(blocks - 1)] =
                (patch.dx, patch.dy, patch.weight);
        }

        // Each patch becomes the median of itself and the patches round it. A
        // patch measured wrongly — a reflection across it, or a stretch of the
        // picture caught as the screen changed — is then outvoted, while a
        // field that bends smoothly is left as it was.
        let median = |values: &mut Vec<f64>| -> f64 {
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
            values[values.len() / 2]
        };
        let mut dx = vec![0.0; blocks * blocks];
        let mut dy = vec![0.0; blocks * blocks];
        for row in 0..blocks {
            for col in 0..blocks {
                let (mut xs, mut ys) = (Vec::with_capacity(9), Vec::with_capacity(9));
                for r in row.saturating_sub(1)..=(row + 1).min(blocks - 1) {
                    for c in col.saturating_sub(1)..=(col + 1).min(blocks - 1) {
                        let (x, y, weight) = measured[r * blocks + c];
                        if weight > 0.0 {
                            xs.push(x);
                            ys.push(y);
                        }
                    }
                }
                if !xs.is_empty() {
                    dx[row * blocks + col] = median(&mut xs);
                    dy[row * blocks + col] = median(&mut ys);
                }
            }
        }

        Self { blocks, pitch, dx, dy }
    }

    fn at(&self, u: f64, v: f64) -> (f64, f64) {
        if self.blocks < 2 {
            return (
                self.dx.first().copied().unwrap_or(0.0),
                self.dy.first().copied().unwrap_or(0.0),
            );
        }

        // Position among the patch middles. Beyond the outermost middles the
        // slope between the last two is carried on, which is how the rings
        // round the perimeter are reached.
        let last = (self.blocks - 2) as f64;
        let (x, y) = (u / self.pitch - 0.5, v / self.pitch - 0.5);
        let (col, row) = (x.floor().clamp(0.0, last), y.floor().clamp(0.0, last));
        let (fx, fy) = ((x - col).clamp(-0.6, 1.6), (y - row).clamp(-0.6, 1.6));
        let at = row as usize * self.blocks + col as usize;
        let blend = |field: &[f64]| {
            let top = field[at] + (field[at + 1] - field[at]) * fx;
            let bottom = field[at + self.blocks]
                + (field[at + self.blocks + 1] - field[at + self.blocks]) * fx;
            top + (bottom - top) * fy
        };
        (blend(&self.dx), blend(&self.dy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::PROFILES;
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

    /// Bends a picture the way a lens does: every point moves along the line
    /// from the centre, by an amount that grows with the cube of its distance.
    fn bend(image: &RgbImage, amount: f64) -> RgbImage {
        let (w, h) = (f64::from(image.width()), f64::from(image.height()));
        let (cx, cy) = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
        let corner = cx * cx + cy * cy;
        let mut out = RgbImage::filled(image.width(), image.height(), Rgb::WHITE);
        for y in 0..image.height() {
            for x in 0..image.width() {
                let (dx, dy) = (f64::from(x) - cx, f64::from(y) - cy);
                let scale = 1.0 + amount * (dx * dx + dy * dy) / corner;
                let c = image.sample_bilinear(cx + dx * scale, cy + dy * scale);
                let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                out.set(x, y, Rgb::new(byte(c.r), byte(c.g), byte(c.b)));
            }
        }
        out
    }

    /// Where a cell-space point really is in a picture bent by `amount`.
    fn truly(
        layout: &FrameLayout,
        cell_px: u32,
        image: &RgbImage,
        amount: f64,
        point: Point,
    ) -> Point {
        let flat = layout.identity_transform(cell_px).map(point).unwrap();
        let (cx, cy) =
            ((f64::from(image.width()) - 1.0) / 2.0, (f64::from(image.height()) - 1.0) / 2.0);
        let corner = cx * cx + cy * cy;
        // Invert the bend by iteration; it is a contraction at these amounts.
        let (mut x, mut y) = (flat.x, flat.y);
        for _ in 0..20 {
            let (dx, dy) = (x - cx, y - cy);
            let scale = 1.0 + amount * (dx * dx + dy * dy) / corner;
            x = cx + (flat.x - cx) / scale;
            y = cy + (flat.y - cy) / scale;
        }
        Point::new(x, y)
    }

    #[test]
    fn an_undistorted_frame_needs_no_correction() {
        for profile in &PROFILES {
            let (layout, image) = painted(profile, 8);
            let transform = layout.identity_transform(8);
            let mesh = Mesh::fit(&layout, &image, &transform);

            assert!(mesh.coverage() > 0.95, "{} located {}", profile.name, mesh.coverage());
            assert!(
                mesh.largest_correction() < 0.04,
                "{} was moved {:.3} cells",
                profile.name,
                mesh.largest_correction()
            );
        }
    }

    #[test]
    fn a_bent_frame_is_followed_to_a_fraction_of_a_cell() {
        // Two percent is a poor lens. A homography through the four corners
        // of this frame is most of a cell out at the middle of each edge.
        let profile = &PROFILES[0];
        let cell_px = 8;
        let (layout, image) = painted(profile, cell_px);
        let amount = 0.02;
        let bent = bend(&image, amount);

        let grid = f64::from(layout.grid());
        let ideal = layout.finder_centres();
        let seen = ideal.map(|p| truly(&layout, cell_px, &image, amount, p));
        let transform = Homography::from_quads(ideal, seen).unwrap();
        let mesh = Mesh::fit(&layout, &bent, &transform);

        let mut worst_before = 0.0f64;
        let mut worst_after = 0.0f64;
        for row in (4..layout.grid()).step_by(8) {
            for col in (4..layout.grid()).step_by(8) {
                let p = Point::new(f64::from(col) + 0.5, f64::from(row) + 0.5);
                let truth = truly(&layout, cell_px, &image, amount, p);
                let before = transform.map(p).unwrap().distance(truth) / f64::from(cell_px);
                let after = mesh.locate(p).unwrap().distance(truth) / f64::from(cell_px);
                worst_before = worst_before.max(before);
                worst_after = worst_after.max(after);
            }
        }

        assert!(worst_before > 0.3, "the test bends too little: {worst_before:.3} of {grid}");
        assert!(worst_after < 0.2, "still {worst_after:.3} cells out, from {worst_before:.3}");
    }

    #[test]
    fn the_mesh_agrees_with_its_homography_when_nothing_is_measured() {
        let (layout, _) = painted(&PROFILES[1], 8);
        let transform = layout.identity_transform(8);
        let mesh = Mesh::from_homography(&layout, &transform);

        for p in [Point::new(0.0, 0.0), Point::new(17.25, 90.5), Point::new(-3.0, 131.0)] {
            let a = transform.map(p).unwrap();
            let b = mesh.locate(p).unwrap();
            assert!(a.distance(b) < 1e-6, "{p:?}: {a:?} against {b:?}");
        }
    }

    #[test]
    fn a_polynomial_is_recovered_from_its_own_samples() {
        let ts: Vec<f64> = (0..40).map(|i| f64::from(i) / 19.5 - 1.0).collect();
        let values: Vec<f64> = ts.iter().map(|t| 0.5 - 1.5 * t + 2.0 * t * t).collect();
        let fit = polyfit(&ts, &values).unwrap();
        for (&t, &value) in ts.iter().zip(values.iter()) {
            let got = fit.iter().rev().fold(0.0, |acc, c| acc * t + c);
            assert!((got - value).abs() < 0.02, "at {t}: {got} against {value}");
        }
    }
}
