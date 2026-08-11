//! Reed-Solomon over GF(2^8), and the interleaving that makes it useful here.
//!
//! The code is the one pinned by `SPEC.md` §5.2.1: field polynomial `0x11D`,
//! generator element `alpha = 2`, first consecutive root `alpha^0`, systematic
//! layout with parity appended. Those four choices are the whole compatibility
//! surface — get any of them wrong and two implementations produce codewords
//! that look plausible and repair nothing.
//!
//! Two capabilities matter for this channel.
//!
//! **Erasures.** A cell the classifier could not call is worth more than a cell
//! it called wrongly: knowing *where* the damage is halves its cost, so `p`
//! parity bytes repair `p/2` errors but `p` erasures. The classifier already
//! produces a confidence margin, so the information is there to be used.
//!
//! **Interleaving.** Glare, a fingertip, a reflection and a compression block
//! are all *local* in the frame, while a Reed-Solomon codeword can only take so
//! many bad bytes before it fails entirely. Spreading consecutive codeword bytes
//! across the frame converts one fatal burst into a scratch on many codewords,
//! which is exactly what the code is good at.

use crate::profile::{Profile, RsPartition};

/// Field-generator polynomial `x^8 + x^4 + x^3 + x^2 + 1` (`SPEC.md` §5.2.1).
pub const FIELD_POLYNOMIAL: u16 = 0x11D;

/// Generator element of the multiplicative group.
pub const GENERATOR: u8 = 0x02;

/// Why error correction gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FecError {
    /// The damage exceeded what the parity can repair. This is the honest,
    /// common outcome on a bad recording, not a defect.
    TooManyErrors,
    /// A codeword was longer than the field allows, or shorter than its parity.
    InvalidLength,
}

// --- GF(2^8) ---------------------------------------------------------------

/// Carry-less multiply followed by modular reduction, used only to build the
/// tables. Everything after that is a table lookup.
const fn mul_reduce(mut a: u16, mut b: u16) -> u16 {
    let mut result = 0u16;
    while b > 0 {
        if b & 1 != 0 {
            result ^= a;
        }
        b >>= 1;
        a <<= 1;
        if a & 0x100 != 0 {
            a ^= FIELD_POLYNOMIAL;
        }
    }
    result
}

const fn build_tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];

    // Everything here stays inside a byte by construction: `mul_reduce` folds
    // bit 8 back in on every step, and the loop stops at the group order.
    let mut x: u16 = 1;
    let mut i: u8 = 0;
    while i < 255 {
        exp[i as usize] = (x & 0xFF) as u8;
        log[(x & 0xFF) as usize] = i;
        x = mul_reduce(x, GENERATOR as u16);
        i += 1;
    }
    // The second copy lets a product of two logarithms be indexed without a
    // modulo in the inner loop.
    let mut j = 255usize;
    while j < 512 {
        exp[j] = exp[j - 255];
        j += 1;
    }

    (exp, log)
}

const TABLES: ([u8; 512], [u8; 256]) = build_tables();
const EXP: [u8; 512] = TABLES.0;
const LOG: [u8; 256] = TABLES.1;

/// Field multiplication.
#[must_use]
pub fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    EXP[LOG[a as usize] as usize + LOG[b as usize] as usize]
}

/// Field division. Dividing by zero yields zero, which never arises on a path
/// this crate takes.
#[must_use]
pub fn div(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    EXP[(LOG[a as usize] as usize + 255 - LOG[b as usize] as usize) % 255]
}

/// `base` raised to a power, with the exponent reduced modulo the group order.
#[must_use]
pub fn pow(base: u8, exponent: i32) -> u8 {
    if base == 0 {
        return 0;
    }
    let log = i32::from(LOG[base as usize]);
    let index = (log * exponent).rem_euclid(255);
    EXP[usize::try_from(index).unwrap_or(0)]
}

/// Multiplicative inverse.
#[must_use]
pub fn inverse(a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    EXP[255 - LOG[a as usize] as usize]
}

// --- polynomials, most significant coefficient first -------------------------

