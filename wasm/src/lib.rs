//! WebAssembly bindings for PhotonProtocol.
//!
//! This crate is an adapter and holds no protocol logic of its own. Everything
//! it exposes delegates to `photon-core`, so the emitter page, the decoder page
//! and the native command line all run the same implementation and cannot drift
//! apart. Anything that belongs to the format belongs in `photon-core`; if a
//! change here would alter what goes on the wire, it is in the wrong crate.
//!
//! The boundary is deliberately narrow. Frames cross it as RGBA byte buffers
//! because that is what a canvas speaks, and reports cross it as JSON strings
//! rather than as mirrored object graphs — every mirrored type is a second
//! declaration of something the specification already defines once.

use photon_core::detect::Detector;
use photon_core::session::{FrameOutcome, Receiver as CoreReceiver, Transmitter};
use photon_core::{PROTOCOL_VERSION, ProfileId, RgbImage, SPEC_VERSION, profile::PROFILES};
use wasm_bindgen::prelude::{JsValue, wasm_bindgen};

/// The protocol version this build implements (`SPEC.md` §5.1).
#[wasm_bindgen(js_name = protocolVersion)]
#[must_use]
pub fn protocol_version() -> u8 {
    PROTOCOL_VERSION
}

/// The version of the specification document this build was written against.
#[wasm_bindgen(js_name = specVersion)]
#[must_use]
pub fn spec_version() -> String {
    SPEC_VERSION.to_owned()
}

/// The physical-layer profiles and their derived geometry, as JSON.
///
/// The pages need this to populate a profile picker and to size a canvas. It is
/// serialised by hand rather than through a mirrored type: the geometry is
/// derived from the specification's formulas in one place, and a second
/// declaration of the same fields would only be somewhere for them to disagree.
#[wasm_bindgen(js_name = profiles)]
#[must_use]
pub fn profiles() -> String {
    let entries: Vec<String> = PROFILES
        .iter()
        .map(|p| {
            format!(
                concat!(
                    r#"{{"id":{},"name":"{}","grid":{},"shapes":{},"colours":{},"#,
                    r#""bitsPerCell":{},"dataCells":{},"payloadCapacity":{},"#,
                    r#""parityRate":{:.4},"linkOverhead":{:.4}}}"#
                ),
                p.id.as_u8(),
                p.name,
                p.grid,
                p.num_shapes,
                p.num_colours,
                p.bits_per_cell(),
                p.data_cells(),
                p.payload_capacity(),
                p.rs_parity_rate(),
                p.link_overhead(),
            )
        })
        .collect();

    format!("[{}]", entries.join(","))
}

/// Turns a profile number from JavaScript into one the protocol knows.
///
/// Returns a plain `String` rather than a `JsValue`, and every entry point
/// converts at the last moment. `JsValue` only exists on `wasm32`, so a helper
/// that produced one could not be tested anywhere else -- and validation of
/// untrusted input from a page is exactly what wants testing.
fn profile_from(id: u8) -> Result<ProfileId, String> {
    ProfileId::from_u8(id).map_err(|e| e.to_string())
}

/// Lifts an internal error to the boundary.
fn to_js(error: impl AsRef<str>) -> JsValue {
    JsValue::from_str(error.as_ref())
}

/// Paints the endless sequence of frames for one file.
///
/// Frames are produced one at a time rather than all at once: a transfer is
/// unbounded by design, and a page that tried to materialise it would run out of
/// memory before it ran out of symbols.
#[wasm_bindgen]
pub struct Emitter {
    transmitter: Transmitter,
    cell_px: u32,
    side: u32,
    rgba: Vec<u8>,
}

#[wasm_bindgen]
impl Emitter {
    /// Prepares a transfer.
    ///
    /// # Errors
    ///
    /// Returns the protocol's own message when the file is empty, the name is
    /// not a bare file name, or the profile cannot carry a symbol.
    #[wasm_bindgen(constructor)]
    pub fn new(
        name: &str,
        bytes: &[u8],
        profile: u8,
        cell_px: u32,
        session_id: u32,
    ) -> Result<Emitter, JsValue> {
        let profile = profile_from(profile).map_err(to_js)?;
        let transmitter =
            Transmitter::new(name, bytes, profile, session_id).map_err(|e| to_js(e.to_string()))?;

        let side = (profile.profile().grid + 2 * photon_core::frame::QUIET_ZONE_CELLS) * cell_px;
        let rgba = vec![0u8; (side as usize) * (side as usize) * 4];

        Ok(Self { transmitter, cell_px, side, rgba })
    }

