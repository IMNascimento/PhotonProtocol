//! `photon film`: a phone camera pointed at a monitor, both simulated.
//!
//! `photon_core::simulate` distorts one painted frame at a time, which measures
//! the classifier and says nothing about a transfer. Almost everything that
//! stops a real transfer lives in what that model leaves out:
//!
//! | Effect | What it does to a transfer |
//! | ------ | -------------------------- |
//! | rolling shutter | each sensor row is exposed at its own moment, so a picture taken while the screen changes is part of one code and part of the next |
//! | display scan-out and pixel response | the screen itself changes top to bottom, and each pixel takes milliseconds to settle |
//! | lens distortion | four corners fix a homography, and a homography cannot bend |
//! | the colour filter array | every sensor pixel sees one colour; the other two are interpolated |
//! | sharpening, tone curve, clipping | the camera's own processing, tuned for faces rather than for codes |
//! | 4:2:0 | the browser is handed colour at half resolution |
//! | hand shake | the pose moves between pictures, and during one |
//!
//! This renders the pictures a browser would be handed by `getUserMedia`, from
//! the frames the sending page would paint, with the display and the camera
//! running on their own clocks. The output is a directory of PNGs for
//! `photon decode` and a `.y4m` file Chrome will play as a camera
//! (`--use-file-for-fake-video-capture`), so the receiving page itself can be
//! tested end to end without a phone.
//!
//! It is a model. It exists so that a change can be shown to survive conditions
//! at least this hostile before anybody is asked to pick up a phone.

// Same reasoning as `photon_core::simulate`: this file converts between pixel
// coordinates, colour channels and indices on nearly every line, every
// conversion is bounded by construction, and wrapping each in `try_from` would
// hide the model rather than protect it.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::struct_excessive_bools
)]

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};

use photon_core::dense::{DenseProfile, DenseTransmitter, QUIET_MODULES};
use photon_core::frame::QUIET_ZONE_CELLS;
use photon_core::session::Transmitter;
use photon_core::{ProfileId, RgbImage};
use rayon::prelude::*;

use crate::media;

/// How hostile the simulated capture is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Preset {
    /// A global shutter, square on, in perfect focus. For telling a broken
    /// harness from a broken decoder.
    Ideal,
    /// A recent phone, held carefully, close to the screen.
    Good,
    /// An ordinary phone held by hand at a slight angle.
    Typical,
    /// An older phone, further away, at an angle, in a room with a lamp.
    Poor,
}

/// Which way the sensor reads itself out, as seen in the picture.
///
/// A phone's sensor is mounted so that its rows run along the phone's long
/// side. Held upright, those rows are vertical in the picture, so the shutter
/// rolls across it sideways — at right angles to the monitor's own top-to-bottom
/// scan. The two sweeps then tear the code along a diagonal, which is the worst
/// case and also the ordinary one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Sweep {
    /// Top to bottom.
    Down,
    /// Bottom to top.
    Up,
    /// Left to right.
    Right,
    /// Right to left.
    Left,
}

/// Everything about the simulated camera that a preset decides.
#[derive(Debug, Clone)]
pub(crate) struct CameraModel {
    /// Side of the code, quiet zone included, as a fraction of the picture's
    /// shorter side.
    pub(crate) fill: f64,
    /// Rotation about the vertical axis, degrees.
    pub(crate) yaw: f64,
    /// Rotation about the horizontal axis, degrees.
    pub(crate) pitch: f64,
    /// Rotation about the optical axis, degrees.
    pub(crate) roll: f64,
    /// Hand tremor, degrees.
    pub(crate) shake: f64,
    /// Radial distortion at the corner of the picture, as a fraction. Positive
    /// is barrel.
    pub(crate) distortion: f64,
    /// Standard deviation of the lens blur, in sensor pixels.
    pub(crate) blur: f64,
    /// Exposure time, milliseconds.
    pub(crate) exposure_ms: f64,
    /// Time the sensor takes to read from its first row to its last.
    pub(crate) readout_ms: f64,
    /// Time constant of the display's pixel response.
    pub(crate) lcd_tau_ms: f64,
    /// Where display white lands relative to sensor saturation. Above one it
    /// clips.
    pub(crate) gain: f32,
    /// Stray light added everywhere, as a fraction of white.
    pub(crate) lift: f32,
    /// Read noise, as a fraction of full scale.
    pub(crate) read_noise: f32,
    /// Shot noise at full scale, as a fraction of full scale.
    pub(crate) shot_noise: f32,
    /// Unsharp-mask amount applied by the camera's processing.
    pub(crate) sharpen: f32,
    /// How much of each colour leaks into the other two before correction.
    pub(crate) crosstalk: f32,
    /// How much of that leak the camera's colour correction undoes.
    pub(crate) correction: f32,
    /// White balance error, as gain on red and on blue.
    pub(crate) white_balance: [f32; 2],
    /// Darkening towards the corners, as a fraction.
    pub(crate) vignette: f32,
    /// Backlight flicker frequency in hertz, or zero for none.
    pub(crate) pwm_hz: f64,
    /// Fraction of each flicker period the backlight is lit.
    pub(crate) pwm_duty: f64,
}

/// What the command line may say of a camera that its preset says otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Overrides {
    /// Side of the code as a fraction of the picture's.
    pub(crate) fill: Option<f64>,
    /// Lens blur, in sensor pixels.
    pub(crate) blur: Option<f64>,
    /// Radial distortion at the corner.
    pub(crate) distortion: Option<f64>,
    /// Exposure time in milliseconds.
    pub(crate) exposure: Option<f64>,
    /// Sensor readout time in milliseconds.
    pub(crate) readout: Option<f64>,
    /// Where white lands relative to saturation.
    pub(crate) gain: Option<f32>,
    /// Hand tremor in degrees.
    pub(crate) shake: Option<f64>,
}