fn poly_add(p: &[u8], q: &[u8]) -> Vec<u8> {
    let len = p.len().max(q.len());
    let mut out = vec![0u8; len];
    for (i, &v) in p.iter().enumerate() {
        out[i + len - p.len()] ^= v;
    }
    for (i, &v) in q.iter().enumerate() {
        out[i + len - q.len()] ^= v;
    }
    out
}

fn poly_scale(p: &[u8], k: u8) -> Vec<u8> {
    p.iter().map(|&v| mul(v, k)).collect()
}

fn poly_mul(p: &[u8], q: &[u8]) -> Vec<u8> {
    if p.is_empty() || q.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u8; p.len() + q.len() - 1];
    for (j, &qj) in q.iter().enumerate() {
        if qj == 0 {
            continue;
        }
        for (i, &pi) in p.iter().enumerate() {
            out[i + j] ^= mul(pi, qj);
        }
    }
    out
}

fn poly_eval(p: &[u8], x: u8) -> u8 {
    let mut acc = 0u8;
    for &coefficient in p {
        acc = mul(acc, x) ^ coefficient;
    }
    acc
}

// --- the codec ---------------------------------------------------------------

/// A systematic Reed-Solomon codec with a fixed parity length.
///
/// The generator polynomial depends only on the parity length, so one codec
/// serves every codeword of a profile, full or shortened. Shortening is free in
/// Reed-Solomon: a shorter block is the same code with leading zeros that are
/// never transmitted.
#[derive(Debug, Clone)]
pub struct RsCodec {
    parity: usize,
    generator: Vec<u8>,
}

impl RsCodec {
    /// A codec producing `parity` parity bytes per codeword.
    ///
    /// # Panics
    ///
    /// Panics if `parity` is zero or exceeds 255, neither of which any profile
    /// asks for.
    #[must_use]
    pub fn new(parity: usize) -> Self {
        assert!(parity > 0 && parity <= 255, "parity must be in 1..=255, got {parity}");
        let mut generator = vec![1u8];
        for i in 0..parity {
            let root = pow(GENERATOR, i32::try_from(i).unwrap_or(0));
            generator = poly_mul(&generator, &[1, root]);
        }
        Self { parity, generator }
    }

    /// Parity bytes per codeword.
    #[must_use]
    pub const fn parity(&self) -> usize {
        self.parity
    }

    /// Byte errors the code repairs when their positions are unknown.
    #[must_use]
    pub const fn correctable_errors(&self) -> usize {
        self.parity / 2
    }

    /// Byte erasures the code repairs when their positions are known.
    #[must_use]
    pub const fn correctable_erasures(&self) -> usize {
        self.parity
    }

    /// Appends parity to `data`, returning the codeword.
    ///
    /// # Errors
    ///
    /// Returns [`FecError::InvalidLength`] if the codeword would exceed the 255
    /// bytes the field can address.
    pub fn encode(&self, data: &[u8]) -> Result<Vec<u8>, FecError> {
        if data.len() + self.parity > 255 {
            return Err(FecError::InvalidLength);
        }

        let mut out = vec![0u8; data.len() + self.parity];
        out[..data.len()].copy_from_slice(data);

        for i in 0..data.len() {
            let coefficient = out[i];
            if coefficient == 0 {
                continue;
            }
            for j in 1..self.generator.len() {
                out[i + j] ^= mul(self.generator[j], coefficient);
            }
        }

        out[..data.len()].copy_from_slice(data);
        Ok(out)
    }

