//! The dense code: black and white, the whole screen, in tiles.
//!
//! The first format this crate defined paints a shape in a colour in each cell.
//! Pointed at a real phone, three things about it turned out to be wrong for
//! the channel, and this format is what is left when they are put right.
//!
//! **A camera sees brightness far better than it sees colour.** Through a real
//! phone an edge between black and white is a pixel and a half wide, while a
//! colour bleeds across four: the sensor measures one colour at each pixel and
//! interpolates the other two, and the video it hands over carries colour at
//! half resolution. A cell that spends its area on a colour spends it on what
//! the camera resolves worst. Here a cell is one module, black or white, and
//! three camera pixels across.
//!
//! **A picture is rarely of one code.** A phone's shutter rolls across the
//! picture while the display repaints down it, so a picture taken as the code
//! changes holds part of the old code and part of the new. With the whole
//! frame under one set of codewords, such a picture is worth nothing, and the
//! faster the codes change the more pictures are such pictures. Here a frame
//! is cut into tiles, each with its own error correction and its own note of
//! which code it belongs to. A tile caught whole is read, whatever happened to
//! the tile beside it.
//!
//! **A lens bends the middle of a code, not its edges.** So a grid measured
//! round the perimeter says nothing of where the middle is. Here every tile
//! has an alignment mark at its centre, which can be located outright, and the
//! lattice of the modules themselves says the rest.
//!
//! A frame carries no header of its own. Each tile says which session and which
//! code it is from, because in a picture that spans a change of code the tiles
//! do not agree, and none of them is wrong.

// Geometry, again. See the note at the head of `register`: every conversion
// here is between an index into a grid of known size and a position in it, and
// is bounded by the size of that grid. And a little linear algebra, where a
// row and a column are what the loops are over and what the matrix is
// indexed by.
#![allow(
    clippy::needless_range_loop,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines
)]

use crate::codec::crc32c;
use crate::error::{Error, Result};
use crate::fec::RsCodec;
use crate::file::{Manifest, compress, decompress, digest};
use crate::geom::{Homography, Point};
use crate::image::{Gray, RgbImage};
use crate::symbol::Rgb;
use crate::transport::{PAYLOAD_ID_LEN, TransportDecoder, TransportEncoder};

/// Modules to a unit of a finder pattern.
///
/// A finder has to be found before anything is known about the picture, at
/// half its size, among everything else in the room. Its rings are three
/// modules wide so that they are nine camera pixels wide where a module is
/// three.
pub const UNIT: u32 = 3;

/// Side of a finder pattern, in modules.
pub const FINDER_PATTERN: u32 = 7 * UNIT;

/// Side of the box a finder pattern sits in, its light separator included.
pub const FINDER_BOX: u32 = 8 * UNIT;

/// Width of the light margin round the code, in modules.
pub const QUIET_MODULES: u32 = 4;

/// Side of an alignment mark: a dark square in a light one.
pub const MARK_BOX: u32 = 8;

/// Side of the dark square in the middle of a mark.
pub const MARK_CORE: u32 = 4;

/// Bytes of every tile's payload that are not its body.
///
/// Kind, profile, session, code, tile, the length of the body, and a CRC over
/// all of it.
pub const TILE_OVERHEAD: usize = 17;

/// What a tile carries.
pub mod tile_kind {
    /// Nothing.
    pub const IDLE: u8 = 0;
    /// The manifest of the transfer.
    pub const MANIFEST: u8 = 1;
    /// One encoding symbol, with its payload identifier in front.
    pub const SYMBOL: u8 = 2;
}

/// A dense profile: how many tiles, of what size, with how much parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DenseProfile {
    /// Identifier carried in every tile.
    pub id: u8,
    /// Name, as the pages show it.
    pub name: &'static str,
    /// Width of a tile in modules.
    pub tile_width: u32,
    /// Height of a tile in modules.
    pub tile_height: u32,
    /// Tiles across.
    pub tile_cols: u32,
    /// Tiles down.
    pub tile_rows: u32,
    /// Parity bytes in each Reed-Solomon codeword.
    pub parity: u32,
}

impl DenseProfile {
    /// Width of the code in modules.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.tile_width * self.tile_cols
    }

    /// Height of the code in modules.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.tile_height * self.tile_rows
    }

    /// Tiles in a frame.
    #[must_use]
    pub const fn tiles(&self) -> u32 {
        self.tile_cols * self.tile_rows
    }

    /// The profile with this identifier.
    #[must_use]
    pub fn from_id(id: u8) -> Option<&'static Self> {
        DENSE_PROFILES.iter().find(|profile| profile.id == id)
    }
}

/// The dense profiles, sparsest first.
///
/// All three are sixteen by nine, near enough, because that is the shape of
/// the screens they are shown on and of the pictures taken of them. They
/// differ in how many modules they put across it: five pixels to a module on
/// a 1080p screen, then four, then three.
pub static DENSE_PROFILES: [DenseProfile; 3] = [
    DenseProfile {
        id: 0x11,
        name: "D1-swift",
        tile_width: 72,
        tile_height: 48,
        tile_cols: 5,
        tile_rows: 4,
        parity: 32,
    },
    DenseProfile {
        id: 0x12,
        name: "D2-rapid",
        tile_width: 75,
        tile_height: 48,
        tile_cols: 6,
        tile_rows: 5,
        parity: 32,
    },
    DenseProfile {
        id: 0x13,
        name: "D3-blaze",
        tile_width: 75,
        tile_height: 48,
        tile_cols: 8,
        tile_rows: 7,
        parity: 32,
    },
];

/// An alignment mark.
#[derive(Debug, Clone, Copy)]
struct Mark {
    /// Its middle, in modules.
    x: f64,
    y: f64,
    /// Whether it is light on dark rather than dark on light.
    ///
    /// The marks of the lower half of a frame are. A frame turned upside down
    /// still has four finder patterns at its corners and a mark at the middle
    /// of every tile; what it does not have is the dark marks at the top.
    inverted: bool,
}

/// How one tile's bytes are cut into codewords.
#[derive(Debug, Clone)]
struct TileCode {
    /// Length of each codeword, parity included.
    lengths: Vec<usize>,
    /// Bytes the tile holds, parity included.
    raw: usize,
    /// Bytes of payload the tile holds.
    capacity: usize,
}

impl TileCode {
    fn new(modules: usize, parity: usize) -> Self {
        let raw = modules / 8;
        let count = raw.div_ceil(255).max(1);
        let lengths: Vec<usize> =
            (0..count).map(|index| raw / count + usize::from(index < raw % count)).collect();
        let capacity = lengths.iter().map(|length| length.saturating_sub(parity)).sum();
        Self { lengths, raw, capacity }
    }
}

/// Which module is what, for one profile.
#[derive(Debug, Clone)]
pub struct DenseLayout {
    profile: DenseProfile,
    /// For every module, row-major: whether it is fixed, and if so whether it
    /// is dark.
    fixed: Vec<Option<bool>>,
    /// The modules of each tile that carry payload, in the order they are
    /// filled.
    tiles: Vec<Vec<u32>>,
    codes: Vec<TileCode>,
    marks: Vec<Mark>,
    codec: RsCodec,
}

impl DenseLayout {
    /// The layout of a profile.
    #[must_use]
    pub fn new(profile: &DenseProfile) -> Self {
        let (width, height) = (profile.width(), profile.height());
        let mut fixed: Vec<Option<bool>> = vec![None; (width * height) as usize];

        for row in 0..height {
            for col in 0..width {
                let near_row = row < FINDER_BOX || row >= height - FINDER_BOX;
                let near_col = col < FINDER_BOX || col >= width - FINDER_BOX;
                if near_row && near_col {
                    fixed[(row * width + col) as usize] = Some(finder_is_dark(profile, row, col));
                }
            }
        }

        let mut marks = Vec::with_capacity(profile.tiles() as usize);
        for tile_row in 0..profile.tile_rows {
            for tile_col in 0..profile.tile_cols {
                let centre_col = tile_col * profile.tile_width + profile.tile_width / 2;
                let centre_row = tile_row * profile.tile_height + profile.tile_height / 2;
                let inverted = tile_row * 2 >= profile.tile_rows;

                for dy in 0..MARK_BOX {
                    for dx in 0..MARK_BOX {
                        let edge = (MARK_BOX - MARK_CORE) / 2;
                        let core = (edge..edge + MARK_CORE).contains(&dx)
                            && (edge..edge + MARK_CORE).contains(&dy);
                        let (row, col) =
                            (centre_row - MARK_BOX / 2 + dy, centre_col - MARK_BOX / 2 + dx);
                        fixed[(row * width + col) as usize] = Some(core != inverted);
                    }
                }
                marks.push(Mark { x: f64::from(centre_col), y: f64::from(centre_row), inverted });
            }
        }

        let mut tiles = vec![Vec::new(); profile.tiles() as usize];
        for row in 0..height {
            for col in 0..width {
                let index = row * width + col;
                if fixed[index as usize].is_none() {
                    let tile =
                        (row / profile.tile_height) * profile.tile_cols + col / profile.tile_width;
                    tiles[tile as usize].push(index);
                }
            }
        }

        let codes =
            tiles.iter().map(|tile| TileCode::new(tile.len(), profile.parity as usize)).collect();

        Self {
            profile: *profile,
            fixed,
            tiles,
            codes,
            marks,
            codec: RsCodec::new(profile.parity as usize),
        }
    }

    /// The profile this layout is of.
    #[must_use]
    pub const fn profile(&self) -> &DenseProfile {
        &self.profile
    }

    /// Whether a tile has a finder pattern in it, and so less room than the
    /// rest.
    #[must_use]
    pub const fn is_corner(&self, tile: u32) -> bool {
        let (row, col) = (tile / self.profile.tile_cols, tile % self.profile.tile_cols);
        (row == 0 || row == self.profile.tile_rows - 1)
            && (col == 0 || col == self.profile.tile_cols - 1)
    }

    /// The modules of a tile that carry its bytes, as indices into the frame
    /// row-major, in the order they are filled.
    #[must_use]
    pub fn tile_modules(&self, tile: u32) -> &[u32] {
        self.tiles.get(tile as usize).map_or(&[], Vec::as_slice)
    }

    /// Bytes of payload a tile holds.
    #[must_use]
    pub fn capacity(&self, tile: u32) -> usize {
        self.codes.get(tile as usize).map_or(0, |code| code.capacity)
    }

    /// Bytes of payload the tiles without a finder pattern hold.
    #[must_use]
    pub fn symbol_tile_capacity(&self) -> usize {
        (0..self.profile.tiles())
            .filter(|&tile| !self.is_corner(tile))
            .map(|tile| self.capacity(tile))
            .min()
            .unwrap_or(0)
    }

    /// The largest encoding symbol a tile can carry.
    #[must_use]
    pub fn symbol_size(&self) -> u16 {
        let room = self.symbol_tile_capacity().saturating_sub(TILE_OVERHEAD + PAYLOAD_ID_LEN);
        // A multiple of eight, which is the alignment RaptorQ declares.
        u16::try_from(room / 8 * 8).unwrap_or(0)
    }

