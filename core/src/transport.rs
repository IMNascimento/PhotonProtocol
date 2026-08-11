//! The transport layer: an endless stream of RaptorQ encoding symbols.
//!
//! The emitter never learns when the receiver started filming, when it stopped,
//! or which frames survived, so it cannot send the file — it sends an unbounded
//! stream of fragments from which *any* sufficiently large subset rebuilds the
//! whole (`SPEC.md` §6). Nothing is ever retransmitted because nothing is ever
//! requested; the receiver simply keeps collecting until it has enough.
//!
//! Two emission policies matter, and both exist to serve a receiver whose
//! recording window the emitter cannot see.
//!
//! Source symbols go first. A receiver who happens to start at the beginning of
//! a pass then often decodes with no repair symbols at all, which is the fastest
//! transfer the format can produce.
//!
//! After that, symbols are drawn round-robin across source blocks rather than
//! block by block. A receiver filming an arbitrary window gets even coverage of
//! the file instead of all of one block and none of another — and RaptorQ
//! decodes per block, so a block left short holds up everything.

use std::collections::HashSet;

use raptorq::{Decoder, Encoder, EncoderBuilder, EncodingPacket, ObjectTransmissionInformation};

use crate::error::{Error, Result};

/// Bytes of FEC Payload ID in front of every symbol (`SPEC.md` §6).
pub const PAYLOAD_ID_LEN: usize = 4;

/// Smallest symbol size the specification recommends.
pub const MIN_SYMBOL_SIZE: u16 = 512;

/// Largest symbol size the specification recommends.
pub const MAX_SYMBOL_SIZE: u16 = 4096;

/// Repair symbols generated per block each time the stream runs dry.
const REPAIR_BATCH: u32 = 16;

/// Working set a receiver is assumed able to spare, which is what decides how
/// many source blocks an object is split into.
///
/// The split is not a detail. A decoder holds one block's working set at a time,
/// so the figure the *emitter* chooses is the memory the *receiver* must find —
/// on a phone, in a browser tab, while decoding video. Ten megabytes matches
/// RaptorQ's own default and is comfortable on any device that can record 4K in
/// the first place.
pub const DEFAULT_DECODER_MEMORY: u64 = 10 * 1024 * 1024;

/// Chooses a symbol size that wastes as little of a frame as possible.
///
/// A frame holds `capacity` payload bytes and each symbol costs `overhead` bytes
/// of framing on top of its body, so the leftover is
/// `capacity mod (symbol_size + overhead)`. Sweeping the recommended range and
/// keeping the best is cheap and beats any rule of thumb, which is the whole
/// argument for doing it by measurement rather than by choosing a round number.
///
/// `reserved` is space the caller intends to spend on other units — the manifest,
/// in practice.
#[must_use]
pub fn choose_symbol_size(capacity: usize, overhead: usize, reserved: usize) -> u16 {
    let usable = capacity.saturating_sub(reserved);
    let mut best = MIN_SYMBOL_SIZE;
    let mut best_waste = usize::MAX;

    let mut candidate = MIN_SYMBOL_SIZE;
    while candidate <= MAX_SYMBOL_SIZE {
        let slot = candidate as usize + overhead;
        if slot <= usable {
            let symbols = usable / slot;
            let waste = usable - symbols * slot;
            // Ties go to the larger symbol: fewer, bigger symbols mean less
            // framing overhead across the whole transfer.
            if waste <= best_waste {
                best_waste = waste;
                best = candidate;
            }
        }
        // The symbol size must stay a multiple of the alignment RaptorQ
        // declares, and 8 divides every alignment the crate produces.
        candidate += 8;
    }

    best
}

/// Produces the endless symbol stream for one transfer.
pub struct TransportEncoder {
    encoder: Encoder,
    config: ObjectTransmissionInformation,
    block_count: usize,
}

impl TransportEncoder {
    /// Prepares a transfer of `data` using symbols of `symbol_size` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for an empty object or an unusable symbol
    /// size.
    pub fn new(data: &[u8], symbol_size: u16) -> Result<Self> {
        Self::with_decoder_memory(data, symbol_size, DEFAULT_DECODER_MEMORY)
    }