    /// Repairs a codeword and returns its data half.
    ///
    /// `erasures` are byte positions within the codeword that the caller already
    /// knows to be unreliable — a cell the classifier refused to call, most
    /// often. Supplying them doubles what the same parity can repair.
    ///
    /// # Errors
    ///
    /// Returns [`FecError::TooManyErrors`] when the damage exceeds the parity,
    /// and [`FecError::InvalidLength`] for a codeword that is not a codeword.
    pub fn decode(&self, codeword: &[u8], erasures: &[usize]) -> Result<Vec<u8>, FecError> {
        let n = codeword.len();
        if n > 255 || n <= self.parity {
            return Err(FecError::InvalidLength);
        }
        if erasures.len() > self.parity || erasures.iter().any(|&p| p >= n) {
            return Err(FecError::TooManyErrors);
        }

        let mut work = codeword.to_vec();
        for &position in erasures {
            work[position] = 0;
        }

        let syndromes = self.syndromes(&work);
        if syndromes.iter().all(|&s| s == 0) {
            work.truncate(n - self.parity);
            return Ok(work);
        }

        let forney = forney_syndromes(&syndromes, erasures, n);
        let locator = self.error_locator(&forney, erasures.len())?;

        let mut reversed = locator.clone();
        reversed.reverse();
        let mut positions = find_error_positions(&reversed, n)?;
        positions.extend_from_slice(erasures);

        let corrected = correct_errata(&work, &syndromes, &positions)?;

        // Re-deriving the syndromes is not paranoia. Beyond its correction
        // radius the algorithm does not fail loudly; it converges on a *different
        // valid codeword*, and would hand back confidently wrong data.
        if self.syndromes(&corrected).iter().any(|&s| s != 0) {
            return Err(FecError::TooManyErrors);
        }

        let mut out = corrected;
        out.truncate(n - self.parity);
        Ok(out)
    }

    fn syndromes(&self, codeword: &[u8]) -> Vec<u8> {
        (0..self.parity)
            .map(|i| poly_eval(codeword, pow(GENERATOR, i32::try_from(i).unwrap_or(0))))
            .collect()
    }

    /// Berlekamp-Massey, seeded with the erasure locator when there is one.
    fn error_locator(&self, syndromes: &[u8], erasure_count: usize) -> Result<Vec<u8>, FecError> {
        let mut locator = vec![1u8];
        let mut previous = vec![1u8];

        for i in 0..self.parity - erasure_count {
            let index = i;
            let mut delta = syndromes[index];
            for j in 1..locator.len() {
                if index < j {
                    break;
                }
                delta ^= mul(locator[locator.len() - 1 - j], syndromes[index - j]);
            }

            previous.push(0);

            if delta != 0 {
                if previous.len() > locator.len() {
                    let scaled = poly_scale(&previous, delta);
                    previous = poly_scale(&locator, inverse(delta));
                    locator = scaled;
                }
                locator = poly_add(&locator, &poly_scale(&previous, delta));
            }
        }

        while locator.first() == Some(&0) {
            locator.remove(0);
        }

        let errors = locator.len().saturating_sub(1);
        if errors * 2 + erasure_count > self.parity {
            return Err(FecError::TooManyErrors);
        }
        Ok(locator)
    }
}

fn forney_syndromes(syndromes: &[u8], erasures: &[usize], n: usize) -> Vec<u8> {
    let mut adjusted = syndromes.to_vec();
    for &position in erasures {
        let exponent = i32::try_from(n - 1 - position).unwrap_or(0);
        let x = pow(GENERATOR, exponent);
        for j in 0..adjusted.len().saturating_sub(1) {
            adjusted[j] = mul(adjusted[j], x) ^ adjusted[j + 1];
        }
    }
    adjusted
}

fn find_error_positions(locator: &[u8], n: usize) -> Result<Vec<usize>, FecError> {
    let expected = locator.len().saturating_sub(1);
    let mut positions = Vec::new();
    for i in 0..n {
        if poly_eval(locator, pow(GENERATOR, i32::try_from(i).unwrap_or(0))) == 0 {
            positions.push(n - 1 - i);
        }
    }
    if positions.len() == expected { Ok(positions) } else { Err(FecError::TooManyErrors) }
}

