//! The two ends of a transfer: a [`Transmitter`] that paints frames forever and
//! a [`Receiver`] that collects whatever it is shown.
//!
//! Everything below this module is a layer of the format. This is where they are
//! wired into the asymmetric pair the channel actually needs. The transmitter
//! has no idea whether anyone is watching and never finds out; the receiver has
//! no idea what it missed and never asks.

use std::collections::HashSet;

use crate::Homography;
use crate::codec::{
    FrameFlags, FrameHeader, PayloadUnit, UNIT_OVERHEAD, encode_units, parse_units, unit_type,
};
use crate::detect::Detector;
use crate::error::{Error, Result};
use crate::fec::PayloadCodec;
use crate::file::{Compression, Manifest, compress, decompress, digest};
use crate::frame::{FrameLayout, bytes_to_cells, cell_byte_span, cells_to_bytes};
use crate::image::RgbImage;
use crate::profile::{PROFILES, ProfileId};
use crate::symbol::Classifier;
use crate::transport::{PAYLOAD_ID_LEN, TransportDecoder, TransportEncoder, choose_symbol_size};

/// How often the manifest is sent, in frames.
///
/// Every frame. `SPEC.md` §7.1 permits one in eight and recommends every frame
/// above 4096 bytes of capacity, and the reference implementation used to follow
/// that literally — which meant `P1-conservative`, at 2611 bytes, sent the
/// manifest in one frame out of eight.
///
/// That was the wrong trade, and precisely backwards. A receiver reads a
/// fraction of the frames shown to it: a camera skips some, a slow decoder
/// skips more, glare takes others. Making the one indispensable unit eight times
/// rarer than the rest multiplies that fraction by itself, and until it arrives
/// every symbol received has nowhere to go. The manifest costs about 3% of a
/// frame on the profile where it was being rationed, and buying a failure mode
/// back for 3% is not a trade worth making.
const MANIFEST_PERIOD: u32 = 1;

/// Confidence below which a cell is offered to the decoder as an erasure.
///
/// Deliberately low. A wrongly marked cell costs one erasure slot and nothing
/// else — the corrector simply computes a zero magnitude for it — whereas
/// exhausting the budget on healthy cells leaves no room for the damage that is
/// actually there.
const ERASURE_CONFIDENCE: f32 = 0.08;

/// A painted frame and the header that describes it.
#[derive(Debug, Clone)]
pub struct Frame {
    /// What the frame declares about itself.
    pub header: FrameHeader,
    /// The painted pixels.
    pub image: RgbImage,
}

/// Paints the endless sequence of frames for one file.
pub struct Transmitter {
    layout: FrameLayout,
    payload_codec: PayloadCodec,
    encoder: TransportEncoder,
    manifest: Manifest,
    manifest_unit: PayloadUnit,
    pending: Vec<Vec<u8>>,
    cursor: usize,
    next_repair: u32,
    session_id: u32,
    frame_seq: u32,
    symbols_per_frame: usize,
    frames_per_pass: usize,
}

impl Transmitter {
    /// Prepares a transfer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for an empty file, a file name that is not a
    /// bare name, or a profile whose frames cannot hold even one symbol.
    pub fn new(name: &str, file: &[u8], profile: ProfileId, session_id: u32) -> Result<Self> {
        if file.is_empty() {
            return Err(Error::Malformed {
                context: "transfer",
                detail: "cannot transmit an empty file",
            });
        }

        let layout = FrameLayout::new(profile.profile());
        let payload_codec = PayloadCodec::for_profile(profile.profile());
        let capacity = payload_codec.capacity();

        let (compression, packed) = compress(file);

        // The manifest is sized before the transport exists, because its own
        // length decides how much room is left for symbols, which decides the
        // symbol size, which is what the manifest has to describe.
        let manifest_len = crate::file::MANIFEST_FIXED_LEN + name.len() + UNIT_OVERHEAD;
        let symbol_overhead = UNIT_OVERHEAD + PAYLOAD_ID_LEN;
        let symbol_size = choose_symbol_size(capacity, symbol_overhead, manifest_len);

        let encoder = TransportEncoder::new(&packed, symbol_size)?;

        let manifest = Manifest {
            compression,
            original_size: file.len() as u64,
            sha256: digest(file),
            oti: encoder.oti(),
            name: name.to_owned(),
        };
        let manifest_unit = PayloadUnit::new(unit_type::MANIFEST, manifest.encode()?);

        let symbol_slot = symbol_size as usize + symbol_overhead;
        let symbols_per_frame = capacity.saturating_sub(manifest_len) / symbol_slot;
        if symbols_per_frame == 0 {
            return Err(Error::Malformed {
                context: "transfer",
                detail: "profile frames are too small to carry a symbol",
            });
        }

        let source_symbols = encoder.source_symbol_count();
        let frames_per_pass = source_symbols.div_ceil(symbols_per_frame);
        let pending = encoder.source_symbols();

        Ok(Self {
            layout,
            payload_codec,
            encoder,
            manifest,
            manifest_unit,
            pending,
            cursor: 0,
            next_repair: 0,
            session_id,
            frame_seq: 0,
            symbols_per_frame,
            frames_per_pass,
        })
    }