    /// Bytes of a file one frame carries.
    #[must_use]
    pub fn bytes_per_frame(&self) -> usize {
        let tiles = (0..self.profile.tiles()).filter(|&tile| !self.is_corner(tile)).count();
        tiles * usize::from(self.symbol_size())
    }

    /// Size of a painted frame in pixels, margin included.
    #[must_use]
    pub const fn image_size(&self, module_px: u32) -> (u32, u32) {
        (
            (self.profile.width() + 2 * QUIET_MODULES) * module_px,
            (self.profile.height() + 2 * QUIET_MODULES) * module_px,
        )
    }

    /// Where the middles of the four finder patterns are, in modules.
    #[must_use]
    pub fn finder_centres(&self) -> [Point; 4] {
        let near = f64::from(FINDER_PATTERN) / 2.0;
        let (right, bottom) =
            (f64::from(self.profile.width()) - near, f64::from(self.profile.height()) - near);
        [
            Point::new(near, near),
            Point::new(right, near),
            Point::new(right, bottom),
            Point::new(near, bottom),
        ]
    }

    /// The map from modules to the pixels of a frame this layout painted.
    #[must_use]
    pub fn identity_transform(&self, module_px: u32) -> Homography {
        let origin = f64::from(QUIET_MODULES * module_px) - 0.5;
        let scale = f64::from(module_px);
        Homography::from_coefficients([scale, 0.0, origin, 0.0, scale, origin, 0.0, 0.0, 1.0])
    }

    /// Turns a tile's payload into the bytes that are painted.
    fn protect(&self, tile: u32, payload: &[u8]) -> Vec<u8> {
        let code = &self.codes[tile as usize];
        let parity = self.profile.parity as usize;

        let mut codewords = Vec::with_capacity(code.lengths.len());
        let mut taken = 0usize;
        for &length in &code.lengths {
            let data = length.saturating_sub(parity);
            let mut block = vec![0u8; data];
            let available = payload.len().saturating_sub(taken).min(data);
            block[..available].copy_from_slice(&payload[taken..taken + available]);
            taken += data;
            codewords.push(self.codec.encode(&block).unwrap_or_else(|_| vec![0u8; length]));
        }

        let mut raw = interleave(&codewords, code.raw);
        whiten(&mut raw, tile);
        raw
    }

    /// Turns the bytes that were read back into a tile, if parity can.
    ///
    /// `sure` says how sure each byte is. The bytes least sure are the ones
    /// most likely wrong, and a byte known to be in doubt costs half the
    /// parity of one that is wrong and not known to be. How many to doubt
    /// cannot be known beforehand, so several numbers are tried, and the
    /// tile's own checksum says which of them gave the tile back.
    fn recover(&self, tile: u32, raw: &[u8], sure: &[f32]) -> Option<(Tile, usize)> {
        let code = &self.codes[tile as usize];
        let parity = self.profile.parity as usize;
        let mut raw = raw.to_vec();
        raw.resize(code.raw, 0);
        whiten(&mut raw, tile);

        let codewords = deinterleave(&raw, &code.lengths);
        let positions: Vec<usize> = (0..code.raw).collect();
        let owners = deinterleave_positions(&positions, &code.lengths);

        // Each codeword's bytes, least sure first.
        let doubts: Vec<Vec<usize>> = owners
            .iter()
            .map(|owned| {
                let mut order: Vec<usize> = (0..owned.len()).collect();
                order.sort_by(|&a, &b| {
                    let (a, b) = (sure.get(owned[a]), sure.get(owned[b]));
                    a.partial_cmp(&b).unwrap_or(core::cmp::Ordering::Equal)
                });
                order
            })
            .collect();

        for doubted in [0, parity / 2] {
            let mut payload = Vec::with_capacity(code.capacity);
            let mut repaired = 0usize;
            let mut whole = true;
            for (codeword, order) in codewords.iter().zip(doubts.iter()) {
                let erasures = &order[..doubted.min(order.len())];
                let Ok(data) = self.codec.decode(codeword, erasures) else {
                    whole = false;
                    break;
                };
                let Ok(clean) = self.codec.encode(&data) else {
                    whole = false;
                    break;
                };
                repaired += clean.iter().zip(codeword.iter()).filter(|(a, b)| a != b).count();
                payload.extend_from_slice(&data);
            }
            if !whole {
                continue;
            }
            if let Some(found) = Tile::decode(&payload) {
                return Some((found, repaired));
            }
        }
        None
    }

    /// Paints a frame.
    ///
    /// `payloads` has one entry for each tile, of at most that tile's capacity.
    #[must_use]
    pub fn render(&self, payloads: &[Vec<u8>], module_px: u32) -> RgbImage {
        let (width, height) = self.image_size(module_px);
        let mut image = RgbImage::filled(width, height, Rgb::WHITE);
        let modules = self.modules(payloads);

        let across = self.profile.width();
        for (index, &light) in modules.iter().enumerate() {
            if light {
                continue;
            }
            let (row, col) = (index as u32 / across, index as u32 % across);
            image.fill_rect(
                (col + QUIET_MODULES) * module_px,
                (row + QUIET_MODULES) * module_px,
                module_px,
                module_px,
                Rgb::BLACK,
            );
        }
        image
    }

    /// Every module of a frame, row-major: whether it is light.
    #[must_use]
    pub fn modules(&self, payloads: &[Vec<u8>]) -> Vec<bool> {
        let mut modules: Vec<bool> =
            self.fixed.iter().map(|fixed| fixed.is_none_or(|dark| !dark)).collect();

        for (tile, cells) in self.tiles.iter().enumerate() {
            let empty = Vec::new();
            let payload = payloads.get(tile).unwrap_or(&empty);
            let raw = self.protect(tile as u32, payload);
            for (position, &module) in cells.iter().enumerate() {
                let bit =
                    raw.get(position / 8).is_some_and(|byte| (byte >> (7 - position % 8)) & 1 == 1);
                modules[module as usize] = bit;
            }
        }
        modules
    }
}

/// Whether a module of a finder box is dark.
fn finder_is_dark(profile: &DenseProfile, row: u32, col: u32) -> bool {
    const ROWS: [u8; 7] =
        [0b111_1111, 0b100_0001, 0b101_1101, 0b101_1101, 0b101_1101, 0b100_0001, 0b111_1111];

    // The pattern hugs the outer corner; the separator is the inner strip.
    let from_edge = |at: u32, size: u32| if at < FINDER_BOX { at } else { size - 1 - at };
    let (r, c) = (from_edge(row, profile.height()) / UNIT, from_edge(col, profile.width()) / UNIT);
    r < 7 && c < 7 && (ROWS[r as usize] >> (6 - c)) & 1 == 1
}

/// Makes a tile's bytes look like noise, whatever they hold; applied twice, it
/// gives back what it was given.
///
/// A reader finds black and white from the darkest and brightest modules of
/// each neighbourhood and the lattice from the edges between modules. A tile of
/// padding has neither. Each tile has a sequence of its own, so that two tiles
/// holding the same bytes are not painted the same.
fn whiten(bytes: &mut [u8], tile: u32) {
    let mut state = 0x5048_544E ^ (tile.wrapping_add(1)).wrapping_mul(0x9E37_79B9);
    for byte in bytes {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte ^= state.to_be_bytes()[0];
    }
}

/// Deals codewords out a byte at a time, so that a blemish is spread across
/// all of them.
fn interleave(codewords: &[Vec<u8>], raw: usize) -> Vec<u8> {
    let longest = codewords.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::with_capacity(raw);
    for position in 0..longest {
        for codeword in codewords {
            if let Some(&byte) = codeword.get(position) {
                out.push(byte);
            }
        }
    }
    out.resize(raw, 0);
    out
}

/// Which byte of the interleaved stream each byte of each codeword came from.
fn deinterleave_positions(positions: &[usize], lengths: &[usize]) -> Vec<Vec<usize>> {
    let mut owners: Vec<Vec<usize>> =
        lengths.iter().map(|&length| Vec::with_capacity(length)).collect();
    let longest = lengths.iter().copied().max().unwrap_or(0);
    let mut next = positions.iter();
    for position in 0..longest {
        for (owner, &length) in owners.iter_mut().zip(lengths.iter()) {
            if position < length
                && let Some(&from) = next.next()
            {
                owner.push(from);
            }
        }
    }
    owners
}

/// The inverse of [`interleave`].
fn deinterleave(raw: &[u8], lengths: &[usize]) -> Vec<Vec<u8>> {
    let mut codewords: Vec<Vec<u8>> =
        lengths.iter().map(|&length| Vec::with_capacity(length)).collect();
    let longest = lengths.iter().copied().max().unwrap_or(0);
    let mut bytes = raw.iter();
    for position in 0..longest {
        for (codeword, &length) in codewords.iter_mut().zip(lengths.iter()) {
            if position < length
                && let Some(&byte) = bytes.next()
            {
                codeword.push(byte);
            }
        }
    }
    for (codeword, &length) in codewords.iter_mut().zip(lengths.iter()) {
        codeword.resize(length, 0);
    }
    codewords
}

// --- what a tile says -------------------------------------------------------------

/// A tile that was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    /// What it carries: one of [`tile_kind`].
    pub kind: u8,
    /// The profile that painted it.
    pub profile: u8,
    /// The transfer it belongs to.
    pub session: u32,
    /// The code it belongs to.
    pub code: u32,
    /// Which tile of the frame it is.
    pub index: u8,
    /// What it carries.
    pub body: Vec<u8>,
}

impl Tile {
    /// Packs the tile into a payload of exactly `capacity` bytes.
    fn encode(&self, capacity: usize) -> Vec<u8> {
        let mut out = vec![0u8; capacity];
        let room = capacity.saturating_sub(TILE_OVERHEAD);
        let length = self.body.len().min(room);

        out[0] = self.kind;
        out[1] = self.profile;
        out[2..6].copy_from_slice(&self.session.to_le_bytes());
        out[6..10].copy_from_slice(&self.code.to_le_bytes());
        out[10] = self.index;
        out[11..13].copy_from_slice(&u16::try_from(length).unwrap_or(0).to_le_bytes());
        out[13..13 + length].copy_from_slice(&self.body[..length]);

        let crc = crc32c(&out[..capacity - 4]);
        out[capacity - 4..].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Unpacks a payload, refusing one whose checksum does not hold.
    fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() < TILE_OVERHEAD {
            return None;
        }
        let (content, stored) = payload.split_at(payload.len() - 4);
        if crc32c(content).to_le_bytes() != stored {
            return None;
        }

        let length = usize::from(u16::from_le_bytes([content[11], content[12]]));
        let body = content.get(13..13 + length)?.to_vec();
        Some(Self {
            kind: content[0],
            profile: content[1],
            session: u32::from_le_bytes([content[2], content[3], content[4], content[5]]),
            code: u32::from_le_bytes([content[6], content[7], content[8], content[9]]),
            index: content[10],
            body,
        })
    }
}

// --- sending ----------------------------------------------------------------------

/// Paints the endless sequence of dense frames for one file.
pub struct DenseTransmitter {
    layout: DenseLayout,
    encoder: TransportEncoder,
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    pending: Vec<Vec<u8>>,
    cursor: usize,
    next_repair: u32,
    session: u32,
    code: u32,
}