    /// Side of each frame in pixels, quiet zone included.
    #[must_use]
    pub fn side(&self) -> u32 {
        self.side
    }

    /// Frames needed to show every source symbol once.
    #[wasm_bindgen(js_name = framesPerPass)]
    #[must_use]
    pub fn frames_per_pass(&self) -> usize {
        self.transmitter.frames_per_pass()
    }

    /// What the transfer will tell the receiver about itself, as JSON.
    #[must_use]
    pub fn manifest(&self) -> String {
        let manifest = self.transmitter.manifest();
        format!(
            r#"{{"name":"{}","originalSize":{},"compression":"{:?}","symbolsPerFrame":{}}}"#,
            manifest.name.replace('"', "\\\""),
            manifest.original_size,
            manifest.compression,
            self.transmitter.symbols_per_frame(),
        )
    }

    /// Paints the next frame and returns it as RGBA, ready for `putImageData`.
    ///
    /// The buffer is reused between calls, so a caller must copy or draw it
    /// before asking for the next frame. At thirty frames a second, allocating a
    /// fresh megabyte each time is work the garbage collector would rather not
    /// have.
    ///
    /// # Errors
    ///
    /// Returns the protocol's own message if a frame cannot be painted, which
    /// would mean an internal contract violation rather than anything a user
    /// did.
    #[wasm_bindgen(js_name = nextFrame)]
    pub fn next_frame(&mut self) -> Result<Vec<u8>, JsValue> {
        let frame = self.transmitter.next_frame(self.cell_px).map_err(|e| to_js(e.to_string()))?;

        let rgb = frame.image.as_raw();
        for (pixel, chunk) in self.rgba.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
            pixel[0] = chunk[0];
            pixel[1] = chunk[1];
            pixel[2] = chunk[2];
            pixel[3] = 0xFF;
        }

        Ok(self.rgba.clone())
    }
}

/// Collects frames until it can rebuild the file.
#[wasm_bindgen]
pub struct Receiver {
    inner: CoreReceiver,
}

#[wasm_bindgen]
impl Receiver {
    /// A receiver expecting frames of the given profile.
    ///
    /// # Errors
    ///
    /// Returns the protocol's own message for an unknown profile.
    #[wasm_bindgen(constructor)]
    pub fn new(profile: u8) -> Result<Receiver, JsValue> {
        let profile = profile_from(profile).map_err(to_js)?;
        Ok(Self { inner: CoreReceiver::new(profile) })
    }

    /// Offers one video frame as RGBA, as `getImageData` produces it.
    ///
    /// Returns a JSON report of what the frame yielded.
    ///
    /// # Errors
    ///
    /// Returns a message if the buffer does not match the stated dimensions,
    /// which is the only way a caller can get this wrong.
    #[wasm_bindgen(js_name = acceptFrame)]
    pub fn accept_frame(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<String, JsValue> {
        let image = rgba_to_rgb(rgba, width, height).map_err(to_js)?;
        let report = self.inner.accept_image(&image);

        let outcome = match report.outcome {
            FrameOutcome::NotLocated => "notLocated",
            FrameOutcome::Decoded => "decoded",
            FrameOutcome::Duplicate => "duplicate",
            FrameOutcome::WrongSession => "wrongSession",
            FrameOutcome::HeaderUnreadable => "headerUnreadable",
            FrameOutcome::PayloadUnrecoverable => "payloadUnrecoverable",
        };

        let (accepted, needed) = self.inner.progress().unwrap_or((0, 0));
        Ok(format!(
            concat!(
                r#"{{"outcome":"{}","newSymbols":{},"unitsAccepted":{},"unitsRejected":{},"#,
                r#""doubtfulRate":{:.5},"accepted":{},"needed":{},"complete":{}}}"#
            ),
            outcome,
            report.new_symbols,
            report.units_accepted,
            report.units_rejected,
            report.doubtful_rate(),
            accepted,
            needed,
            self.inner.is_complete(),
        ))
    }

    /// Whether enough has been collected to rebuild the file.
    #[wasm_bindgen(js_name = isComplete)]
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }

