//! `photon encode`: a file in, a sequence of frames out.

use std::path::{Path, PathBuf};

use photon_core::ProfileId;
use photon_core::session::Transmitter;

use crate::media::{self, MediaError};

/// Runs the encoder.
///
/// # Errors
///
/// Returns a message suitable for printing when the file cannot be read, the
/// transfer cannot be prepared, or frames cannot be written.
pub(crate) fn run(
    input: &Path,
    output: &Path,
    profile: ProfileId,
    cell_px: u32,
    passes: f64,
    fps: u32,
    video: bool,
) -> Result<(), String> {
    let bytes =
        std::fs::read(input).map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    let name = file_name(input)?;

    // A session identifier only has to be unlikely to collide with another
    // transfer someone is filming nearby, and there is no entropy source in the
    // protocol crate by design.
    let session_id = session_seed(&bytes, &name);

    let mut transmitter = Transmitter::new(&name, &bytes, profile, session_id)
        .map_err(|e| format!("cannot prepare the transfer: {e}"))?;

    std::fs::create_dir_all(output)
        .map_err(|e| format!("cannot create {}: {e}", output.display()))?;

    let per_pass = transmitter.frames_per_pass();
    let total = frame_count(per_pass, passes);

    println!("File            {} ({} bytes)", name, bytes.len());
    println!("Compression     {:?}", transmitter.compression());
    println!("Profile         {}", profile.profile().name);
    println!("Cell size       {cell_px} px");
    println!("Symbols/frame   {}", transmitter.symbols_per_frame());
    println!("Frames per pass {per_pass}");
    println!("Writing         {total} frames to {}", output.display());

    for index in 0..total {
        let frame = transmitter.next_frame(cell_px).map_err(|e| format!("frame {index}: {e}"))?;
        let path = output.join(format!("frame-{:06}.png", index + 1));
        media::write_png(&path, &frame.image).map_err(|e| format!("frame {index}: {e}"))?;
    }

    let side = transmitter_side(cell_px, profile);
    println!("Frame size      {side}x{side} px");

    if video {
        let target = output.join("photon.mp4");
        media::assemble_video(output, &target, fps).map_err(|e: MediaError| e.to_string())?;
        println!("Video           {} at {fps} fps", target.display());
    }

    println!();
    println!("Display these full screen, at maximum brightness, with any adaptive");
    println!("brightness or colour-temperature adjustment turned off. Hold each frame");
    println!("for at least two display refreshes.");

    Ok(())
}

/// How many frames `--passes` asks for.
///
/// Bounded on both sides so that an absurd request stays absurd instead of
/// wrapping into a small one, and a negative one becomes a single pass rather
/// than an empty run.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped into [1, 1e6] immediately before the conversion"
)]
fn frame_count(per_pass: usize, passes: f64) -> usize {
    let requested = (per_pass as f64) * passes.max(0.0);
    if requested.is_finite() {
        requested.ceil().clamp(1.0, 1_000_000.0) as usize
    } else {
        per_pass.max(1)
    }
}

fn transmitter_side(cell_px: u32, profile: ProfileId) -> u32 {
    (profile.profile().grid + 2 * photon_core::frame::QUIET_ZONE_CELLS) * cell_px
}

fn file_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(std::borrow::ToOwned::to_owned)
        .ok_or_else(|| format!("{} has no usable file name", path.display()))
}

/// A session identifier derived from the transfer itself.
///
/// Deterministic on purpose: encoding the same file twice produces the same
/// identifier, so a half-filmed recording and a fresh one of the same transfer
/// can still be combined. It is not a secret and does not need to be.
fn session_seed(bytes: &[u8], name: &str) -> u32 {
    let digest = photon_core::file::digest(bytes);
    let mut seed = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
    for byte in name.as_bytes() {
        seed = seed.rotate_left(5) ^ u32::from(*byte);
    }
    seed
}

/// Where the encoder writes when the caller does not say.
#[must_use]
pub(crate) fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("photon");
    PathBuf::from(format!("{stem}-frames"))
}
