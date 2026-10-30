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
use crate::register::Mesh;
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

/// Whether frames are whitened. They are, unless a bench says otherwise.
static WHITENING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Turns whitening off, or back on, for everything in this process.
///
/// For a bench reading pictures that were taken before frames were whitened,
/// and for nothing else: the pictures a real phone could not read are the
/// best evidence there is of what a real phone does, and they were painted the
/// old way. A sender and a receiver that disagree about this share no frames.
pub fn set_whitening(on: bool) {
    WHITENING.store(on, core::sync::atomic::Ordering::Relaxed);
}

/// Makes what is painted look like noise, whatever is sent; applied twice, it
/// gives back what it was given.
///
/// A frame that is mostly padding is mostly one cell repeated, and so is a file
/// of zeros. A decoder measures a frame by what is in it: where black and
/// white are from the darkest and brightest of each neighbourhood, where the
/// lattice is from the edges between cells, what each ink looks like from the
/// cells painted in it. A frame of one cell repeated has no white, few edges
/// and one ink, and reads as nothing.
///
/// The bytes are combined with a fixed sequence after error correction and
/// before painting, so that every frame has every symbol in it about equally
/// often. The sequence is a 32-bit xorshift started at `PHTN`, one byte from
/// the top of each step.
fn whiten(bytes: &mut [u8]) {
    if !WHITENING.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut state = u32::from_be_bytes(crate::FRAME_MAGIC);
    for byte in bytes {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte ^= state.to_be_bytes()[0];
    }
}

/// Cells between the ones the classifier is refitted to.
///
/// Co-prime with every grid width, so the cells chosen wander across the frame
/// instead of falling in columns.
const REFIT_STRIDE: usize = 3;

/// A painted frame and the header that describes it.
#[derive(Debug, Clone)]
pub struct Frame {
    /// What the frame declares about itself.
    pub header: FrameHeader,
    /// The painted pixels.
    pub image: RgbImage,
}

/// What one frame carries, before it is drawn.
#[derive(Debug, Clone)]
pub struct FrameContents {
    /// What the frame declares about itself.
    pub header: FrameHeader,
    /// The header, error-corrected, as written into both bands.
    pub header_codeword: Vec<u8>,
    /// One value per payload cell, in the layout's data order.
    pub cells: Vec<u16>,
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
        let contents = self.next_contents()?;
        Ok(Frame {
            header: contents.header,
            image: self.layout.render(&contents.header_codeword, &contents.cells, cell_px),
        })
    }

    /// The next frame as cell values rather than as pixels.
    ///
    /// What a frame *says*, separated from how it is drawn. A bench that knows
    /// what every cell was meant to be can count the ones a decoder read
    /// wrongly, which is the measurement everything else is a proxy for.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] only for internal contract violations.
    pub fn next_contents(&mut self) -> Result<FrameContents> {
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

        let mut raw = self.payload_codec.encode(&payload).map_err(|_| Error::Malformed {
            context: "frame payload",
            detail: "payload does not fit the frame",
        })?;
        whiten(&mut raw);
        let bits = self.layout.profile().bits_per_cell();
        let cells = bytes_to_cells(&raw, bits, self.layout.data_cells().len());

        let codeword_len = self.layout.profile().header_codeword_len() as usize;
        let header_codeword = header.encode_codeword(codeword_len)?;

        self.frame_seq = self.frame_seq.wrapping_add(1);
        Ok(FrameContents { header, header_codeword, cells })
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
    /// The four finder centres in the picture, when the frame was located:
    /// top-left, top-right, bottom-right, bottom-left.
    ///
    /// Where the code is. A caller reading a video can look there first in the
    /// next picture, and can show the person holding the camera what it has
    /// found.
    pub corners: Option<[crate::Point; 4]>,
    /// How far the sampling grid had to be bent to fit, in cells.
    pub correction: Option<f64>,
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
    /// The value each payload cell was read as, when the read got that far.
    ///
    /// Before error correction, so it is what the classifier decided and not
    /// what the parity made of it.
    pub cells: Vec<u16>,
    /// The four finder centres in the picture, when the frame was located.
    pub corners: Option<[crate::Point; 4]>,
    /// How far the sampling grid had to be bent to fit, in cells.
    pub correction: Option<f64>,
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
            cells: Vec::new(),
            corners: None,
            correction: None,
        }
    }
}