impl CameraModel {
    /// The same camera with what was asked for laid over it.
    pub(crate) fn with(mut self, asked: &Overrides) -> Self {
        self.fill = asked.fill.unwrap_or(self.fill);
        self.blur = asked.blur.unwrap_or(self.blur);
        self.distortion = asked.distortion.unwrap_or(self.distortion);
        self.exposure_ms = asked.exposure.unwrap_or(self.exposure_ms);
        self.readout_ms = asked.readout.unwrap_or(self.readout_ms);
        self.gain = asked.gain.unwrap_or(self.gain);
        self.shake = asked.shake.unwrap_or(self.shake);
        self
    }

    pub(crate) fn preset(preset: Preset) -> Self {
        match preset {
            Preset::Ideal => Self {
                fill: 0.92,
                yaw: 0.0,
                pitch: 0.0,
                roll: 0.0,
                shake: 0.0,
                distortion: 0.0,
                blur: 0.0,
                exposure_ms: 0.5,
                readout_ms: 0.0,
                lcd_tau_ms: 0.01,
                gain: 0.9,
                lift: 0.0,
                read_noise: 0.0,
                shot_noise: 0.0,
                sharpen: 0.0,
                crosstalk: 0.0,
                correction: 0.0,
                white_balance: [1.0, 1.0],
                vignette: 0.0,
                pwm_hz: 0.0,
                pwm_duty: 1.0,
            },
            Preset::Good => Self {
                fill: 0.88,
                yaw: 3.0,
                pitch: -2.0,
                roll: 1.0,
                shake: 0.05,
                distortion: 0.004,
                blur: 0.7,
                exposure_ms: 8.0,
                readout_ms: 20.0,
                lcd_tau_ms: 4.0,
                gain: 0.95,
                lift: 0.01,
                read_noise: 0.004,
                shot_noise: 0.012,
                sharpen: 0.4,
                crosstalk: 0.12,
                correction: 0.8,
                white_balance: [1.03, 0.97],
                vignette: 0.10,
                pwm_hz: 0.0,
                pwm_duty: 1.0,
            },
            Preset::Typical => Self {
                fill: 0.80,
                yaw: 6.0,
                pitch: -4.0,
                roll: 2.0,
                shake: 0.15,
                distortion: 0.009,
                blur: 1.0,
                exposure_ms: 12.0,
                readout_ms: 28.0,
                lcd_tau_ms: 6.0,
                gain: 1.12,
                lift: 0.03,
                read_noise: 0.008,
                shot_noise: 0.02,
                sharpen: 0.7,
                crosstalk: 0.18,
                correction: 0.7,
                white_balance: [1.08, 0.92],
                vignette: 0.18,
                pwm_hz: 0.0,
                pwm_duty: 1.0,
            },
            Preset::Poor => Self {
                fill: 0.66,
                yaw: -10.0,
                pitch: 8.0,
                roll: -4.0,
                shake: 0.4,
                distortion: 0.016,
                blur: 1.4,
                exposure_ms: 20.0,
                readout_ms: 33.0,
                lcd_tau_ms: 8.0,
                gain: 1.3,
                lift: 0.06,
                read_noise: 0.015,
                shot_noise: 0.03,
                sharpen: 1.0,
                crosstalk: 0.24,
                correction: 0.6,
                white_balance: [1.15, 0.85],
                vignette: 0.28,
                pwm_hz: 240.0,
                pwm_duty: 0.6,
            },
        }
    }
}

/// What to film and how.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    /// The file the simulated sender is sending.
    pub(crate) input: PathBuf,
    /// Where the pictures go.
    pub(crate) out: PathBuf,
    /// Physical-layer profile.
    pub(crate) profile: ProfileId,
    /// The dense profile, which is filmed instead when there is one.
    pub(crate) dense: Option<&'static DenseProfile>,
    /// The monitor, in pixels.
    pub(crate) screen: (u32, u32),
    /// Pixels per cell, or `None` to fit the screen the way the sending page
    /// does.
    pub(crate) cell_px: Option<u32>,
    /// Display refreshes each code is held for.
    pub(crate) hold: u32,
    /// Display refresh rate, hertz.
    pub(crate) refresh: f64,
    /// The camera's picture, in pixels.
    pub(crate) camera: (u32, u32),
    /// Pictures per second.
    pub(crate) fps: f64,
    /// How long to film.
    pub(crate) seconds: f64,
    /// The camera.
    pub(crate) model: CameraModel,
    /// Seed for every random draw.
    pub(crate) seed: u64,
    /// Which way the shutter rolls, or `None` for what a phone held the way
    /// the picture's shape implies would do.
    pub(crate) sweep: Option<Sweep>,
    /// Where in a code's time on screen the first picture is taken, from 0 to
    /// 1, or `None` to draw it at random.
    pub(crate) phase: Option<f64>,
    /// Write PNGs.
    pub(crate) png: bool,
    /// Write a `.y4m` file.
    pub(crate) y4m: bool,
}

/// Fraction of the available box the sending page fills. Kept equal to
/// `FIT_MARGIN` in `web-emitter/emitter.js`, because this stands in for it.
const FIT_MARGIN: f64 = 0.96;

/// Width of the monitor's bezel, in screen pixels.
const BEZEL: f64 = 26.0;

/// Height of the status bar the sending page draws along the bottom.
const OVERLAY_BAR: u32 = 38;

/// Fraction of a refresh the display spends scanning out its rows.
const SCANOUT: f64 = 0.92;

/// Samples taken per sensor pixel.
const TAPS: usize = 32;

/// The session every filmed transfer is given, so that `photon decode --truth`
/// can paint the same codes again.
pub(crate) const SESSION: u32 = 0x00C0_FFEE;

// --- small deterministic noise ------------------------------------------------

#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut rng = Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        for _ in 0..4 {
            rng.next_u64();
        }
        rng
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in `[0, 1)`.
    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal, by Box-Muller.
    fn normal(&mut self) -> f32 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        ((-2.0 * u1.ln()).sqrt() * (core::f64::consts::TAU * u2).cos()) as f32
    }
}