    /// The manifest every frame advertises.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Frames needed to show every source symbol once.
    ///
    /// A receiver that films this many frames cleanly, from any starting point,
    /// has seen enough — the fountain code does not care which ones.
    #[must_use]
    pub const fn frames_per_pass(&self) -> usize {
        self.frames_per_pass
    }

    /// Symbols each frame carries.
    #[must_use]
    pub const fn symbols_per_frame(&self) -> usize {
        self.symbols_per_frame
    }

    /// How the payload was packed.
    #[must_use]
    pub const fn compression(&self) -> Compression {
        self.manifest.compression
    }

    /// Paints the next frame. This never runs out.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] only for internal contract violations, which
    /// would mean a profile table and a codec that disagree.
    pub fn next_frame(&mut self, cell_px: u32) -> Result<Frame> {
        let capacity = self.payload_codec.capacity();
        let mut units = Vec::new();
        let mut used = 0usize;
        let mut flags = FrameFlags::empty();

        let include_manifest = self.frame_seq.is_multiple_of(MANIFEST_PERIOD);
        if include_manifest && self.manifest_unit.encoded_len() <= capacity {
            used += self.manifest_unit.encoded_len();
            units.push(self.manifest_unit.clone());
            flags = flags.union(FrameFlags::HAS_MANIFEST);
        }

        for _ in 0..self.symbols_per_frame {
            let symbol = self.next_symbol();
            let unit = PayloadUnit::new(unit_type::RQ_SYMBOL, symbol);
            if used + unit.encoded_len() > capacity {
                break;
            }
            used += unit.encoded_len();
            units.push(unit);
        }

        let pass_length = u32::try_from(self.frames_per_pass.max(1)).unwrap_or(u32::MAX);
        if self.frame_seq.is_multiple_of(pass_length) {
            flags = flags.union(FrameFlags::LOOP_RESTART);
        }

        let payload = encode_units(&units, capacity)?;
        let header = FrameHeader {
            profile: self.layout.profile().id,
            flags,
            unit_count: u8::try_from(units.len()).unwrap_or(u8::MAX),
            session_id: self.session_id,
            frame_seq: self.frame_seq,
            payload_len: u16::try_from(payload.len()).unwrap_or(u16::MAX),
        };

        let raw = self.payload_codec.encode(&payload).map_err(|_| Error::Malformed {
            context: "frame payload",
            detail: "payload does not fit the frame",
        })?;
        let bits = self.layout.profile().bits_per_cell();
        let cells = bytes_to_cells(&raw, bits, self.layout.data_cells().len());

        let codeword_len = self.layout.profile().header_codeword_len() as usize;
        let codeword = header.encode_codeword(codeword_len)?;

        self.frame_seq = self.frame_seq.wrapping_add(1);
        Ok(Frame { header, image: self.layout.render(&codeword, &cells, cell_px) })
    }

    fn next_symbol(&mut self) -> Vec<u8> {
        if self.cursor >= self.pending.len() {
            self.pending =
                self.encoder.repair_symbols(self.next_repair, self.encoder.repair_batch());
            self.next_repair = self.next_repair.saturating_add(self.encoder.repair_batch());
            self.cursor = 0;
        }
        let symbol = self.pending.get(self.cursor).cloned().unwrap_or_default();
        self.cursor += 1;
        symbol
    }
}

/// What happened to one frame the receiver was shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// No code area could be located in the image at all.
    NotLocated,
    /// The picture caught two different codes at once.
    ///
    /// The header is carried at both edges, so a camera that opened its shutter
    /// across a screen refresh produces two copies that disagree. The payload
    /// between them is half of one code and half of the next, which no amount
    /// of error correction repairs — but the disagreement itself is a precise
    /// diagnosis, and the fix is on the sending device.
    Straddled,
    /// The frame was read and its units were taken.
    Decoded,
    /// The frame belongs to this session but has already been seen.
    Duplicate,
    /// The frame belongs to a different transmission.
    WrongSession,
    /// Neither copy of the header could be recovered.
    HeaderUnreadable,
    /// The header was read but the payload could not be repaired.
    PayloadUnrecoverable,
}

/// What the receiver learned from one frame.
///
/// `SPEC.md` §9.2 requires a decoder to explain itself. This is that
/// explanation, per frame, and it is what a user interface turns into "hold
/// steadier" or "you are nearly there".
#[derive(Debug, Clone)]
pub struct FrameReport {
    /// What happened.
    pub outcome: FrameOutcome,
    /// The header, when it was readable.
    pub header: Option<FrameHeader>,
    /// Units whose checksum verified.
    pub units_accepted: usize,
    /// Units dropped as corrupt.
    pub units_rejected: usize,
    /// Symbols this frame contributed that had not been seen before.
    pub new_symbols: usize,
    /// Cells the classifier was unsure of.
    pub doubtful_cells: usize,
    /// Payload cells in the frame, for turning the above into a rate.
    pub total_cells: usize,
    /// Camera pixels per cell, when the frame was located.
    ///
    /// The number that decides whether a capture can work at all, and the one a
    /// user can actually act on: it rises by stepping closer. See
    /// [`crate::detect::Detection::pixels_per_cell`].
    pub pixels_per_cell: Option<f64>,
}