/// Forney's algorithm: locations are known, this finds the magnitudes.
fn correct_errata(
    codeword: &[u8],
    syndromes: &[u8],
    positions: &[usize],
) -> Result<Vec<u8>, FecError> {
    let n = codeword.len();

    let coefficient_positions: Vec<i32> =
        positions.iter().map(|&p| i32::try_from(n - 1 - p).unwrap_or(0)).collect();

    let mut errata_locator = vec![1u8];
    for &position in &coefficient_positions {
        errata_locator = poly_mul(&errata_locator, &poly_add(&[1], &[pow(GENERATOR, position), 0]));
    }

    // Forney's evaluator is `S(x) * Lambda(x) mod x^(t+1)`, and `S(x)` starts at
    // degree one: syndrome `S_i` is the coefficient of `x^(i+1)`, never of
    // `x^0`. The leading zero below supplies that degree shift. Dropping it
    // costs one position in the truncation that follows, which yields error
    // magnitudes that are individually plausible and collectively wrong.
    let mut syndrome_poly = vec![0u8];
    syndrome_poly.extend_from_slice(syndromes);
    syndrome_poly.reverse();
    let product = poly_mul(&syndrome_poly, &errata_locator);
    let keep = errata_locator.len();
    let start = product.len().saturating_sub(keep);
    let mut evaluator = product[start..].to_vec();
    evaluator.reverse();

    let locations: Vec<u8> = coefficient_positions.iter().map(|&c| pow(GENERATOR, c)).collect();

    let mut correction = vec![0u8; n];
    for (i, &location) in locations.iter().enumerate() {
        let location_inverse = inverse(location);

        let mut derivative = 1u8;
        for (j, &other) in locations.iter().enumerate() {
            if i != j {
                derivative = mul(derivative, 1 ^ mul(location_inverse, other));
            }
        }
        if derivative == 0 {
            return Err(FecError::TooManyErrors);
        }

        let mut reversed_evaluator = evaluator.clone();
        reversed_evaluator.reverse();
        let numerator = mul(location, poly_eval(&reversed_evaluator, location_inverse));

        correction[positions[i]] = div(numerator, derivative);
    }

    Ok(poly_add(codeword, &correction))
}

// --- frame-level partitioning and interleaving -------------------------------

/// Cuts a frame's payload into codewords, interleaves them, and reverses both.
///
/// The partition comes from the profile (`SPEC.md` §5.2.2); the interleaving is
/// column-major across codewords (`SPEC.md` §5.2.3), which is what turns a
/// local defect into a survivable one.
#[derive(Debug, Clone)]
pub struct PayloadCodec {
    codec: RsCodec,
    partition: RsPartition,
    data_len: usize,
    raw_len: usize,
    capacity: usize,
}

impl PayloadCodec {
    /// The codec for a profile's frames.
    #[must_use]
    pub fn for_profile(profile: &Profile) -> Self {
        Self {
            codec: RsCodec::new(profile.rs_parity_len() as usize),
            partition: profile.rs_partition(),
            data_len: profile.rs_data_len as usize,
            raw_len: profile.raw_bytes() as usize,
            capacity: profile.payload_capacity() as usize,
        }
    }

    /// Payload bytes one frame carries.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Raw bytes the data region holds, parity included.
    #[must_use]
    pub const fn raw_len(&self) -> usize {
        self.raw_len
    }

    /// Codeword lengths in order, full blocks then any shortened tail.
    fn codeword_lengths(&self) -> Vec<usize> {
        let mut lengths = vec![255usize; self.partition.full_codewords as usize];
        if let Some((n, _)) = self.partition.shortened {
            lengths.push(n as usize);
        }
        lengths
    }

    /// Data lengths per codeword, matching [`Self::codeword_lengths`].
    fn data_lengths(&self) -> Vec<usize> {
        let mut lengths = vec![self.data_len; self.partition.full_codewords as usize];
        if let Some((_, k)) = self.partition.shortened {
            lengths.push(k as usize);
        }
        lengths
    }

    /// Encodes a frame payload into the interleaved raw byte stream.
    ///
    /// `payload` is zero-padded to the frame's capacity, as `SPEC.md` §5.3
    /// requires for the unused tail.
    ///
    /// # Errors
    ///
    /// Returns [`FecError::InvalidLength`] if the payload exceeds the frame's
    /// capacity.
    pub fn encode(&self, payload: &[u8]) -> Result<Vec<u8>, FecError> {
        if payload.len() > self.capacity {
            return Err(FecError::InvalidLength);
        }

        let mut padded = payload.to_vec();
        padded.resize(self.capacity, 0);

        let mut codewords = Vec::new();
        let mut offset = 0usize;
        for k in self.data_lengths() {
            codewords.push(self.codec.encode(&padded[offset..offset + k])?);
            offset += k;
        }

        Ok(interleave(&codewords, self.raw_len))
    }