/// A value in `[0, 1)` that depends only on two integers.
fn hash01(x: i64, y: i64) -> f32 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// The radical inverse of `index` in `base`: one axis of a Halton sequence.
fn halton(mut index: u32, base: u32) -> f64 {
    let mut result = 0.0;
    let mut fraction = 1.0 / f64::from(base);
    while index > 0 {
        result += f64::from(index % base) * fraction;
        index /= base;
        fraction /= f64::from(base);
    }
    result
}

// --- the display ----------------------------------------------------------------

/// sRGB to linear light.
fn srgb_to_linear_table() -> [f32; 256] {
    core::array::from_fn(|i| {
        let v = i as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    })
}

/// Linear light to sRGB, as a fraction.
fn linear_to_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055f32.mul_add(v.powf(1.0 / 2.4), -0.055) }
}

/// Whatever is painting the codes.
enum Sender {
    Cells(Box<Transmitter>),
    Dense(Box<DenseTransmitter>),
}

impl Sender {
    fn next_code(&mut self, px: u32) -> Result<RgbImage, String> {
        match self {
            Self::Cells(transmitter) => {
                transmitter.next_frame(px).map(|frame| frame.image).map_err(|e| e.to_string())
            }
            Self::Dense(transmitter) => Ok(transmitter.next_frame(px)),
        }
    }
}

/// The monitor and what it is showing.
struct Display {
    width: u32,
    height: u32,
    /// Top-left corner of the code on the screen.
    origin: (u32, u32),
    /// Width and height of the code, quiet zone included.
    size: (u32, u32),
    /// Height of the status bar along the bottom of the screen.
    bar: u32,
    hold: u32,
    refresh_period: f64,
    /// Codes on hand, oldest first, and the index of the oldest.
    codes: VecDeque<RgbImage>,
    first_code: usize,
    sender: Sender,
    cell_px: u32,
    lut: [f32; 256],
}

impl Display {
    /// Makes sure codes `from..=to` are on hand.
    fn ensure(&mut self, from: usize, to: usize) -> Result<(), String> {
        while self.first_code + self.codes.len() <= to {
            let code = self.sender.next_code(self.cell_px)?;
            self.codes.push_back(code);
        }
        while self.first_code < from && self.codes.len() > 1 {
            self.codes.pop_front();
            self.first_code += 1;
        }
        Ok(())
    }

    fn code(&self, index: usize) -> Option<&RgbImage> {
        index.checked_sub(self.first_code).and_then(|i| self.codes.get(i))
    }
}

/// What a point of the scene looks like, apart from the code itself.
#[derive(Clone, Copy)]
enum Surface {
    /// Inside the code: look the pixel up.
    Code(u32, u32),
    /// Anything that does not change from one code to the next.
    Fixed([f32; 3]),
}

impl Display {
    /// What is at scene position `(u, v)`, in screen pixels from the screen's
    /// top-left corner.
    fn surface(&self, u: f64, v: f64) -> Surface {
        let (w, h) = (f64::from(self.width), f64::from(self.height));

        if u >= 0.0 && v >= 0.0 && u < w && v < h {
            let (i, j) = (u as u32, v as u32);
            if j >= self.height.saturating_sub(self.bar) {
                // The status bar: white text on near-black, modelled as its
                // average.
                return Surface::Fixed([0.06, 0.06, 0.06]);
            }
            let (x0, y0) = self.origin;
            if i >= x0 && j >= y0 && i < x0 + self.size.0 && j < y0 + self.size.1 {
                return Surface::Code(i - x0, j - y0);
            }
            // The stage around the code, which the page paints white.
            return Surface::Fixed([1.0, 1.0, 1.0]);
        }

        if u >= -BEZEL && v >= -BEZEL && u < w + BEZEL && v < h + BEZEL {
            return Surface::Fixed([0.015, 0.015, 0.015]);
        }

        // Beyond the monitor: a wall above, and below it a desk with things on
        // it. The clutter is there on purpose. A detector that has only ever
        // seen a code on a plain background has not been asked whether it can
        // tell a finder pattern from a keyboard.
        let wall = 0.22 + 0.06 * ((u / w).sin() as f32) + 0.04 * ((v / h * 1.7).cos() as f32);
        if v < h + BEZEL {
            return Surface::Fixed([wall, wall * 0.97, wall * 0.9]);
        }

        let key = 46.0;
        let (kx, ky) = ((u / key).floor() as i64, (v / key).floor() as i64);
        let (fx, fy) = (u / key - kx as f64, v / key - ky as f64);
        let gap = fx < 0.12 || fy < 0.12;
        let shade = if gap { 0.02 } else { 0.05 + 0.5 * hash01(kx, ky) };
        Surface::Fixed([shade, shade, shade * 0.95])
    }

    /// When the display starts showing code `k` on screen row `v`.
    fn switch_time(&self, k: usize, v: f64) -> f64 {
        let row = (v / f64::from(self.height)).clamp(0.0, 1.0);
        (k as f64 * f64::from(self.hold) + SCANOUT * row) * self.refresh_period
    }

    /// The code being switched to at time `t` on row `v`.
    fn code_at(&self, t: f64, v: f64) -> usize {
        let row = (v / f64::from(self.height)).clamp(0.0, 1.0);
        let refreshes = t / self.refresh_period - SCANOUT * row;
        if refreshes <= 0.0 { 0 } else { (refreshes / f64::from(self.hold)).floor() as usize }
    }
}

/// How much of an exposure each code contributed, for one sensor pixel.
///
/// A pixel of the display does not jump to its new value. It approaches it
/// exponentially, so for some milliseconds after a change the light leaving the
/// screen is a blend of the old code and the new one — and an exposure that
/// overlaps those milliseconds records the blend.
#[derive(Clone, Copy, Default)]
struct Blend {
    first: usize,
    weights: [f32; 4],
}

