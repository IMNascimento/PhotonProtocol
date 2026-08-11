//! The link layer: frame headers, payload units, and the checksums over both.
//!
//! A frame is self-describing (`SPEC.md` §5). It says which profile drew it,
//! which transmission it belongs to, and what it carries, so a decoder that
//! starts filming halfway through can use the very first frame it catches.
//!
//! Two decisions here are worth stating, because both trade capacity for
//! something the channel cannot otherwise provide.
//!
//! The header is carried twice, at opposite edges, in solid black-or-white
//! cells rather than in the profile's alphabet. It has to be readable before the
//! profile is known — it is what declares the profile — and it is the one part
//! of a frame whose loss costs everything else.
//!
//! Every unit carries its own CRC. A frame whose error correction only partly
//! succeeded then yields the symbols that survived instead of being discarded
//! whole, which matters on a channel where a lost frame can never be asked for
//! again.

use crate::error::{Error, Result};
use crate::fec::{FecError, RsCodec};
use crate::profile::{HEADER_DATA_LEN, ProfileId};
use crate::{FRAME_MAGIC, PROTOCOL_VERSION};

/// Length of the frame header before error correction (`SPEC.md` §5.1).
pub const HEADER_LEN: usize = HEADER_DATA_LEN as usize;

/// Bytes of framing each payload unit costs: type, length and CRC.
pub const UNIT_OVERHEAD: usize = 7;

// --- checksums ---------------------------------------------------------------

/// CRC-16/CCITT-FALSE: polynomial `0x1021`, initial value `0xFFFF`, no
/// reflection, no final XOR (`SPEC.md` §5.1).
#[must_use]
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// CRC-32C (Castagnoli): reflected polynomial `0x82F6_3B78`, initial and final
/// value `0xFFFF_FFFF`, input and output reflected (`SPEC.md` §5.3).
#[must_use]
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
        }
    }
    !crc
}

// --- frame header ------------------------------------------------------------

/// Flag bits of the frame header (`SPEC.md` §5.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameFlags(u8);

impl FrameFlags {
    /// The frame carries a manifest unit.
    pub const HAS_MANIFEST: Self = Self(0b0000_0001);
    /// The frame is the first of an emission pass.
    pub const LOOP_RESTART: Self = Self(0b0000_0010);

    /// No flags set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// The raw flag byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Flags from a raw byte, discarding the reserved bits a future version may
    /// define. `SPEC.md` §5.1.1 requires them to be ignored, not rejected: that
    /// is what lets a later revision add one without breaking this decoder.
    #[must_use]
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & 0b0000_0011)
    }

    /// Whether every flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The header every frame carries, twice (`SPEC.md` §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Which profile drew this frame.
    pub profile: ProfileId,
    /// Flags (`SPEC.md` §5.1.1).
    pub flags: FrameFlags,
    /// Number of payload units in the frame.
    pub unit_count: u8,
    /// Identifies the transmission, so two recordings cannot be mixed.
    pub session_id: u32,
    /// Index of this frame in the emission sequence.
    pub frame_seq: u32,
    /// Bytes of the payload that carry units.
    pub payload_len: u16,
}

