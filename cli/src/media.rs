//! Talking to the outside world: PNG files and `ffmpeg`.
//!
//! The protocol crate deliberately knows nothing about file formats or video
//! containers. Everything that does lives here, at the edge, where it can be
//! replaced without the codec noticing.

use std::path::{Path, PathBuf};
use std::process::Command;

use photon_core::RgbImage;

/// What went wrong at the edge of the program.
#[derive(Debug)]
pub(crate) enum MediaError {
    /// A required external tool is not installed.
    ToolMissing(&'static str),
    /// An external tool ran and failed.
    ToolFailed { tool: &'static str, detail: String },
    /// A file could not be read or written.
    Io(std::io::Error),
    /// An image could not be decoded or encoded.
    Image(String),
    /// A recording contained no frames.
    NoFrames,
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolMissing(tool) => write!(
                f,
                "{tool} is not on PATH. It is needed to read video files; install it from \
                 https://ffmpeg.org or decode a directory of PNG frames instead"
            ),
            Self::ToolFailed { tool, detail } => write!(f, "{tool} failed: {detail}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Image(detail) => write!(f, "{detail}"),
            Self::NoFrames => f.write_str("the recording contained no frames"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<std::io::Error> for MediaError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

type Result<T> = std::result::Result<T, MediaError>;

/// Writes an image as a PNG.
///
/// # Errors
///
/// Returns [`MediaError::Image`] if encoding fails and [`MediaError::Io`] if the
/// file cannot be written.
pub(crate) fn write_png(path: &Path, image: &RgbImage) -> Result<()> {
    let buffer: image::RgbImage =
        image::ImageBuffer::from_raw(image.width(), image.height(), image.as_raw().to_vec())
            .ok_or_else(|| MediaError::Image("frame dimensions do not match its buffer".into()))?;
    buffer.save(path).map_err(|e| MediaError::Image(e.to_string()))
}

/// Reads a PNG, or any other format the build supports.
///
/// # Errors
///
/// Returns [`MediaError::Image`] if the file is not a readable image.
pub(crate) fn read_image(path: &Path) -> Result<RgbImage> {
    let decoded =
        image::open(path).map_err(|e| MediaError::Image(format!("{}: {e}", path.display())))?;
    let rgb = decoded.to_rgb8();
    let (width, height) = (rgb.width(), rgb.height());
    RgbImage::from_raw(width, height, rgb.into_raw())
        .ok_or_else(|| MediaError::Image(format!("{}: unexpected buffer size", path.display())))
}

/// Every image file in a directory, in name order.
///
/// Name order is frame order because the extractor pads its numbering, and it is
/// the only order available: a recording's frames carry no timestamps once they
/// are on disk.
///
/// # Errors
///
/// Returns [`MediaError::Io`] if the directory cannot be read.
pub(crate) fn frame_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|e| e.to_str()).is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "bmp")
            })
        })
        .collect();
    paths.sort();
    Ok(paths)
}

/// What a recording is, as far as this program needs to know.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VideoInfo {
    /// Frames per second, as recorded.
    pub(crate) fps: f64,
    /// Length in seconds.
    pub(crate) duration: f64,
}

/// Asks `ffprobe` about a recording.
///
/// # Errors
///
/// Returns [`MediaError::ToolMissing`] if `ffprobe` is absent, and
/// [`MediaError::ToolFailed`] if it cannot read the file.
pub(crate) fn probe(video: &Path) -> Result<VideoInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(video)
        .output()
        .map_err(|_| MediaError::ToolMissing("ffprobe"))?;

    if !output.status.success() {
        return Err(MediaError::ToolFailed {
            tool: "ffprobe",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut fps = 0.0;
    let mut duration = 0.0;

    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "r_frame_rate" => fps = parse_rational(value.trim()),
            "duration" => duration = value.trim().parse().unwrap_or(0.0),
            _ => {}
        }
    }

    Ok(VideoInfo { fps, duration })
}

/// `ffprobe` reports frame rates as exact fractions, because 30000/1001 is not
/// 30 and the difference accumulates across a recording.
fn parse_rational(text: &str) -> f64 {
    match text.split_once('/') {
        Some((numerator, denominator)) => {
            let n: f64 = numerator.parse().unwrap_or(0.0);
            let d: f64 = denominator.parse().unwrap_or(1.0);
            if d == 0.0 { 0.0 } else { n / d }
        }
        None => text.parse().unwrap_or(0.0),
    }
}

/// Extracts every frame of a recording into `directory` as PNG.
///
/// # Errors
///
/// Returns [`MediaError::ToolMissing`] if `ffmpeg` is absent,
/// [`MediaError::ToolFailed`] if extraction fails, and [`MediaError::NoFrames`]
/// if the recording turned out to contain none.
pub(crate) fn extract_frames(
    video: &Path,
    directory: &Path,
    limit: Option<usize>,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(directory)?;

    let pattern = directory.join("frame-%06d.png");
    let mut command = Command::new("ffmpeg");
    command.arg("-v").arg("error").arg("-i").arg(video);

    if let Some(count) = limit {
        command.arg("-frames:v").arg(count.to_string());
    }

    // -fps_mode passthrough keeps every stored frame exactly once. Letting
    // ffmpeg resample would duplicate or drop frames to hit a nominal rate,
    // which would corrupt the frame counts this tool reports.
    let output = command
        .arg("-fps_mode")
        .arg("passthrough")
        .arg(&pattern)
        .output()
        .map_err(|_| MediaError::ToolMissing("ffmpeg"))?;

    if !output.status.success() {
        return Err(MediaError::ToolFailed {
            tool: "ffmpeg",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    let frames = frame_paths(directory)?;
    if frames.is_empty() { Err(MediaError::NoFrames) } else { Ok(frames) }
}

/// Assembles a directory of numbered PNGs into a video.
///
/// # Errors
///
/// Returns [`MediaError::ToolMissing`] if `ffmpeg` is absent and
/// [`MediaError::ToolFailed`] if encoding fails.
pub(crate) fn assemble_video(directory: &Path, output: &Path, fps: u32) -> Result<()> {
    let pattern = directory.join("frame-%06d.png");

    // Lossless, because a bench tool that quietly compressed its own reference
    // frames would be measuring the compressor as much as the protocol.
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-framerate"])
        .arg(fps.to_string())
        .arg("-i")
        .arg(&pattern)
        .args(["-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv444p"])
        .arg(output)
        .output()
        .map_err(|_| MediaError::ToolMissing("ffmpeg"))?;

    if result.status.success() {
        Ok(())
    } else {
        Err(MediaError::ToolFailed {
            tool: "ffmpeg",
            detail: String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        })
    }
}