fn blend_for(display: &Display, open: f64, close: f64, v: f64, tau: f64) -> Blend {
    let span = (close - open).max(1e-9);
    let last = display.code_at(close, v);
    let first = display.code_at(open, v).saturating_sub(1).max(last.saturating_sub(3));

    let mut blend = Blend { first, weights: [0.0; 4] };
    let mut add = |k: usize, amount: f64| {
        if k >= first && k - first < 4 {
            blend.weights[k - first] += (amount / span) as f32;
        }
    };

    for k in first..=last {
        let start = if k == 0 { f64::NEG_INFINITY } else { display.switch_time(k, v) };
        let end = display.switch_time(k + 1, v);
        let (p, q) = (open.max(start), close.min(end));
        if q <= p {
            continue;
        }
        if k == 0 || tau <= 1e-6 {
            add(k, q - p);
            continue;
        }
        // Light still coming from the previous code while this one settles.
        let residual = tau * ((-(p - start) / tau).exp() - (-(q - start) / tau).exp());
        add(k - 1, residual);
        add(k, (q - p) - residual);
    }

    let total: f32 = blend.weights.iter().sum();
    if total > 0.0 {
        for weight in &mut blend.weights {
            *weight /= total;
        }
    } else {
        blend.weights[0] = 1.0;
    }
    blend
}

/// Fraction of `[open, close]` the backlight was lit, relative to its average.
fn backlight(open: f64, close: f64, hz: f64, duty: f64) -> f32 {
    if hz <= 0.0 || duty >= 1.0 {
        return 1.0;
    }
    let period = 1.0 / hz;
    let lit_until = |t: f64| {
        let cycles = (t / period).floor();
        let phase = t - cycles * period;
        cycles * duty * period + phase.min(duty * period)
    };
    let lit = lit_until(close) - lit_until(open);
    (lit / ((close - open).max(1e-9) * duty)) as f32
}

// --- the camera -----------------------------------------------------------------

/// Where the camera is and which way it points, at one instant.
#[derive(Clone, Copy)]
struct Pose {
    position: [f64; 3],
    right: [f64; 3],
    down: [f64; 3],
    forward: [f64; 3],
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalise(a: [f64; 3]) -> [f64; 3] {
    let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-12);
    [a[0] / n, a[1] / n, a[2] / n]
}

impl Pose {
    /// A camera `distance` from `target`, looking at it.
    ///
    /// The scene's axes are the screen's: x to the right, y down, z into the
    /// screen, in screen pixels, with the origin at the screen's centre.
    fn looking_at(target: [f64; 2], distance: f64, yaw: f64, pitch: f64, roll: f64) -> Self {
        let position = [
            target[0] + distance * yaw.sin() * pitch.cos(),
            target[1] + distance * pitch.sin(),
            -distance * yaw.cos() * pitch.cos(),
        ];
        let forward = normalise([target[0] - position[0], target[1] - position[1], -position[2]]);
        let right = normalise(cross([0.0, 1.0, 0.0], forward));
        let down = cross(forward, right);

        let (s, c) = roll.sin_cos();
        let rolled_right =
            [c * right[0] + s * down[0], c * right[1] + s * down[1], c * right[2] + s * down[2]];
        let rolled_down =
            [c * down[0] - s * right[0], c * down[1] - s * right[1], c * down[2] - s * right[2]];

        Self { position, right: rolled_right, down: rolled_down, forward }
    }

    /// Where the ray through normalised image point `(x, y)` meets the screen.
    fn onto_screen(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let ray = [
            x * self.right[0] + y * self.down[0] + self.forward[0],
            x * self.right[1] + y * self.down[1] + self.forward[1],
            x * self.right[2] + y * self.down[2] + self.forward[2],
        ];
        if ray[2] <= 1e-9 {
            return None;
        }
        let t = -self.position[2] / ray[2];
        Some((self.position[0] + t * ray[0], self.position[1] + t * ray[1]))
    }
}

/// Hand tremor: a few slow sinusoids, so the pose wanders rather than jumps.
struct Tremor {
    components: Vec<[f64; 3]>,
    amplitude: f64,
}

impl Tremor {
    fn new(rng: &mut Rng, degrees: f64) -> Self {
        let components = (0..5)
            .map(|_| {
                [
                    0.7 + rng.uniform() * 7.0,
                    rng.uniform() * core::f64::consts::TAU,
                    0.5 + rng.uniform(),
                ]
            })
            .collect();
        Self { components, amplitude: degrees.to_radians() }
    }

    fn at(&self, t: f64) -> f64 {
        let sum: f64 = self
            .components
            .iter()
            .map(|[hz, phase, weight]| weight * (core::f64::consts::TAU * hz * t + phase).sin())
            .sum();
        self.amplitude * sum / 2.2
    }
}

/// One picture, in linear light, before the sensor has quantised anything.
struct Exposure {
    width: usize,
    height: usize,
    /// Three channels per pixel.
    light: Vec<f32>,
}

struct Rig<'a> {
    options: &'a Options,
    sweep: Sweep,
    focal: f64,
    distance: f64,
    taps: Vec<(f64, f64)>,
    tremor: [Tremor; 3],
}