impl FrameHeader {
    /// Serialises the header, CRC included.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..4].copy_from_slice(&FRAME_MAGIC);
        out[4] = PROTOCOL_VERSION;
        out[5] = self.profile.as_u8();
        out[6] = self.flags.bits();
        out[7] = self.unit_count;
        out[8..12].copy_from_slice(&self.session_id.to_le_bytes());
        out[12..16].copy_from_slice(&self.frame_seq.to_le_bytes());
        out[16..18].copy_from_slice(&self.payload_len.to_le_bytes());
        let crc = crc16_ccitt(&out[0..18]);
        out[18..20].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Parses a header.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for a bad magic, length or checksum,
    /// [`Error::VersionUnsupported`] for a protocol version this build does not
    /// implement, and [`Error::ProfileUnsupported`] for an unknown profile.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let Some(bytes) = bytes.get(..HEADER_LEN) else {
            return Err(Error::Malformed { context: "frame header", detail: "too short" });
        };
        if bytes[0..4] != FRAME_MAGIC {
            return Err(Error::Malformed { context: "frame header", detail: "bad magic" });
        }

        // The checksum is verified before anything is believed, so a corrupted
        // length cannot steer the parser.
        let stored = u16::from_le_bytes([bytes[18], bytes[19]]);
        if stored != crc16_ccitt(&bytes[0..18]) {
            return Err(Error::Malformed { context: "frame header", detail: "checksum mismatch" });
        }

        if bytes[4] != PROTOCOL_VERSION {
            return Err(Error::VersionUnsupported { found: bytes[4], supported: PROTOCOL_VERSION });
        }

        Ok(Self {
            profile: ProfileId::from_u8(bytes[5])?,
            flags: FrameFlags::from_bits_truncate(bytes[6]),
            unit_count: bytes[7],
            session_id: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            frame_seq: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            payload_len: u16::from_le_bytes([bytes[16], bytes[17]]),
        })
    }

    /// Error-corrected header, expanded to fill a band (`SPEC.md` §5.1.2).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] if the requested codeword length cannot hold
    /// the header, which only a malformed profile table could cause.
    pub fn encode_codeword(&self, codeword_len: usize) -> Result<Vec<u8>> {
        if codeword_len <= HEADER_LEN {
            return Err(Error::Malformed {
                context: "frame header",
                detail: "codeword shorter than the header",
            });
        }
        let codec = RsCodec::new(codeword_len - HEADER_LEN);
        codec.encode(&self.encode()).map_err(|_| Error::Malformed {
            context: "frame header",
            detail: "codeword exceeds the field",
        })
    }

    /// Repairs and parses one copy of the header.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HeaderUnrecoverable`] when error correction fails, and
    /// whatever [`FrameHeader::decode`] returns otherwise.
    pub fn decode_codeword(codeword: &[u8], erasures: &[usize]) -> Result<Self> {
        if codeword.len() <= HEADER_LEN {
            return Err(Error::HeaderUnrecoverable);
        }
        let codec = RsCodec::new(codeword.len() - HEADER_LEN);
        let repaired = codec
            .decode(codeword, erasures)
            .or_else(|_| codec.decode(codeword, &[]))
            .map_err(|_: FecError| Error::HeaderUnrecoverable)?;
        Self::decode(&repaired)
    }
}

// --- payload units -----------------------------------------------------------

/// Unit type codes (`SPEC.md` §5.3.1).
pub mod unit_type {
    /// Filler; terminates parsing of the frame.
    pub const PADDING: u8 = 0x00;
    /// A session manifest.
    pub const MANIFEST: u8 = 0x01;
    /// A RaptorQ encoding symbol, FEC Payload ID included.
    pub const RQ_SYMBOL: u8 = 0x02;
}

/// One unit of a frame payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadUnit {
    /// Type code. Unknown codes are preserved rather than dropped, so a caller
    /// can tell "I did not understand this" from "this was not there".
    pub kind: u8,
    /// Unit body.
    pub data: Vec<u8>,
}

impl PayloadUnit {
    /// A unit of the given type.
    #[must_use]
    pub fn new(kind: u8, data: Vec<u8>) -> Self {
        Self { kind, data }
    }

    /// Bytes this unit occupies in a payload, framing included.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        UNIT_OVERHEAD + self.data.len()
    }

    fn write_into(&self, out: &mut Vec<u8>) -> Result<()> {
        let length = u16::try_from(self.data.len()).map_err(|_| Error::Malformed {
            context: "payload unit",
            detail: "body exceeds 65535 bytes",
        })?;

        let start = out.len();
        out.push(self.kind);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&self.data);
        let crc = crc32c(&out[start..]);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(())
    }
}

/// Packs units into a payload byte stream.
///
/// # Errors
///
/// Returns [`Error::Malformed`] if the units do not fit the frame's capacity, or
/// if one is longer than its length field can express.
pub fn encode_units(units: &[PayloadUnit], capacity: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(capacity);
    for unit in units {
        unit.write_into(&mut out)?;
    }
    if out.len() > capacity {
        return Err(Error::Malformed {
            context: "frame payload",
            detail: "units exceed the frame capacity",
        });
    }
    Ok(out)
}