impl DenseTransmitter {
    /// Prepares a transfer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for an empty file, a file name that is not
    /// a bare name or is too long for a tile, or a profile whose tiles cannot
    /// hold a symbol.
    pub fn new(name: &str, file: &[u8], profile: &DenseProfile, session: u32) -> Result<Self> {
        if file.is_empty() {
            return Err(Error::Malformed {
                context: "transfer",
                detail: "cannot transmit an empty file",
            });
        }

        let layout = DenseLayout::new(profile);
        let symbol_size = layout.symbol_size();
        if symbol_size == 0 {
            return Err(Error::Malformed {
                context: "transfer",
                detail: "profile tiles are too small to carry a symbol",
            });
        }

        let (compression, packed) = compress(file);
        let encoder = TransportEncoder::new(&packed, symbol_size)?;
        let manifest = Manifest {
            compression,
            original_size: file.len() as u64,
            sha256: digest(file),
            oti: encoder.oti(),
            name: name.to_owned(),
        };
        let manifest_bytes = manifest.encode()?;

        let corner = (0..profile.tiles())
            .filter(|&tile| layout.is_corner(tile))
            .map(|tile| layout.capacity(tile))
            .min()
            .unwrap_or(0);
        if manifest_bytes.len() + TILE_OVERHEAD > corner {
            return Err(Error::Malformed {
                context: "transfer",
                detail: "the file name is too long for a tile",
            });
        }

        let pending = encoder.source_symbols();
        Ok(Self {
            layout,
            encoder,
            manifest,
            manifest_bytes,
            pending,
            cursor: 0,
            next_repair: 0,
            session,
            code: 0,
        })
    }

    /// The layout frames are painted in.
    #[must_use]
    pub const fn layout(&self) -> &DenseLayout {
        &self.layout
    }

    /// The manifest the transfer advertises.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Frames needed to show every source symbol once.
    #[must_use]
    pub fn frames_per_pass(&self) -> usize {
        let tiles = (0..self.layout.profile.tiles())
            .filter(|&tile| !self.layout.is_corner(tile))
            .count()
            .max(1);
        self.encoder.source_symbol_count().div_ceil(tiles)
    }

    fn next_symbol(&mut self) -> Vec<u8> {
        if self.cursor >= self.pending.len() {
            let batch = self.encoder.repair_batch();
            self.pending = self.encoder.repair_symbols(self.next_repair, batch);
            self.next_repair = self.next_repair.saturating_add(batch);
            self.cursor = 0;
        }
        let symbol = self.pending.get(self.cursor).cloned().unwrap_or_default();
        self.cursor += 1;
        symbol
    }

    /// The payload of every tile of the next frame.
    pub fn next_payloads(&mut self) -> Vec<Vec<u8>> {
        let profile = self.layout.profile;
        let payloads = (0..profile.tiles())
            .map(|index| {
                // The four tiles with a finder pattern in them have less room
                // than the rest, and the manifest needs sending somewhere.
                let (kind, body) = if self.layout.is_corner(index) {
                    (tile_kind::MANIFEST, self.manifest_bytes.clone())
                } else {
                    (tile_kind::SYMBOL, self.next_symbol())
                };
                Tile {
                    kind,
                    profile: profile.id,
                    session: self.session,
                    code: self.code,
                    index: u8::try_from(index).unwrap_or(u8::MAX),
                    body,
                }
                .encode(self.layout.capacity(index))
            })
            .collect();
        self.code = self.code.wrapping_add(1);
        payloads
    }

    /// Paints the next frame. This never runs out.
    pub fn next_frame(&mut self, module_px: u32) -> RgbImage {
        let payloads = self.next_payloads();
        self.layout.render(&payloads, module_px)
    }

    /// The next frame as modules rather than as pixels: whether each is light.
    pub fn next_modules(&mut self) -> Vec<bool> {
        let payloads = self.next_payloads();
        self.layout.modules(&payloads)
    }
}

// --- reading ----------------------------------------------------------------------

/// Side of the patches the lattice is measured in, in modules.
const PATCH: u32 = 24;

/// The power a camera's numbers are raised to, to have light back.
const GAMMA: f32 = 2.2;

/// How faint a patch's edges may be, as a fraction of the median patch's,
/// before its phase is not believed.
const FAINT: f64 = 0.35;

/// Samples to a module when looking for edges.
const SAMPLES_PER_MODULE: u32 = 4;

/// The cosine and sine of where an edge is in the lattice's period.
///
/// An edge halfway between two samples, of four to a module, is an eighth of
/// the way round from a line of the lattice, or three, five or seven.
const ROUND: [(f32, f32); 4] =
    [(EIGHTH, -EIGHTH), (EIGHTH, EIGHTH), (-EIGHTH, EIGHTH), (-EIGHTH, -EIGHTH)];
const EIGHTH: f32 = core::f32::consts::FRAC_1_SQRT_2;

/// How far from where the homography puts it a mark is looked for, in modules.
const MARK_REACH: f64 = 3.5;

/// How strongly the marks must stand out before a frame is believed to be
/// there, as a fraction of the range between dark and light.
const MIN_MARK_SCORE: f64 = 0.25;

/// Everything a picture yielded.
#[derive(Debug, Clone, Default)]
pub struct DenseReading {
    /// Whether a frame was found in the picture at all.
    pub located: bool,
    /// The profile the frame was painted in.
    pub profile: Option<u8>,
    /// The four finder centres in the picture.
    pub corners: Option<[Point; 4]>,
    /// Camera pixels to a module.
    pub pixels_per_module: Option<f64>,
    /// How far the grid had to be bent to fit, in modules.
    pub correction: f64,
    /// The tiles that were read.
    pub tiles: Vec<Tile>,
    /// Tiles whose error correction or checksum failed.
    pub tiles_lost: usize,
    /// Bytes that error correction repaired, in the tiles that were read.
    pub bytes_repaired: usize,
    /// Bytes in the tiles that were read.
    pub bytes_read: usize,
    /// The tiles that were not, as they were seen.
    pub unread: Vec<Unread>,
    /// Every module as it was read, row-major: whether it is light. Kept only
    /// when asked for.
    pub modules: Vec<bool>,
}

/// Reads dense frames out of pictures, and keeps nothing.
#[derive(Debug, Clone)]
pub struct DenseReader {
    layouts: Vec<DenseLayout>,
    keep_modules: bool,
    keep_unread: bool,
    equalise: bool,
}

impl Default for DenseReader {
    fn default() -> Self {
        Self::new()
    }
}

impl DenseReader {
    /// A reader for every dense profile.
    #[must_use]
    pub fn new() -> Self {
        Self {
            layouts: DENSE_PROFILES.iter().map(DenseLayout::new).collect(),
            keep_modules: false,
            keep_unread: true,
            equalise: true,
        }
    }

    /// Has readings leave out the tiles that would not read, for a bench to
    /// measure what reading them again is worth.
    #[must_use]
    pub const fn without_unread(mut self) -> Self {
        self.keep_unread = false;
        self
    }

    /// Has modules decided against a threshold and nothing more, for a bench
    /// to measure what taking the channel out is worth.
    #[must_use]
    pub const fn without_equalising(mut self) -> Self {
        self.equalise = false;
        self
    }

    /// Has readings keep every module as it was read, for a bench to compare
    /// with what was painted.
    #[must_use]
    pub const fn keeping_modules(mut self) -> Self {
        self.keep_modules = true;
        self
    }

    /// Reads a picture.
    #[must_use]
    pub fn read(&self, image: &RgbImage) -> DenseReading {
        let gray = Gray::from_image(image);
        let Some(corners) = crate::detect::locate_corners(&gray) else {
            return DenseReading::default();
        };

        // Which profile, and which way up. The finder patterns say where the
        // corners are and nothing else, so each possibility is tried and the
        // marks say which it was.
        let mut best: Option<(f64, &DenseLayout, Homography, [Point; 4])> = None;
        for layout in &self.layouts {
            for turn in 0..4 {
                let turned: [Point; 4] = core::array::from_fn(|i| corners[(i + turn) % 4]);
                let Some(transform) = Homography::from_quads(layout.finder_centres(), turned)
                else {
                    continue;
                };
                if !is_square(&transform, layout) {
                    continue;
                }
                let score = mark_score(&gray, layout, &transform);
                if best.as_ref().is_none_or(|(found, ..)| score > *found) {
                    best = Some((score, layout, transform, turned));
                }
            }
        }
        let Some((score, layout, transform, corners)) = best else {
            return DenseReading::default();
        };
        if score < MIN_MARK_SCORE {
            return DenseReading::default();
        }

        let field = Field::measure(&gray, layout, &transform);
        let samples = sample_modules(&gray, layout, &transform, &field);
        let seen = level(layout, &samples);
        let tiles = (0..layout.profile.tiles()).map(|tile| layout.bounds(tile)).collect();
        let mut region = Region::new(
            layout.profile.width() as usize,
            layout.profile.height() as usize,
            seen,
            &layout.fixed,
            1.0,
            tiles,
        );
        if self.equalise {
            region.settle();
        }
        let belief = region.beliefs();

        let mut reading = DenseReading {
            located: true,
            profile: Some(layout.profile.id),
            corners: Some(corners),
            pixels_per_module: Some(pixels_per_module(&transform, layout)),
            correction: field.largest(),
            ..DenseReading::default()
        };

        for tile in 0..layout.profile.tiles() {
            let Some((found, repaired)) = layout.read_tile(tile, &layout.cut(tile, &belief)) else {
                reading.tiles_lost += 1;
                if self.keep_unread {
                    let seen = layout
                        .cut(tile, &region.seen)
                        .iter()
                        .map(|&value| (value * KEPT_SCALE).round().clamp(-127.0, 127.0) as i8)
                        .collect();
                    reading.unread.push(Unread { index: tile as u8, seen });
                }
                continue;
            };
            reading.bytes_repaired += repaired;
            reading.bytes_read += layout.codes[tile as usize].raw;

            // The code that was on the screen before this one, or the one
            // after, may be under it.
            let seen = layout.cut(tile, &region.seen);
            let known = Known { code: found.code, modules: layout.painted(&found) };
            if self.equalise
                && let Some(under) = layout.read_under(tile, seen, &[&known])
                && under.code != found.code
            {
                reading.tiles.push(under);
            }
            reading.tiles.push(found);
        }

        if self.keep_modules {
            reading.modules = belief.iter().map(|&value| value > 0.0).collect();
        }
        reading
    }
}

/// Camera pixels to a module, at the middle of the code.
fn pixels_per_module(transform: &Homography, layout: &DenseLayout) -> f64 {
    let (x, y) =
        (f64::from(layout.profile.width()) / 2.0, f64::from(layout.profile.height()) / 2.0);
    let here = transform.map(Point::new(x, y));
    let across = transform.map(Point::new(x + 1.0, y));
    let down = transform.map(Point::new(x, y + 1.0));
    match (here, across, down) {
        (Some(a), Some(b), Some(c)) => f64::midpoint(a.distance(b), a.distance(c)),
        _ => 0.0,
    }
}