impl FrameReport {
    /// Fraction of cells the classifier was unsure of.
    ///
    /// The single most useful number for a user: it rises long before decoding
    /// fails, so it can say "move closer" while there is still time to.
    #[must_use]
    pub fn doubtful_rate(&self) -> f64 {
        if self.total_cells == 0 {
            return 0.0;
        }
        self.doubtful_cells as f64 / self.total_cells as f64
    }
}

/// Everything one picture yielded, before it is folded into a transfer.
///
/// Produced by [`Receiver::examine`] and consumed by [`Receiver::absorb`]. The
/// split exists so the expensive half can run anywhere — across cores, or off a
/// browser's main thread — while the half that depends on frame order stays
/// where it must.
#[derive(Debug, Clone)]
pub struct FrameReading {
    /// How far the read got.
    pub outcome: FrameOutcome,
    /// The header, when it was readable.
    pub header: Option<FrameHeader>,
    /// Units whose checksum verified.
    pub units: Vec<PayloadUnit>,
    /// Units dropped as corrupt.
    pub units_rejected: usize,
    /// Cells the classifier was unsure of.
    pub doubtful_cells: usize,
    /// Payload cells in the frame.
    pub total_cells: usize,
    /// Camera pixels per cell, when the frame was located.
    pub pixels_per_cell: Option<f64>,
}

impl FrameReading {
    /// The reading for a picture with no code in it.
    #[must_use]
    fn not_located() -> Self {
        Self {
            outcome: FrameOutcome::NotLocated,
            header: None,
            units: Vec::new(),
            units_rejected: 0,
            doubtful_cells: 0,
            total_cells: 0,
            pixels_per_cell: None,
        }
    }
}

/// A file rebuilt from a recording.
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    /// The name the sender declared, already checked to be a bare file name.
    pub name: String,
    /// The reconstructed bytes, digest verified.
    pub bytes: Vec<u8>,
}

/// The geometry and codec for one candidate profile.
struct Candidate {
    id: ProfileId,
    layout: FrameLayout,
    payload_codec: PayloadCodec,
}

/// Collects frames until it can rebuild the file.
pub struct Receiver {
    candidates: Vec<Candidate>,
    detector: Detector,
    session_id: Option<u32>,
    manifest: Option<Manifest>,
    transport: Option<TransportDecoder>,
    /// Symbols that arrived before the manifest did.
    ///
    /// They are perfectly good symbols; the only thing missing is the
    /// transmission information needed to feed them anywhere. Dropping them
    /// would waste every frame before the first readable manifest, which on a
    /// profile that carries the manifest occasionally is most of them.
    orphans: Vec<Vec<u8>>,
    seen_frames: HashSet<u32>,
    frames_seen: usize,
    frames_used: usize,
    frames_located: usize,
    frames_headered: usize,
}

