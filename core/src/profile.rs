//! Physical-layer profiles and the frame geometry derived from them.
//!
//! A profile fixes every physical parameter of a frame: how many cells a side,
//! how many shapes and colours make up the alphabet, and how much of the frame
//! is spent on parity. Everything else — where the header bands sit, how many
//! cells carry data, how the Reed-Solomon codewords are cut, how many payload
//! bytes fit — follows from those four numbers by the formulas of `SPEC.md`
//! §4.2.7, §4.4 and §5.2.2.
//!
//! Deriving the geometry rather than tabulating it is deliberate. The tables in
//! the specification are what a second implementation will copy, and a table
//! that is maintained by hand drifts from the formulas beside it. Here the
//! formulas are the only source, and the tests assert that they reproduce the
//! published tables exactly.
//!
//! The parity rates run opposite to intuition: the conservative profile spends
//! *more* of its smaller frame on parity, not less. A profile is one point on a
//! robustness curve rather than a density dial, and [`ProfileId::P1Conservative`]
//! exists to survive conditions — a shaky hand, a dim screen, a cheap sensor —
//! in which [`ProfileId::P3Dense`] would not decode at all.

use crate::error::Error;

/// Cells occupied by the four finder boxes: four boxes of 8x8 (`SPEC.md`
/// §4.2.1).
const FINDER_CELLS: u32 = 4 * 8 * 8;

/// Cells occupied by the orientation tag: one 3x3 block (`SPEC.md` §4.2.2).
const ORIENTATION_CELLS: u32 = 3 * 3;

/// Cells occupied by the central alignment module, separator included: 7x7
/// (`SPEC.md` §4.2.5).
const ALIGNMENT_CELLS: u32 = 7 * 7;

/// Ring-shaped regions spanning a full edge each: the timing ring and the
/// calibration ring, two edges apiece per axis (`SPEC.md` §4.2.3, §4.2.4).
const RING_ROWS: u32 = 8;

/// Length of the frame header before error correction (`SPEC.md` §5.1).
pub const HEADER_DATA_LEN: u32 = 20;

/// Shortest header codeword the format allows, which is the header doubled: 20
/// data bytes and 20 parity bytes. It sets the minimum height of a header band.
pub const MIN_HEADER_CODEWORD_LEN: u32 = 2 * HEADER_DATA_LEN;

/// Block length of the payload Reed-Solomon code over GF(2^8).
pub const RS_BLOCK_LEN: u32 = 255;

/// Identifier carried in the `profile_id` field of every frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ProfileId {
    /// 96x96 cells, 4 bits per cell, 31.4% parity. The profile that still works
    /// when the recording is poor.
    P1Conservative = 0x01,
    /// 128x128 cells, 5 bits per cell, 22.0% parity. The default; every emitter
    /// must implement it.
    P2Standard = 0x02,
    /// 160x160 cells, 6 bits per cell, 12.5% parity. Opt-in density for a steady
    /// hand and a good camera.
    P3Dense = 0x03,
}

impl ProfileId {
    /// The byte written to the frame header.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parses a `profile_id` byte.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ProfileUnsupported`] for an unknown identifier. The
    /// format forbids guessing at the geometry of a profile one does not know.
    pub const fn from_u8(byte: u8) -> Result<Self, Error> {
        match byte {
            0x01 => Ok(Self::P1Conservative),
            0x02 => Ok(Self::P2Standard),
            0x03 => Ok(Self::P3Dense),
            found => Err(Error::ProfileUnsupported { found }),
        }
    }

    /// The parameters behind this identifier.
    #[must_use]
    pub const fn profile(self) -> &'static Profile {
        match self {
            Self::P1Conservative => &PROFILES[0],
            Self::P2Standard => &PROFILES[1],
            Self::P3Dense => &PROFILES[2],
        }
    }
}

/// How a frame's raw byte capacity is cut into Reed-Solomon codewords
/// (`SPEC.md` §5.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RsPartition {
    /// Number of full `RS(255, k)` codewords.
    pub full_codewords: u32,
    /// The trailing shortened codeword as `(n, k)`, when the remainder is long
    /// enough to carry at least one data byte on top of the parity.
    pub shortened: Option<(u32, u32)>,
    /// Trailing raw bytes too few to form a shortened codeword. They are
    /// transmitted as zero and ignored.
    pub unused_bytes: u32,
}

impl RsPartition {
    /// Total number of codewords, which is also the interleaving stride of
    /// `SPEC.md` §5.2.3.
    #[must_use]
    pub const fn codeword_count(&self) -> u32 {
        self.full_codewords + if self.shortened.is_some() { 1 } else { 0 }
    }
}