/// A picture read as far as its frame's header.
///
/// Reading a picture has a cheap half and an expensive one, and the header
/// falls between them: it says which code this is before any of the payload
/// has been touched. A camera sees most codes more than once, so a caller that
/// looks at the header first can drop the repeats for a fraction of what
/// reading them would cost.
#[derive(Debug, Clone)]
pub struct FrameOpening {
    /// [`FrameOutcome::Decoded`] when the header was read and the payload is
    /// worth reading; otherwise why it is not.
    pub outcome: FrameOutcome,
    /// The header, when it was readable.
    pub header: Option<FrameHeader>,
    /// The profile whose geometry the frame was read with.
    pub profile: ProfileId,
    /// Camera pixels per cell.
    pub pixels_per_cell: Option<f64>,
    /// The four finder centres in the picture.
    pub corners: Option<[crate::Point; 4]>,
    /// How far the sampling grid had to be bent to fit, in cells.
    pub correction: f64,
    mesh: Mesh,
    /// The homography the finder patterns gave, from which the other grids
    /// are made when the fitted one turns out not to read.
    transform: Homography,
}

impl FrameOpening {
    /// The reading this opening amounts to if nothing more is read.
    fn reading(&self, total_cells: usize) -> FrameReading {
        FrameReading {
            // Not yet decoded, whatever the opening says: that is for whoever
            // reads the payload to claim.
            outcome: if self.outcome == FrameOutcome::Decoded {
                FrameOutcome::PayloadUnrecoverable
            } else {
                self.outcome
            },
            header: self.header,
            units: Vec::new(),
            units_rejected: 0,
            doubtful_cells: 0,
            total_cells,
            pixels_per_cell: self.pixels_per_cell,
            cells: Vec::new(),
            corners: self.corners,
            correction: Some(self.correction),
        }
    }

    /// The reading for a frame that was opened and then not wanted.
    #[must_use]
    pub fn declined(&self) -> FrameReading {
        let mut reading = self.reading(0);
        reading.outcome = FrameOutcome::Duplicate;
        reading
    }
}

/// First byte of a serialised reading, so that something else handed to
/// [`FrameReading::from_bytes`] is refused rather than misread.
const READING_MAGIC: u8 = 0xF7;

impl FrameReading {
    /// Packs the reading into bytes, to be carried between threads.
    ///
    /// Not part of the protocol and not stable: it exists so that a browser can
    /// read pictures on several cores and fold the results together on one,
    /// and both ends of it are always the same build.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(READING_MAGIC);
        out.push(outcome_code(self.outcome));

        match self.header {
            Some(header) => {
                out.push(1);
                out.extend_from_slice(&header.encode());
            }
            None => out.push(0),
        }

        let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX).to_le_bytes();
        out.extend_from_slice(&count(self.units_rejected));
        out.extend_from_slice(&count(self.doubtful_cells));
        out.extend_from_slice(&count(self.total_cells));
        out.extend_from_slice(&self.pixels_per_cell.unwrap_or(f64::NAN).to_le_bytes());
        out.extend_from_slice(&self.correction.unwrap_or(f64::NAN).to_le_bytes());

        match self.corners {
            Some(corners) => {
                out.push(1);
                for corner in corners {
                    out.extend_from_slice(&corner.x.to_le_bytes());
                    out.extend_from_slice(&corner.y.to_le_bytes());
                }
            }
            None => out.push(0),
        }

        out.extend_from_slice(&count(self.units.len()));
        for unit in &self.units {
            out.push(unit.kind);
            out.extend_from_slice(&count(unit.data.len()));
            out.extend_from_slice(&unit.data);
        }
        out
    }

    /// Unpacks a reading packed by [`FrameReading::to_bytes`].
    ///
    /// Returns `None` for anything that is not one.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut cursor = Cursor { bytes, at: 0 };

        if cursor.byte()? != READING_MAGIC {
            return None;
        }
        let outcome = outcome_from(cursor.byte()?)?;

        let header = if cursor.byte()? == 0 {
            None
        } else {
            Some(FrameHeader::decode(cursor.take(crate::codec::HEADER_LEN)?).ok()?)
        };

        let units_rejected = cursor.count()?;
        let doubtful_cells = cursor.count()?;
        let total_cells = cursor.count()?;
        let pixels_per_cell = Some(cursor.float()?).filter(|v| v.is_finite());
        let correction = Some(cursor.float()?).filter(|v| v.is_finite());

        let corners = if cursor.byte()? == 0 {
            None
        } else {
            let mut corners = [crate::Point::new(0.0, 0.0); 4];
            for corner in &mut corners {
                *corner = crate::Point::new(cursor.float()?, cursor.float()?);
            }
            Some(corners)
        };

        let unit_count = cursor.count()?;
        let mut units = Vec::new();
        for _ in 0..unit_count {
            let kind = cursor.byte()?;
            let length = cursor.count()?;
            units.push(PayloadUnit::new(kind, cursor.take(length)?.to_vec()));
        }

        Some(Self {
            outcome,
            header,
            units,
            units_rejected,
            doubtful_cells,
            total_cells,
            pixels_per_cell,
            cells: Vec::new(),
            corners,
            correction,
        })
    }
}