/// Orphan symbols to hold before the manifest arrives.
///
/// Bounded because the buffer is fed by untrusted input: a recording of
/// something that merely resembles a frame must not be able to grow it without
/// limit. Generous enough to cover any plausible wait for a manifest.
const MAX_ORPHANS: usize = 4096;

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    /// A receiver that works out the profile from what it sees.
    ///
    /// The frames say which profile drew them, in a header written the same way
    /// for every profile precisely so that it can be read before the profile is
    /// known. Requiring a person to match a setting on two devices, and giving
    /// them nothing but a failure when they do not, was a mistake this replaces.
    #[must_use]
    pub fn new() -> Self {
        Self::over(PROFILES.iter().map(|p| p.id).collect())
    }

    /// A receiver restricted to one profile.
    ///
    /// Slightly cheaper, and worth it only when the profile is genuinely known.
    #[must_use]
    pub fn for_profile(profile: ProfileId) -> Self {
        Self::over(vec![profile])
    }

    fn over(profiles: Vec<ProfileId>) -> Self {
        let detector =
            if profiles.len() == 1 { Detector::for_profile(profiles[0]) } else { Detector::new() };

        Self {
            candidates: profiles
                .into_iter()
                .map(|id| Candidate {
                    id,
                    layout: FrameLayout::new(id.profile()),
                    payload_codec: PayloadCodec::for_profile(id.profile()),
                })
                .collect(),
            detector,
            session_id: None,
            manifest: None,
            transport: None,
            orphans: Vec::new(),
            seen_frames: HashSet::new(),
            frames_seen: 0,
            frames_used: 0,
            frames_located: 0,
            frames_headered: 0,
        }
    }

    /// The candidate for a profile, if this receiver is considering it.
    fn candidate(&self, profile: ProfileId) -> Option<&Candidate> {
        self.candidates.iter().find(|c| c.id == profile)
    }

    /// The profile this receiver has settled on, once a frame has been read.
    #[must_use]
    pub fn profile(&self) -> Option<ProfileId> {
        (self.candidates.len() == 1).then(|| self.candidates[0].id)
    }

    /// The manifest, once any frame has carried a readable one.
    #[must_use]
    pub const fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// Frames offered, and frames that yielded something.
    #[must_use]
    pub const fn frame_counts(&self) -> (usize, usize) {
        (self.frames_seen, self.frames_used)
    }

    /// How far the frames got: seen, located, header read, used.
    ///
    /// The shape of a failure is in these four numbers. Nothing located means
    /// aim; located but no header means the capture is too poor; headers but no
    /// manifest means keep going a little longer.
    #[must_use]
    pub const fn stage_counts(&self) -> (usize, usize, usize, usize) {
        (self.frames_seen, self.frames_located, self.frames_headered, self.frames_used)
    }

    /// Symbols held back because the manifest has not arrived yet.
    #[must_use]
    pub fn orphan_symbols(&self) -> usize {
        self.orphans.len()
    }

    /// Distinct symbols collected and the number needed, once the manifest has
    /// arrived.
    #[must_use]
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.transport.as_ref().map(|t| (t.symbols_accepted(), t.symbols_needed()))
    }

    /// Whether enough has been collected to rebuild the file.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.transport.as_ref().is_some_and(TransportDecoder::is_complete)
    }

    /// Offers one video frame, locating the code area first.
    ///
    /// This is the entry point a decoder actually uses: it is handed pictures,
    /// not frames. Images with no code in them are the common case — a recording
    /// starts before the phone is pointed at anything — so failing to locate one
    /// is an ordinary outcome rather than an error.
    pub fn accept_image(&mut self, image: &RgbImage) -> FrameReport {
        self.absorb(self.examine(image))
    }

    /// Offers one already-located frame.
    ///
    /// `transform` maps cell space onto the image. Callers that have a picture
    /// rather than a located frame want [`Receiver::accept_image`]; this exists
    /// for tests and for callers doing their own detection.
    pub fn accept_frame(&mut self, image: &RgbImage, transform: &Homography) -> FrameReport {
        // Without a detection there is nothing to say which profile drew the
        // frame, so every candidate is tried and the first readable header wins.
        for candidate in &self.candidates {
            let reading = read_frame(candidate, image, transform);
            if reading.header.is_some() {
                return self.absorb(reading);
            }
        }
        self.absorb(FrameReading::not_located())
    }

    /// Reads everything a single picture can yield, changing nothing.
    ///
    /// This is where a frame's cost lies — detection, per-cell classification
    /// and error correction — and none of it depends on any other frame. Keeping
    /// it free of the receiver's state is what lets a video be decoded across
    /// every core at once, and what lets a browser do the same work off the main
    /// thread. The order-dependent part, which is small, is [`Receiver::absorb`].
    #[must_use]
    pub fn examine(&self, image: &RgbImage) -> FrameReading {
        match self.detector.detect(image) {
            Ok(detection) => match self.candidate(detection.profile) {
                Some(candidate) => {
                    let mut reading = read_frame(candidate, image, &detection.transform);
                    reading.pixels_per_cell = Some(detection.pixels_per_cell());
                    reading
                }
                None => FrameReading::not_located(),
            },
            Err(_) => FrameReading::not_located(),
        }
    }

    /// Folds a reading into the transfer.
    ///
    /// Everything here depends on what came before: which session was locked
    /// onto, which frames have already been seen, which symbols are new. It is
    /// cheap, and it is the only part that has to happen in order.
    pub fn absorb(&mut self, reading: FrameReading) -> FrameReport {
        self.frames_seen += 1;

        let mut report = FrameReport {
            outcome: reading.outcome,
            header: reading.header,
            units_accepted: 0,
            units_rejected: reading.units_rejected,
            new_symbols: 0,
            doubtful_cells: reading.doubtful_cells,
            total_cells: reading.total_cells,
            pixels_per_cell: reading.pixels_per_cell,
        };

        if reading.pixels_per_cell.is_some() {
            self.frames_located += 1;
        }
        if reading.header.is_some() {
            self.frames_headered += 1;
        }

        let Some(header) = reading.header else {
            return report;
        };
        if reading.outcome != FrameOutcome::Decoded {
            return report;
        }

        // The frames say which profile drew them. Once one has, stop
        // considering the others: it makes every later frame cheaper and
        // removes any chance of a stray fit to the wrong geometry.
        if self.candidates.len() > 1 {
            self.candidates.retain(|c| c.id == header.profile);
            self.detector = Detector::for_profile(header.profile);
        }

        match self.session_id {
            Some(known) if known != header.session_id => {
                report.outcome = FrameOutcome::WrongSession;
                return report;
            }
            None => self.session_id = Some(header.session_id),
            Some(_) => {}
        }

        if !self.seen_frames.insert(header.frame_seq) {
            report.outcome = FrameOutcome::Duplicate;
            return report;
        }

        report.units_accepted = reading.units.len();
        for unit in reading.units {
            match unit.kind {
                unit_type::MANIFEST => report.new_symbols += self.take_manifest(&unit.data),
                unit_type::RQ_SYMBOL => match self.transport.as_mut() {
                    Some(transport) => {
                        if transport.push(&unit.data) {
                            report.new_symbols += 1;
                        }
                    }
                    // No manifest yet, so there is nowhere to put this symbol.
                    // Hold it rather than drop it: it is a perfectly good
                    // symbol and the manifest is on its way.
                    None => {
                        if self.orphans.len() < MAX_ORPHANS {
                            self.orphans.push(unit.data);
                        }
                    }
                },
                // Unknown types are the format's extension point. Skipping one
                // is correct behaviour, not an error.
                _ => {}
            }
        }

        if report.new_symbols > 0 || report.units_accepted > 0 {
            self.frames_used += 1;
        }
        report
    }

    /// Rebuilds the file.
    ///
    /// # Errors
    ///
    /// Returns the stage that is missing, never a bare failure: no manifest yet,
    /// not enough symbols and how many are short, a decompression failure, or a
    /// digest mismatch.
    pub fn finish(&mut self) -> Result<ReceivedFile> {
        // `SPEC.md` §9.2 requires the stage that gave up to be named. Reporting
        // a missing manifest when no frame was ever located sends someone off
        // to film for longer when what they actually need is to aim, or to fix
        // a setting -- the two most common real failures, told apart here by
        // how far the frames got.
        if self.manifest.is_none() {
            if self.frames_located == 0 {
                return Err(Error::NoFinders);
            }
            if self.frames_headered == 0 {
                return Err(Error::HeaderUnrecoverable);
            }
            return Err(Error::NoManifest);
        }

        let Some(manifest) = self.manifest.clone() else {
            return Err(Error::NoManifest);
        };
        let Some(transport) = self.transport.as_mut() else {
            return Err(Error::NoManifest);
        };

        let Some(packed) = transport.take_result() else {
            return Err(Error::InsufficientSymbols {
                accepted: transport.symbols_accepted(),
                needed: transport.symbols_needed(),
            });
        };

        let size =
            usize::try_from(manifest.original_size).map_err(|_| Error::DecompressionFailed)?;
        let file = decompress(manifest.compression, &packed, size)?;
        manifest.verify(&file)?;

        Ok(ReceivedFile { name: manifest.name.clone(), bytes: file })
    }

    /// Takes the manifest from a unit, and releases any symbols that were
    /// waiting for it.
    fn take_manifest(&mut self, bytes: &[u8]) -> usize {
        if self.manifest.is_some() {
            return 0;
        }
        let Ok(manifest) = Manifest::decode(bytes) else { return 0 };
        if !manifest.compression.is_supported() {
            return 0;
        }
        let Ok(mut transport) = TransportDecoder::new(manifest.oti) else { return 0 };

        // Everything held back until this moment is now usable. On a profile
        // that carries the manifest only occasionally these are most of the
        // frames read so far, and throwing them away was making a slow channel
        // slower for no reason.
        let mut released = 0usize;
        for symbol in std::mem::take(&mut self.orphans) {
            if transport.push(&symbol) {
                released += 1;
            }
        }

        self.manifest = Some(manifest);
        self.transport = Some(transport);
        released
    }
}