    /// Prepares a transfer, capping the working set a receiver will need.
    ///
    /// The cap decides how many source blocks the object is split into, and the
    /// emitter is choosing on the receiver's behalf: it is the receiver that has
    /// to find the memory, in a browser tab, on a phone. Lower it when the
    /// receiving device is known to be constrained.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for an empty object or an unusable symbol
    /// size.
    pub fn with_decoder_memory(data: &[u8], symbol_size: u16, memory: u64) -> Result<Self> {
        if data.is_empty() {
            return Err(Error::Malformed {
                context: "transport",
                detail: "cannot transmit an empty object",
            });
        }
        if symbol_size == 0 {
            return Err(Error::Malformed {
                context: "transport",
                detail: "symbol size must be positive",
            });
        }

        let mut builder = EncoderBuilder::new();
        builder.set_max_packet_size(symbol_size);
        builder.set_decoder_memory_requirement(memory);

        let encoder = builder.build(data);
        let config = encoder.get_config();
        let block_count = encoder.get_block_encoders().len();

        Ok(Self { encoder, config, block_count })
    }

    /// The Object Transmission Information a receiver needs, serialised for the
    /// manifest (`SPEC.md` §7.1).
    #[must_use]
    pub fn oti(&self) -> [u8; 12] {
        self.config.serialize()
    }

    /// Source blocks the object was split into.
    #[must_use]
    pub const fn block_count(&self) -> usize {
        self.block_count
    }

    /// Total source symbols across every block.
    #[must_use]
    pub fn source_symbol_count(&self) -> usize {
        self.encoder.get_block_encoders().iter().map(|b| b.source_packets().len()).sum()
    }

    /// Every source symbol, round-robin across blocks and already serialised.
    ///
    /// This is the first pass of an emission loop (`SPEC.md` §6).
    #[must_use]
    pub fn source_symbols(&self) -> Vec<Vec<u8>> {
        let per_block: Vec<Vec<EncodingPacket>> = self
            .encoder
            .get_block_encoders()
            .iter()
            .map(raptorq::SourceBlockEncoder::source_packets)
            .collect();
        round_robin(&per_block)
    }

    /// A batch of repair symbols, `count` from each block, starting at
    /// `start_id`.
    ///
    /// Encoding Symbol IDs have to keep rising for the whole session, so the
    /// caller owns that counter rather than this type guessing at it.
    #[must_use]
    pub fn repair_symbols(&self, start_id: u32, count: u32) -> Vec<Vec<u8>> {
        let per_block: Vec<Vec<EncodingPacket>> = self
            .encoder
            .get_block_encoders()
            .iter()
            .map(|block| block.repair_packets(start_id, count))
            .collect();
        round_robin(&per_block)
    }

    /// Repair symbols to generate per block each time a caller runs dry.
    #[must_use]
    pub const fn repair_batch(&self) -> u32 {
        REPAIR_BATCH
    }

    /// The endless stream of symbols, each serialised as a FEC Payload ID
    /// followed by its body.
    #[must_use]
    pub fn stream(&self) -> SymbolStream<'_> {
        SymbolStream { encoder: self, pending: self.source_symbols(), cursor: 0, next_repair: 0 }
    }
}

/// An iterator over encoding symbols that never ends.
pub struct SymbolStream<'a> {
    encoder: &'a TransportEncoder,
    pending: Vec<Vec<u8>>,
    cursor: usize,
    next_repair: u32,
}

impl Iterator for SymbolStream<'_> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.pending.len() {
            self.pending = self.encoder.repair_symbols(self.next_repair, REPAIR_BATCH);
            self.next_repair = self.next_repair.saturating_add(REPAIR_BATCH);
            self.cursor = 0;
            if self.pending.is_empty() {
                return None;
            }
        }
        let symbol = self.pending.get(self.cursor)?.clone();
        self.cursor += 1;
        Some(symbol)
    }
}

/// Interleaves per-block packet lists so consecutive symbols come from
/// different source blocks, and serialises them.
fn round_robin(per_block: &[Vec<EncodingPacket>]) -> Vec<Vec<u8>> {
    let longest = per_block.iter().map(Vec::len).max().unwrap_or(0);
    let total: usize = per_block.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);

    for index in 0..longest {
        for block in per_block {
            if let Some(packet) = block.get(index) {
                out.push(packet.serialize());
            }
        }
    }
    out
}

/// Collects symbols until the object can be rebuilt.
pub struct TransportDecoder {
    decoder: Decoder,
    config: ObjectTransmissionInformation,
    seen: HashSet<(u8, u32)>,
    symbol_size: usize,
    result: Option<Vec<u8>>,
}