/// A physical-layer profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// Identifier carried in the frame header.
    pub id: ProfileId,
    /// Human-readable name, as used in the specification and the user interface.
    pub name: &'static str,
    /// Side of the code area in cells, written `G` in the specification.
    pub grid: u32,
    /// Size of the shape alphabet (`SPEC.md` §4.3.1).
    pub num_shapes: u32,
    /// Size of the colour palette (`SPEC.md` §4.3.2).
    pub num_colours: u32,
    /// Data length `k` of the payload code `RS(255, k)`.
    pub rs_data_len: u32,
}

/// Every profile defined by `SPEC.md` §8, in identifier order.
pub static PROFILES: [Profile; 3] = [
    Profile {
        id: ProfileId::P1Conservative,
        name: "P1-conservative",
        grid: 96,
        num_shapes: 4,
        num_colours: 4,
        rs_data_len: 175,
    },
    Profile {
        id: ProfileId::P2Standard,
        name: "P2-standard",
        grid: 128,
        num_shapes: 8,
        num_colours: 4,
        rs_data_len: 199,
    },
    Profile {
        id: ProfileId::P3Dense,
        name: "P3-dense",
        grid: 160,
        num_shapes: 8,
        num_colours: 8,
        rs_data_len: 223,
    },
];

impl Profile {
    /// Bits carried by one data cell: `log2(shapes) + log2(colours)`.
    #[must_use]
    pub const fn bits_per_cell(&self) -> u32 {
        self.num_shapes.ilog2() + self.num_colours.ilog2()
    }

    /// Smallest cell, in device pixels, this profile can be painted at and
    /// still read back (`SPEC.md` §4.1).
    ///
    /// It is a property of the shape alphabet rather than of the grid. The
    /// 8-shape alphabet uses every one of the 16 sub-cells independently, so
    /// each needs pixels of its own: at 8 the finest of them gets two, at 7 it
    /// gets one, and one pixel averages away in any resampling. The 4-shape
    /// alphabet is four half-planes, whose finest feature is half a cell rather
    /// than a quarter, and it survives proportionally smaller.
    ///
    /// Measured, not reasoned about: on a pristine channel with no camera in
    /// the path, `simulate::eight_pixels_per_cell_is_a_cliff_and_not_a_slope`
    /// puts the 8-shape profiles at 5.9% and 2.5% of cells wrong one pixel
    /// below their floor and at exactly zero on it.
    #[must_use]
    pub const fn min_cell_px(&self) -> u32 {
        if self.num_shapes > 4 { 8 } else { 6 }
    }

    /// Usable cells along one edge of a ring, and along one row of a header
    /// band: `G - 16`, the grid less the eight cells consumed by a finder box at
    /// each end.
    #[must_use]
    pub const fn band_width(&self) -> u32 {
        self.grid - 16
    }

    /// Height of a header band in rows (`SPEC.md` §4.2.6).
    #[must_use]
    pub const fn header_rows(&self) -> u32 {
        let bits = MIN_HEADER_CODEWORD_LEN * 8;
        let width = self.band_width();
        bits.div_ceil(width)
    }

    /// Length in bytes of the header codeword `RS(nh, 20)`, which expands to
    /// fill the band it is written into.
    #[must_use]
    pub const fn header_codeword_len(&self) -> u32 {
        self.header_rows() * self.band_width() / 8
    }

    /// Byte errors the header code corrects, or half as many again in erasures.
    #[must_use]
    pub const fn header_correctable_errors(&self) -> u32 {
        (self.header_codeword_len() - HEADER_DATA_LEN) / 2
    }

    /// Cells reserved for structure rather than payload: finder boxes,
    /// orientation tag, alignment module, both rings and both header bands
    /// (`SPEC.md` §4.2.7).
    #[must_use]
    pub const fn reserved_cells(&self) -> u32 {
        let fixed = FINDER_CELLS + ORIENTATION_CELLS + ALIGNMENT_CELLS;
        let banded = RING_ROWS + 2 * self.header_rows();
        fixed + banded * self.band_width()
    }

    /// Cells that carry payload.
    #[must_use]
    pub const fn data_cells(&self) -> u32 {
        self.grid * self.grid - self.reserved_cells()
    }

    /// Whole bytes recoverable from the data region before error correction
    /// (`SPEC.md` §4.4).
    #[must_use]
    pub const fn raw_bytes(&self) -> u32 {
        self.data_cells() * self.bits_per_cell() / 8
    }