/// Parses as many units as survive their checksums.
///
/// Units whose CRC fails are dropped individually; a corrupted length field
/// stops parsing but keeps everything already accepted (`SPEC.md` §5.3). The
/// second return value counts the units that were rejected, which is what the
/// decoder reports to the user as frame quality.
#[must_use]
pub fn parse_units(payload: &[u8], payload_len: usize) -> (Vec<PayloadUnit>, usize) {
    let limit = payload_len.min(payload.len());
    let mut units = Vec::new();
    let mut rejected = 0usize;
    let mut offset = 0usize;

    while offset + UNIT_OVERHEAD <= limit {
        let kind = payload[offset];
        if kind == unit_type::PADDING {
            break;
        }

        let length = usize::from(u16::from_le_bytes([payload[offset + 1], payload[offset + 2]]));
        let end = offset + 3 + length;
        let crc_end = end + 4;
        if crc_end > limit {
            // A length that runs past the frame is itself corruption. Stop, but
            // keep what already verified: on a simplex channel a salvaged symbol
            // is one that never has to be filmed again.
            rejected += 1;
            break;
        }

        let stored = u32::from_le_bytes([
            payload[end],
            payload[end + 1],
            payload[end + 2],
            payload[end + 3],
        ]);
        if stored == crc32c(&payload[offset..end]) {
            units.push(PayloadUnit::new(kind, payload[offset + 3..end].to_vec()));
        } else {
            rejected += 1;
        }

        offset = crc_end;
    }

    (units, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> FrameHeader {
        FrameHeader {
            profile: ProfileId::P2Standard,
            flags: FrameFlags::HAS_MANIFEST,
            unit_count: 7,
            session_id: 0x0BAD_C0DE,
            frame_seq: 1,
            payload_len: 7047,
        }
    }

    #[test]
    fn crc_check_values_match_the_specification() {
        // SPEC.md 10.1. These are the published check values for both
        // algorithms; a wrong polynomial, reflection or initial value fails here
        // rather than in somebody else's decoder.
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn the_frame_header_matches_the_published_test_vector() {
        // SPEC.md 10.2, byte for byte.
        let encoded = sample_header().encode();
        let expected: [u8; 20] = [
            0x50, 0x48, 0x54, 0x4E, 0x01, 0x02, 0x01, 0x07, 0xDE, 0xC0, 0xAD, 0x0B, 0x01, 0x00,
            0x00, 0x00, 0x87, 0x1B, 0x17, 0xB8,
        ];
        assert_eq!(encoded, expected, "encoded header does not match SPEC.md 10.2");
        assert_eq!(crc16_ccitt(&expected[0..18]), 0xB817);
    }

    #[test]
    fn headers_round_trip() {
        let header = sample_header();
        assert_eq!(FrameHeader::decode(&header.encode()).unwrap(), header);
    }

    #[test]
    fn a_corrupted_header_is_rejected_rather_than_believed() {
        // Every byte matters: a header that parses wrongly sends the decoder
        // into the wrong profile's geometry, which yields a full frame of
        // confident nonsense.
        let encoded = sample_header().encode();
        for index in 0..HEADER_LEN {
            let mut damaged = encoded;
            damaged[index] ^= 0x40;
            assert!(
                FrameHeader::decode(&damaged).is_err(),
                "corruption at byte {index} went undetected"
            );
        }
    }

    #[test]
    fn an_unknown_profile_or_version_is_named_not_guessed() {
        let mut encoded = sample_header().encode();
        encoded[5] = 0x7F;
        let crc = crc16_ccitt(&encoded[0..18]);
        encoded[18..20].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(FrameHeader::decode(&encoded), Err(Error::ProfileUnsupported { found: 0x7F }));

        let mut encoded = sample_header().encode();
        encoded[4] = 0x02;
        let crc = crc16_ccitt(&encoded[0..18]);
        encoded[18..20].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            FrameHeader::decode(&encoded),
            Err(Error::VersionUnsupported { found: 0x02, supported: PROTOCOL_VERSION })
        );
    }

    #[test]
    fn reserved_flag_bits_are_ignored_not_rejected() {
        // This is what lets a later revision of the format add a flag without
        // every existing decoder refusing the frame.
        let flags = FrameFlags::from_bits_truncate(0b1111_1111);
        assert!(flags.contains(FrameFlags::HAS_MANIFEST));
        assert!(flags.contains(FrameFlags::LOOP_RESTART));
        assert_eq!(flags.bits(), 0b0000_0011);
    }

    #[test]
    fn the_header_codeword_survives_its_correction_radius() {
        let header = sample_header();
        let codeword = header.encode_codeword(42).unwrap();
        assert_eq!(codeword.len(), 42);

        let correctable = (42 - HEADER_LEN) / 2;
        let mut damaged = codeword.clone();
        for i in 0..correctable {
            damaged[i * 3 % 42] ^= 0xFF;
        }
        assert_eq!(FrameHeader::decode_codeword(&damaged, &[]).unwrap(), header);
    }

    #[test]
    fn header_recovery_falls_back_when_the_erasures_are_wrong() {
        // Erasures come from a confidence heuristic, and a heuristic can point
        // at healthy bytes. Losing the frame because the hint was bad would be
        // worse than ignoring the hint.
        let header = sample_header();
        let codeword = header.encode_codeword(42).unwrap();
        let misleading: Vec<usize> = (0..22).collect();
        assert_eq!(FrameHeader::decode_codeword(&codeword, &misleading).unwrap(), header);
    }

    #[test]
    fn units_round_trip() {
        let units = vec![
            PayloadUnit::new(unit_type::MANIFEST, vec![1, 2, 3]),
            PayloadUnit::new(unit_type::RQ_SYMBOL, vec![9; 64]),
        ];
        let payload = encode_units(&units, 512).unwrap();
        let (parsed, rejected) = parse_units(&payload, payload.len());
        assert_eq!(parsed, units);
        assert_eq!(rejected, 0);
    }

    #[test]
    fn a_damaged_unit_is_dropped_and_the_rest_survive() {
        // The reason each unit carries its own checksum: on a simplex channel a
        // salvaged symbol is one nobody has to film again.
        let units: Vec<PayloadUnit> =
            (0..5).map(|i| PayloadUnit::new(unit_type::RQ_SYMBOL, vec![i; 32])).collect();
        let mut payload = encode_units(&units, 1024).unwrap();

        let second = units[0].encoded_len() + 4;
        payload[second] ^= 0xFF;

        let (parsed, rejected) = parse_units(&payload, payload.len());
        assert_eq!(rejected, 1);
        assert_eq!(parsed.len(), 4);
        assert!(parsed.contains(&units[0]), "units before the damage must survive");
        assert!(parsed.contains(&units[4]), "units after the damage must survive");
        assert!(!parsed.contains(&units[1]), "the damaged unit must not be accepted");
    }

    #[test]
    fn a_length_running_past_the_frame_stops_parsing_without_panicking() {
        // Hostile or corrupted input must not be able to steer an index.
        let units = vec![PayloadUnit::new(unit_type::RQ_SYMBOL, vec![7; 16])];
        let mut payload = encode_units(&units, 256).unwrap();
        payload[1] = 0xFF;
        payload[2] = 0xFF;

        let (parsed, rejected) = parse_units(&payload, payload.len());
        assert!(parsed.is_empty());
        assert_eq!(rejected, 1);
    }

    #[test]
    fn padding_terminates_parsing() {
        let units = vec![PayloadUnit::new(unit_type::MANIFEST, vec![4, 5, 6])];
        let mut payload = encode_units(&units, 128).unwrap();
        let used = payload.len();
        payload.resize(128, 0);

        let (parsed, _) = parse_units(&payload, 128);
        assert_eq!(parsed.len(), 1);

        let (parsed, _) = parse_units(&payload, used);
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn unknown_unit_types_are_preserved_for_the_caller_to_skip() {
        // The format's extension point. A decoder must carry an unrecognised
        // unit past the parser so that "did not understand" stays distinct from
        // "was not there".
        let units = vec![
            PayloadUnit::new(0x7F, vec![1, 2]),
            PayloadUnit::new(unit_type::RQ_SYMBOL, vec![3, 4]),
        ];
        let payload = encode_units(&units, 128).unwrap();
        let (parsed, rejected) = parse_units(&payload, payload.len());
        assert_eq!(rejected, 0);
        assert_eq!(parsed, units);
    }

    #[test]
    fn units_that_overflow_the_frame_are_refused() {
        let units = vec![PayloadUnit::new(unit_type::RQ_SYMBOL, vec![0; 100])];
        assert!(encode_units(&units, 50).is_err());
    }
}