impl Rig<'_> {
    fn pose_at(&self, t: f64, centre: [f64; 2]) -> Pose {
        let model = &self.options.model;
        Pose::looking_at(
            centre,
            self.distance,
            model.yaw.to_radians() + self.tremor[0].at(t),
            model.pitch.to_radians() + self.tremor[1].at(t),
            model.roll.to_radians() + self.tremor[2].at(t),
        )
    }

    /// Exposes one picture whose first row finishes exposing at `start`.
    fn expose(&self, display: &Display, start: f64) -> Exposure {
        let model = &self.options.model;
        let (width, height) = (self.options.camera.0 as usize, self.options.camera.1 as usize);
        let (cx, cy) = ((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0);
        let corner = (cx * cx + cy * cy) / (self.focal * self.focal);

        let exposure = model.exposure_ms / 1000.0;
        let readout = model.readout_ms / 1000.0;
        let tau = model.lcd_tau_ms / 1000.0;

        // The code sits in the middle of the screen, and that is what a person
        // aims at.
        let target = [
            f64::from(display.origin.0) + f64::from(display.size.0) / 2.0
                - f64::from(display.width) / 2.0,
            f64::from(display.origin.1) + f64::from(display.size.1) / 2.0
                - f64::from(display.height) / 2.0,
        ];
        let half = [f64::from(display.width) / 2.0, f64::from(display.height) / 2.0];

        // Everything that depends on when a pixel was exposed, worked out once
        // for each position along the sweep rather than once per pixel.
        let along = match self.sweep {
            Sweep::Down | Sweep::Up => height,
            Sweep::Right | Sweep::Left => width,
        };
        let moments: Vec<(f64, f64, Pose, f32)> = (0..along)
            .map(|index| {
                let fraction = match self.sweep {
                    Sweep::Down | Sweep::Right => index as f64 / along as f64,
                    Sweep::Up | Sweep::Left => 1.0 - index as f64 / along as f64,
                };
                let close = start + readout * fraction;
                let open = close - exposure;
                // The pose this part of the picture was exposed from. A moving
                // hand bends the picture, because no two rows share a moment.
                let pose = self.pose_at(f64::midpoint(open, close), target);
                let flicker = backlight(open, close, model.pwm_hz, model.pwm_duty);
                (open, close, pose, flicker)
            })
            .collect();

        let mut light = vec![0.0f32; width * height * 3];

        light.par_chunks_mut(width * 3).enumerate().for_each(|(y, row)| {
            for x in 0..width {
                let (open, close, pose, flicker) = match self.sweep {
                    Sweep::Down | Sweep::Up => moments[y],
                    Sweep::Right | Sweep::Left => moments[x],
                };

                let project = |px: f64, py: f64| -> Option<(f64, f64)> {
                    let (nx, ny) = ((px - cx) / self.focal, (py - cy) / self.focal);
                    let r2 = nx * nx + ny * ny;
                    let bend = 1.0 + model.distortion * r2 / corner;
                    pose.onto_screen(nx * bend, ny * bend).map(|(u, v)| (u + half[0], v + half[1]))
                };

                let (px, py) = (x as f64, y as f64);
                let Some((u0, v0)) = project(px, py) else { continue };
                let Some((ux, vx)) = project(px + 1.0, py) else { continue };
                let Some((uy, vy)) = project(px, py + 1.0) else { continue };
                let jacobian = [ux - u0, uy - u0, vx - v0, vy - v0];

                let blend = blend_for(display, open, close, v0, tau);

                let mut sum = [0.0f32; 3];
                for &(dx, dy) in &self.taps {
                    let u = u0 + jacobian[0] * dx + jacobian[1] * dy;
                    let v = v0 + jacobian[2] * dx + jacobian[3] * dy;

                    match display.surface(u, v) {
                        Surface::Fixed(colour) => {
                            // Only the screen itself flickers; the wall is lit
                            // by the room.
                            let inside = u >= 0.0
                                && v >= 0.0
                                && u < f64::from(display.width)
                                && v < f64::from(display.height);
                            let k = if inside { flicker } else { 1.0 };
                            sum[0] += colour[0] * k;
                            sum[1] += colour[1] * k;
                            sum[2] += colour[2] * k;
                        }
                        Surface::Code(i, j) => {
                            for (offset, &weight) in blend.weights.iter().enumerate() {
                                if weight <= 0.0 {
                                    continue;
                                }
                                let Some(code) = display.code(blend.first + offset) else {
                                    continue;
                                };
                                let pixel = code.get(i, j);
                                let k = weight * flicker;
                                sum[0] += display.lut[pixel.r as usize] * k;
                                sum[1] += display.lut[pixel.g as usize] * k;
                                sum[2] += display.lut[pixel.b as usize] * k;
                            }
                        }
                    }
                }

                let n = self.taps.len() as f32;
                let (dx, dy) = ((px - cx) / cx.max(1.0), (py - cy) / cy.max(1.0));
                let falloff = 1.0 - model.vignette * f64::midpoint(dx * dx, dy * dy) as f32;
                row[x * 3] = sum[0] / n * falloff;
                row[x * 3 + 1] = sum[1] / n * falloff;
                row[x * 3 + 2] = sum[2] / n * falloff;
            }
        });

        Exposure { width, height, light }
    }
}

/// Which colour the filter over sensor pixel `(x, y)` passes. RGGB.
const fn filter_at(x: usize, y: usize) -> usize {
    match (y & 1, x & 1) {
        (0, 0) => 0,
        (1, 1) => 2,
        _ => 1,
    }
}

