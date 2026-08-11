//! A minimal RGB image buffer.
//!
//! Deliberately not an image *library*. The protocol crate has to paint frames
//! and read them back on every target it supports, but it must not learn about
//! PNG, JPEG, MP4 or a canvas — those are the adapters' business. What is left
//! is a flat buffer, a setter and a bilinear sampler, which is all the codec
//! ever asks for.

use crate::symbol::{Rgb, Rgbf};

/// A tightly packed 8-bit RGB image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl RgbImage {
    /// A new image filled with `fill`.
    ///
    /// # Panics
    ///
    /// Panics if the pixel count overflows `usize`, which no real frame does.
    #[must_use]
    pub fn filled(width: u32, height: u32, fill: Rgb) -> Self {
        let len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(3))
            .expect("image dimensions overflow");
        let mut data = Vec::with_capacity(len);
        for _ in 0..(len / 3) {
            data.extend_from_slice(&[fill.r, fill.g, fill.b]);
        }
        Self { width, height, data }
    }

    /// Wraps an existing tightly packed RGB buffer.
    ///
    /// Returns `None` if the buffer length does not match the dimensions, which
    /// is the only way an adapter can get this wrong.
    #[must_use]
    pub fn from_raw(width: u32, height: u32, data: Vec<u8>) -> Option<Self> {
        let expected = (width as usize).checked_mul(height as usize)?.checked_mul(3)?;
        (data.len() == expected).then_some(Self { width, height, data })
    }

    /// Image width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The underlying buffer, three bytes per pixel in row-major order.
    #[must_use]
    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }

    /// The underlying buffer, mutably.
    #[must_use]
    pub fn as_raw_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Consumes the image and returns its buffer.
    #[must_use]
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    fn offset(&self, x: u32, y: u32) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| (y as usize * self.width as usize + x as usize) * 3)
    }

    /// The pixel at `(x, y)`, or black outside the image.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> Rgb {
        match self.offset(x, y) {
            Some(o) => Rgb::new(self.data[o], self.data[o + 1], self.data[o + 2]),
            None => Rgb::BLACK,
        }
    }

    /// Writes the pixel at `(x, y)`. Out-of-range coordinates are ignored.
    pub fn set(&mut self, x: u32, y: u32, colour: Rgb) {
        if let Some(o) = self.offset(x, y) {
            self.data[o] = colour.r;
            self.data[o + 1] = colour.g;
            self.data[o + 2] = colour.b;
        }
    }

    /// Fills an axis-aligned rectangle, clipped to the image.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, colour: Rgb) {
        for yy in y..y.saturating_add(h).min(self.height) {
            for xx in x..x.saturating_add(w).min(self.width) {
                self.set(xx, yy, colour);
            }
        }
    }

    /// Bilinear sample at a continuous pixel coordinate.
    ///
    /// Coordinates are in pixel units with `(0.0, 0.0)` at the centre of the
    /// top-left pixel. Samples outside the image clamp to the edge rather than
    /// returning black: a frame that reaches the border of the recording should
    /// degrade at its edge, not acquire a false black rim that the classifier
    /// would then read as ink.
    #[must_use]
    pub fn sample_bilinear(&self, x: f64, y: f64) -> Rgbf {
        if self.width == 0 || self.height == 0 {
            return Rgbf::ZERO;
        }

        let max_x = f64::from(self.width - 1);
        let max_y = f64::from(self.height - 1);
        let clamped_x = x.clamp(0.0, max_x);
        let clamped_y = y.clamp(0.0, max_y);

        let (x0, fx) = split_coordinate(clamped_x);
        let (y0, fy) = split_coordinate(clamped_y);
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);

        let p00 = Rgbf::from(self.get(x0, y0));
        let p10 = Rgbf::from(self.get(x1, y0));
        let p01 = Rgbf::from(self.get(x0, y1));
        let p11 = Rgbf::from(self.get(x1, y1));

        let lerp = |a: f32, b: f32, t: f32| (b - a).mul_add(t, a);
        Rgbf::new(
            lerp(lerp(p00.r, p10.r, fx), lerp(p01.r, p11.r, fx), fy),
            lerp(lerp(p00.g, p10.g, fx), lerp(p01.g, p11.g, fx), fy),
            lerp(lerp(p00.b, p10.b, fx), lerp(p01.b, p11.b, fx), fy),
        )
    }
}