/// Reads fields out of a byte slice, refusing to run past its end.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(length)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn count(&mut self) -> Option<usize> {
        let raw: [u8; 4] = self.take(4)?.try_into().ok()?;
        usize::try_from(u32::from_le_bytes(raw)).ok()
    }

    fn float(&mut self) -> Option<f64> {
        let raw: [u8; 8] = self.take(8)?.try_into().ok()?;
        Some(f64::from_le_bytes(raw))
    }
}

const fn outcome_code(outcome: FrameOutcome) -> u8 {
    match outcome {
        FrameOutcome::NotLocated => 0,
        FrameOutcome::Straddled => 1,
        FrameOutcome::Decoded => 2,
        FrameOutcome::Duplicate => 3,
        FrameOutcome::WrongSession => 4,
        FrameOutcome::HeaderUnreadable => 5,
        FrameOutcome::PayloadUnrecoverable => 6,
    }
}

const fn outcome_from(code: u8) -> Option<FrameOutcome> {
    Some(match code {
        0 => FrameOutcome::NotLocated,
        1 => FrameOutcome::Straddled,
        2 => FrameOutcome::Decoded,
        3 => FrameOutcome::Duplicate,
        4 => FrameOutcome::WrongSession,
        5 => FrameOutcome::HeaderUnreadable,
        6 => FrameOutcome::PayloadUnrecoverable,
        _ => return None,
    })
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
        // A camera sees most codes more than once, and reading the payload is
        // most of the cost of a picture. The header says which code this is
        // before any of that is spent, so a code already taken is dropped
        // there.
        let session = self.session_id;
        let seen = &self.seen_frames;
        let reading = self.examine_unless(image, &|header: &FrameHeader| {
            session.is_some_and(|known| known != header.session_id)
                || (session == Some(header.session_id) && seen.contains(&header.frame_seq))
        });
        self.absorb(reading)
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
            let opening = open_frame(candidate, image, transform);
            if opening.header.is_some() {
                return self.absorb(self.read(image, &opening));
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
        self.examine_unless(image, &|_| false)
    }

    /// Reads a picture, stopping at the header if `unwanted` says the frame is
    /// not worth the rest.
    fn examine_unless(
        &self,
        image: &RgbImage,
        unwanted: &dyn Fn(&FrameHeader) -> bool,
    ) -> FrameReading {
        let Some(opening) = self.open(image) else {
            return FrameReading::not_located();
        };
        if opening.header.as_ref().is_some_and(unwanted) {
            return opening.declined();
        }
        self.read(image, &opening)
    }

    /// Finds the frame in a picture and reads its header, and no more.
    ///
    /// Returns `None` when there is no frame to be found. Otherwise the opening
    /// says which code the picture holds, and [`Receiver::read`] reads the rest
    /// of it if the caller decides it is wanted.
    #[must_use]
    pub fn open(&self, image: &RgbImage) -> Option<FrameOpening> {
        let detection = self.detector.detect(image).ok()?;
        let candidate = self.candidate(detection.profile)?;

        let mut opening = open_frame(candidate, image, &detection.transform);
        opening.pixels_per_cell = Some(detection.pixels_per_cell());
        opening.corners = Some(detection.corners);
        Some(opening)
    }

    /// Reads the payload of a picture already opened.
    ///
    /// `image` must be the picture the opening was made from.
    #[must_use]
    pub fn read(&self, image: &RgbImage, opening: &FrameOpening) -> FrameReading {
        match self.candidate(opening.profile) {
            Some(candidate) => read_payload(candidate, image, opening),
            None => FrameReading::not_located(),
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
            corners: reading.corners,
            correction: reading.correction,
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
        if reading.outcome == FrameOutcome::Duplicate
            && self.session_id.is_some_and(|known| known != header.session_id)
        {
            report.outcome = FrameOutcome::WrongSession;
            return report;
        }
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
fn read_headers(layout: &FrameLayout, image: &RgbImage, mesh: &Mesh) -> [Option<FrameHeader>; 2] {
    let codeword_len = layout.profile().header_codeword_len() as usize;
    let (dark, light) = timing_levels(layout, image, mesh);
    let threshold = f32::midpoint(dark, light);

    core::array::from_fn(|band| {
        let cells = layout.header_band(band);
        let mut bytes = vec![0u8; codeword_len];
        for (index, &cell) in cells.iter().take(codeword_len * 8).enumerate() {
            let luma = layout.sample_cell_core(image, mesh, cell).luma();
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
fn timing_levels(layout: &FrameLayout, image: &RgbImage, mesh: &Mesh) -> (f32, f32) {
    let grid = layout.grid();
    let mut dark = (0.0f32, 0u32);
    let mut light = (0.0f32, 0u32);

    let mut consider = |row: u32, col: u32, even: bool| {
        let luma = layout.sample_cell_core(image, mesh, row * grid + col).luma();
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

/// Side of the neighbourhoods black and white are measured in, in cells.
const LEVEL_BLOCK: u32 = 12;

/// What black and white look like in each neighbourhood of a frame.
struct Levels {
    blocks: u32,
    /// Per neighbourhood: black, and the reciprocal of white less black, in
    /// each channel.
    black: Vec<[f32; 3]>,
    scale: Vec<[f32; 3]>,
}

impl Levels {
    /// Measures them from the payload.
    ///
    /// Half of every cell is unpainted and a quarter of all cells are painted
    /// white, so in any neighbourhood of a hundred cells the darkest sub-cells
    /// are black and the brightest are white, whatever the file holds. Taken a
    /// little in from either end, so that a few stray values cannot set them.
    fn of(layout: &FrameLayout, samples: &[crate::symbol::CellSample]) -> Self {
        let grid = layout.grid();
        let blocks = grid.div_ceil(LEVEL_BLOCK);
        let count = (blocks * blocks) as usize;
        let mut values: Vec<[Vec<f32>; 3]> =
            (0..count).map(|_| [Vec::new(), Vec::new(), Vec::new()]).collect();

        for (sample, &cell) in samples.iter().zip(layout.data_cells()) {
            let (row, col) = layout.coordinates(cell);
            let slot = &mut values[((row / LEVEL_BLOCK) * blocks + col / LEVEL_BLOCK) as usize];
            for sub in &sample.sub {
                slot[0].push(sub.r);
                slot[1].push(sub.g);
                slot[2].push(sub.b);
            }
        }

        let mut black = vec![[0.0f32; 3]; count];
        let mut scale = vec![[1.0f32; 3]; count];
        let mut measured = vec![false; count];
        for (index, channels) in values.iter_mut().enumerate() {
            for (channel, list) in channels.iter_mut().enumerate() {
                if list.len() < 64 {
                    continue;
                }
                list.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
                let dark = list[list.len() * 8 / 100];
                let light = list[list.len() * 96 / 100];
                black[index][channel] = dark;
                scale[index][channel] = 1.0 / (light - dark).max(0.04);
                measured[index] = true;
            }
        }

        // A neighbourhood with no payload in it — a corner, under a finder
        // pattern — takes the levels of the nearest one that has.
        let places: Vec<(u32, u32)> =
            (0..blocks).flat_map(|row| (0..blocks).map(move |col| (row, col))).collect();
        for (index, &(row, col)) in places.iter().enumerate() {
            if measured[index] {
                continue;
            }
            let nearest = places
                .iter()
                .enumerate()
                .filter(|(other, _)| measured[*other])
                .min_by_key(|(_, (r, c))| r.abs_diff(row) + c.abs_diff(col))
                .map(|(other, _)| other);
            if let Some(other) = nearest {
                black[index] = black[other];
                scale[index] = scale[other];
            }
        }

        Self { blocks, black, scale }
    }

    /// Puts a cell on the scale of its neighbourhood: black at nought, white at
    /// one.
    fn apply(&self, layout: &FrameLayout, cell: u32, sample: &mut crate::symbol::CellSample) {
        let (row, col) = layout.coordinates(cell);

        // Between the middles of the four neighbourhoods nearest, so that the
        // scale does not step at their boundaries.
        let last = (self.blocks - 1) as f32;
        let place =
            |at: u32| ((at as f32 + 0.5) / LEVEL_BLOCK as f32 - 0.5).clamp(0.0, last.max(0.0));
        let (x, y) = (place(col), place(row));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "both were clamped to the neighbourhoods immediately above"
        )]
        let (x0, y0) = (x.floor() as u32, y.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(self.blocks - 1), (y0 + 1).min(self.blocks - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);

        let blend = |field: &[[f32; 3]], channel: usize| {
            let at = |r: u32, c: u32| field[(r * self.blocks + c) as usize][channel];
            let top = at(y0, x0) + (at(y0, x1) - at(y0, x0)) * fx;
            let bottom = at(y1, x0) + (at(y1, x1) - at(y1, x0)) * fx;
            top + (bottom - top) * fy
        };
        let black: [f32; 3] = core::array::from_fn(|channel| blend(&self.black, channel));
        let scale: [f32; 3] = core::array::from_fn(|channel| blend(&self.scale, channel));

        for sub in &mut sample.sub {
            *sub = crate::symbol::Rgbf::new(
                (sub.r - black[0]) * scale[0],
                (sub.g - black[1]) * scale[1],
                (sub.b - black[2]) * scale[2],
            );
        }
    }
}

/// Fits the per-frame classifier.
///
/// To the payload itself where the palette allows it, with the calibration ring
/// to start from. Where two inks differ in brightness alone — the white and
/// grey of the 8-colour palette — the payload cannot sort itself by hue, and
/// the ring is fitted to first and the payload after.
fn fit_classifier(
    layout: &FrameLayout,
    image: &RgbImage,
    mesh: &Mesh,
    samples: &[crate::symbol::CellSample],
    levels: &Levels,
) -> Classifier {
    let alphabet = layout.alphabet();
    let labelled: Vec<(u16, crate::symbol::CellSample)> = layout
        .calibration_cells()
        .iter()
        .enumerate()
        .map(|(index, &cell)| {
            let mut sample = layout.sample_cell_in(image, mesh, cell);
            levels.apply(layout, cell, &mut sample);
            (layout.calibration_value(index), sample)
        })
        .collect();

    if alphabet.colours.len() <= 4 {
        return Classifier::from_payload(alphabet, samples, &labelled);
    }

    let from_ring = Classifier::fit(alphabet, &labelled);
    let (subset, first): (Vec<crate::symbol::CellSample>, Vec<crate::symbol::Classification>) =
        samples
            .iter()
            .step_by(REFIT_STRIDE)
            .map(|sample| (*sample, from_ring.classify(sample)))
            .unzip();
    from_ring.refit(&subset, &first)
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

/// Reads a located frame as far as its header.
fn open_frame(candidate: &Candidate, image: &RgbImage, transform: &Homography) -> FrameOpening {
    let layout = &candidate.layout;

    // The grid the finder patterns imply, bent to fit what the timing ring
    // says the lens did to it.
    let mesh = Mesh::fit(layout, image, transform);

    let mut opening = FrameOpening {
        outcome: FrameOutcome::HeaderUnreadable,
        header: None,
        profile: candidate.id,
        correction: mesh.largest_correction(),
        mesh,
        transform: *transform,
        pixels_per_cell: None,
        corners: None,
    };

    // Through the grid fitted to the payload, and if that reads nothing,
    // through the others. A fit is a measurement and a measurement can be
    // wrong; the header is checked twice over, so whichever grid reads it is
    // right about it.
    let mut headers = read_headers(layout, image, &opening.mesh);
    if headers[0].is_none() && headers[1].is_none() {
        for other in other_grids(layout, image, transform) {
            headers = read_headers(layout, image, &other);
            if headers[0].is_some() || headers[1].is_some() {
                opening.correction = other.largest_correction();
                opening.mesh = other;
                break;
            }
        }
    }
    let Some(header) = headers[0].or(headers[1]) else {
        return opening;
    };

    // Two readable copies that name different frames mean the shutter caught a
    // screen refresh. The payload between them is a splice; decoding it would
    // burn the frame's error correction on damage that is not random.
    if let (Some(top), Some(bottom)) = (headers[0], headers[1])
        && top.frame_seq != bottom.frame_seq
    {
        opening.header = Some(header);
        opening.outcome = FrameOutcome::Straddled;
        return opening;
    }
    // Two profiles may share a grid and differ in what they paint in it. The
    // header is written the same way for both, and says which this is.
    if header.profile.profile().grid == layout.grid() {
        opening.profile = header.profile;
    } else {
        // A header that names another geometry means this candidate's
        // happened to produce a readable header for someone else's frame.
        // Believing it would decode the payload against the wrong cell map.
        return opening;
    }

    opening.header = Some(header);
    opening.outcome = FrameOutcome::Decoded;
    opening
}

/// Reads the payload of a frame whose header has been read.
fn read_payload(candidate: &Candidate, image: &RgbImage, opening: &FrameOpening) -> FrameReading {
    let reading = read_payload_through(candidate, image, opening, &opening.mesh);
    if reading.outcome != FrameOutcome::PayloadUnrecoverable {
        return reading;
    }

    // The grid that read the header did not read the payload. Each of the
    // others costs another reading and is sometimes the better, and parity
    // says which without being told.
    for other in other_grids(&candidate.layout, image, &opening.transform) {
        let again = read_payload_through(candidate, image, opening, &other);
        if again.outcome == FrameOutcome::Decoded {
            return again;
        }
    }
    reading
}

/// The grids to fall back on, in the order they are worth trying: the one
/// fitted to the timing ring alone, then the homography as it stands.
fn other_grids(
    layout: &FrameLayout,
    image: &RgbImage,
    transform: &Homography,
) -> impl Iterator<Item = Mesh> {
    (0..2).map(move |which| match which {
        0 => Mesh::fit_to_ring(layout, image, transform),
        _ => Mesh::from_homography(layout, transform),
    })
}

/// Reads the payload through one grid.
fn read_payload_through(
    candidate: &Candidate,
    image: &RgbImage,
    opening: &FrameOpening,
    mesh: &Mesh,
) -> FrameReading {
    let layout = &candidate.layout;
    let codec = &candidate.payload_codec;

    let mut reading = opening.reading(layout.data_cells().len());
    let Some(header) = opening.header else {
        return reading;
    };
    if opening.outcome != FrameOutcome::Decoded {
        return reading;
    }

    let bits = layout.profile().bits_per_cell();
    let mut samples: Vec<crate::symbol::CellSample> =
        layout.data_cells().iter().map(|&cell| layout.sample_cell_in(image, mesh, cell)).collect();

    // What black and white look like is not one thing across a picture. A
    // screen seen from a little above is washed out along its bottom edge, a
    // lamp lifts one corner, and a lens darkens all four. Each cell is
    // measured against the black and the white of its own neighbourhood.
    let levels = Levels::of(layout, &samples);
    for (sample, &cell) in samples.iter_mut().zip(layout.data_cells()) {
        levels.apply(layout, cell, sample);
    }

    // The classifier is fitted to this frame and to no other: exposure and
    // white balance move while the camera records, so references from any
    // other frame are already stale.
    let classifier = fit_classifier(layout, image, mesh, &samples, &levels);

    let mut cells = Vec::with_capacity(samples.len());
    let mut doubtful = Vec::new();
    for (index, sample) in samples.iter().enumerate() {
        let call = classifier.classify(sample);
        cells.push(call.value);
        if call.confidence < ERASURE_CONFIDENCE {
            doubtful.push(index);
        }
    }
    reading.doubtful_cells = doubtful.len();

    let mut raw = cells_to_bytes(&cells, bits, codec.raw_len());
    whiten(&mut raw);
    let erasures = erasure_positions(&doubtful, bits);
    reading.cells = cells;

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

    #[test]
    fn the_whitening_sequence_is_the_one_the_specification_gives() {
        // SPEC.md §5.2.4. Whitening nothing gives the sequence itself.
        let mut bytes = [0u8; 8];
        whiten(&mut bytes);
        assert_eq!(bytes, [0x02, 0xFC, 0x36, 0xD8, 0x64, 0x66, 0x97, 0xE0]);
    }
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
    fn a_code_already_taken_is_dropped_at_its_header() {
        // A camera sees most codes more than once, and reading a payload is
        // most of what a picture costs. The second picture of a code has to
        // stop at the header, which is only visible from outside as a reading
        // that never got as far as cells.
        let file = incompressible(6_000);
        let mut tx = Transmitter::new("twice.bin", &file, ProfileId::P1Conservative, 9).unwrap();
        let mut rx = Receiver::new();

        let frame = tx.next_frame(8).unwrap();
        let first = rx.accept_image(&frame.image);
        assert_eq!(first.outcome, FrameOutcome::Decoded);
        assert!(first.total_cells > 0);

        let again = rx.accept_image(&frame.image);
        assert_eq!(again.outcome, FrameOutcome::Duplicate);
        assert_eq!(again.total_cells, 0, "the payload of a repeated code was read");
        assert!(again.corners.is_some(), "a repeated code still says where it is");
    }

    #[test]
    fn a_picture_can_be_opened_in_one_place_and_absorbed_in_another() {
        // What a browser does with several cores: readers that keep nothing
        // open and read the pictures, and one receiver that never sees a
        // picture folds in what they read, carried between them as bytes.
        let file = incompressible(20_000);
        let mut tx = Transmitter::new("apart.bin", &file, ProfileId::P1Conservative, 3).unwrap();
        let reader = Receiver::new();
        let mut collector = Receiver::new();

        let mut frames = 0usize;
        while !collector.is_complete() && frames < 100 {
            let frame = tx.next_frame(8).unwrap();

            let opening = reader.open(&frame.image).expect("located");
            assert_eq!(opening.outcome, FrameOutcome::Decoded);
            assert_eq!(opening.header.map(|h| h.frame_seq), Some(u32::try_from(frames).unwrap()));

            let reading = reader.read(&frame.image, &opening);
            let carried = FrameReading::from_bytes(&reading.to_bytes()).expect("unpacked");
            assert_eq!(carried.outcome, reading.outcome);
            assert_eq!(carried.header, reading.header);
            assert_eq!(carried.units, reading.units);
            assert_eq!(carried.corners, reading.corners);

            assert_eq!(collector.absorb(carried).outcome, FrameOutcome::Decoded);
            frames += 1;
        }

        assert!(collector.is_complete());
        assert_eq!(collector.finish().unwrap().bytes, file);
    }

    #[test]
    fn a_reading_cut_short_is_refused() {
        // The bytes cross a boundary this crate does not control.
        let file = incompressible(3_000);
        let mut tx = Transmitter::new("cut.bin", &file, ProfileId::P1Conservative, 3).unwrap();
        let frame = tx.next_frame(8).unwrap();
        let bytes = Receiver::new().examine(&frame.image).to_bytes();

        assert!(FrameReading::from_bytes(&bytes).is_some());
        for length in [0, 1, 2, 10, 30, bytes.len() / 2, bytes.len() - 1] {
            assert!(
                FrameReading::from_bytes(&bytes[..length]).is_none(),
                "{length} of {} bytes were accepted",
                bytes.len()
            );
        }
    }

    #[test]
    fn a_hostile_file_name_never_reaches_a_frame() {
        assert!(Transmitter::new("../escape", b"data", ProfileId::P2Standard, 1).is_err());
    }
}