impl TransportDecoder {
    /// A decoder for the transfer described by `oti`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] if the transmission information is
    /// self-contradictory — a zero symbol size, or a transfer length of zero.
    /// Both arrive from a manifest that crossed the channel, so neither can be
    /// assumed sane.
    pub fn new(oti: [u8; 12]) -> Result<Self> {
        let config = ObjectTransmissionInformation::deserialize(&oti);
        if config.symbol_size() == 0 || config.transfer_length() == 0 {
            return Err(Error::Malformed {
                context: "transmission information",
                detail: "zero symbol size or transfer length",
            });
        }

        Ok(Self {
            decoder: Decoder::new(config),
            config,
            seen: HashSet::new(),
            symbol_size: config.symbol_size() as usize,
            result: None,
        })
    }

    /// Source symbols the object needs, which is what progress is measured
    /// against.
    #[must_use]
    pub fn symbols_needed(&self) -> usize {
        let transfer = self.config.transfer_length();
        let symbol = u64::from(self.config.symbol_size());
        usize::try_from(transfer.div_ceil(symbol)).unwrap_or(usize::MAX)
    }

    /// Distinct symbols accepted so far.
    #[must_use]
    pub fn symbols_accepted(&self) -> usize {
        self.seen.len()
    }

    /// Whether the object has been rebuilt.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.result.is_some()
    }

    /// Offers one serialised symbol.
    ///
    /// Returns `true` if it was new and usable. Duplicates are common and cheap
    /// to reject — a camera at 60 frames per second sees most displayed frames
    /// more than once — and feeding them to the decoder would waste the work
    /// without advancing anything.
    pub fn push(&mut self, payload: &[u8]) -> bool {
        if payload.len() != PAYLOAD_ID_LEN + self.symbol_size {
            return false;
        }

        let block = payload[0];
        let symbol_id =
            u32::from(payload[1]) << 16 | u32::from(payload[2]) << 8 | u32::from(payload[3]);
        if !self.seen.insert((block, symbol_id)) {
            return false;
        }

        if self.result.is_none() {
            self.result = self.decoder.decode(EncodingPacket::deserialize(payload));
        }
        true
    }

    /// The rebuilt object, if there is one.
    #[must_use]
    pub fn take_result(&mut self) -> Option<Vec<u8>> {
        self.result.take()
    }

    /// The rebuilt object, borrowed.
    #[must_use]
    pub fn result(&self) -> Option<&[u8]> {
        self.result.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| u8::try_from((i * 31 + 7) % 251).unwrap()).collect()
    }

    #[test]
    fn source_symbols_alone_rebuild_the_object() {
        // The best case the emission policy is designed to produce: a receiver
        // that catches the start of a pass never needs a repair symbol.
        let data = payload(20_000);
        let encoder = TransportEncoder::new(&data, 1024).unwrap();
        let mut decoder = TransportDecoder::new(encoder.oti()).unwrap();

        for symbol in encoder.stream().take(encoder.source_symbol_count()) {
            decoder.push(&symbol);
        }

        assert!(decoder.is_complete(), "source symbols alone did not suffice");
        assert_eq!(decoder.take_result().unwrap(), data);
    }

    #[test]
    fn a_window_that_misses_the_start_still_rebuilds_the_object() {
        // The case that actually happens. Nobody starts filming at symbol zero,
        // and the emitter has no way to know when they did start.
        let data = payload(20_000);
        let encoder = TransportEncoder::new(&data, 1024).unwrap();
        let mut decoder = TransportDecoder::new(encoder.oti()).unwrap();

        let skipped = encoder.source_symbol_count() / 3;
        let mut stream = encoder.stream().skip(skipped);
        for _ in 0..encoder.source_symbol_count() * 2 {
            if decoder.is_complete() {
                break;
            }
            let symbol = stream.next().expect("the stream never ends");
            decoder.push(&symbol);
        }

        assert!(decoder.is_complete(), "a mid-stream window failed to decode");
        assert_eq!(decoder.take_result().unwrap(), data);
    }

    #[test]
    fn losing_a_third_of_the_symbols_costs_time_not_correctness() {
        // The property the whole design rests on: dropped frames are a
        // throughput cost, never a failure.
        let data = payload(16_000);
        let encoder = TransportEncoder::new(&data, 1024).unwrap();
        let mut decoder = TransportDecoder::new(encoder.oti()).unwrap();

        let mut delivered = 0usize;
        for (index, symbol) in encoder.stream().take(4000).enumerate() {
            if index % 3 == 0 {
                continue;
            }
            decoder.push(&symbol);
            delivered += 1;
            if decoder.is_complete() {
                break;
            }
        }

        assert!(decoder.is_complete(), "lossy delivery failed after {delivered} symbols");
        assert_eq!(decoder.take_result().unwrap(), data);
    }

    #[test]
    fn duplicates_are_rejected_without_being_counted() {
        // A camera sees most displayed frames more than once. Counting a
        // duplicate as progress would make the reported figure a lie.
        let data = payload(8_000);
        let encoder = TransportEncoder::new(&data, 1024).unwrap();
        let mut decoder = TransportDecoder::new(encoder.oti()).unwrap();

        let symbols: Vec<Vec<u8>> = encoder.stream().take(4).collect();
        assert!(decoder.push(&symbols[0]));
        assert!(!decoder.push(&symbols[0]), "a duplicate was accepted");
        assert_eq!(decoder.symbols_accepted(), 1);
    }

    #[test]
    fn malformed_symbols_are_refused_without_panicking() {
        // Symbols reach this layer after passing a CRC, but a CRC can collide
        // and the video file is untrusted input regardless.
        let data = payload(8_000);
        let encoder = TransportEncoder::new(&data, 1024).unwrap();
        let mut decoder = TransportDecoder::new(encoder.oti()).unwrap();

        assert!(!decoder.push(&[]));
        assert!(!decoder.push(&[0; 3]));
        assert!(!decoder.push(&[0; 1027]));
        assert!(!decoder.push(&[0; 2000]));
        assert_eq!(decoder.symbols_accepted(), 0);
    }

    #[test]
    fn contradictory_transmission_information_is_refused() {
        assert!(TransportDecoder::new([0; 12]).is_err());
    }

    #[test]
    fn progress_is_reported_against_a_meaningful_target() {
        let data = payload(50_000);
        let encoder = TransportEncoder::new(&data, 1000).unwrap();
        let decoder = TransportDecoder::new(encoder.oti()).unwrap();
        assert_eq!(decoder.symbols_needed(), 50);
        assert_eq!(decoder.symbols_accepted(), 0);
    }

    #[test]
    fn symbols_are_drawn_round_robin_across_blocks() {
        // A receiver filming an arbitrary window must get even coverage. Since
        // RaptorQ decodes per block, a block left short holds up the whole file
        // however many symbols the others have.
        let data = payload(200_000);
        let encoder = TransportEncoder::with_decoder_memory(&data, 512, 8 * 1024).unwrap();
        assert!(encoder.block_count() > 1, "the test needs a multi-block object");

        let first: Vec<u8> =
            encoder.stream().take(encoder.block_count() * 2).map(|s| s[0]).collect();
        let distinct: HashSet<u8> = first.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            encoder.block_count(),
            "the first symbols came from {} of {} blocks",
            distinct.len(),
            encoder.block_count()
        );
    }

    #[test]
    fn the_stream_keeps_producing_new_symbols_indefinitely() {
        // The emitter loops until the user stops it, and must not start
        // repeating symbol identifiers while it does.
        let data = payload(10_000);
        let encoder = TransportEncoder::new(&data, 1024).unwrap();

        let mut seen = HashSet::new();
        for symbol in encoder.stream().take(500) {
            let id = (
                symbol[0],
                u32::from(symbol[1]) << 16 | u32::from(symbol[2]) << 8 | u32::from(symbol[3]),
            );
            assert!(seen.insert(id), "symbol {id:?} was emitted twice");
        }
        assert_eq!(seen.len(), 500);
    }

    #[test]
    fn the_chosen_symbol_size_fills_the_frame() {
        // Every wasted byte at the end of a frame is throughput given away for
        // the length of the transfer, so the choice is made by sweeping rather
        // than by picking a round number.
        for capacity in [2611usize, 7047, 15244] {
            let size = choose_symbol_size(capacity, 11, 80);
            assert!((MIN_SYMBOL_SIZE..=MAX_SYMBOL_SIZE).contains(&size));
            assert_eq!(size % 8, 0, "symbol size must respect the alignment");

            let usable = capacity - 80;
            let slot = size as usize + 11;
            let waste = usable % slot;
            assert!(
                waste < slot,
                "capacity {capacity} wastes {waste} bytes with symbol size {size}"
            );
            assert!(waste * 20 < usable, "capacity {capacity} wastes {waste} of {usable} bytes");
        }
    }

    #[test]
    fn an_empty_object_is_refused() {
        assert!(TransportEncoder::new(&[], 1024).is_err());
        assert!(TransportEncoder::new(&[1, 2, 3], 0).is_err());
    }
}