/// Whether a module comes out about as wide as it is tall.
///
/// A frame is not square, so matching its corners a quarter of a turn round
/// gives a map that stretches it one way and squeezes it the other.
fn is_square(transform: &Homography, layout: &DenseLayout) -> bool {
    let (x, y) =
        (f64::from(layout.profile.width()) / 2.0, f64::from(layout.profile.height()) / 2.0);
    let here = transform.map(Point::new(x, y));
    let across = transform.map(Point::new(x + 1.0, y));
    let down = transform.map(Point::new(x, y + 1.0));
    match (here, across, down) {
        (Some(a), Some(b), Some(c)) => {
            let (w, h) = (a.distance(b), a.distance(c));
            w > 0.5 && h > 0.5 && w / h < 1.5 && h / w < 1.5
        }
        _ => false,
    }
}

/// Brightness at a position in the picture, from 0 to 1, or `None` outside it.
fn luma(gray: &Gray, at: Point) -> Option<f64> {
    let (width, height) = (gray.width() as usize, gray.height() as usize);
    if at.x < 0.0 || at.y < 0.0 || at.x > (width - 1) as f64 || at.y > (height - 1) as f64 {
        return None;
    }
    let (x0, y0) = (at.x as usize, at.y as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (fx, fy) = (at.x - x0 as f64, at.y - y0 as f64);
    let data = gray.as_raw();
    let value = |x: usize, y: usize| f64::from(data[y * width + x]);
    let top = value(x0, y0) + (value(x1, y0) - value(x0, y0)) * fx;
    let bottom = value(x0, y1) + (value(x1, y1) - value(x0, y1)) * fx;
    Some((top + (bottom - top) * fy) / 255.0)
}

/// Modules to the side of the blocks a map is worked out at the corners of.
const WARP_BLOCK: u32 = 8;

/// Where in the picture each module is: the homography with a field laid over
/// it.
///
/// Worked out at the corners of blocks of modules and interpolated between
/// them, because a reader asks where a module is a million times a picture,
/// and across a few modules neither the homography nor the field bends by
/// anything that matters.
struct Warp {
    cols: usize,
    rows: usize,
    nodes: Vec<[f32; 2]>,
}

impl Warp {
    fn new(layout: &DenseLayout, transform: &Homography, field: &Field) -> Self {
        let cols = layout.profile.width().div_ceil(WARP_BLOCK) as usize + 1;
        let rows = layout.profile.height().div_ceil(WARP_BLOCK) as usize + 1;
        let nodes = (0..rows * cols)
            .map(|at| {
                let x = f64::from(WARP_BLOCK) * (at % cols) as f64;
                let y = f64::from(WARP_BLOCK) * (at / cols) as f64;
                let (dx, dy) = field.at(x, y);
                transform
                    .map(Point::new(x + dx, y + dy))
                    .map_or([-1e6, -1e6], |point| [point.x as f32, point.y as f32])
            })
            .collect();
        Self { cols, rows, nodes }
    }

    /// Where in the picture a position in modules is.
    #[inline]
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        let (gx, gy) = (x / WARP_BLOCK as f32, y / WARP_BLOCK as f32);
        let (bx, by) = ((gx as usize).min(self.cols - 2), (gy as usize).min(self.rows - 2));
        let (fx, fy) = (gx - bx as f32, gy - by as f32);
        let at = by * self.cols + bx;
        let (a, b) = (self.nodes[at], self.nodes[at + 1]);
        let (c, d) = (self.nodes[at + self.cols], self.nodes[at + self.cols + 1]);

        let top = (a[0] + (b[0] - a[0]) * fx, a[1] + (b[1] - a[1]) * fx);
        let bottom = (c[0] + (d[0] - c[0]) * fx, c[1] + (d[1] - c[1]) * fx);
        (top.0 + (bottom.0 - top.0) * fy, top.1 + (bottom.1 - top.1) * fy)
    }
}

/// The picture's value at a position in it, between the four pixels nearest,
/// each put through `table` first; or `None` outside the picture.
#[inline]
fn shade(gray: &Gray, table: &[f32; 256], x: f32, y: f32) -> Option<f32> {
    let (width, height) = (gray.width() as usize, gray.height() as usize);
    if !(x >= 0.0 && y >= 0.0 && x < (width - 1) as f32 && y < (height - 1) as f32) {
        return None;
    }
    let (x0, y0) = (x as usize, y as usize);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = y0 * width + x0;
    let data = gray.as_raw();
    let value = |index: usize| table[usize::from(data[index])];
    let top = value(at) + (value(at + 1) - value(at)) * fx;
    let bottom = value(at + width) + (value(at + width + 1) - value(at + width)) * fx;
    Some(top + (bottom - top) * fy)
}

/// A camera's numbers as they are, from nought to one.
fn as_written() -> [f32; 256] {
    core::array::from_fn(|value| value as f32 / 255.0)
}

/// A camera's numbers as light.
///
/// A lens adds the light of neighbouring modules, and a display changing from
/// one code to the next adds the light of both. Neither adds the numbers a
/// camera writes light down in, which go as its root, more or less.
fn as_light() -> [f32; 256] {
    core::array::from_fn(|value| (value as f32 / 255.0).powf(GAMMA))
}

/// How well a picture's marks match where a map says they are: the brightness
/// of their light parts less that of their dark parts, averaged over all of
/// them.
fn mark_score(gray: &Gray, layout: &DenseLayout, transform: &Homography) -> f64 {
    let mut total = 0.0;
    let mut count = 0.0;
    for mark in &layout.marks {
        let sample = |dx: f64, dy: f64| {
            transform.map(Point::new(mark.x + dx, mark.y + dy)).and_then(|at| luma(gray, at))
        };
        // The middle of the core, and the middle of the border on each side.
        let core = [(-0.5, -0.5), (0.5, -0.5), (-0.5, 0.5), (0.5, 0.5)];
        let border = [(-3.0, 0.0), (3.0, 0.0), (0.0, -3.0), (0.0, 3.0)];
        let mean = |points: &[(f64, f64)]| -> Option<f64> {
            let mut sum = 0.0;
            for &(dx, dy) in points {
                sum += sample(dx, dy)?;
            }
            Some(sum / points.len() as f64)
        };
        if let (Some(inner), Some(outer)) = (mean(&core), mean(&border)) {
            total += if mark.inverted { inner - outer } else { outer - inner };
            count += 1.0;
        }
    }
    if count > 0.0 { total / count } else { 0.0 }
}

/// How far each part of a frame is from where the homography puts it.
struct Field {
    /// Patches across and down.
    cols: usize,
    rows: usize,
    /// Patch middles are this many modules apart.
    pitch: (f64, f64),
    dx: Vec<f64>,
    dy: Vec<f64>,
}