/// Splits a non-negative pixel coordinate into its integer and fractional part.
///
/// Both narrowing conversions here are the point of the function rather than an
/// accident, so the lint is silenced once, in one place, instead of at each
/// call. The caller has already clamped the coordinate into the image, so the
/// floor is non-negative and far below `u32::MAX`; the fraction lies in `[0, 1)`
/// and loses nothing an interpolation weight would miss.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "coordinate is pre-clamped into the image; the fraction is in [0, 1)"
)]
fn split_coordinate(value: f64) -> (u32, f32) {
    let floor = value.floor();
    (floor as u32, (value - floor) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_coordinate_separates_the_parts() {
        let (whole, fraction) = split_coordinate(7.25);
        assert_eq!(whole, 7);
        assert!((fraction - 0.25).abs() < 1e-6);

        let (whole, fraction) = split_coordinate(0.0);
        assert_eq!(whole, 0);
        assert!(fraction.abs() < 1e-6);
    }

    #[test]
    fn pixels_round_trip() {
        let mut img = RgbImage::filled(4, 3, Rgb::BLACK);
        assert_eq!(img.width(), 4);
        assert_eq!(img.height(), 3);
        img.set(2, 1, Rgb::new(10, 20, 30));
        assert_eq!(img.get(2, 1), Rgb::new(10, 20, 30));
        assert_eq!(img.get(0, 0), Rgb::BLACK);
    }

    #[test]
    fn out_of_range_access_is_inert() {
        let mut img = RgbImage::filled(2, 2, Rgb::WHITE);
        img.set(9, 9, Rgb::BLACK);
        assert_eq!(img.get(9, 9), Rgb::BLACK);
        assert_eq!(img.as_raw().len(), 2 * 2 * 3);
    }

    #[test]
    fn from_raw_rejects_a_mismatched_buffer() {
        assert!(RgbImage::from_raw(2, 2, vec![0; 11]).is_none());
        assert!(RgbImage::from_raw(2, 2, vec![0; 12]).is_some());
    }

    #[test]
    fn bilinear_sampling_interpolates() {
        let mut img = RgbImage::filled(2, 1, Rgb::BLACK);
        img.set(0, 0, Rgb::new(0, 0, 0));
        img.set(1, 0, Rgb::new(255, 255, 255));

        let mid = img.sample_bilinear(0.5, 0.0);
        assert!((mid.r - 0.5).abs() < 0.01, "{mid:?}");

        let left = img.sample_bilinear(0.0, 0.0);
        assert!(left.r < 0.01, "{left:?}");
    }

    #[test]
    fn sampling_outside_clamps_to_the_edge() {
        // A false black rim would read as ink to the classifier, so the edge
        // must extend rather than fall away.
        let img = RgbImage::filled(2, 2, Rgb::WHITE);
        let outside = img.sample_bilinear(-5.0, -5.0);
        assert!(outside.r > 0.99, "{outside:?}");
        let far = img.sample_bilinear(50.0, 50.0);
        assert!(far.r > 0.99, "{far:?}");
    }

    #[test]
    fn fill_rect_clips() {
        let mut img = RgbImage::filled(3, 3, Rgb::BLACK);
        img.fill_rect(1, 1, 10, 10, Rgb::WHITE);
        assert_eq!(img.get(2, 2), Rgb::WHITE);
        assert_eq!(img.get(0, 0), Rgb::BLACK);
    }
}