    /// Decodes an interleaved raw byte stream back into a frame payload.
    ///
    /// `erasures` are positions in the raw stream the caller does not trust.
    ///
    /// # Errors
    ///
    /// Returns [`FecError::TooManyErrors`] if any codeword is beyond repair, and
    /// [`FecError::InvalidLength`] if the stream is the wrong size.
    pub fn decode(&self, raw: &[u8], erasures: &[usize]) -> Result<Vec<u8>, FecError> {
        if raw.len() != self.raw_len {
            return Err(FecError::InvalidLength);
        }

        let lengths = self.codeword_lengths();
        let (codewords, codeword_erasures) = deinterleave(raw, &lengths, erasures);

        let mut payload = Vec::with_capacity(self.capacity);
        for (codeword, erased) in codewords.iter().zip(codeword_erasures.iter()) {
            payload.extend_from_slice(&self.codec.decode(codeword, erased)?);
        }
        payload.truncate(self.capacity);
        Ok(payload)
    }
}

/// Column-major interleaving across codewords (`SPEC.md` §5.2.3).
///
/// The codewords are ragged — the last one is shortened — so the traversal is a
/// column sweep over rows of unequal length, which is what the specification
/// states and what the range loop expresses directly.
fn interleave(codewords: &[Vec<u8>], raw_len: usize) -> Vec<u8> {
    let longest = codewords.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::with_capacity(raw_len);
    for column in 0..longest {
        for codeword in codewords {
            if let Some(&byte) = codeword.get(column) {
                out.push(byte);
            }
        }
    }
    out.resize(raw_len, 0);
    out
}