impl Field {
    /// Measures it: coarsely from the marks, which can be located outright,
    /// and then finely from the lattice of the modules, which is known to
    /// within a module and no better.
    fn measure(gray: &Gray, layout: &DenseLayout, transform: &Homography) -> Self {
        let profile = &layout.profile;
        let marks = locate_marks(gray, layout, transform);
        let coarse = Self {
            cols: profile.tile_cols as usize,
            rows: profile.tile_rows as usize,
            pitch: (f64::from(profile.tile_width), f64::from(profile.tile_height)),
            dx: marks.iter().map(|m| m.0).collect(),
            dy: marks.iter().map(|m| m.1).collect(),
        }
        .settled();

        let cols = (profile.width() / PATCH).max(2) as usize;
        let rows = (profile.height() / PATCH).max(2) as usize;
        let pitch =
            (f64::from(profile.width()) / cols as f64, f64::from(profile.height()) / rows as f64);

        let mut across = vec![(0.0f64, 0.0f64); cols * rows];
        let mut down = vec![(0.0f64, 0.0f64); cols * rows];
        let warp = Warp::new(layout, transform, &coarse);
        let table = as_written();

        let step = 1.0 / SAMPLES_PER_MODULE as f32;

        // Along the middle of every other row of modules, and down the middle
        // of every other column. Wherever the brightness changes between two
        // samples there is an edge between them, and every edge is on a line
        // of the lattice.
        let mut scan = |horizontal: bool| {
            let (lines, length, along, beside) = if horizontal {
                (profile.height(), profile.width(), pitch.0 as f32, pitch.1 as f32)
            } else {
                (profile.width(), profile.height(), pitch.1 as f32, pitch.0 as f32)
            };
            let (patches_along, patches_beside) =
                if horizontal { (cols, rows) } else { (rows, cols) };
            let mut sums = vec![(0.0f32, 0.0f32); patches_along];

            for line in (0..lines).step_by(4) {
                let fixed = line as f32 + 0.5;
                sums.fill((0.0, 0.0));
                let mut previous: Option<f32> = None;
                for index in 0..=length * SAMPLES_PER_MODULE {
                    let moving = index as f32 * step;
                    let (x, y) = if horizontal { (moving, fixed) } else { (fixed, moving) };
                    let (px, py) = warp.at(x, y);
                    let here = shade(gray, &table, px, py);

                    if let (Some(a), Some(b)) = (previous, here) {
                        let patch =
                            (((moving - step / 2.0) / along) as usize).min(patches_along - 1);
                        let strength = (a - b).abs();
                        let (cos, sin) = ROUND[(index % SAMPLES_PER_MODULE) as usize];
                        sums[patch].0 += strength * cos;
                        sums[patch].1 += strength * sin;
                    }
                    previous = here;
                }

                let other = ((fixed / beside) as usize).min(patches_beside - 1);
                for (patch, sum) in sums.iter().enumerate() {
                    let slot = if horizontal {
                        &mut across[other * cols + patch]
                    } else {
                        &mut down[patch * cols + other]
                    };
                    slot.0 += f64::from(sum.0);
                    slot.1 += f64::from(sum.1);
                }
            }
        };
        scan(true);
        scan(false);

        // The lattice was read through the coarse field, so what its phase
        // gives is what the coarse field left over, give or take a whole
        // number of modules.
        let turn = |sum: (f64, f64)| sum.1.atan2(sum.0) / core::f64::consts::TAU;
        let measured: Vec<(f64, f64)> = (0..cols * rows)
            .map(|at| {
                let (x, y) =
                    (((at % cols) as f64 + 0.5) * pitch.0, ((at / cols) as f64 + 0.5) * pitch.1);
                let (cx, cy) = coarse.at(x, y);
                (cx + turn(across[at]), cy + turn(down[at]))
            })
            .collect();

        // A patch with next to no edges in it has a phase, as everything has,
        // and it means nothing. A patch caught as the code changed is one.
        let weight = |at: usize| across[at].0.hypot(across[at].1).min(down[at].0.hypot(down[at].1));
        let mut weights: Vec<f64> = (0..cols * rows).map(weight).collect();
        weights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let faint = weights[weights.len() / 2] * FAINT;

        let mut fine =
            Self { cols, rows, pitch, dx: vec![0.0; cols * rows], dy: vec![0.0; cols * rows] };
        let mut known = vec![false; cols * rows];

        // Among the marks the coarse field is measured, and what it left over
        // is a fraction of a module: the whole number is nought.
        let reach = |values: &mut dyn Iterator<Item = f64>| {
            values.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), v| {
                (low.min(v), high.max(v))
            })
        };
        let (left, right) = reach(&mut layout.marks.iter().map(|mark| mark.x));
        let (top, bottom) = reach(&mut layout.marks.iter().map(|mark| mark.y));
        for at in 0..cols * rows {
            let (x, y) =
                (((at % cols) as f64 + 0.5) * pitch.0, ((at / cols) as f64 + 0.5) * pitch.1);
            let among = x > left - 2.0 && x < right + 2.0 && y > top - 2.0 && y < bottom + 2.0;
            if among && weight(at) >= faint {
                (fine.dx[at], fine.dy[at]) = measured[at];
                known[at] = true;
            }
        }
        fine.outvote(&known);

        // Beyond the marks the coarse field is a guess, and a lens bends a
        // picture most at its edges, so the guess can be out by more than the
        // half module a phase can be trusted across. The field just inside is
        // a better guide, because from one patch to the next it hardly moves:
        // each patch takes the whole number that puts it nearest to the
        // patches beside it that are already settled. One too faint to have
        // been measured takes what they say outright.
        loop {
            let mut settled = Vec::new();
            for at in (0..cols * rows).filter(|&at| !known[at]) {
                let (row, col) = (at / cols, at % cols);
                let (mut sum_x, mut sum_y, mut count) = (0.0, 0.0, 0.0);
                for r in row.saturating_sub(1)..=(row + 1).min(rows - 1) {
                    for c in col.saturating_sub(1)..=(col + 1).min(cols - 1) {
                        if known[r * cols + c] {
                            sum_x += fine.dx[r * cols + c];
                            sum_y += fine.dy[r * cols + c];
                            count += 1.0;
                        }
                    }
                }
                if count == 0.0 {
                    continue;
                }
                let (x, y) = (sum_x / count, sum_y / count);
                if weight(at) < faint {
                    settled.push((at, x, y));
                } else {
                    let nearest =
                        |measured: f64, expected: f64| measured + (expected - measured).round();
                    settled.push((at, nearest(measured[at].0, x), nearest(measured[at].1, y)));
                }
            }
            if settled.is_empty() {
                break;
            }
            for (at, x, y) in settled {
                fine.dx[at] = x;
                fine.dy[at] = y;
                known[at] = true;
            }
        }
        fine.settled()
    }

    /// Replaces each patch that is known with the median of itself and the
    /// known patches round it.
    fn outvote(&mut self, known: &[bool]) {
        let (cols, rows) = (self.cols, self.rows);
        let smooth = |field: &[f64]| -> Vec<f64> {
            (0..rows * cols)
                .map(|at| {
                    if !known[at] {
                        return field[at];
                    }
                    let (row, col) = (at / cols, at % cols);
                    let mut near = Vec::with_capacity(9);
                    for r in row.saturating_sub(1)..=(row + 1).min(rows - 1) {
                        for c in col.saturating_sub(1)..=(col + 1).min(cols - 1) {
                            if known[r * cols + c] {
                                near.push(field[r * cols + c]);
                            }
                        }
                    }
                    near.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
                    near[near.len() / 2]
                })
                .collect()
        };
        self.dx = smooth(&self.dx);
        self.dy = smooth(&self.dy);
    }

    /// The same field with each patch replaced by the median of itself and the
    /// patches round it, so that one measured wrongly is outvoted.
    fn settled(mut self) -> Self {
        let median = |values: &mut Vec<f64>| {
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
            values[values.len() / 2]
        };
        let smooth = |field: &[f64], cols: usize, rows: usize| -> Vec<f64> {
            (0..rows * cols)
                .map(|at| {
                    let (row, col) = (at / cols, at % cols);
                    let mut near = Vec::with_capacity(9);
                    for r in row.saturating_sub(1)..=(row + 1).min(rows - 1) {
                        for c in col.saturating_sub(1)..=(col + 1).min(cols - 1) {
                            near.push(field[r * cols + c]);
                        }
                    }
                    median(&mut near)
                })
                .collect()
        };
        self.dx = smooth(&self.dx, self.cols, self.rows);
        self.dy = smooth(&self.dy, self.cols, self.rows);
        self
    }

    /// The displacement at a position in modules.
    fn at(&self, x: f64, y: f64) -> (f64, f64) {
        if self.cols < 2 || self.rows < 2 {
            return (
                self.dx.first().copied().unwrap_or(0.0),
                self.dy.first().copied().unwrap_or(0.0),
            );
        }
        let (px, py) = (x / self.pitch.0 - 0.5, y / self.pitch.1 - 0.5);
        let col = px.floor().clamp(0.0, (self.cols - 2) as f64);
        let row = py.floor().clamp(0.0, (self.rows - 2) as f64);
        // Beyond the outermost patches the slope between the last two is
        // carried on, but not far.
        let (fx, fy) = ((px - col).clamp(-0.6, 1.6), (py - row).clamp(-0.6, 1.6));
        let at = row as usize * self.cols + col as usize;

        let blend = |field: &[f64]| {
            let top = field[at] + (field[at + 1] - field[at]) * fx;
            let bottom =
                field[at + self.cols] + (field[at + self.cols + 1] - field[at + self.cols]) * fx;
            top + (bottom - top) * fy
        };
        (blend(&self.dx), blend(&self.dy))
    }

    fn largest(&self) -> f64 {
        self.dx.iter().zip(self.dy.iter()).map(|(x, y)| x.hypot(*y)).fold(0.0, f64::max)
    }
}

/// Finds each mark, and says how far it is from where the homography puts it,
/// in modules.
///
/// A mark is a square of one shade in a square of the other, so where it is
/// can be found by laying that shape over the picture everywhere nearby and
/// seeing where it fits best. Unlike the phase of a lattice, the answer is not
/// one of several a module apart.
fn locate_marks(gray: &Gray, layout: &DenseLayout, transform: &Homography) -> Vec<(f64, f64)> {
    const STEP: f64 = 0.5;
    let reach = (MARK_REACH / STEP) as i32;
    let half = (f64::from(MARK_BOX) / 2.0 / STEP) as i32;
    let core = (f64::from(MARK_CORE) / 2.0 / STEP) as i32;
    let side = (2 * (reach + half) + 1) as usize;

    layout
        .marks
        .iter()
        .map(|mark| {
            // Brightness on a grid of half modules, as a summed-area table.
            let mut table = vec![0.0f64; (side + 1) * (side + 1)];
            for j in 0..side {
                let mut run = 0.0;
                for i in 0..side {
                    // The middle of each step, so that a box of a whole number
                    // of steps is as far to one side of its centre as to the
                    // other.
                    let dx = (f64::from(i as i32 - reach - half) + 0.5) * STEP;
                    let dy = (f64::from(j as i32 - reach - half) + 0.5) * STEP;
                    let value = transform
                        .map(Point::new(mark.x + dx, mark.y + dy))
                        .and_then(|at| luma(gray, at))
                        .unwrap_or(0.5);
                    run += value;
                    table[(j + 1) * (side + 1) + i + 1] = table[j * (side + 1) + i + 1] + run;
                }
            }
            let sum = |x0: i32, y0: i32, x1: i32, y1: i32| {
                let at = |x: i32, y: i32| table[y as usize * (side + 1) + x as usize];
                at(x1, y1) - at(x0, y1) - at(x1, y0) + at(x0, y0)
            };

            let score = |ox: i32, oy: i32| {
                let (cx, cy) = (reach + half + ox, reach + half + oy);
                let whole = sum(cx - half, cy - half, cx + half, cy + half);
                let inner = sum(cx - core, cy - core, cx + core, cy + core);
                let (area, inner_area) = (f64::from(4 * half * half), f64::from(4 * core * core));
                let contrast = (whole - inner) / (area - inner_area) - inner / inner_area;
                if mark.inverted { -contrast } else { contrast }
            };

            let mut best = (f64::NEG_INFINITY, 0, 0);
            for oy in -reach..=reach {
                for ox in -reach..=reach {
                    let found = score(ox, oy);
                    if found > best.0 {
                        best = (found, ox, oy);
                    }
                }
            }

            // Between the steps, by the parabola through the best and the two
            // either side of it.
            let refine = |before: f64, at: f64, after: f64| {
                let bend = before - 2.0 * at + after;
                if bend.abs() < 1e-9 {
                    0.0
                } else {
                    (0.5 * (before - after) / bend).clamp(-0.5, 0.5)
                }
            };
            let (ox, oy) = (best.1, best.2);
            let inside = |v: i32| v > -reach && v < reach;
            let fx =
                if inside(ox) { refine(score(ox - 1, oy), best.0, score(ox + 1, oy)) } else { 0.0 };
            let fy =
                if inside(oy) { refine(score(ox, oy - 1), best.0, score(ox, oy + 1)) } else { 0.0 };
            ((f64::from(ox) + fx) * STEP, (f64::from(oy) + fy) * STEP)
        })
        .collect()
}

/// The light at the middle of every module, row-major.
fn sample_modules(
    gray: &Gray,
    layout: &DenseLayout,
    transform: &Homography,
    field: &Field,
) -> Vec<f32> {
    let (width, height) = (layout.profile.width(), layout.profile.height());
    let warp = Warp::new(layout, transform, field);
    let table = as_light();

    let mut samples = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        for col in 0..width {
            let (x, y) = warp.at(col as f32 + 0.5, row as f32 + 0.5);
            // A module that is not in the picture is as likely dark as light.
            samples.push(shade(gray, &table, x, y).unwrap_or(0.2));
        }
    }
    samples
}

/// Side of the neighbourhoods black and white are measured in, in modules.
const LEVEL_BLOCK: u32 = 24;

/// Times the light of each module's neighbours is taken back out of it.
const SWEEPS: usize = 2;

/// Times the channel is measured again from what the sweeps decided.
const ROUNDS: usize = 2;

/// What a camera does to a module: how much of its own light lands at its
/// middle, and how much of each neighbour's.
///
/// At three camera pixels to a module a lens, a colour filter and a sharpening
/// filter between them spread a module's light over its neighbours, so that a
/// light module among dark ones comes out grey. Held against a threshold it is
/// read wrongly, and no amount of parity is a match for every lone module in
/// the frame. But the spreading is the same for every module of a tile, and a
/// tile has some thousands of modules to measure it from.
///
/// Measured for each tile rather than for the frame, because it also takes up
/// what is left of the grid being slightly out: a grid a fifth of a module to
/// the left is a channel with more of the neighbour on the right in it.
#[derive(Debug, Clone, Copy)]
struct Channel {
    /// Row-major over the three-by-three neighbourhood; the module's own is
    /// the fifth.
    taps: [f32; 9],
    offset: f32,
}

/// One in this many modules is used to measure a channel. A tile has
/// thousands and a channel has ten numbers in it.
const FIT_STRIDE: usize = 3;

