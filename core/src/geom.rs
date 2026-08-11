//! Planar projective geometry: the homography between cell space and pixels.
//!
//! A phone screen is flat and a camera is a pinhole, so the map between the two
//! is a plane-to-plane projectivity — a 3x3 matrix, eight free parameters, fixed
//! exactly by four point correspondences. That is why the format spends four
//! corners on finder patterns: it is the smallest number that determines the
//! transform.
//!
//! Four points determine it but do not *check* it. Any four detections, right or
//! wrong, produce some homography, and a wrong one yields a frame of confident
//! nonsense. The centre alignment marker is the fifth point that turns the
//! transform into something falsifiable, via [`Homography::reprojection_error`].

/// A point in a plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

impl Point {
    /// A point from its coordinates.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Euclidean distance to another point.
    #[must_use]
    pub fn distance(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// A plane-to-plane projective transform, stored row-major and normalised so
/// that `m[8] == 1` whenever that is possible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography {
    m: [f64; 9],
}

impl Homography {
    /// The identity transform.
    #[must_use]
    pub const fn identity() -> Self {
        Self { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] }
    }

    /// The transform's coefficients, row-major.
    #[must_use]
    pub const fn coefficients(&self) -> &[f64; 9] {
        &self.m
    }

    /// A transform from raw coefficients.
    #[must_use]
    pub const fn from_coefficients(m: [f64; 9]) -> Self {
        Self { m }
    }

    /// The transform carrying the unit square's corners to `dst`.
    ///
    /// `dst` is ordered top-left, top-right, bottom-right, bottom-left, matching
    /// the corner order used everywhere else in the crate.
    ///
    /// Returns `None` when the four destination points are degenerate — three
    /// collinear, or two coincident — which is exactly the case a detector hits
    /// when it has locked onto something that is not a frame.
    #[must_use]
    pub fn from_unit_square(dst: [Point; 4]) -> Option<Self> {
        // Classic direct construction. With sources (0,0), (1,0), (1,1), (0,1)
        // the projective terms fall out of the diagonal mismatch, so no linear
        // solve is needed.
        let [p0, p1, p2, p3] = dst;

        let dx1 = p1.x - p2.x;
        let dx2 = p3.x - p2.x;
        let dy1 = p1.y - p2.y;
        let dy2 = p3.y - p2.y;
        let sx = p0.x - p1.x + p2.x - p3.x;
        let sy = p0.y - p1.y + p2.y - p3.y;

        let den = dx1 * dy2 - dx2 * dy1;
        if den.abs() < 1e-12 {
            // An affine quadrilateral: parallelogram, or degenerate.
            if sx.abs() > 1e-9 || sy.abs() > 1e-9 {
                return None;
            }
            let m = [p1.x - p0.x, p3.x - p0.x, p0.x, p1.y - p0.y, p3.y - p0.y, p0.y, 0.0, 0.0, 1.0];
            return Self::validated(m);
        }

        let g = (sx * dy2 - dx2 * sy) / den;
        let h = (dx1 * sy - sx * dy1) / den;

        let m = [
            p1.x - p0.x + g * p1.x,
            p3.x - p0.x + h * p3.x,
            p0.x,
            p1.y - p0.y + g * p1.y,
            p3.y - p0.y + h * p3.y,
            p0.y,
            g,
            h,
            1.0,
        ];
        Self::validated(m)
    }

    /// The transform carrying `src` to `dst`, both in corner order.
    #[must_use]
    pub fn from_quads(src: [Point; 4], dst: [Point; 4]) -> Option<Self> {
        let a = Self::from_unit_square(src)?;
        let b = Self::from_unit_square(dst)?;
        Some(b.compose(&a.inverse()?))
    }

    /// Accepts a candidate matrix only if it is finite and actually invertible.
    ///
    /// The finiteness check alone is not enough. Four coincident points take the
    /// affine branch and produce an all-zero matrix, which is perfectly finite
    /// and maps the entire frame onto a single pixel — a detector that had
    /// locked onto one blob would be handed a transform and would go on to
    /// "decode" it.
    fn validated(m: [f64; 9]) -> Option<Self> {
        if !m.iter().all(|v| v.is_finite()) {
            return None;
        }
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        (det.abs() > 1e-12).then_some(Self { m })
    }

    /// Maps a point through the transform.
    ///
    /// Points on the horizon — those whose homogeneous weight vanishes — have no
    /// image, and are reported as `None` rather than as an infinity that would
    /// silently poison a sampling loop.
    #[must_use]
    pub fn map(&self, point: Point) -> Option<Point> {
        let m = &self.m;
        let weight = m[6] * point.x + m[7] * point.y + m[8];
        if weight.abs() < 1e-12 {
            return None;
        }
        let mapped_x = (m[0] * point.x + m[1] * point.y + m[2]) / weight;
        let mapped_y = (m[3] * point.x + m[4] * point.y + m[5]) / weight;
        (mapped_x.is_finite() && mapped_y.is_finite()).then(|| Point::new(mapped_x, mapped_y))
    }

