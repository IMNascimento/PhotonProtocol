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
use crate::profile::ProfileId;
use crate::symbol::Classifier;
use crate::transport::{PAYLOAD_ID_LEN, TransportDecoder, TransportEncoder, choose_symbol_size};

/// Payload capacity above which the manifest is worth putting in every frame
/// (`SPEC.md` §7.1).
const MANIFEST_IN_EVERY_FRAME_ABOVE: usize = 4096;

/// Otherwise the manifest goes in one frame out of this many.
const MANIFEST_PERIOD: u32 = 8;

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

        let include_manifest = capacity > MANIFEST_IN_EVERY_FRAME_ABOVE
            || self.frame_seq.is_multiple_of(MANIFEST_PERIOD);
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

/// A file rebuilt from a recording.
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    /// The name the sender declared, already checked to be a bare file name.
    pub name: String,
    /// The reconstructed bytes, digest verified.
    pub bytes: Vec<u8>,
}

/// Collects frames until it can rebuild the file.
pub struct Receiver {
    layout: FrameLayout,
    payload_codec: PayloadCodec,
    detector: Detector,
    session_id: Option<u32>,
    manifest: Option<Manifest>,
    transport: Option<TransportDecoder>,
    seen_frames: HashSet<u32>,
    frames_seen: usize,
    frames_used: usize,
}

impl Receiver {
    /// A receiver expecting frames of the given profile.
    #[must_use]
    pub fn new(profile: ProfileId) -> Self {
        Self {
            layout: FrameLayout::new(profile.profile()),
            payload_codec: PayloadCodec::for_profile(profile.profile()),
            detector: Detector::for_profile(profile),
            session_id: None,
            manifest: None,
            transport: None,
            seen_frames: HashSet::new(),
            frames_seen: 0,
            frames_used: 0,
        }
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
        if let Ok(detection) = self.detector.detect(image) {
            return self.accept_frame(image, &detection.transform);
        }

        self.frames_seen += 1;
        FrameReport {
            outcome: FrameOutcome::NotLocated,
            header: None,
            units_accepted: 0,
            units_rejected: 0,
            new_symbols: 0,
            doubtful_cells: 0,
            total_cells: self.layout.data_cells().len(),
        }
    }