/// Measures the taps that best explain `target` from `regress`, over a
/// rectangle of modules.
///
/// `target` is `width` across. `regress` is two wider and two taller, with a
/// border all round it, so that every module has eight neighbours.
fn fit_channel(
    target: &[f32],
    regress: &[f32],
    width: usize,
    rows: core::ops::Range<usize>,
    cols: core::ops::Range<usize>,
) -> Option<Channel> {
    const N: usize = 10;
    let padded = width + 2;
    let mut normal = [[0.0f64; N + 1]; N];

    for row in rows {
        let first = cols.start + (row * 2) % FIT_STRIDE;
        for col in (first..cols.end).step_by(FIT_STRIDE) {
            let at = row * padded + col;
            let terms: [f32; N] = [
                regress[at],
                regress[at + 1],
                regress[at + 2],
                regress[at + padded],
                regress[at + padded + 1],
                regress[at + padded + 2],
                regress[at + 2 * padded],
                regress[at + 2 * padded + 1],
                regress[at + 2 * padded + 2],
                1.0,
            ];
            let seen = target[row * width + col];
            for i in 0..N {
                for j in i..N {
                    normal[i][j] += f64::from(terms[i] * terms[j]);
                }
                normal[i][N] += f64::from(terms[i] * seen);
            }
        }
    }
    for i in 0..N {
        for j in 0..i {
            normal[i][j] = normal[j][i];
        }
        // A whisker of reluctance, so that a rectangle of fixed modules, which
        // has no two neighbourhoods alike enough to tell its taps apart, gives
        // small taps rather than none.
        normal[i][i] += 1e-3;
    }

    // Gaussian elimination, pivoting on the largest of each column.
    for pivot in 0..N {
        let best = (pivot..N).max_by(|&a, &b| {
            normal[a][pivot]
                .abs()
                .partial_cmp(&normal[b][pivot].abs())
                .unwrap_or(core::cmp::Ordering::Equal)
        })?;
        normal.swap(pivot, best);
        if normal[pivot][pivot].abs() < 1e-9 {
            return None;
        }
        for row in 0..N {
            if row == pivot {
                continue;
            }
            let factor = normal[row][pivot] / normal[pivot][pivot];
            for col in pivot..=N {
                normal[row][col] -= factor * normal[pivot][col];
            }
        }
    }

    let solved: [f32; N] = core::array::from_fn(|i| (normal[i][N] / normal[i][i]) as f32);
    let mut taps = [0.0f32; 9];
    taps.copy_from_slice(&solved[..9]);
    Some(Channel { taps, offset: solved[9] })
}

/// Solves three equations in three unknowns.
fn solve3(mut m: [[f64; 4]; 3]) -> Option<[f32; 3]> {
    for pivot in 0..3 {
        let best = (pivot..3).max_by(|&a, &b| {
            m[a][pivot].abs().partial_cmp(&m[b][pivot].abs()).unwrap_or(core::cmp::Ordering::Equal)
        })?;
        m.swap(pivot, best);
        if m[pivot][pivot].abs() < 1e-9 {
            return None;
        }
        for row in 0..3 {
            if row != pivot {
                let factor = m[row][pivot] / m[pivot][pivot];
                for col in pivot..4 {
                    m[row][col] -= factor * m[pivot][col];
                }
            }
        }
    }
    Some(core::array::from_fn(|i| (m[i][3] / m[i][i]) as f32))
}

/// A rectangle of modules on its way to being decided: a frame, or one tile
/// of one.
struct Region {
    width: usize,
    height: usize,
    /// Brightness of each module with the black and white of its
    /// neighbourhood taken out: minus a half for black, a half for white.
    seen: Vec<f32>,
    /// What each module is believed to be, from minus one for dark to one for
    /// light, with a border of one module all round.
    belief: Vec<f32>,
    /// Whether each module is one the layout fixes.
    fixed: Vec<bool>,
    /// The rectangles a channel is measured over.
    tiles: Vec<(core::ops::Range<usize>, core::ops::Range<usize>)>,
}

impl Region {
    /// `beyond` is what lies past the edge: light round a frame, which has a
    /// margin, and nothing that is known round a tile.
    fn new(
        width: usize,
        height: usize,
        seen: Vec<f32>,
        fixed: &[Option<bool>],
        beyond: f32,
        tiles: Vec<(core::ops::Range<usize>, core::ops::Range<usize>)>,
    ) -> Self {
        let padded = width + 2;
        let mut belief = vec![beyond; padded * (height + 2)];
        for row in 0..height {
            for col in 0..width {
                let at = row * width + col;
                belief[(row + 1) * padded + col + 1] = match fixed[at] {
                    Some(true) => -1.0,
                    Some(false) => 1.0,
                    None => (2.0 * seen[at]).clamp(-1.0, 1.0),
                };
            }
        }
        let fixed = fixed.iter().map(Option::is_some).collect();
        Self { width, height, seen, belief, fixed, tiles }
    }

    /// Takes the light of each module's neighbours back out of it, tile by
    /// tile, and decides what is left.
    fn settle(&mut self) {
        let padded = self.width + 2;
        for _ in 0..ROUNDS {
            let decided: Vec<f32> = self
                .belief
                .iter()
                .map(|&value| {
                    if value > 0.0 {
                        1.0
                    } else if value < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }
                })
                .collect();
            let channels: Vec<Option<Channel>> = self
                .tiles
                .iter()
                .map(|(rows, cols)| {
                    fit_channel(&self.seen, &decided, self.width, rows.clone(), cols.clone())
                        // A module none of whose own light reaches its own
                        // middle cannot be read, whatever else is known.
                        .filter(|channel| channel.taps[4] > 0.05)
                })
                .collect();

            for _ in 0..SWEEPS {
                for ((rows, cols), channel) in self.tiles.iter().zip(channels.iter()) {
                    let Some(channel) = channel else { continue };
                    let taps = channel.taps;
                    let own = 1.0 / taps[4];
                    for row in rows.clone() {
                        for col in cols.clone() {
                            if self.fixed[row * self.width + col] {
                                continue;
                            }
                            let at = row * padded + col;
                            let belief = &self.belief;
                            let others = taps[0] * belief[at]
                                + taps[1] * belief[at + 1]
                                + taps[2] * belief[at + 2]
                                + taps[3] * belief[at + padded]
                                + taps[5] * belief[at + padded + 2]
                                + taps[6] * belief[at + 2 * padded]
                                + taps[7] * belief[at + 2 * padded + 1]
                                + taps[8] * belief[at + 2 * padded + 2];
                            let rest = self.seen[row * self.width + col] - channel.offset - others;
                            self.belief[at + padded + 1] = (rest * own).clamp(-1.0, 1.0);
                        }
                    }
                }
            }
        }
    }

    /// Takes out of what was seen whatever a code that is known put there.
    /// Says whether it had put anything.
    ///
    /// A display does not change from one code to the next at once, and a
    /// camera's shutter is open for a while, so a picture taken as the code
    /// changes has the light of both in it. Neither can be read from the sum.
    /// But the code before was read from the picture before, and what is known
    /// can be taken away.
    fn take_out(&mut self, known: &[i8]) -> bool {
        let padded = self.width + 2;
        let mut regress = vec![0.0f32; padded * (self.height + 2)];
        let mut alike = 0.0f32;
        for row in 0..self.height {
            for col in 0..self.width {
                let at = row * self.width + col;
                let value = f32::from(known[at]);
                regress[(row + 1) * padded + col + 1] = value;
                if !self.fixed[at] {
                    alike += value * self.seen[at];
                }
            }
        }
        // Two codes that have nothing to do with each other agree on half
        // their modules, give or take a few.
        if alike / (self.seen.len() as f32) < PRESENT / 2.0 {
            return false;
        }

        let Some(channel) =
            fit_channel(&self.seen, &regress, self.width, 0..self.height, 0..self.width)
        else {
            return false;
        };
        if channel.taps[4] < PRESENT {
            return false;
        }

        let mut there = vec![0.0f32; self.seen.len()];
        for row in 0..self.height {
            for col in 0..self.width {
                let at = row * padded + col;
                for (tap, weight) in channel.taps.iter().enumerate() {
                    there[row * self.width + col] +=
                        weight * regress[at + (tap / 3) * padded + tap % 3];
                }
            }
        }

        // A shutter rolls across a tile in a few milliseconds, and in a few
        // milliseconds a display gets some way from one code to the next. So
        // there is more of the known code at one side of a tile than at the
        // other, and how much more is measured.
        let (mid_row, mid_col) = (self.height as f32 / 2.0, self.width as f32 / 2.0);
        let mut normal = [[0.0f64; 4]; 3];
        for row in 0..self.height {
            for col in 0..self.width {
                let at = row * self.width + col;
                let terms = [
                    there[at],
                    there[at] * (row as f32 - mid_row) / mid_row,
                    there[at] * (col as f32 - mid_col) / mid_col,
                ];
                for i in 0..3 {
                    for j in 0..3 {
                        normal[i][j] += f64::from(terms[i] * terms[j]);
                    }
                    normal[i][3] += f64::from(terms[i] * (self.seen[at] - channel.offset));
                }
            }
        }
        let slope = solve3(normal).unwrap_or([1.0, 0.0, 0.0]);

        for row in 0..self.height {
            for col in 0..self.width {
                let at = row * self.width + col;
                let gain = slope[0]
                    + slope[1] * (row as f32 - mid_row) / mid_row
                    + slope[2] * (col as f32 - mid_col) / mid_col;
                self.seen[at] -= there[at] * gain;
            }
        }

        // What the layout fixes was in the code that is known as it is in the
        // one that is not, and so much of it has just been taken out.
        true
    }

    /// The modules of the region, without the border: whether each is
    /// believed light, and how firmly.
    fn beliefs(&self) -> Vec<f32> {
        let padded = self.width + 2;
        (0..self.height)
            .flat_map(|row| {
                let from = (row + 1) * padded + 1;
                self.belief[from..from + self.width].iter().copied()
            })
            .collect()
    }
}

/// How much of a tile's light a known code must account for, as a fraction of
/// the range between dark and light, before it is believed to be in it.
const PRESENT: f32 = 0.06;

/// How far from grey, on the whole, the modules of a tile must be for there
/// to be a code in them, as a fraction of the range between dark and light.
const FAINTEST: f32 = 0.03;

/// What a module's brightness is multiplied by to be kept in a byte.
const KEPT_SCALE: f32 = 100.0;

/// The black and white of each module's neighbourhood taken out of it: minus
/// a half for black, a half for white.
fn level(layout: &DenseLayout, samples: &[f32]) -> Vec<f32> {
    let (width, height) = (layout.profile.width(), layout.profile.height());
    let (cols, rows) = (width.div_ceil(LEVEL_BLOCK), height.div_ceil(LEVEL_BLOCK));

    // Half of every neighbourhood is dark and half is light, whatever the
    // file holds, because the bytes are whitened before they are painted.
    let mut lists: Vec<Vec<f32>> = vec![Vec::new(); (cols * rows) as usize];
    for row in 0..height {
        for col in 0..width {
            lists[((row / LEVEL_BLOCK) * cols + col / LEVEL_BLOCK) as usize]
                .push(samples[(row * width + col) as usize]);
        }
    }
    let levels: Vec<(f32, f32)> = lists
        .iter_mut()
        .map(|list| {
            if list.is_empty() {
                return (0.0, 1.0);
            }
            let (low, high) = (list.len() * 15 / 100, list.len() * 85 / 100);
            let order = |a: &f32, b: &f32| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal);
            let dark = *list.select_nth_unstable_by(low, order).1;
            let light = *list.select_nth_unstable_by(high, order).1;
            (dark, light.max(dark + 0.01))
        })
        .collect();

    let mut seen = Vec::with_capacity(samples.len());
    for row in 0..height {
        for col in 0..width {
            // Between the levels of the four neighbourhoods nearest.
            let place = |at: u32, limit: u32| {
                ((at as f32 + 0.5) / LEVEL_BLOCK as f32 - 0.5).clamp(0.0, (limit - 1) as f32)
            };
            let (x, y) = (place(col, cols), place(row, rows));
            let (x0, y0) = (x.floor() as u32, y.floor() as u32);
            let (x1, y1) = ((x0 + 1).min(cols - 1), (y0 + 1).min(rows - 1));
            let (fx, fy) = (x - x0 as f32, y - y0 as f32);
            let blend = |pick: fn(&(f32, f32)) -> f32| {
                let at = |r: u32, c: u32| pick(&levels[(r * cols + c) as usize]);
                let top = at(y0, x0) + (at(y0, x1) - at(y0, x0)) * fx;
                let bottom = at(y1, x0) + (at(y1, x1) - at(y1, x0)) * fx;
                top + (bottom - top) * fy
            };
            let (dark, light) = (blend(|l| l.0), blend(|l| l.1));
            let sample = samples[(row * width + col) as usize];
            seen.push((sample - dark) / (light - dark) - 0.5);
        }
    }
    seen
}

