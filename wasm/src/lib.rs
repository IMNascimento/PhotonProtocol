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

use photon_core::dense::{
    DENSE_PROFILES, DenseLayout, DenseProfile, DenseReader as CoreDenseReader, DenseReading,
    DenseReceiver as CoreDenseReceiver, DenseTransmitter, QUIET_MODULES,
};
use photon_core::detect::Detector;
use photon_core::session::{
    FrameOpening, FrameOutcome, FrameReading, FrameReport, Receiver as CoreReceiver, Transmitter,
};
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
                    r#""minCellPx":{},"parityRate":{:.4},"linkOverhead":{:.4}}}"#
                ),
                p.id.as_u8(),
                p.name,
                p.grid,
                p.num_shapes,
                p.num_colours,
                p.bits_per_cell(),
                p.data_cells(),
                p.payload_capacity(),
                p.min_cell_px(),
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
        let pixels = self.rgba.as_chunks_mut::<4>().0.iter_mut();
        for (pixel, chunk) in pixels.zip(rgb.as_chunks::<3>().0) {
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
    /// A receiver.
    ///
    /// Pass a profile only if it is genuinely known; omit it and the frames are
    /// asked which profile drew them, which they carry in a header written the
    /// same way for every profile precisely so it can be read first.
    ///
    /// # Errors
    ///
    /// Returns the protocol's own message for an unknown profile.
    #[wasm_bindgen(constructor)]
    pub fn new(profile: Option<u8>) -> Result<Receiver, JsValue> {
        let inner = match profile {
            Some(id) => CoreReceiver::for_profile(profile_from(id).map_err(to_js)?),
            None => CoreReceiver::new(),
        };
        Ok(Self { inner })
    }

    /// The profile the frames turned out to be drawn with.
    #[wasm_bindgen(js_name = detectedProfile)]
    #[must_use]
    pub fn detected_profile(&self) -> Option<u8> {
        self.inner.profile().map(photon_core::ProfileId::as_u8)
    }

    /// Offers one video frame as RGBA, as `getImageData` produces it.
    ///
    /// Returns a JSON report of what the frame yielded.
    ///
    /// The buffer is taken rather than borrowed so that it can be read where it
    /// lands. Borrowing it meant copying every pixel a second time to drop an
    /// alpha channel nothing looks at.
    ///
    /// # Errors
    ///
    /// Returns a message if the buffer does not match the stated dimensions,
    /// which is the only way a caller can get this wrong.
    #[wasm_bindgen(js_name = acceptFrame)]
    pub fn accept_frame(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<String, JsValue> {
        let image = picture_from(rgba, width, height).map_err(to_js)?;
        let report = self.inner.accept_image(&image);
        Ok(self.describe(&report))
    }

    /// Folds in a picture that a [`Reader`] read.
    ///
    /// Returns the same JSON report as [`Receiver::accept_frame`].
    ///
    /// # Errors
    ///
    /// Returns a message if the bytes are not a reading.
    pub fn absorb(&mut self, reading: &[u8]) -> Result<String, JsValue> {
        let reading = FrameReading::from_bytes(reading)
            .ok_or_else(|| to_js("these bytes are not a reading"))?;
        let report = self.inner.absorb(reading);
        Ok(self.describe(&report))
    }

    /// A frame report as JSON.
    fn describe(&self, report: &FrameReport) -> String {
        let (accepted, needed) = self.inner.progress().unwrap_or((0, 0));
        let sequence =
            report.header.map_or_else(|| "null".to_owned(), |header| header.frame_seq.to_string());

        format!(
            concat!(
                r#"{{"outcome":"{}","newSymbols":{},"unitsAccepted":{},"unitsRejected":{},"#,
                r#""doubtfulRate":{:.5},"pixelsPerCell":{},"correction":{},"corners":{},"#,
                r#""sequence":{},"accepted":{},"needed":{},"complete":{}}}"#
            ),
            outcome_name(report.outcome),
            report.new_symbols,
            report.units_accepted,
            report.units_rejected,
            report.doubtful_rate(),
            number(report.pixels_per_cell),
            number(report.correction),
            corners_json(report.corners),
            sequence,
            accepted,
            needed,
            self.inner.is_complete(),
        )
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

    /// The declared size of the file in bytes, once a manifest has arrived.
    ///
    /// A float because that is what JavaScript counts in, and exact for any
    /// file this protocol could carry in a lifetime.
    #[wasm_bindgen(js_name = fileSize)]
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "exact below 2^53 bytes, which is eight petabytes"
    )]
    pub fn file_size(&self) -> Option<f64> {
        self.inner.manifest().map(|m| m.original_size as f64)
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

/// The name a page knows an outcome by.
const fn outcome_name(outcome: FrameOutcome) -> &'static str {
    match outcome {
        FrameOutcome::NotLocated => "notLocated",
        FrameOutcome::Straddled => "straddled",
        FrameOutcome::Decoded => "decoded",
        FrameOutcome::Duplicate => "duplicate",
        FrameOutcome::WrongSession => "wrongSession",
        FrameOutcome::HeaderUnreadable => "headerUnreadable",
        FrameOutcome::PayloadUnrecoverable => "payloadUnrecoverable",
    }
}

/// A measurement as JSON: `null` rather than a placeholder when it was never
/// taken. A page showing "0.0 pixels per cell" would be reporting something
/// nobody measured.
fn number(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| format!("{value:.2}"))
}