    /// Composition: the transform applying `self` after `other`.
    #[must_use]
    pub fn compose(&self, other: &Self) -> Self {
        let (a, b) = (&self.m, &other.m);
        let mut m = [0.0f64; 9];
        for row in 0..3 {
            for col in 0..3 {
                m[row * 3 + col] =
                    a[row * 3] * b[col] + a[row * 3 + 1] * b[3 + col] + a[row * 3 + 2] * b[6 + col];
            }
        }
        Self { m }
    }

    /// The inverse transform, or `None` when the matrix is singular.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        let m = &self.m;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];

        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        if det.abs() < 1e-12 {
            return None;
        }

        let mut inv = [0.0f64; 9];
        for (slot, value) in inv.iter_mut().zip(c.iter()) {
            *slot = value / det;
        }
        Self::validated(inv)
    }

    /// Distance between where the transform puts `src` and where `observed`
    /// actually is.
    ///
    /// This is the only thing standing between a mis-detection and a frame of
    /// confident nonsense: four points always produce *some* homography, so the
    /// fifth point has to be checked against it.
    #[must_use]
    pub fn reprojection_error(&self, src: Point, observed: Point) -> f64 {
        self.map(src).map_or(f64::INFINITY, |p| p.distance(observed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-6
    }

    #[test]
    fn identity_maps_points_to_themselves() {
        let h = Homography::identity();
        let p = Point::new(3.5, -2.25);
        assert!(close(h.map(p).unwrap(), p));
    }

    #[test]
    fn unit_square_lands_on_its_corners() {
        let dst = [
            Point::new(10.0, 20.0),
            Point::new(110.0, 25.0),
            Point::new(105.0, 130.0),
            Point::new(5.0, 120.0),
        ];
        let h = Homography::from_unit_square(dst).expect("non-degenerate quad");

        let src = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(0.0, 1.0),
        ];
        for (s, d) in src.iter().zip(dst.iter()) {
            assert!(close(h.map(*s).unwrap(), *d), "{s:?} -> {:?} != {d:?}", h.map(*s));
        }
    }

    #[test]
    fn a_parallelogram_is_handled_without_projective_terms() {
        // The general construction divides by a determinant that vanishes for
        // an affine quad, so this branch has to exist and has to be right.
        let dst = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 2.0),
            Point::new(12.0, 12.0),
            Point::new(2.0, 10.0),
        ];
        let h = Homography::from_unit_square(dst).expect("parallelogram");
        assert!(close(h.map(Point::new(1.0, 1.0)).unwrap(), dst[2]));
        assert!(h.coefficients()[6].abs() < 1e-12);
        assert!(h.coefficients()[7].abs() < 1e-12);
    }

    #[test]
    fn degenerate_quads_are_rejected() {
        // A detector that has locked onto four collinear blobs must not be
        // handed a transform that maps a frame onto a line.
        let collinear = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(2.0, 2.0),
            Point::new(3.0, 3.0),
        ];
        assert!(Homography::from_unit_square(collinear).is_none());

        let coincident = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 0.0),
            Point::new(0.0, 0.0),
            Point::new(0.0, 0.0),
        ];
        assert!(Homography::from_unit_square(coincident).is_none());
    }

    #[test]
    fn inverse_undoes_the_transform() {
        let dst = [
            Point::new(-4.0, 7.0),
            Point::new(96.0, 1.0),
            Point::new(120.0, 90.0),
            Point::new(11.0, 111.0),
        ];
        let h = Homography::from_unit_square(dst).unwrap();
        let inv = h.inverse().expect("invertible");

        for p in [Point::new(0.25, 0.75), Point::new(0.5, 0.5), Point::new(0.9, 0.1)] {
            let there = h.map(p).unwrap();
            let back = inv.map(there).unwrap();
            assert!(close(back, p), "{p:?} -> {there:?} -> {back:?}");
        }
    }

    #[test]
    fn quad_to_quad_carries_the_corners_across() {
        let src = [
            Point::new(2.0, 2.0),
            Point::new(10.0, 3.0),
            Point::new(11.0, 9.0),
            Point::new(1.0, 8.0),
        ];
        let dst = [
            Point::new(100.0, 50.0),
            Point::new(400.0, 60.0),
            Point::new(380.0, 300.0),
            Point::new(90.0, 280.0),
        ];
        let h = Homography::from_quads(src, dst).expect("both quads valid");
        for (s, d) in src.iter().zip(dst.iter()) {
            assert!(h.map(*s).unwrap().distance(*d) < 1e-6);
        }
    }

    #[test]
    fn reprojection_error_measures_the_miss() {
        let dst = [
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let h = Homography::from_unit_square(dst).unwrap();
        let centre = Point::new(0.5, 0.5);
        assert!(h.reprojection_error(centre, Point::new(50.0, 50.0)) < 1e-9);
        assert!((h.reprojection_error(centre, Point::new(53.0, 54.0)) - 5.0).abs() < 1e-9);
    }
}