/// Reads both copies of the header.
///
/// Both, not the first that works. The copies sit at opposite edges of the
/// frame, and a camera whose shutter spanned a screen refresh will have caught
/// a different code at each end. When they disagree the payload between them is
/// a splice of two codes and is not worth decoding — and, far more usefully,
/// the disagreement says exactly what is wrong and where to fix it.
fn read_headers(
    layout: &FrameLayout,
    image: &RgbImage,
    transform: &Homography,
) -> [Option<FrameHeader>; 2] {
    let codeword_len = layout.profile().header_codeword_len() as usize;
    let (dark, light) = timing_levels(layout, image, transform);
    let threshold = f32::midpoint(dark, light);

    core::array::from_fn(|band| {
        let cells = layout.header_band(band);
        let mut bytes = vec![0u8; codeword_len];
        for (index, &cell) in cells.iter().take(codeword_len * 8).enumerate() {
            let luma = layout.sample_cell_mean(image, transform, cell).luma();
            if luma > threshold {
                bytes[index / 8] |= 1 << (7 - index % 8);
            }
        }
        FrameHeader::decode_codeword(&bytes, &[]).ok()
    })
}

/// Black and white levels measured from this frame's timing ring.
///
/// The ring alternates by construction, so it is a per-frame reference for what
/// "dark" and "light" mean through this camera at this exposure — which is the
/// only way to threshold the header without assuming an exposure the emitter
/// never chose.
fn timing_levels(layout: &FrameLayout, image: &RgbImage, transform: &Homography) -> (f32, f32) {
    let grid = layout.grid();
    let mut dark = (0.0f32, 0u32);
    let mut light = (0.0f32, 0u32);

    let mut consider = |row: u32, col: u32, even: bool| {
        let luma = layout.sample_cell_mean(image, transform, row * grid + col).luma();
        if even {
            dark.0 += luma;
            dark.1 += 1;
        } else {
            light.0 += luma;
            light.1 += 1;
        }
    };

    for col in 8..grid - 8 {
        consider(0, col, col % 2 == 0);
        consider(grid - 1, col, col % 2 == 0);
    }
    for row in 8..grid - 8 {
        consider(row, 0, row % 2 == 0);
        consider(row, grid - 1, row % 2 == 0);
    }

    let mean = |(sum, count): (f32, u32)| if count == 0 { 0.0 } else { sum / count as f32 };
    (mean(dark), mean(light))
}