/// The sensor and everything the camera does to its output.
fn develop(exposure: &Exposure, model: &CameraModel, seed: u64) -> RgbImage {
    let (width, height) = (exposure.width, exposure.height);

    // Stray light scales with how bright the scene is overall: it is the scene's
    // own light scattered inside the lens.
    let mean = exposure.light.iter().sum::<f32>() / exposure.light.len().max(1) as f32;
    let veil = model.lift * mean.max(0.05) * 2.0;

    // Each filter passes some of its neighbours' light.
    let leak = model.crosstalk;
    let own = 1.0 - leak;
    let mix = |c: [f32; 3]| -> [f32; 3] {
        [
            own * c[0] + leak * 0.8 * c[1] + leak * 0.2 * c[2],
            leak * 0.5 * c[0] + own * c[1] + leak * 0.5 * c[2],
            leak * 0.2 * c[0] + leak * 0.8 * c[1] + own * c[2],
        ]
    };

    // The mosaic: one colour per pixel, with noise, clipped at saturation.
    let mut mosaic = vec![0.0f32; width * height];
    mosaic.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let mut rng = Rng::new(seed ^ ((y as u64 + 1) * 0x0100_0000_01B3));
        for (x, slot) in row.iter_mut().enumerate() {
            let at = (y * width + x) * 3;
            let scene = [
                exposure.light[at] + veil,
                exposure.light[at + 1] + veil,
                exposure.light[at + 2] + veil,
            ];
            let seen = mix(scene);
            let channel = filter_at(x, y);
            let balance = match channel {
                0 => model.white_balance[0],
                2 => model.white_balance[1],
                _ => 1.0,
            };
            let signal = (seen[channel] * model.gain * balance).max(0.0);
            let noise = model.shot_noise.mul_add(signal.sqrt(), 0.0).hypot(model.read_noise);
            *slot = (signal + rng.normal() * noise).clamp(0.0, 1.0);
        }
    });

    // Bilinear demosaicing: every missing colour is the mean of the nearest
    // pixels that measured it.
    let at = |x: isize, y: isize| -> f32 {
        let xx = x.clamp(0, width as isize - 1) as usize;
        let yy = y.clamp(0, height as isize - 1) as usize;
        mosaic[yy * width + xx]
    };
    let mut rgb = vec![0.0f32; width * height * 3];
    rgb.par_chunks_mut(width * 3).enumerate().for_each(|(y, row)| {
        for x in 0..width {
            let (ix, iy) = (x as isize, y as isize);
            let here = at(ix, iy);
            let cross4 = (at(ix - 1, iy) + at(ix + 1, iy) + at(ix, iy - 1) + at(ix, iy + 1)) / 4.0;
            let diagonal4 =
                (at(ix - 1, iy - 1) + at(ix + 1, iy - 1) + at(ix - 1, iy + 1) + at(ix + 1, iy + 1))
                    / 4.0;
            let horizontal = f32::midpoint(at(ix - 1, iy), at(ix + 1, iy));
            let vertical = f32::midpoint(at(ix, iy - 1), at(ix, iy + 1));

            let (r, g, b) = match (y & 1, x & 1) {
                (0, 0) => (here, cross4, diagonal4),
                (1, 1) => (diagonal4, cross4, here),
                // A green pixel on a red row has red beside it and blue above.
                (0, 1) => (horizontal, here, vertical),
                _ => (vertical, here, horizontal),
            };

            // Colour correction undoes part of the filter leak, which is also
            // what makes it amplify noise.
            let k = model.correction * leak;
            let own = 1.0 + k;
            let corrected = [
                own * r - k * 0.8 * g - k * 0.2 * b,
                own * g - k * 0.5 * r - k * 0.5 * b,
                own * b - k * 0.2 * r - k * 0.8 * g,
            ];

            row[x * 3] = linear_to_srgb(corrected[0]);
            row[x * 3 + 1] = linear_to_srgb(corrected[1]);
            row[x * 3 + 2] = linear_to_srgb(corrected[2]);
        }
    });

    if model.sharpen > 0.0 {
        rgb = sharpen(&rgb, width, height, model.sharpen);
    }

    let bytes: Vec<u8> = rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    RgbImage::from_raw(width as u32, height as u32, bytes)
        .unwrap_or_else(|| RgbImage::filled(1, 1, photon_core::Rgb::BLACK))
}

/// Unsharp masking, which is what gives phone pictures their halos.
fn sharpen(rgb: &[f32], width: usize, height: usize, amount: f32) -> Vec<f32> {
    const KERNEL: [f32; 3] = [0.25, 0.5, 0.25];

    let mut horizontal = vec![0.0f32; rgb.len()];
    horizontal.par_chunks_mut(width * 3).enumerate().for_each(|(y, row)| {
        for x in 0..width {
            for c in 0..3 {
                let mut acc = 0.0;
                for (k, weight) in KERNEL.iter().enumerate() {
                    let xx = (x as isize + k as isize - 1).clamp(0, width as isize - 1) as usize;
                    acc += rgb[(y * width + xx) * 3 + c] * weight;
                }
                row[x * 3 + c] = acc;
            }
        }
    });

    let mut out = vec![0.0f32; rgb.len()];
    out.par_chunks_mut(width * 3).enumerate().for_each(|(y, row)| {
        for x in 0..width {
            for c in 0..3 {
                let mut blurred = 0.0;
                for (k, weight) in KERNEL.iter().enumerate() {
                    let yy = (y as isize + k as isize - 1).clamp(0, height as isize - 1) as usize;
                    blurred += horizontal[(yy * width + x) * 3 + c] * weight;
                }
                let original = rgb[(y * width + x) * 3 + c];
                row[x * 3 + c] = amount.mul_add(original - blurred, original);
            }
        }
    });
    out
}

// --- what the browser is handed -------------------------------------------------

/// A picture as three planes, colour at half resolution.
struct Planes {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// BT.709, limited range: what a browser assumes of high-definition video.
fn to_planes(image: &RgbImage) -> Planes {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let raw = image.as_raw();
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));

    let mut y = vec![0u8; width * height];
    let mut cb = vec![0.0f32; width * height];
    let mut cr = vec![0.0f32; width * height];

    for index in 0..width * height {
        let (r, g, b) = (
            f32::from(raw[index * 3]) / 255.0,
            f32::from(raw[index * 3 + 1]) / 255.0,
            f32::from(raw[index * 3 + 2]) / 255.0,
        );
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        y[index] = (16.0 + 219.0 * luma).round().clamp(0.0, 255.0) as u8;
        cb[index] = (b - luma) / 1.8556;
        cr[index] = (r - luma) / 1.5748;
    }

    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for cy in 0..ch {
        for cx in 0..cw {
            let mut sums = (0.0f32, 0.0f32, 0.0f32);
            for dy in 0..2 {
                for dx in 0..2 {
                    let (x, yy) = ((cx * 2 + dx).min(width - 1), (cy * 2 + dy).min(height - 1));
                    sums.0 += cb[yy * width + x];
                    sums.1 += cr[yy * width + x];
                    sums.2 += 1.0;
                }
            }
            u[cy * cw + cx] = (128.0 + 224.0 * sums.0 / sums.2).round().clamp(0.0, 255.0) as u8;
            v[cy * cw + cx] = (128.0 + 224.0 * sums.1 / sums.2).round().clamp(0.0, 255.0) as u8;
        }
    }

    Planes { width, height, y, u, v }
}