    /// The declared file name, once a manifest has arrived.
    #[wasm_bindgen(js_name = fileName)]
    #[must_use]
    pub fn file_name(&self) -> Option<String> {
        self.inner.manifest().map(|m| m.name.clone())
    }

    /// Rebuilds the file.
    ///
    /// # Errors
    ///
    /// Returns the specification's own failure code and message — which stage
    /// gave up and how close it came — rather than a bare failure.
    pub fn finish(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner.finish().map(|file| file.bytes).map_err(|e| to_js(format!("{}: {e}", e.code())))
    }
}

/// Whether a picture contains a locatable frame, without decoding it.
///
/// Cheap enough to run on a preview, and useful for telling someone their aim is
/// wrong while they can still do something about it.
///
/// # Errors
///
/// Returns a message if the buffer does not match the stated dimensions, or if
/// the profile is one this build does not implement.
#[wasm_bindgen(js_name = locateFrame)]
pub fn locate_frame(rgba: &[u8], width: u32, height: u32, profile: u8) -> Result<bool, JsValue> {
    let image = rgba_to_rgb(rgba, width, height).map_err(to_js)?;
    let detector = Detector::for_profile(profile_from(profile).map_err(to_js)?);
    Ok(detector.detect(&image).is_ok())
}

/// Drops the alpha a canvas insists on carrying.
fn rgba_to_rgb(rgba: &[u8], width: u32, height: u32) -> Result<RgbImage, String> {
    let expected = (width as usize) * (height as usize) * 4;
    if rgba.len() != expected {
        return Err(format!("expected {expected} bytes for {width}x{height}, got {}", rgba.len()));
    }

    let mut rgb = Vec::with_capacity(expected / 4 * 3);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }

    RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| "frame dimensions do not match its buffer".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_json_is_well_formed() {
        // No JSON parser is linked in, so assert the shape structurally: the
        // page consumes this with JSON.parse and a stray quote would only
        // surface in a browser.
        let json = profiles();
        assert!(json.starts_with('[') && json.ends_with(']'), "{json}");
        assert_eq!(json.matches("\"id\":").count(), PROFILES.len());
        assert_eq!(json.matches('{').count(), json.matches('}').count());
        assert_eq!(json.matches('"').count() % 2, 0, "unbalanced quotes in {json}");
        assert!(!json.contains(",,") && !json.contains("[,") && !json.contains(",]"), "{json}");
    }

    #[test]
    fn bindings_report_the_core_versions() {
        assert_eq!(protocol_version(), PROTOCOL_VERSION);
        assert_eq!(spec_version(), SPEC_VERSION);
    }

    #[test]
    fn a_file_survives_the_bindings() {
        // The same round trip the native tests make, but through the exact API
        // the pages call. A binding that drops the alpha channel wrongly, or
        // mis-sizes a buffer, would pass every test in the core crate.
        let file: Vec<u8> = (0..9000u32).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect();
        let mut emitter = Emitter::new("through.bin", &file, 0x02, 8, 4242).expect("prepared");
        let side = emitter.side();

        let mut receiver = Receiver::new(0x02).expect("receiver");
        for _ in 0..40 {
            if receiver.is_complete() {
                break;
            }
            let rgba = emitter.next_frame().expect("painted");
            let report = receiver.accept_frame(&rgba, side, side).expect("accepted");
            assert!(report.contains("\"outcome\":\"decoded\""), "{report}");
        }

        assert!(receiver.is_complete(), "the transfer never completed");
        assert_eq!(receiver.file_name().as_deref(), Some("through.bin"));
        assert_eq!(receiver.finish().expect("finished"), file);
    }

    #[test]
    fn a_mis_sized_buffer_is_refused_rather_than_read_past() {
        // A page can hand over any buffer it likes. Reading past one would be a
        // memory-safety bug reachable from a web page, so the check is asserted
        // directly rather than through the binding, which cannot run here.
        assert!(rgba_to_rgb(&[0; 16], 100, 100).is_err());
        assert!(rgba_to_rgb(&[], 1, 1).is_err());
        assert!(rgba_to_rgb(&[0; 4], 1, 1).is_ok());
    }

    #[test]
    fn an_unknown_profile_is_refused_at_the_boundary() {
        assert!(profile_from(0x7F).is_err());
        assert!(profile_from(0x00).is_err());
        assert!(profile_from(0x02).is_ok());
    }
}