    /// Offers one already-located frame.
    ///
    /// `transform` maps cell space onto the image. Callers that have a picture
    /// rather than a located frame want [`Receiver::accept_image`]; this exists
    /// for tests and for callers doing their own detection.
    pub fn accept_frame(&mut self, image: &RgbImage, transform: &Homography) -> FrameReport {
        self.frames_seen += 1;

        let mut report = FrameReport {
            outcome: FrameOutcome::HeaderUnreadable,
            header: None,
            units_accepted: 0,
            units_rejected: 0,
            new_symbols: 0,
            doubtful_cells: 0,
            total_cells: self.layout.data_cells().len(),
        };

        let Some(header) = self.read_header(image, transform) else {
            return report;
        };
        report.header = Some(header);

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

        // The classifier is fitted to this frame's own calibration ring, never
        // to the nominal palette: exposure and white balance move while the
        // camera records, so references from any other frame are already stale.
        let classifier = self.fit_classifier(image, transform);
        let bits = self.layout.profile().bits_per_cell();

        let mut cells = Vec::with_capacity(self.layout.data_cells().len());
        let mut doubtful = Vec::new();
        for (index, &cell) in self.layout.data_cells().iter().enumerate() {
            let sample = self.layout.sample_cell(image, transform, cell);
            let call = classifier.classify(&sample);
            cells.push(call.value);
            if call.confidence < ERASURE_CONFIDENCE {
                doubtful.push(index);
            }
        }
        report.doubtful_cells = doubtful.len();

        let raw = cells_to_bytes(&cells, bits, self.payload_codec.raw_len());
        let erasures = Self::erasure_positions(&doubtful, bits);

        // Erasure hints come from a heuristic. If they do not help, the same
        // frame may still decode without them, and a frame given up on is one
        // somebody has to film again.
        let Ok(payload) = self
            .payload_codec
            .decode(&raw, &erasures)
            .or_else(|_| self.payload_codec.decode(&raw, &[]))
        else {
            report.outcome = FrameOutcome::PayloadUnrecoverable;
            return report;
        };

        let (units, rejected) = parse_units(&payload, usize::from(header.payload_len));
        report.units_accepted = units.len();
        report.units_rejected = rejected;
        report.outcome = FrameOutcome::Decoded;

        for unit in units {
            match unit.kind {
                unit_type::MANIFEST => self.take_manifest(&unit.data),
                unit_type::RQ_SYMBOL => {
                    if let Some(transport) = self.transport.as_mut()
                        && transport.push(&unit.data)
                    {
                        report.new_symbols += 1;
                    }
                }
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

    /// Reads whichever copy of the header survives.
    fn read_header(&self, image: &RgbImage, transform: &Homography) -> Option<FrameHeader> {
        let codeword_len = self.layout.profile().header_codeword_len() as usize;
        let (dark, light) = self.timing_levels(image, transform);
        let threshold = f32::midpoint(dark, light);

        for band in 0..2 {
            let cells = self.layout.header_band(band);
            let mut bytes = vec![0u8; codeword_len];
            for (index, &cell) in cells.iter().take(codeword_len * 8).enumerate() {
                let luma = self.layout.sample_cell_mean(image, transform, cell).luma();
                if luma > threshold {
                    bytes[index / 8] |= 1 << (7 - index % 8);
                }
            }
            if let Ok(header) = FrameHeader::decode_codeword(&bytes, &[]) {
                return Some(header);
            }
        }
        None
    }

    /// Black and white levels measured from this frame's timing ring.
    ///
    /// The ring alternates by construction, so it is a per-frame reference for
    /// what "dark" and "light" mean through this camera at this exposure —
    /// which is the only way to threshold the header without assuming an
    /// exposure the emitter never chose.
    fn timing_levels(&self, image: &RgbImage, transform: &Homography) -> (f32, f32) {
        let grid = self.layout.grid();
        let mut dark = (0.0f32, 0u32);
        let mut light = (0.0f32, 0u32);

        let mut consider = |row: u32, col: u32, even: bool| {
            let luma = self.layout.sample_cell_mean(image, transform, row * grid + col).luma();
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
    fn fit_classifier(&self, image: &RgbImage, transform: &Homography) -> Classifier {
        let labelled: Vec<(u16, crate::symbol::CellSample)> = self
            .layout
            .calibration_cells()
            .iter()
            .enumerate()
            .map(|(index, &cell)| {
                (
                    self.layout.calibration_value(index),
                    self.layout.sample_cell(image, transform, cell),
                )
            })
            .collect();
        Classifier::fit(self.layout.alphabet(), &labelled)
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

    fn take_manifest(&mut self, bytes: &[u8]) {
        if self.manifest.is_some() {
            return;
        }
        let Ok(manifest) = Manifest::decode(bytes) else { return };
        if !manifest.compression.is_supported() {
            return;
        }
        let Ok(transport) = TransportDecoder::new(manifest.oti) else { return };
        self.manifest = Some(manifest);
        self.transport = Some(transport);
    }
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
            let mut rx = Receiver::new(profile.id);

            let mut frames = 0usize;
            while !rx.is_complete() && frames < 400 {
                let frame = tx.next_frame(8).unwrap();
                let transform = rx.layout.identity_transform(8);
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
        let mut rx = Receiver::new(ProfileId::P2Standard);
        let transform = rx.layout.identity_transform(8);

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

        let mut rx = Receiver::new(ProfileId::P2Standard);
        let transform = rx.layout.identity_transform(8);

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
        let mut rx = Receiver::new(ProfileId::P2Standard);
        let transform = rx.layout.identity_transform(8);

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
        let mut rx = Receiver::new(ProfileId::P1Conservative);
        let transform = rx.layout.identity_transform(8);

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
    fn a_receiver_that_has_seen_nothing_says_so() {
        let mut rx = Receiver::new(ProfileId::P2Standard);
        assert_eq!(rx.finish().unwrap_err(), Error::NoManifest);
        assert!(rx.progress().is_none());
    }

    #[test]
    fn every_frame_advertises_the_manifest_when_it_can_afford_to() {
        // Getting the file name and the progress target from the very first
        // decoded frame is what lets a user interface say something useful
        // immediately instead of after eight frames.
        let file = sample_file(20_000);
        let mut tx = Transmitter::new("meta.bin", &file, ProfileId::P2Standard, 9).unwrap();
        let mut rx = Receiver::new(ProfileId::P2Standard);
        let transform = rx.layout.identity_transform(8);

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