/// Fits the per-frame classifier to the calibration ring.
fn fit_classifier(layout: &FrameLayout, image: &RgbImage, transform: &Homography) -> Classifier {
    let labelled: Vec<(u16, crate::symbol::CellSample)> = layout
        .calibration_cells()
        .iter()
        .enumerate()
        .map(|(index, &cell)| {
            (layout.calibration_value(index), layout.sample_cell(image, transform, cell))
        })
        .collect();
    Classifier::fit(layout.alphabet(), &labelled)
}

/// Turns doubtful cells into the byte positions they touched.
fn erasure_positions(doubtful: &[usize], bits: u32) -> Vec<usize> {
    let mut positions = Vec::with_capacity(doubtful.len() * 2);
    for &index in doubtful {
        let (first, last) = cell_byte_span(index, bits);
        for position in first..=last {
            positions.push(position);
        }
    }
    positions.sort_unstable();
    positions.dedup();
    positions
}

/// Reads a located frame down to its payload units, for one candidate profile.
fn read_frame(candidate: &Candidate, image: &RgbImage, transform: &Homography) -> FrameReading {
    let layout = &candidate.layout;
    let codec = &candidate.payload_codec;

    let mut reading = FrameReading {
        outcome: FrameOutcome::HeaderUnreadable,
        header: None,
        units: Vec::new(),
        units_rejected: 0,
        doubtful_cells: 0,
        total_cells: layout.data_cells().len(),
        pixels_per_cell: None,
    };

    let headers = read_headers(layout, image, transform);
    let Some(header) = headers[0].or(headers[1]) else {
        return reading;
    };

    // Two readable copies that name different frames mean the shutter caught a
    // screen refresh. The payload between them is a splice; decoding it would
    // burn the frame's error correction on damage that is not random.
    if let (Some(top), Some(bottom)) = (headers[0], headers[1])
        && top.frame_seq != bottom.frame_seq
    {
        reading.header = Some(header);
        reading.outcome = FrameOutcome::Straddled;
        return reading;
    }
    // A header that names a different profile means this candidate's geometry
    // happened to produce a readable header for someone else's frame. Believing
    // it would decode the payload against the wrong cell map.
    if header.profile != candidate.id {
        return reading;
    }
    reading.header = Some(header);

    // The classifier is fitted to this frame's own calibration ring, never to
    // the nominal palette: exposure and white balance move while the camera
    // records, so references from any other frame are already stale.
    let classifier = fit_classifier(layout, image, transform);
    let bits = layout.profile().bits_per_cell();

    let mut cells = Vec::with_capacity(layout.data_cells().len());
    let mut doubtful = Vec::new();
    for (index, &cell) in layout.data_cells().iter().enumerate() {
        let sample = layout.sample_cell(image, transform, cell);
        let call = classifier.classify(&sample);
        cells.push(call.value);
        if call.confidence < ERASURE_CONFIDENCE {
            doubtful.push(index);
        }
    }
    reading.doubtful_cells = doubtful.len();

    let raw = cells_to_bytes(&cells, bits, codec.raw_len());
    let erasures = erasure_positions(&doubtful, bits);

    // Erasure hints come from a heuristic. If they do not help, the same frame
    // may still decode without them, and a frame given up on is one somebody
    // has to film again.
    let Ok(payload) = codec.decode(&raw, &erasures).or_else(|_| codec.decode(&raw, &[])) else {
        reading.outcome = FrameOutcome::PayloadUnrecoverable;
        return reading;
    };

    let (units, rejected) = parse_units(&payload, usize::from(header.payload_len));
    reading.units = units;
    reading.units_rejected = rejected;
    reading.outcome = FrameOutcome::Decoded;
    reading
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::PROFILES;

    fn sample_file(len: usize) -> Vec<u8> {
        // Text-like input, so compression is exercised rather than skipped.
        let words =
            ["photon", "protocol", "screen", "camera", "fountain", "symbol", "frame", "light"];
        let mut out = Vec::with_capacity(len);
        let mut index = 0usize;
        while out.len() < len {
            out.extend_from_slice(words[index % words.len()].as_bytes());
            out.push(b' ');
            index += 1;
        }
        out.truncate(len);
        out
    }

    /// Input that will not compress, so a test can be sure of how many frames a
    /// transfer really needs. Text compresses so hard that a "large" file can
    /// otherwise fit in a single frame.
    fn incompressible(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                u8::try_from(state & 0xFF).unwrap_or(0)
            })
            .collect()
    }

    #[test]
    fn a_file_survives_the_round_trip_for_every_profile() {
        for profile in &PROFILES {
            let file = sample_file(9_000);
            let mut tx = Transmitter::new("report.pdf", &file, profile.id, 0x0BAD_C0DE).unwrap();
            let mut rx = Receiver::for_profile(profile.id);

            let mut frames = 0usize;
            while !rx.is_complete() && frames < 400 {
                let frame = tx.next_frame(8).unwrap();
                let transform = FrameLayout::new(profile.id.profile()).identity_transform(8);
                let report = rx.accept_frame(&frame.image, &transform);
                assert_eq!(
                    report.outcome,
                    FrameOutcome::Decoded,
                    "{} frame {frames} was not decoded",
                    profile.name
                );
                assert_eq!(report.units_rejected, 0, "{} rejected a unit", profile.name);
                frames += 1;
            }

            assert!(rx.is_complete(), "{} never completed", profile.name);
            let received = rx.finish().unwrap();
            assert_eq!(received.name, "report.pdf");
            assert_eq!(received.bytes, file, "{} reconstructed the wrong bytes", profile.name);
        }
    }

    #[test]
    fn the_receiver_can_start_filming_late() {
        // Nobody starts recording at frame zero. The manifest has to keep
        // arriving, and the fountain code has to accept whatever window it gets.
        let file = sample_file(12_000);
        let mut tx = Transmitter::new("late.bin", &file, ProfileId::P2Standard, 7).unwrap();
        let mut rx = Receiver::for_profile(ProfileId::P2Standard);
        let transform = FrameLayout::new(ProfileId::P2Standard.profile()).identity_transform(8);

        for _ in 0..5 {
            tx.next_frame(8).unwrap();
        }

        let mut frames = 0usize;
        while !rx.is_complete() && frames < 400 {
            let frame = tx.next_frame(8).unwrap();
            rx.accept_frame(&frame.image, &transform);
            frames += 1;
        }

        assert!(rx.is_complete());
        assert_eq!(rx.finish().unwrap().bytes, file);
    }

    #[test]
    fn frames_from_another_transmission_are_refused() {
        // Two people filming two screens in the same room is not a strange
        // scenario, and mixing their symbols would corrupt both files silently.
        let file = sample_file(4_000);
        let mut first = Transmitter::new("a.bin", &file, ProfileId::P2Standard, 1).unwrap();
        let mut second = Transmitter::new("b.bin", &file, ProfileId::P2Standard, 2).unwrap();

        let mut rx = Receiver::for_profile(ProfileId::P2Standard);
        let transform = FrameLayout::new(ProfileId::P2Standard.profile()).identity_transform(8);

        let frame = first.next_frame(8).unwrap();
        assert_eq!(rx.accept_frame(&frame.image, &transform).outcome, FrameOutcome::Decoded);

        let intruder = second.next_frame(8).unwrap();
        assert_eq!(
            rx.accept_frame(&intruder.image, &transform).outcome,
            FrameOutcome::WrongSession
        );
    }

    #[test]
    fn a_repeated_frame_is_recognised_as_a_duplicate() {
        // A camera sees most displayed frames more than once. Re-decoding one
        // is wasted work, and counting it as progress would misreport.
        let file = sample_file(4_000);
        let mut tx = Transmitter::new("dup.bin", &file, ProfileId::P2Standard, 3).unwrap();
        let mut rx = Receiver::for_profile(ProfileId::P2Standard);
        let transform = FrameLayout::new(ProfileId::P2Standard.profile()).identity_transform(8);

        let frame = tx.next_frame(8).unwrap();
        assert_eq!(rx.accept_frame(&frame.image, &transform).outcome, FrameOutcome::Decoded);

        let repeat = rx.accept_frame(&frame.image, &transform);
        assert_eq!(repeat.outcome, FrameOutcome::Duplicate);
        assert_eq!(repeat.new_symbols, 0);
    }

    #[test]
    fn finishing_early_says_how_far_short_it_is() {
        // SPEC.md 9.2: never a bare failure. A user deciding whether to keep
        // filming needs the distance, not the verdict.
        let file = incompressible(60_000);
        let mut tx = Transmitter::new("short.bin", &file, ProfileId::P1Conservative, 5).unwrap();
        let mut rx = Receiver::for_profile(ProfileId::P1Conservative);
        let transform = FrameLayout::new(ProfileId::P1Conservative.profile()).identity_transform(8);

        let frame = tx.next_frame(8).unwrap();
        rx.accept_frame(&frame.image, &transform);

        match rx.finish() {
            Err(Error::InsufficientSymbols { accepted, needed }) => {
                assert!(accepted > 0, "the frame carried symbols");
                assert!(needed > accepted, "needed {needed} accepted {accepted}");
            }
            Err(other) => panic!("expected a shortfall report, got {other}"),
            Ok(received) => panic!("one frame should not carry {} bytes", received.bytes.len()),
        }
    }

    #[test]
    fn a_failure_names_the_stage_that_actually_gave_up() {
        // SPEC.md 9.2 asks for the stage, and the stages have completely
        // different fixes: nothing located means aim the camera, a located
        // frame with no readable header means the capture is too poor, and
        // headers without a manifest means keep going. Reporting "no manifest"
        // for all three -- which this used to do -- sends two thirds of people
        // to film for longer when filming longer cannot help them.
        let mut rx = Receiver::for_profile(ProfileId::P2Standard);
        assert_eq!(rx.finish().unwrap_err(), Error::NoFinders);
        assert!(rx.progress().is_none());

        let blank = RgbImage::filled(400, 400, crate::Rgb::WHITE);
        assert_eq!(rx.accept_image(&blank).outcome, FrameOutcome::NotLocated);
        assert_eq!(rx.finish().unwrap_err(), Error::NoFinders);
        assert_eq!(rx.stage_counts(), (1, 0, 0, 0));
    }

    #[test]
    fn symbols_that_arrive_before_the_manifest_are_kept() {
        // Until the manifest lands there is nowhere to put a symbol, and the
        // receiver used to drop them. On a channel where a receiver reads a
        // fraction of the frames, that threw away everything before the first
        // readable manifest for no reason.
        let file = incompressible(40_000);
        let profile = ProfileId::P1Conservative;
        let mut tx = Transmitter::new("orphans.bin", &file, profile, 21).unwrap();
        let mut rx = Receiver::for_profile(profile);
        let transform = FrameLayout::new(profile.profile()).identity_transform(8);

        // Feed a frame's units in by hand, with the manifest withheld, so the
        // symbols have to wait for it.
        let frame = tx.next_frame(8).unwrap();
        let mut reading = rx.examine(&frame.image);
        assert_eq!(reading.outcome, FrameOutcome::Decoded);

        let withheld: Vec<_> =
            reading.units.iter().filter(|u| u.kind == unit_type::MANIFEST).cloned().collect();
        assert!(!withheld.is_empty(), "the frame should have carried a manifest");
        reading.units.retain(|u| u.kind != unit_type::MANIFEST);

        let report = rx.absorb(reading);
        assert_eq!(report.new_symbols, 0, "no manifest yet, so nothing can be counted");
        assert!(rx.orphan_symbols() > 0, "the symbols should have been kept");

        // Now let a manifest through: the held symbols must be released.
        let next = tx.next_frame(8).unwrap();
        let report = rx.accept_frame(&next.image, &transform);
        assert!(
            report.new_symbols > 1,
            "the manifest should have released the held symbols, got {}",
            report.new_symbols
        );
        assert_eq!(rx.orphan_symbols(), 0);
    }

    #[test]
    fn the_receiver_works_out_the_profile_by_itself() {
        // Requiring a person to match a setting on two devices, and giving them
        // nothing but a failure when they do not, was the single most likely way
        // to make this look broken when it was not.
        for profile in &PROFILES {
            let file = sample_file(6_000);
            let mut tx = Transmitter::new("auto.txt", &file, profile.id, 31).unwrap();
            let mut rx = Receiver::new();

            let mut frames = 0usize;
            while !rx.is_complete() && frames < 200 {
                let frame = tx.next_frame(8).unwrap();
                let report = rx.accept_image(&frame.image);
                assert_eq!(report.outcome, FrameOutcome::Decoded, "{}", profile.name);
                frames += 1;
            }

            assert_eq!(rx.profile(), Some(profile.id), "{} was not identified", profile.name);
            assert_eq!(rx.finish().unwrap().bytes, file, "{}", profile.name);
        }
    }

    #[test]
    fn every_frame_advertises_the_manifest_when_it_can_afford_to() {
        // Getting the file name and the progress target from the very first
        // decoded frame is what lets a user interface say something useful
        // immediately instead of after eight frames.
        let file = sample_file(20_000);
        let mut tx = Transmitter::new("meta.bin", &file, ProfileId::P2Standard, 9).unwrap();
        let mut rx = Receiver::for_profile(ProfileId::P2Standard);
        let transform = FrameLayout::new(ProfileId::P2Standard.profile()).identity_transform(8);

        let frame = tx.next_frame(8).unwrap();
        assert!(frame.header.flags.contains(FrameFlags::HAS_MANIFEST));
        rx.accept_frame(&frame.image, &transform);

        assert_eq!(rx.manifest().map(|m| m.name.as_str()), Some("meta.bin"));
        assert!(rx.progress().is_some());
    }

    #[test]
    fn an_empty_file_is_refused_before_anything_is_painted() {
        assert!(Transmitter::new("empty.bin", &[], ProfileId::P2Standard, 1).is_err());
    }

    #[test]
    fn a_hostile_file_name_never_reaches_a_frame() {
        assert!(Transmitter::new("../escape", b"data", ProfileId::P2Standard, 1).is_err());
    }
}