/// Reverses [`interleave`], carrying erasure positions across with the bytes.
///
/// The column sweep is the same ragged traversal as the forward direction, and
/// has to stay a mirror image of it: a de-interleaver that walked the rows in a
/// different order would still produce codewords, just not the ones that were
/// sent.
#[expect(clippy::needless_range_loop, reason = "column sweep over ragged rows")]
fn deinterleave(
    raw: &[u8],
    lengths: &[usize],
    erasures: &[usize],
) -> (Vec<Vec<u8>>, Vec<Vec<usize>>) {
    let mut codewords: Vec<Vec<u8>> = lengths.iter().map(|&n| vec![0u8; n]).collect();
    let mut positions: Vec<Vec<usize>> = vec![Vec::new(); lengths.len()];

    let erased: std::collections::HashSet<usize> = erasures.iter().copied().collect();
    let longest = lengths.iter().copied().max().unwrap_or(0);

    let mut index = 0usize;
    for column in 0..longest {
        for (word, &length) in lengths.iter().enumerate() {
            if column >= length {
                continue;
            }
            if let Some(&byte) = raw.get(index) {
                codewords[word][column] = byte;
                if erased.contains(&index) {
                    positions[word].push(column);
                }
            }
            index += 1;
        }
    }

    (codewords, positions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::PROFILES;

    /// A deterministic pseudo-random source. Tests must not depend on a seed
    /// that changes between runs, or a rare failure becomes unreproducible.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % bound as u64).unwrap_or(0)
        }

        fn byte(&mut self) -> u8 {
            u8::try_from(self.next() & 0xFF).unwrap_or(0)
        }
    }

    #[test]
    fn the_field_is_the_one_the_specification_pins() {
        // Every one of these is a compatibility surface. A different reduction
        // polynomial or a non-primitive generator yields a code that works
        // perfectly on its own and interoperates with nothing.

        // Doubling 0x80 overflows into bit 8 and is reduced by the field
        // polynomial, so this single product identifies 0x11D and no other.
        assert_eq!(mul(0x80, 0x02), 0x1D, "the reduction polynomial is not 0x11D");
        assert_eq!(mul(0x40, 0x02), 0x80, "doubling below the overflow must not reduce");

        assert_eq!(pow(GENERATOR, 0), 1);
        assert_eq!(pow(GENERATOR, 1), GENERATOR);
        assert_eq!(pow(GENERATOR, 255), 1, "the group order must be 255");

        // Primitivity: the powers of the generator must visit all 255 non-zero
        // elements before returning to 1. A generator of smaller order would
        // make some syndromes alias each other.
        let mut seen = [false; 256];
        for i in 0..255 {
            let value = pow(GENERATOR, i);
            assert_ne!(value, 0);
            assert!(!seen[value as usize], "{GENERATOR} is not primitive: repeat at {i}");
            seen[value as usize] = true;
        }

        for a in 1..=255u8 {
            assert_eq!(mul(a, inverse(a)), 1, "{a} has a wrong inverse");
            assert_eq!(div(a, a), 1);
            assert_eq!(mul(a, 1), a);
        }
    }

    #[test]
    fn a_clean_codeword_decodes_unchanged() {
        let codec = RsCodec::new(32);
        let data: Vec<u8> = (0..100u8).collect();
        let codeword = codec.encode(&data).unwrap();
        assert_eq!(codeword.len(), data.len() + 32);
        assert_eq!(&codeword[..data.len()], &data[..], "the code must be systematic");
        assert_eq!(codec.decode(&codeword, &[]).unwrap(), data);
    }

    #[test]
    fn errors_are_repaired_up_to_the_radius() {
        let mut rng = Rng(0x5EED_1234_ABCD_0001);
        let codec = RsCodec::new(32);
        let limit = codec.correctable_errors();

        for trial in 0..200 {
            let data: Vec<u8> = (0..120).map(|_| rng.byte()).collect();
            let clean = codec.encode(&data).unwrap();
            let mut damaged = clean.clone();

            let count = 1 + trial % limit;
            let mut hit = Vec::new();
            while hit.len() < count {
                let position = rng.below(damaged.len());
                if !hit.contains(&position) {
                    hit.push(position);
                    damaged[position] ^= rng.byte() | 1;
                }
            }

            assert_eq!(codec.decode(&damaged, &[]).unwrap(), data, "trial {trial}, {count} errors");
        }
    }

    #[test]
    fn erasures_are_repaired_at_twice_the_rate() {
        // The whole reason the classifier reports a confidence margin.
        let mut rng = Rng(0x5EED_1234_ABCD_0002);
        let codec = RsCodec::new(32);
        let limit = codec.correctable_erasures();
        assert_eq!(limit, 2 * codec.correctable_errors());

        for trial in 0..100 {
            let data: Vec<u8> = (0..120).map(|_| rng.byte()).collect();
            let clean = codec.encode(&data).unwrap();
            let mut damaged = clean.clone();

            let count = 1 + trial % limit;
            let mut hit: Vec<usize> = Vec::new();
            while hit.len() < count {
                let position = rng.below(damaged.len());
                if !hit.contains(&position) {
                    hit.push(position);
                    damaged[position] ^= rng.byte() | 1;
                }
            }

            assert_eq!(
                codec.decode(&damaged, &hit).unwrap(),
                data,
                "trial {trial}, {count} erasures"
            );
        }
    }

    #[test]
    fn damage_beyond_the_radius_is_reported_not_invented() {
        // Past its radius the algorithm does not stall -- it converges on a
        // different valid codeword. Without the final syndrome check this would
        // return confidently wrong data, which on this channel means a file that
        // fails its digest after a minute of filming, with no explanation.
        let mut rng = Rng(0x5EED_1234_ABCD_0003);
        let codec = RsCodec::new(16);

        let mut detected = 0;
        for _ in 0..200 {
            let data: Vec<u8> = (0..100).map(|_| rng.byte()).collect();
            let clean = codec.encode(&data).unwrap();
            let mut damaged = clean.clone();

            let mut hit = Vec::new();
            while hit.len() < codec.correctable_errors() + 3 {
                let position = rng.below(damaged.len());
                if !hit.contains(&position) {
                    hit.push(position);
                    damaged[position] ^= rng.byte() | 1;
                }
            }

            match codec.decode(&damaged, &[]) {
                Err(FecError::TooManyErrors) => detected += 1,
                Err(other) => panic!("unexpected error {other:?}"),
                Ok(recovered) => {
                    assert_ne!(recovered, data, "excess damage must never decode as the original");
                }
            }
        }
        assert!(detected > 150, "only {detected}/200 over-damaged codewords were rejected");
    }

    #[test]
    fn shortened_codewords_behave_like_full_ones() {
        // Every profile ends its frame with a shortened codeword, so this is not
        // an edge case but the common path.
        let codec = RsCodec::new(56);
        let data: Vec<u8> = (0..82u8).collect();
        let codeword = codec.encode(&data).unwrap();
        assert_eq!(codeword.len(), 138);

        let mut damaged = codeword;
        for i in 0..codec.correctable_errors() {
            damaged[i * 3] ^= 0x7F;
        }
        assert_eq!(codec.decode(&damaged, &[]).unwrap(), data);
    }

    #[test]
    fn interleaving_survives_a_burst_that_would_kill_one_codeword() {
        // The point of interleaving, stated as a test: a contiguous run of
        // damage far longer than any single codeword can absorb, spread thin
        // enough by the interleaver that every codeword survives it.
        let profile = &PROFILES[1];
        let codec = PayloadCodec::for_profile(profile);
        let payload: Vec<u8> =
            (0..codec.capacity()).map(|i| u8::try_from(i % 251).unwrap()).collect();

        let raw = codec.encode(&payload).unwrap();
        assert_eq!(raw.len(), codec.raw_len());

        let partition = profile.rs_partition();
        let words = partition.codeword_count() as usize;
        let per_codeword = profile.rs_parity_len() as usize / 2;
        let burst = words * per_codeword;

        let mut damaged = raw.clone();
        for byte in damaged.iter_mut().take(burst) {
            *byte ^= 0xFF;
        }

        assert_eq!(codec.decode(&damaged, &[]).unwrap(), payload, "burst of {burst} bytes");
    }

    #[test]
    fn a_frame_payload_round_trips_for_every_profile() {
        for profile in &PROFILES {
            let codec = PayloadCodec::for_profile(profile);
            let payload: Vec<u8> =
                (0..codec.capacity()).map(|i| u8::try_from(i % 241).unwrap()).collect();
            let raw = codec.encode(&payload).unwrap();
            assert_eq!(raw.len(), profile.raw_bytes() as usize, "{}", profile.name);
            assert_eq!(codec.decode(&raw, &[]).unwrap(), payload, "{}", profile.name);
        }
    }

    #[test]
    fn an_oversized_payload_is_refused() {
        let codec = PayloadCodec::for_profile(&PROFILES[0]);
        let payload = vec![0u8; codec.capacity() + 1];
        assert_eq!(codec.encode(&payload), Err(FecError::InvalidLength));
    }

    #[test]
    fn erasure_positions_follow_the_bytes_through_the_interleaver() {
        // If the erasure map and the byte map ever disagree, erasure decoding
        // would point the corrector at healthy bytes and leave the damage.
        let profile = &PROFILES[0];
        let codec = PayloadCodec::for_profile(profile);
        let payload: Vec<u8> =
            (0..codec.capacity()).map(|i| u8::try_from(i % 253).unwrap()).collect();
        let raw = codec.encode(&payload).unwrap();

        let partition = profile.rs_partition();
        let words = partition.codeword_count() as usize;
        let burst = words * profile.rs_parity_len() as usize;

        let mut damaged = raw;
        let mut erasures = Vec::new();
        for (index, byte) in damaged.iter_mut().enumerate().take(burst) {
            *byte ^= 0xA5;
            erasures.push(index);
        }

        assert_eq!(codec.decode(&damaged, &erasures).unwrap(), payload);
    }
}