/// The picture a browser draws from those planes.
fn from_planes(planes: &Planes) -> RgbImage {
    let (width, height) = (planes.width, planes.height);
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut out = vec![0u8; width * height * 3];

    let chroma = |plane: &[u8], x: usize, y: usize| -> f32 {
        // Chroma samples sit between luma samples; interpolate between them the
        // way a video renderer does.
        let fx = ((x as f32 - 0.5) / 2.0).max(0.0);
        let fy = ((y as f32 - 0.5) / 2.0).max(0.0);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let get = |cx: usize, cy: usize| f32::from(plane[cy.min(ch - 1) * cw + cx.min(cw - 1)]);
        let top = get(x0, y0) + (get(x1, y0) - get(x0, y0)) * tx;
        let bottom = get(x0, y1) + (get(x1, y1) - get(x0, y1)) * tx;
        top + (bottom - top) * ty
    };

    out.par_chunks_mut(width * 3).enumerate().for_each(|(y, row)| {
        for x in 0..width {
            let luma = (f32::from(planes.y[y * width + x]) - 16.0) / 219.0;
            let cb = (chroma(&planes.u, x, y) - 128.0) / 224.0;
            let cr = (chroma(&planes.v, x, y) - 128.0) / 224.0;
            let r = luma + 1.5748 * cr;
            let b = luma + 1.8556 * cb;
            let g = (luma - 0.2126 * r - 0.0722 * b) / 0.7152;
            let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            row[x * 3] = to_byte(r);
            row[x * 3 + 1] = to_byte(g);
            row[x * 3 + 2] = to_byte(b);
        }
    });

    RgbImage::from_raw(width as u32, height as u32, out)
        .unwrap_or_else(|| RgbImage::filled(1, 1, photon_core::Rgb::BLACK))
}

// --- the run --------------------------------------------------------------------

/// The cell size the sending page would choose for this screen.
pub(crate) fn fit_cell_px(profile: ProfileId, screen: (u32, u32)) -> u32 {
    let cells = profile.profile().grid + 2 * QUIET_ZONE_CELLS;
    let shortest = f64::from(screen.0.min(screen.1)) * FIT_MARGIN;
    ((shortest / f64::from(cells)).floor() as u32).max(3)
}

/// The module size the sending page would choose for this screen: the largest
/// whole number of pixels at which the code fits.
pub(crate) fn fit_module_px(profile: &DenseProfile, screen: (u32, u32)) -> u32 {
    let across = screen.0 / (profile.width() + 2 * QUIET_MODULES);
    let down = screen.1 / (profile.height() + 2 * QUIET_MODULES);
    across.min(down).max(1)
}