impl DenseLayout {
    /// The rows and columns of a tile.
    fn bounds(&self, tile: u32) -> (core::ops::Range<usize>, core::ops::Range<usize>) {
        let profile = &self.profile;
        let (row, col) = ((tile / profile.tile_cols) as usize, (tile % profile.tile_cols) as usize);
        let (tall, wide) = (profile.tile_height as usize, profile.tile_width as usize);
        (row * tall..(row + 1) * tall, col * wide..(col + 1) * wide)
    }

    /// Copies a tile's rectangle out of something that has a value for every
    /// module of the frame.
    fn cut<T: Copy>(&self, tile: u32, frame: &[T]) -> Vec<T> {
        let (rows, cols) = self.bounds(tile);
        let width = self.profile.width() as usize;
        rows.flat_map(|row| frame[row * width + cols.start..row * width + cols.end].iter().copied())
            .collect()
    }

    /// Where in its tile's rectangle each of the tile's modules is, in the
    /// order they are filled.
    fn places(&self, tile: u32) -> Vec<usize> {
        let (rows, cols) = self.bounds(tile);
        let width = self.profile.width() as usize;
        self.tiles[tile as usize]
            .iter()
            .map(|&module| {
                let (row, col) = (module as usize / width, module as usize % width);
                (row - rows.start) * cols.len() + col - cols.start
            })
            .collect()
    }

    /// Every module of a tile as it was painted, row-major over the tile's
    /// rectangle: one for light, minus one for dark.
    fn painted(&self, tile: &Tile) -> Vec<i8> {
        let index = u32::from(tile.index);
        let mut modules: Vec<i8> = self
            .cut(index, &self.fixed)
            .iter()
            .map(|fixed| if *fixed == Some(true) { -1 } else { 1 })
            .collect();
        let raw = self.protect(index, &tile.encode(self.capacity(index)));
        for (position, place) in self.places(index).into_iter().enumerate() {
            let light =
                raw.get(position / 8).is_some_and(|byte| (byte >> (7 - position % 8)) & 1 == 1);
            modules[place] = if light { 1 } else { -1 };
        }
        modules
    }

    /// Turns what is believed of a tile's modules into the tile, if parity
    /// can. `beliefs` is row-major over the tile's rectangle.
    fn read_tile(&self, tile: u32, beliefs: &[f32]) -> Option<(Tile, usize)> {
        let mut raw = vec![0u8; self.codes[tile as usize].raw];
        // How sure the least sure bit of each byte is.
        let mut sure = vec![f32::INFINITY; raw.len()];
        for (position, place) in self.places(tile).into_iter().enumerate() {
            let value = beliefs[place];
            if let Some(byte) = raw.get_mut(position / 8) {
                if value > 0.0 {
                    *byte |= 1 << (7 - position % 8);
                }
                sure[position / 8] = sure[position / 8].min(value.abs());
            }
        }
        self.recover(tile, &raw, &sure)
            .filter(|(found, _)| found.profile == self.profile.id && u32::from(found.index) == tile)
    }

    /// Reads a tile that would not read, now that codes that may have been in
    /// the picture with it are known.
    fn read_again(&self, unread: &Unread, known: &[&Known]) -> Option<Tile> {
        let seen = unread.seen.iter().map(|&v| f32::from(v) / KEPT_SCALE).collect();
        self.read_under(u32::from(unread.index), seen, known)
    }

    /// Takes the codes that are known out of what was seen of a tile, and
    /// reads what is left.
    fn read_under(&self, index: u32, seen: Vec<f32>, known: &[&Known]) -> Option<Tile> {
        let (rows, cols) = self.bounds(index);
        if seen.len() != rows.len() * cols.len() {
            return None;
        }

        let mut region = Region::new(
            cols.len(),
            rows.len(),
            seen,
            &self.cut(index, &self.fixed),
            0.0,
            vec![(0..rows.len(), 0..cols.len())],
        );
        let mut taken = false;
        for known in known {
            taken |= region.take_out(&known.modules);
        }
        if !taken {
            return None;
        }
        // What is left is fainter than what was there, and is measured afresh.
        // If it is as faint as a camera's noise there was nothing else there.
        if region.restart() < FAINTEST {
            return None;
        }
        region.settle();
        self.read_tile(index, &region.beliefs()).map(|(tile, _)| tile)
    }
}

impl Region {
    /// Believes of each module that is not fixed what is now seen of it. Says
    /// how far from grey the modules are, on the whole.
    fn restart(&mut self) -> f32 {
        let padded = self.width + 2;
        let mut spread = 0.0f32;
        for (at, &seen) in self.seen.iter().enumerate() {
            if !self.fixed[at] {
                spread += seen.abs();
            }
        }
        let found = spread / self.seen.len() as f32;
        let spread = found.max(0.02);
        for row in 0..self.height {
            for col in 0..self.width {
                let at = row * self.width + col;
                if !self.fixed[at] {
                    self.belief[(row + 1) * padded + col + 1] =
                        (self.seen[at] / spread).clamp(-1.0, 1.0);
                }
            }
        }
        found
    }
}

/// A tile that would not read, as it was seen, kept in case it will once more
/// is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    /// Which tile of the frame it is.
    pub index: u8,
    /// The brightness of every module of the tile's rectangle, row-major, with
    /// black and white taken out: about minus fifty for black and fifty for
    /// white.
    pub seen: Vec<i8>,
}

/// A tile that was read, as it was painted.
#[derive(Debug, Clone)]
struct Known {
    code: u32,
    modules: Vec<i8>,
}

// --- receiving --------------------------------------------------------------------

/// What one picture added to a transfer.
#[derive(Debug, Clone, Default)]
pub struct DenseReport {
    /// Whether a frame was found in the picture.
    pub located: bool,
    /// Tiles read from the picture.
    pub tiles_read: usize,
    /// Tiles whose error correction or checksum failed.
    pub tiles_lost: usize,
    /// Tiles that belonged to some other transfer.
    pub tiles_foreign: usize,
    /// Tiles, of this picture or of one before it, that would not read and
    /// now have.
    pub tiles_read_again: usize,
    /// Symbols that had not been seen before.
    pub new_symbols: usize,
}

/// A tile waiting for the code that was in the picture with it to be known.
struct Waiting {
    picture: u64,
    unread: Unread,
    /// The codes that have been taken out of it, to no avail.
    tried: Vec<u32>,
}

/// Codes kept for each tile. A picture has the light of two in it, or three.
const KNOWN_KEPT: usize = 4;

/// Pictures a tile waits for. The code it is waiting for is on the screen for
/// one or two.
const PICTURES_WAITED: u64 = 6;

/// Collects tiles until it can rebuild the file.
#[derive(Default)]
pub struct DenseReceiver {
    session: Option<u32>,
    manifest: Option<Manifest>,
    transport: Option<TransportDecoder>,
    /// Symbols that arrived before the manifest did.
    waiting: Vec<Vec<u8>>,
    layout: Option<DenseLayout>,
    /// For each tile of the frame, the last few codes it was read from.
    known: Vec<Vec<Known>>,
    unread: Vec<Waiting>,
    pictures: u64,
}

/// Symbols to hold before the manifest arrives. Bounded because it is filled
/// from whatever a camera is pointed at.
const MAX_WAITING: usize = 16_384;