    /// Parity length `p = 255 - k` of the payload code.
    #[must_use]
    pub const fn rs_parity_len(&self) -> u32 {
        RS_BLOCK_LEN - self.rs_data_len
    }

    /// How the raw bytes are cut into codewords (`SPEC.md` §5.2.2).
    #[must_use]
    pub const fn rs_partition(&self) -> RsPartition {
        let raw = self.raw_bytes();
        let parity = self.rs_parity_len();
        let full = raw / RS_BLOCK_LEN;
        let remainder = raw % RS_BLOCK_LEN;

        // A shortened codeword needs room for the same parity plus at least one
        // data byte; anything less would spend cells on parity protecting
        // nothing.
        if remainder > parity {
            RsPartition {
                full_codewords: full,
                shortened: Some((remainder, remainder - parity)),
                unused_bytes: 0,
            }
        } else {
            RsPartition { full_codewords: full, shortened: None, unused_bytes: remainder }
        }
    }

    /// Payload bytes one frame can carry, error correction already deducted.
    #[must_use]
    pub const fn payload_capacity(&self) -> u32 {
        let partition = self.rs_partition();
        let shortened_data = match partition.shortened {
            Some((_, k)) => k,
            None => 0,
        };
        partition.full_codewords * self.rs_data_len + shortened_data
    }

    /// Parity fraction of the payload Reed-Solomon code, `p / 255`.
    ///
    /// This is the number tabulated in `SPEC.md` §8 and the one that describes
    /// the code's strength. It is a property of the code alone.
    #[must_use]
    pub fn rs_parity_rate(&self) -> f64 {
        f64::from(self.rs_parity_len()) / f64::from(RS_BLOCK_LEN)
    }