/// Films a transfer.
///
/// # Errors
///
/// Returns a message suitable for printing.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let file = std::fs::read(&options.input)
        .map_err(|e| format!("cannot read {}: {e}", options.input.display()))?;
    let name = options
        .input
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "the input has no usable file name".to_owned())?;

    let profile = options.profile.profile();
    let cell_px = options.cell_px.unwrap_or_else(|| match options.dense {
        Some(dense) => fit_module_px(dense, options.screen),
        None => fit_cell_px(options.profile, options.screen),
    });
    let size = if let Some(dense) = options.dense {
        (
            (dense.width() + 2 * QUIET_MODULES) * cell_px,
            (dense.height() + 2 * QUIET_MODULES) * cell_px,
        )
    } else {
        let side = (profile.grid + 2 * QUIET_ZONE_CELLS) * cell_px;
        (side, side)
    };
    if size.0 > options.screen.0 || size.1 > options.screen.1 {
        return Err(format!(
            "a code of {}x{} pixels does not fit a {}x{} screen",
            size.0, size.1, options.screen.0, options.screen.1
        ));
    }

    let (sender, frames_per_pass, carries, profile_name, unit) = if let Some(dense) = options.dense
    {
        let transmitter =
            DenseTransmitter::new(name, &file, dense, SESSION).map_err(|e| e.to_string())?;
        let (passes, carries) =
            (transmitter.frames_per_pass(), transmitter.layout().bytes_per_frame());
        (Sender::Dense(Box::new(transmitter)), passes, carries, dense.name, "module")
    } else {
        let transmitter =
            Transmitter::new(name, &file, options.profile, SESSION).map_err(|e| e.to_string())?;
        let passes = transmitter.frames_per_pass();
        (
            Sender::Cells(Box::new(transmitter)),
            passes,
            profile.payload_capacity() as usize,
            profile.name,
            "cell",
        )
    };

    let mut display = Display {
        width: options.screen.0,
        height: options.screen.1,
        origin: ((options.screen.0 - size.0) / 2, (options.screen.1 - size.1) / 2),
        size,
        // A dense code is shown with the whole screen to itself.
        bar: if options.dense.is_some() { 0 } else { OVERLAY_BAR },
        hold: options.hold.max(1),
        refresh_period: 1.0 / options.refresh,
        codes: VecDeque::new(),
        first_code: 0,
        sender,
        cell_px,
        lut: srgb_to_linear_table(),
    };

    let model = &options.model;
    let (width, height) = options.camera;
    let long = f64::from(width.max(height));
    // A phone's main camera sees about 66 degrees along its longer side when
    // recording video.
    let focal = (long / 2.0) / 33.0f64.to_radians().tan();
    // Camera pixels to a screen pixel: `fill` is how much of the picture the
    // code takes up along whichever side it reaches first.
    let magnification = model.fill
        * (f64::from(width) / f64::from(size.0)).min(f64::from(height) / f64::from(size.1));
    let distance = focal / magnification;

    let mut rng = Rng::new(options.seed);
    let sigma = model.blur.max(0.0);
    let taps: Vec<(f64, f64)> = (0..TAPS as u32)
        .map(|i| {
            // The pixel's own aperture, plus the lens.
            let (bx, by) = (halton(i + 1, 2) - 0.5, halton(i + 1, 3) - 0.5);
            let (u1, u2) = (halton(i + 1, 5).max(1e-6), halton(i + 1, 7));
            let radius = sigma * (-2.0 * u1.ln()).sqrt();
            let angle = core::f64::consts::TAU * u2;
            (bx + radius * angle.cos(), by + radius * angle.sin())
        })
        .collect();

    // Upright pictures come from a phone held upright, whose shutter rolls
    // sideways.
    let sweep = options.sweep.unwrap_or(if height > width { Sweep::Right } else { Sweep::Down });

    let rig = Rig {
        options,
        sweep,
        focal,
        distance,
        taps,
        tremor: [
            Tremor::new(&mut rng, model.shake),
            Tremor::new(&mut rng, model.shake),
            Tremor::new(&mut rng, model.shake * 0.5),
        ],
    };

    std::fs::create_dir_all(&options.out)
        .map_err(|e| format!("cannot create {}: {e}", options.out.display()))?;

    let mut video = if options.y4m {
        let path = options.out.join("camera.y4m");
        let file = std::fs::File::create(&path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let mut writer = std::io::BufWriter::with_capacity(1 << 22, file);
        let (numerator, denominator) = ((options.fps * 1000.0).round() as u64, 1000u64);
        writeln!(
            writer,
            "YUV4MPEG2 W{width} H{height} F{numerator}:{denominator} Ip A1:1 C420jpeg"
        )
        .map_err(|e| e.to_string())?;
        Some(writer)
    } else {
        None
    };

    let pictures = (options.seconds * options.fps).ceil() as usize;
    // The camera and the display share no clock. Start the camera part-way
    // through a code, as it would be.
    let code_period = f64::from(display.hold) * display.refresh_period;
    let phase = options.phase.unwrap_or_else(|| rng.uniform()).clamp(0.0, 1.0) * code_period;

    println!("File            {name} ({} bytes)", file.len());
    println!("Profile         {profile_name}");
    println!("Carries         {carries} bytes a code");
    println!(
        "Screen          {}x{} at {:.0} Hz, code {}x{} px, {cell_px} px per {unit}",
        options.screen.0, options.screen.1, options.refresh, size.0, size.1
    );
    println!(
        "Codes           {:.1} per second ({} refreshes each), {frames_per_pass} per pass",
        1.0 / code_period,
        display.hold
    );
    println!(
        "Camera          {width}x{height} at {:.2} fps, {pictures} pictures, shutter rolling {sweep:?}",
        options.fps
    );
    println!(
        "Seen at         about {:.1} camera pixels per {unit}",
        f64::from(cell_px) * magnification
    );
    println!();

    for picture in 0..pictures {
        let start = phase + picture as f64 / options.fps;
        let open = start - model.exposure_ms / 1000.0;
        let close = start + model.readout_ms / 1000.0;

        let first = display.code_at(open.max(0.0), f64::from(display.height)).saturating_sub(1);
        let last = display.code_at(close.max(0.0), 0.0);
        display.ensure(first, last)?;

        let exposure = rig.expose(&display, start);
        let developed = develop(&exposure, model, options.seed ^ (picture as u64 + 1));
        let planes = to_planes(&developed);

        if let Some(writer) = video.as_mut() {
            writer.write_all(b"FRAME\n").map_err(|e| e.to_string())?;
            writer.write_all(&planes.y).map_err(|e| e.to_string())?;
            writer.write_all(&planes.u).map_err(|e| e.to_string())?;
            writer.write_all(&planes.v).map_err(|e| e.to_string())?;
        }

        if options.png {
            let path = options.out.join(format!("frame-{picture:06}.png"));
            media::write_png(&path, &from_planes(&planes)).map_err(|e| e.to_string())?;
        }

        if picture % 30 == 29 || picture + 1 == pictures {
            println!("  filmed {:>5} of {pictures}", picture + 1);
        }
    }

    if let Some(mut writer) = video {
        writer.flush().map_err(|e| e.to_string())?;
    }

    describe(&options.out, options, profile_name, cell_px, size)?;
    println!();
    println!("Written to      {}", options.out.display());
    Ok(())
}

/// Records what was filmed beside the pictures, so a result can be traced to
/// the conditions that produced it.
fn describe(
    out: &Path,
    options: &Options,
    profile: &str,
    cell_px: u32,
    size: (u32, u32),
) -> Result<(), String> {
    let model = &options.model;
    let text = format!(
        concat!(
            "input        {}\nprofile      {}\nscreen       {}x{} at {} Hz\n",
            "code         {}x{} px, {} px per cell\nhold         {} refreshes\n",
            "camera       {}x{} at {} fps for {} s\nseed         {}\nmodel        {:#?}\n",
        ),
        options.input.display(),
        profile,
        options.screen.0,
        options.screen.1,
        options.refresh,
        size.0,
        size.1,
        cell_px,
        options.hold,
        options.camera.0,
        options.camera.1,
        options.fps,
        options.seconds,
        options.seed,
        model,
    );
    std::fs::write(out.join("filmed.txt"), text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cell_size_matches_what_the_sending_page_chooses() {
        // floor(1080 * 0.96 / 104) and floor(1080 * 0.96 / 136).
        assert_eq!(fit_cell_px(ProfileId::P1Conservative, (1920, 1080)), 9);
        assert_eq!(fit_cell_px(ProfileId::P2Standard, (1920, 1080)), 7);
    }

    #[test]
    fn a_steady_backlight_does_not_flicker() {
        assert!((backlight(0.0, 0.01, 0.0, 1.0) - 1.0).abs() < 1e-6);
        // An exposure of a whole number of periods sees the average exactly.
        assert!((backlight(0.003, 0.003 + 2.0 / 240.0, 240.0, 0.6) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn colour_survives_the_planes_away_from_edges() {
        let image = RgbImage::filled(16, 16, photon_core::Rgb::new(200, 60, 30));
        let back = from_planes(&to_planes(&image));
        let pixel = back.get(8, 8);
        assert!((i32::from(pixel.r) - 200).abs() <= 2, "{pixel:?}");
        assert!((i32::from(pixel.g) - 60).abs() <= 2, "{pixel:?}");
        assert!((i32::from(pixel.b) - 30).abs() <= 2, "{pixel:?}");
    }
}