impl DenseReceiver {
    /// A receiver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The manifest, once a tile has carried one.
    #[must_use]
    pub const fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// Symbols collected and symbols needed, once the manifest has arrived.
    #[must_use]
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.transport.as_ref().map(|t| (t.symbols_accepted(), t.symbols_needed()))
    }

    /// Whether enough has been collected to rebuild the file.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.transport.as_ref().is_some_and(TransportDecoder::is_complete)
    }

    /// Folds in what a picture yielded.
    pub fn absorb(&mut self, reading: &DenseReading) -> DenseReport {
        let mut report = DenseReport {
            located: reading.located,
            tiles_lost: reading.tiles_lost,
            ..DenseReport::default()
        };
        self.pictures += 1;

        for tile in &reading.tiles {
            self.accept(tile, &mut report);
        }

        // Tiles that would not read say nothing of what they are, so until a
        // tile that did read has said, the picture is taken at its word.
        if self.layout.is_none()
            && let Some(profile) = reading.profile
        {
            self.lay_out(profile);
        }
        if self.layout.is_some() && reading.profile == self.layout.as_ref().map(|l| l.profile.id) {
            let picture = self.pictures;
            self.unread.retain(|waiting| waiting.picture + PICTURES_WAITED > picture);
            self.unread.extend(reading.unread.iter().map(|unread| Waiting {
                picture,
                unread: unread.clone(),
                tried: Vec::new(),
            }));
            self.read_again(&mut report);
        }
        report
    }

    /// Takes a tile that was read: what it carries, and what it looked like.
    fn accept(&mut self, tile: &Tile, report: &mut DenseReport) {
        match self.session {
            Some(known) if known != tile.session => {
                report.tiles_foreign += 1;
                return;
            }
            None => self.session = Some(tile.session),
            Some(_) => {}
        }
        report.tiles_read += 1;

        match tile.kind {
            tile_kind::MANIFEST => report.new_symbols += self.take_manifest(&tile.body),
            tile_kind::SYMBOL => match self.transport.as_mut() {
                Some(transport) => {
                    if transport.push(&tile.body) {
                        report.new_symbols += 1;
                    }
                }
                None => {
                    if self.waiting.len() < MAX_WAITING {
                        self.waiting.push(tile.body.clone());
                    }
                }
            },
            _ => {}
        }

        if self.layout.as_ref().is_none_or(|layout| layout.profile.id != tile.profile) {
            self.lay_out(tile.profile);
        }
        if let (Some(layout), Some(known)) =
            (self.layout.as_ref(), self.known.get_mut(usize::from(tile.index)))
            && layout.profile.id == tile.profile
            && known.iter().all(|other| other.code != tile.code)
        {
            if known.len() >= KNOWN_KEPT {
                known.remove(0);
            }
            known.push(Known { code: tile.code, modules: layout.painted(tile) });
        }
    }

    /// Takes up the layout of a profile, and forgets what was kept under any
    /// other.
    fn lay_out(&mut self, profile: u8) {
        self.layout = DenseProfile::from_id(profile).map(DenseLayout::new);
        let tiles = self.layout.as_ref().map_or(0, |layout| layout.profile.tiles());
        self.known = vec![Vec::new(); tiles as usize];
        self.unread.clear();
    }

    /// Goes back over the tiles that would not read, for as long as going
    /// back over them reads one: a tile that is read is a code that is known,
    /// and may be what the same tile of the picture after was waiting for.
    fn read_again(&mut self, report: &mut DenseReport) {
        loop {
            let mut read = Vec::new();
            if let Some(layout) = self.layout.as_ref() {
                for (at, waiting) in self.unread.iter_mut().enumerate() {
                    let Some(known) = self.known.get(usize::from(waiting.unread.index)) else {
                        continue;
                    };
                    if known.iter().all(|known| waiting.tried.contains(&known.code)) {
                        continue;
                    }
                    waiting.tried = known.iter().map(|known| known.code).collect();
                    let known: Vec<&Known> = known.iter().rev().collect();
                    if let Some(tile) = layout.read_again(&waiting.unread, &known) {
                        read.push((at, tile));
                    }
                }
            }
            if read.is_empty() {
                return;
            }
            for (at, tile) in read.into_iter().rev() {
                self.unread.remove(at);
                report.tiles_read_again += 1;
                self.accept(&tile, report);
            }
        }
    }

    fn take_manifest(&mut self, bytes: &[u8]) -> usize {
        if self.manifest.is_some() {
            return 0;
        }
        let Ok(manifest) = Manifest::decode(bytes) else { return 0 };
        if !manifest.compression.is_supported() {
            return 0;
        }
        let Ok(mut transport) = TransportDecoder::new(manifest.oti) else { return 0 };

        let mut released = 0usize;
        for symbol in core::mem::take(&mut self.waiting) {
            if transport.push(&symbol) {
                released += 1;
            }
        }
        self.manifest = Some(manifest);
        self.transport = Some(transport);
        released
    }

    /// Rebuilds the file.
    ///
    /// # Errors
    ///
    /// Returns the stage that is missing: no manifest yet, not enough symbols
    /// and how many are short, a decompression failure, or a digest mismatch.
    pub fn finish(&mut self) -> Result<(String, Vec<u8>)> {
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
        Ok((manifest.name, file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state & 0xFF) as u8
            })
            .collect()
    }

    #[test]
    fn every_module_belongs_to_a_tile_or_is_fixed() {
        for profile in &DENSE_PROFILES {
            let layout = DenseLayout::new(profile);
            let carried: usize = layout.tiles.iter().map(Vec::len).sum();
            let fixed = layout.fixed.iter().filter(|f| f.is_some()).count();
            assert_eq!(
                carried + fixed,
                (profile.width() * profile.height()) as usize,
                "{}",
                profile.name
            );

            // Four finder boxes, and a mark in every tile.
            let expected = 4 * FINDER_BOX * FINDER_BOX + profile.tiles() * MARK_BOX * MARK_BOX;
            assert_eq!(fixed, expected as usize, "{}", profile.name);
        }
    }

    #[test]
    fn a_tile_holds_a_symbol_and_a_corner_holds_a_manifest() {
        for profile in &DENSE_PROFILES {
            let layout = DenseLayout::new(profile);
            assert!(layout.symbol_size() >= 256, "{} holds {}", profile.name, layout.symbol_size());
            assert_eq!(layout.symbol_size() % 8, 0);

            let corner = layout.capacity(0);
            assert!(corner >= 200, "{} has {corner} bytes in a corner", profile.name);
        }
    }

    #[test]
    fn a_tile_survives_as_many_bad_bytes_as_its_parity_allows() {
        let layout = DenseLayout::new(&DENSE_PROFILES[1]);
        let tile = 8;
        let payload = Tile {
            kind: tile_kind::SYMBOL,
            profile: layout.profile.id,
            session: 7,
            code: 99,
            index: tile as u8,
            body: noise(usize::from(layout.symbol_size()) + PAYLOAD_ID_LEN),
        }
        .encode(layout.capacity(tile));

        let mut raw = layout.protect(tile, &payload);
        // Each codeword repairs half its parity, and the bytes are dealt out
        // between them.
        let codewords = layout.codes[tile as usize].lengths.len();
        let bad = codewords * (layout.profile.parity as usize / 2);
        for byte in raw.iter_mut().take(bad) {
            *byte ^= 0xA5;
        }

        let sure = vec![1.0; raw.len()];
        let (recovered, repaired) = layout.recover(tile, &raw, &sure).expect("recovered");
        assert_eq!(repaired, bad);
        assert_eq!(recovered.code, 99);
    }

    #[test]
    fn a_tile_with_a_wrong_checksum_is_refused() {
        let tile =
            Tile { kind: 2, profile: 0x12, session: 1, code: 2, index: 3, body: vec![9; 40] };
        let mut payload = tile.encode(120);
        assert_eq!(Tile::decode(&payload), Some(tile));
        payload[20] ^= 1;
        assert_eq!(Tile::decode(&payload), None);
    }

    #[test]
    fn a_painted_frame_reads_back_exactly() {
        for profile in &DENSE_PROFILES {
            let file = noise(40_000);
            let mut sender = DenseTransmitter::new("a.bin", &file, profile, 5).expect("prepared");
            let image = sender.next_frame(3);

            let reading = DenseReader::new().read(&image);
            assert!(reading.located, "{} was not found", profile.name);
            assert_eq!(reading.profile, Some(profile.id));
            assert_eq!(reading.tiles_lost, 0, "{}", profile.name);
            assert_eq!(reading.tiles.len(), profile.tiles() as usize);
            assert_eq!(reading.bytes_repaired, 0, "{}", profile.name);
        }
    }

    #[test]
    fn a_frame_upside_down_reads_the_right_way_up() {
        let profile = &DENSE_PROFILES[0];
        let mut sender =
            DenseTransmitter::new("a.bin", &noise(9_000), profile, 5).expect("prepared");
        let image = sender.next_frame(4);

        let mut turned = RgbImage::filled(image.width(), image.height(), Rgb::WHITE);
        for y in 0..image.height() {
            for x in 0..image.width() {
                turned.set(image.width() - 1 - x, image.height() - 1 - y, image.get(x, y));
            }
        }

        let reading = DenseReader::new().read(&turned);
        assert!(reading.located);
        assert_eq!(reading.tiles.len(), profile.tiles() as usize);
    }

    #[test]
    fn a_file_survives_the_round_trip() {
        for profile in &DENSE_PROFILES {
            let file = noise(150_000);
            let mut sender = DenseTransmitter::new("b.bin", &file, profile, 11).expect("prepared");
            let reader = DenseReader::new();
            let mut receiver = DenseReceiver::new();

            let mut frames = 0;
            while !receiver.is_complete() && frames < 200 {
                let image = sender.next_frame(3);
                receiver.absorb(&reader.read(&image));
                frames += 1;
            }

            assert!(receiver.is_complete(), "{} never completed", profile.name);
            let (name, bytes) = receiver.finish().expect("finished");
            assert_eq!(name, "b.bin");
            assert_eq!(bytes, file);
            // Within a frame of the fewest it could take.
            assert!(
                frames <= sender.frames_per_pass() + 1,
                "{} took {frames} frames for {}",
                profile.name,
                sender.frames_per_pass()
            );
        }
    }

    /// What a camera sees of a display that is part of the way from one code
    /// to the next: so much of the light of one, and the rest of the other's.
    fn blended(first: &RgbImage, second: &RgbImage, share: f32) -> RgbImage {
        let mut out = first.clone();
        for y in 0..first.height() {
            for x in 0..first.width() {
                let light = |image: &RgbImage| (f32::from(image.get(x, y).g) / 255.0).powf(GAMMA);
                let both = share * light(first) + (1.0 - share) * light(second);
                let written = (both.powf(1.0 / GAMMA) * 255.0).round() as u8;
                out.set(x, y, Rgb::new(written, written, written));
            }
        }
        out
    }

    #[test]
    fn the_code_under_a_code_is_read_as_well() {
        let profile = &DENSE_PROFILES[1];
        let mut sender =
            DenseTransmitter::new("d.bin", &noise(90_000), profile, 3).expect("prepared");
        let first = sender.next_frame(3);
        let second = sender.next_frame(3);

        let reading = DenseReader::new().read(&blended(&first, &second, 0.7));
        let tiles = profile.tiles() as usize;
        assert_eq!(reading.tiles.iter().filter(|tile| tile.code == 0).count(), tiles);
        // The four tiles with the manifest in them say the same from one code
        // to the next, all but the number of the code, and what two codes have
        // in common cannot be taken out of one and left in the other.
        assert_eq!(reading.tiles.iter().filter(|tile| tile.code == 1).count(), tiles - 4);
        assert!(reading.tiles.iter().all(|tile| tile.code == 0 || tile.kind == tile_kind::SYMBOL));
    }

    #[test]
    fn a_tile_that_would_not_read_does_once_the_code_with_it_is_known() {
        let profile = &DENSE_PROFILES[1];
        let file = noise(90_000);
        let mut sender = DenseTransmitter::new("e.bin", &file, profile, 3).expect("prepared");
        let first = sender.next_frame(3);
        let second = sender.next_frame(3);
        let reader = DenseReader::new();

        // Half of each: neither can be told from the other.
        let both = reader.read(&blended(&first, &second, 0.5));
        assert!(both.tiles.is_empty(), "{} tiles read from an even blend", both.tiles.len());
        assert_eq!(both.unread.len(), profile.tiles() as usize);

        // Whichever way round the pictures come.
        for known_first in [true, false] {
            let mut receiver = DenseReceiver::new();
            let alone = reader.read(&first);
            let reports = if known_first {
                [receiver.absorb(&alone), receiver.absorb(&both)]
            } else {
                [receiver.absorb(&both), receiver.absorb(&alone)]
            };
            let again: usize = reports.iter().map(|report| report.tiles_read_again).sum();
            assert_eq!(again, profile.tiles() as usize - 4, "known first: {known_first}");
        }
    }

    #[test]
    fn a_picture_of_two_codes_yields_the_tiles_of_both() {
        // What a rolling shutter does: the top of one code and the bottom of
        // the next. With one set of codewords across the frame this picture is
        // worth nothing.
        let profile = &DENSE_PROFILES[1];
        let mut sender =
            DenseTransmitter::new("c.bin", &noise(90_000), profile, 3).expect("prepared");
        let first = sender.next_frame(3);
        let second = sender.next_frame(3);

        let mut spliced = first.clone();
        for y in first.height() / 2..first.height() {
            for x in 0..first.width() {
                spliced.set(x, y, second.get(x, y));
            }
        }

        let reading = DenseReader::new().read(&spliced);
        let from_first = reading.tiles.iter().filter(|t| t.code == 0).count();
        let from_second = reading.tiles.iter().filter(|t| t.code == 1).count();
        let tiles = profile.tiles() as usize;
        assert!(from_first >= tiles / 2 - profile.tile_cols as usize, "{from_first} of the first");
        assert!(
            from_second >= tiles / 2 - profile.tile_cols as usize,
            "{from_second} of the second"
        );
    }
}