    /// Share of the frame's raw bytes that do not reach the transport layer.
    ///
    /// Slightly above [`Profile::rs_parity_rate`] because the shortened trailing
    /// codeword carries the full parity over fewer data bytes. This is the
    /// figure that actually costs throughput, so it is what the bench command
    /// line reports.
    #[must_use]
    pub fn link_overhead(&self) -> f64 {
        let raw = f64::from(self.raw_bytes());
        let payload = f64::from(self.payload_capacity());
        (raw - payload) / raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables published in `SPEC.md` §8. If a formula in this module ever
    /// disagrees with them, one of the two is wrong and the specification wins.
    struct Expected {
        id: ProfileId,
        bits_per_cell: u32,
        band_width: u32,
        header_rows: u32,
        header_codeword_len: u32,
        reserved_cells: u32,
        data_cells: u32,
        raw_bytes: u32,
        full_codewords: u32,
        shortened: Option<(u32, u32)>,
        payload_capacity: u32,
    }

    const SPEC_TABLE: [Expected; 3] = [
        Expected {
            id: ProfileId::P1Conservative,
            bits_per_cell: 4,
            band_width: 80,
            header_rows: 4,
            header_codeword_len: 40,
            reserved_cells: 1594,
            data_cells: 7622,
            raw_bytes: 3811,
            full_codewords: 14,
            shortened: Some((241, 161)),
            payload_capacity: 2611,
        },
        Expected {
            id: ProfileId::P2Standard,
            bits_per_cell: 5,
            band_width: 112,
            header_rows: 3,
            header_codeword_len: 42,
            reserved_cells: 1882,
            data_cells: 14502,
            raw_bytes: 9063,
            full_codewords: 35,
            shortened: Some((138, 82)),
            payload_capacity: 7047,
        },
        Expected {
            id: ProfileId::P3Dense,
            bits_per_cell: 6,
            band_width: 144,
            header_rows: 3,
            header_codeword_len: 54,
            reserved_cells: 2330,
            data_cells: 23270,
            raw_bytes: 17452,
            full_codewords: 68,
            shortened: Some((112, 80)),
            payload_capacity: 15244,
        },
    ];

    #[test]
    fn geometry_reproduces_the_specification_tables() {
        for expected in &SPEC_TABLE {
            let p = expected.id.profile();
            let name = p.name;
            assert_eq!(p.bits_per_cell(), expected.bits_per_cell, "{name} bits/cell");
            assert_eq!(p.band_width(), expected.band_width, "{name} band width");
            assert_eq!(p.header_rows(), expected.header_rows, "{name} header rows");
            assert_eq!(
                p.header_codeword_len(),
                expected.header_codeword_len,
                "{name} header codeword"
            );
            assert_eq!(p.reserved_cells(), expected.reserved_cells, "{name} reserved cells");
            assert_eq!(p.data_cells(), expected.data_cells, "{name} data cells");
            assert_eq!(p.raw_bytes(), expected.raw_bytes, "{name} raw bytes");

            let partition = p.rs_partition();
            assert_eq!(partition.full_codewords, expected.full_codewords, "{name} full codewords");
            assert_eq!(partition.shortened, expected.shortened, "{name} shortened codeword");
            assert_eq!(p.payload_capacity(), expected.payload_capacity, "{name} payload capacity");
        }
    }

    #[test]
    fn profile_ids_round_trip() {
        for profile in &PROFILES {
            let parsed = ProfileId::from_u8(profile.id.as_u8()).expect("known identifier");
            assert_eq!(parsed, profile.id);
            assert_eq!(parsed.profile(), profile);
        }
    }

    #[test]
    fn unknown_profile_is_rejected_not_guessed() {
        // Guessing at the geometry would produce a plausible-looking frame full
        // of noise, which is worse than admitting the profile is unknown.
        assert_eq!(ProfileId::from_u8(0x00), Err(Error::ProfileUnsupported { found: 0x00 }));
        assert_eq!(ProfileId::from_u8(0xFF), Err(Error::ProfileUnsupported { found: 0xFF }));
    }

    #[test]
    fn header_codeword_fits_the_reed_solomon_field() {
        // RS over GF(2^8) cannot express a codeword longer than 255 bytes, and
        // the header codeword is sized to fill its band, so a large grid could
        // in principle overflow it.
        for profile in &PROFILES {
            let n = profile.header_codeword_len();
            assert!(n <= RS_BLOCK_LEN, "{} header codeword {n} exceeds GF(256)", profile.name);
            assert!(n >= MIN_HEADER_CODEWORD_LEN, "{} header codeword {n} too short", profile.name);
        }
    }

    #[test]
    fn header_band_holds_its_codeword() {
        for profile in &PROFILES {
            let cells = profile.header_rows() * profile.band_width();
            let bits = profile.header_codeword_len() * 8;
            assert!(bits <= cells, "{} header does not fit its band", profile.name);
            assert!(cells - bits < 8, "{} wastes a whole byte of band", profile.name);
        }
    }

    #[test]
    fn alphabets_are_powers_of_two() {
        // The cell value packs as `(colour << log2(shapes)) | shape`, which is
        // only lossless when both alphabets are powers of two.
        for profile in &PROFILES {
            assert!(profile.num_shapes.is_power_of_two(), "{}", profile.name);
            assert!(profile.num_colours.is_power_of_two(), "{}", profile.name);
            assert_eq!(
                profile.bits_per_cell(),
                profile.num_shapes.ilog2() + profile.num_colours.ilog2()
            );
        }
    }

    #[test]
    fn grids_are_multiples_of_thirty_two() {
        for profile in &PROFILES {
            assert_eq!(profile.grid % 32, 0, "{} grid {}", profile.name, profile.grid);
        }
    }

    #[test]
    fn link_overhead_exceeds_the_code_rate_but_not_by_much() {
        // The shortened trailing codeword pays full parity on a short block, so
        // the frame always loses a little more than the code alone would
        // suggest. A large gap would mean the partition is wasting a frame.
        for profile in &PROFILES {
            let code = profile.rs_parity_rate();
            let link = profile.link_overhead();
            assert!(link >= code, "{} link overhead below code rate", profile.name);
            assert!(
                link - code < 0.01,
                "{} wastes {:.2}% on the tail",
                profile.name,
                (link - code) * 100.0
            );
        }
    }

    #[test]
    fn payload_capacity_fits_the_header_length_field() {
        // `payload_len` is a u16, so a profile whose frame could carry more than
        // 65535 bytes would be unrepresentable.
        for profile in &PROFILES {
            assert!(
                u16::try_from(profile.payload_capacity()).is_ok(),
                "{} payload capacity overflows payload_len",
                profile.name
            );
        }
    }

    #[test]
    fn robustness_decreases_monotonically_across_profiles() {
        // The profile ladder is a robustness curve, so parity must fall as
        // density rises. If this ever inverts, a profile has lost its purpose.
        let rates: Vec<f64> = PROFILES.iter().map(Profile::rs_parity_rate).collect();
        for pair in rates.windows(2) {
            let [sparser, denser] = pair else { unreachable!("windows(2) yields pairs") };
            assert!(denser < sparser, "parity rate must fall as density rises: {rates:?}");
        }

        let bits: Vec<u32> = PROFILES.iter().map(Profile::bits_per_cell).collect();
        for pair in bits.windows(2) {
            let [lighter, denser] = pair else { unreachable!("windows(2) yields pairs") };
            assert!(denser >= lighter, "bits per cell must not fall as profiles densify: {bits:?}");
        }
    }
}