/// Four corners as JSON, or `null`.
fn corners_json(corners: Option<[photon_core::Point; 4]>) -> String {
    corners.map_or_else(
        || "null".to_owned(),
        |corners| {
            let points: Vec<String> =
                corners.iter().map(|p| format!("[{:.1},{:.1}]", p.x, p.y)).collect();
            format!("[{}]", points.join(","))
        },
    )
}

/// Reads pictures, and keeps nothing.
///
/// A phone has eight cores and a [`Receiver`] uses one. Reading a picture is
/// most of the work of a transfer and depends on no other picture, so a page
/// can run several of these side by side, each in a worker of its own, and
/// hand what they read to one receiver to be folded together.
///
/// Reading is in two steps because the header sits between a cheap half and an
/// expensive one. [`Reader::open`] says which code a picture holds; the page,
/// which knows which codes it already has, then either asks for the rest with
/// [`Reader::read`] or moves on.
#[wasm_bindgen]
pub struct Reader {
    inner: CoreReceiver,
    held: Option<(RgbImage, FrameOpening)>,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl Reader {
    /// A reader.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self { inner: CoreReceiver::new(), held: None }
    }

    /// Finds the frame in a picture and reads its header.
    ///
    /// Returns JSON: `outcome` is `opened` when the header was read, and
    /// otherwise says why there is nothing more to read.
    ///
    /// # Errors
    ///
    /// Returns a message if the buffer does not match the stated dimensions.
    pub fn open(&mut self, rgba: Vec<u8>, width: u32, height: u32) -> Result<String, JsValue> {
        let image = picture_from(rgba, width, height).map_err(to_js)?;
        self.held = None;

        let Some(opening) = self.inner.open(&image) else {
            return Ok(r#"{"outcome":"notLocated"}"#.to_owned());
        };

        let outcome = match opening.outcome {
            FrameOutcome::Decoded => "opened",
            other => outcome_name(other),
        };
        let (sequence, session) = opening.header.map_or_else(
            || ("null".to_owned(), "null".to_owned()),
            |header| (header.frame_seq.to_string(), header.session_id.to_string()),
        );
        let summary = format!(
            concat!(
                r#"{{"outcome":"{}","sequence":{},"session":{},"pixelsPerCell":{},"#,
                r#""correction":{},"corners":{}}}"#
            ),
            outcome,
            sequence,
            session,
            number(opening.pixels_per_cell),
            number(Some(opening.correction)),
            corners_json(opening.corners),
        );

        if opening.outcome == FrameOutcome::Decoded {
            self.held = Some((image, opening));
        }
        Ok(summary)
    }

    /// Reads the payload of the picture last opened.
    ///
    /// Returns the reading packed for [`Receiver::absorb`], or nothing if no
    /// picture is being held.
    pub fn read(&mut self) -> Vec<u8> {
        self.held
            .take()
            .map(|(image, opening)| self.inner.read(&image, &opening).to_bytes())
            .unwrap_or_default()
    }

    /// Lets go of the picture last opened without reading it.
    pub fn release(&mut self) {
        self.held = None;
    }
}

/// The dense profiles, as JSON.
#[wasm_bindgen(js_name = denseProfiles)]
#[must_use]
pub fn dense_profiles() -> String {
    let entries: Vec<String> = DENSE_PROFILES
        .iter()
        .map(|profile| {
            let layout = DenseLayout::new(profile);
            format!(
                concat!(
                    r#"{{"id":{},"name":"{}","width":{},"height":{},"quiet":{},"#,
                    r#""tiles":{},"symbolSize":{},"payloadCapacity":{}}}"#
                ),
                profile.id,
                profile.name,
                profile.width(),
                profile.height(),
                QUIET_MODULES,
                profile.tiles(),
                layout.symbol_size(),
                layout.bytes_per_frame(),
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Turns a dense profile number from JavaScript into one the protocol knows.
fn dense_profile_from(id: u8) -> Result<&'static DenseProfile, String> {
    DenseProfile::from_id(id).ok_or_else(|| format!("{id:#04x} is not a dense profile"))
}

/// Paints the endless sequence of dense frames for one file.
///
/// A frame is handed over at one pixel to a module, margin included, and the
/// page scales it up by a whole number. Sixty frames a second of two million
/// pixels each is more than a page should be copying about, and scaling by a
/// whole number without smoothing is something a browser does exactly.
#[wasm_bindgen]
pub struct DenseEmitter {
    transmitter: DenseTransmitter,
    width: u32,
    height: u32,
}

#[wasm_bindgen]
impl DenseEmitter {
    /// Prepares a transfer.
    ///
    /// # Errors
    ///
    /// Returns the protocol's own message when the file is empty, the name is
    /// not a bare file name or is too long, or the profile is not a dense one.
    #[wasm_bindgen(constructor)]
    pub fn new(
        name: &str,
        bytes: &[u8],
        profile: u8,
        session_id: u32,
    ) -> Result<DenseEmitter, JsValue> {
        let profile = dense_profile_from(profile).map_err(to_js)?;
        let transmitter = DenseTransmitter::new(name, bytes, profile, session_id)
            .map_err(|e| to_js(e.to_string()))?;
        Ok(Self {
            transmitter,
            width: profile.width() + 2 * QUIET_MODULES,
            height: profile.height() + 2 * QUIET_MODULES,
        })
    }

    /// Width of each frame in modules, margin included.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height of each frame in modules, margin included.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
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
            r#"{{"name":"{}","originalSize":{},"compression":"{:?}","bytesPerFrame":{}}}"#,
            manifest.name.replace('"', "\\\""),
            manifest.original_size,
            manifest.compression,
            self.transmitter.layout().bytes_per_frame(),
        )
    }

    /// Paints the next frame and returns it as RGBA, one pixel to a module.
    #[wasm_bindgen(js_name = nextFrame)]
    pub fn next_frame(&mut self) -> Vec<u8> {
        let modules = self.transmitter.next_modules();
        let (width, height) = (self.width as usize, self.height as usize);
        let quiet = QUIET_MODULES as usize;
        let across = width - 2 * quiet;

        let mut rgba = vec![0xFFu8; width * height * 4];
        for (index, _) in modules.iter().enumerate().filter(|(_, light)| !**light) {
            let (row, col) = (index / across + quiet, index % across + quiet);
            let at = (row * width + col) * 4;
            rgba[at..at + 3].fill(0);
        }
        rgba
    }
}

/// Reads pictures of dense codes, and keeps nothing.
#[wasm_bindgen]
pub struct DenseReader {
    inner: CoreDenseReader,
    summary: String,
}

impl Default for DenseReader {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl DenseReader {
    /// A reader.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self { inner: CoreDenseReader::new(), summary: String::new() }
    }

    /// Reads a picture.
    ///
    /// Returns the reading packed for [`DenseReceiver::absorb`], or nothing if
    /// there is no dense code in the picture.
    ///
    /// # Errors
    ///
    /// Returns a message if the buffer does not match the stated dimensions.
    pub fn read(&mut self, rgba: Vec<u8>, width: u32, height: u32) -> Result<Vec<u8>, JsValue> {
        let image = picture_from(rgba, width, height).map_err(to_js)?;
        let reading = self.inner.read(&image);
        self.summary = format!(
            concat!(
                r#"{{"located":{},"profile":{},"tilesRead":{},"tilesLost":{},"#,
                r#""pixelsPerCell":{},"correction":{:.2},"corners":{}}}"#
            ),
            reading.located,
            reading.profile.map_or_else(|| "null".to_owned(), |id| id.to_string()),
            reading.tiles.len(),
            reading.tiles_lost,
            number(reading.pixels_per_module),
            reading.correction,
            corners_json(reading.corners),
        );
        Ok(if reading.located { reading.to_bytes() } else { Vec::new() })
    }

    /// What the picture last read came to, as JSON.
    #[must_use]
    pub fn summary(&self) -> String {
        self.summary.clone()
    }
}

/// Collects the tiles of dense codes until it can rebuild the file.
#[wasm_bindgen]
#[derive(Default)]
pub struct DenseReceiver {
    inner: CoreDenseReceiver,
}

#[wasm_bindgen]
impl DenseReceiver {
    /// A receiver.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds in a picture that a [`DenseReader`] read, and says as JSON what
    /// it added.
    ///
    /// # Errors
    ///
    /// Returns a message if the bytes are not a reading.
    pub fn absorb(&mut self, reading: &[u8]) -> Result<String, JsValue> {
        let reading = DenseReading::from_bytes(reading)
            .ok_or_else(|| to_js("these bytes are not a reading"))?;
        let report = self.inner.absorb(&reading);
        let (accepted, needed) = self.inner.progress().unwrap_or((0, 0));

        // In the words the page has for a frame of cells, so that it can count
        // and explain both the same way.
        let outcome = if report.new_symbols > 0 {
            "decoded"
        } else if report.tiles_foreign > 0 && report.tiles_read == 0 {
            "wrongSession"
        } else if report.tiles_read > 0 {
            "duplicate"
        } else {
            "payloadUnrecoverable"
        };
        Ok(format!(
            concat!(
                r#"{{"outcome":"{}","newSymbols":{},"tilesRead":{},"tilesLost":{},"#,
                r#""tilesReadAgain":{},"accepted":{},"needed":{},"complete":{}}}"#
            ),
            outcome,
            report.new_symbols,
            report.tiles_read,
            report.tiles_lost,
            report.tiles_read_again,
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

    /// The declared size of the file in bytes, once a manifest has arrived.
    #[wasm_bindgen(js_name = fileSize)]
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "exact below 2^53 bytes, which is eight petabytes"
    )]
    pub fn file_size(&self) -> Option<f64> {
        self.inner.manifest().map(|m| m.original_size as f64)
    }

    /// Rebuilds the file.
    ///
    /// # Errors
    ///
    /// Returns the specification's own failure code and message.
    pub fn finish(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner.finish().map(|(_, bytes)| bytes).map_err(|e| to_js(format!("{}: {e}", e.code())))
    }
}

/// Reports how far detection gets on a picture, as JSON.
///
/// For telling someone why nothing is being read while they can still do
/// something about it. "Not found" on its own sends people to adjust the wrong
/// thing: corner patterns found but no whole frame usually means the display is
/// clipped, which looks perfectly fine from in front of it.
///
/// # Errors
///
/// Returns a message if the buffer does not match the stated dimensions.
#[wasm_bindgen(js_name = inspectFrame)]
pub fn inspect_frame(rgba: Vec<u8>, width: u32, height: u32) -> Result<String, JsValue> {
    let image = picture_from(rgba, width, height).map_err(to_js)?;
    let detector = Detector::new();
    let found = detector.detect(&image).ok();
    let diagnosis = detector.diagnose(&image);

    Ok(format!(
        r#"{{"located":{},"finderCandidates":{},"quadFound":{},"pixelsPerCell":{}}}"#,
        found.is_some(),
        diagnosis.finder_candidates,
        diagnosis.quad_found,
        found.map_or_else(|| "null".to_owned(), |d| format!("{:.2}", d.pixels_per_cell())),
    ))
}

/// Wraps the buffer a canvas produced, alpha and all.
fn picture_from(rgba: Vec<u8>, width: u32, height: u32) -> Result<RgbImage, String> {
    let expected = (width as usize) * (height as usize) * 4;
    if rgba.len() != expected {
        return Err(format!("expected {expected} bytes for {width}x{height}, got {}", rgba.len()));
    }
    RgbImage::from_rgba(width, height, rgba)
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

        let mut receiver = Receiver::new(Some(0x02)).expect("receiver");
        for _ in 0..40 {
            if receiver.is_complete() {
                break;
            }
            let rgba = emitter.next_frame().expect("painted");
            let report = receiver.accept_frame(rgba, side, side).expect("accepted");
            assert!(report.contains("\"outcome\":\"decoded\""), "{report}");
        }

        assert!(receiver.is_complete(), "the transfer never completed");
        assert_eq!(receiver.file_name().as_deref(), Some("through.bin"));
        assert_eq!(receiver.finish().expect("finished"), file);
    }

    #[test]
    fn a_file_survives_being_read_in_one_place_and_collected_in_another() {
        // The path the receiving page takes: pictures are read by readers that
        // keep nothing, and what they read is folded together by a receiver
        // that never sees a picture.
        let file: Vec<u8> = (0..9000u32).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect();
        let mut emitter = Emitter::new("apart.bin", &file, 0x01, 8, 99).expect("prepared");
        let side = emitter.side();

        let mut reader = Reader::new();
        let mut receiver = Receiver::new(None).expect("receiver");

        for _ in 0..40 {
            if receiver.is_complete() {
                break;
            }
            let rgba = emitter.next_frame().expect("painted");
            let summary = reader.open(rgba, side, side).expect("opened");
            assert!(summary.contains("\"outcome\":\"opened\""), "{summary}");

            let reading = reader.read();
            assert!(!reading.is_empty(), "an opened picture yielded no reading");
            let report = receiver.absorb(&reading).expect("absorbed");
            assert!(report.contains("\"outcome\":\"decoded\""), "{report}");
        }

        assert!(receiver.is_complete(), "the transfer never completed");
        assert_eq!(receiver.finish().expect("finished"), file);
    }

    #[test]
    fn a_file_survives_the_dense_bindings() {
        // Bytes that will not compress, so that the transfer is more than a
        // frame long.
        let mut state = 0x9E37_79B9u32;
        let file: Vec<u8> = (0..60_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect();
        let mut emitter = DenseEmitter::new("dense.bin", &file, 0x12, 77).expect("prepared");
        let (width, height) = (emitter.width(), emitter.height());

        let mut reader = DenseReader::new();
        let mut receiver = DenseReceiver::new();
        for _ in 0..40 {
            if receiver.is_complete() {
                break;
            }
            // Scaled up by three, as the page does.
            let small = emitter.next_frame();
            let mut rgba = vec![0u8; (width * 3 * height * 3 * 4) as usize];
            for y in 0..height * 3 {
                for x in 0..width * 3 {
                    let from = ((y / 3 * width + x / 3) * 4) as usize;
                    let to = ((y * width * 3 + x) * 4) as usize;
                    rgba[to..to + 4].copy_from_slice(&small[from..from + 4]);
                }
            }

            let reading = reader.read(rgba, width * 3, height * 3).expect("read");
            assert!(reader.summary().contains("\"located\":true"), "{}", reader.summary());
            let report = receiver.absorb(&reading).expect("absorbed");
            assert!(report.contains("\"outcome\":\"decoded\""), "{report}");
        }

        assert!(receiver.is_complete(), "the transfer never completed");
        assert_eq!(receiver.file_name().as_deref(), Some("dense.bin"));
        assert_eq!(receiver.finish().expect("finished"), file);
    }

    #[test]
    fn dense_profiles_json_is_well_formed() {
        let json = dense_profiles();
        assert!(json.starts_with('[') && json.ends_with(']'), "{json}");
        assert_eq!(json.matches("\"id\":").count(), DENSE_PROFILES.len());
        assert_eq!(json.matches('{').count(), json.matches('}').count());
        assert!(dense_profile_from(0x12).is_ok());
        assert!(dense_profile_from(0x02).is_err());
    }

    #[test]
    fn bytes_that_are_not_a_reading_are_refused() {
        // Checked through the core, because the binding's own error type only
        // exists in a browser.
        assert!(FrameReading::from_bytes(&[]).is_none());
        assert!(FrameReading::from_bytes(&[1, 2, 3, 4]).is_none());
        assert!(FrameReading::from_bytes(&[0xF7, 2, 1, 0, 0]).is_none());
    }

    #[test]
    fn a_mis_sized_buffer_is_refused_rather_than_read_past() {
        // A page can hand over any buffer it likes. Reading past one would be a
        // memory-safety bug reachable from a web page, so the check is asserted
        // directly rather than through the binding, which cannot run here.
        assert!(picture_from(vec![0; 16], 100, 100).is_err());
        assert!(picture_from(Vec::new(), 1, 1).is_err());
        assert!(picture_from(vec![0; 4], 1, 1).is_ok());
    }

    #[test]
    fn an_unknown_profile_is_refused_at_the_boundary() {
        assert!(profile_from(0x7F).is_err());
        assert!(profile_from(0x00).is_err());
        assert!(profile_from(0x02).is_ok());
    }
}
