//! The session layer: what is being sent, how it was packed, and whether it
//! arrived intact.
//!
//! The manifest (`SPEC.md` §7.1) is small and everything else is meaningless
//! without it, so it is sent redundantly rather than once. It carries the digest
//! that is the protocol's only end-to-end guarantee: Reed-Solomon, the unit
//! checksums and RaptorQ all protect *throughput*, and only the SHA-256 protects
//! correctness.
//!
//! Everything here treats the manifest as hostile input. It arrives over a
//! channel anyone can film and anyone can replace, and it declares a file name
//! and a decompressed size — one of which will be used to write to a disk and
//! the other to allocate memory.

use std::io::Write;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::{MANIFEST_MAGIC, MANIFEST_VERSION};

/// Manifest length before the file name (`SPEC.md` §7.1).
pub const MANIFEST_FIXED_LEN: usize = 61;

/// Longest file name the format can express.
pub const MAX_NAME_LEN: usize = 255;

/// Brotli quality used by the emitter.
///
/// The highest setting. Encoder time is not a constraint on this channel: a
/// frame carries a few kilobytes and the display emits about thirty a second,
/// so the compressor finishes long before the channel does, and every byte it
/// saves is a frame nobody has to film.
pub const BROTLI_QUALITY: u32 = 11;

/// Brotli window size, as a base-2 logarithm.
pub const BROTLI_WINDOW: u32 = 22;

/// Compression must save at least this fraction to be worth declaring
/// (`SPEC.md` §7.2).
const MIN_COMPRESSION_GAIN: f64 = 0.02;

/// Compression algorithms (`SPEC.md` §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Stored as-is.
    None,
    /// Brotli, RFC 7932. Required of every implementation.
    Brotli,
    /// Zstandard, RFC 8878. Registered but optional, and not implemented here:
    /// its reference library is C, and both web pages run as WebAssembly.
    Zstd,
}

impl Compression {
    /// The byte written to the manifest.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::None => 0x00,
            Self::Brotli => 0x01,
            Self::Zstd => 0x02,
        }
    }

    /// Parses a compression byte.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for an unregistered algorithm.
    pub const fn from_u8(byte: u8) -> Result<Self> {
        match byte {
            0x00 => Ok(Self::None),
            0x01 => Ok(Self::Brotli),
            0x02 => Ok(Self::Zstd),
            _ => Err(Error::Malformed {
                context: "manifest",
                detail: "unregistered compression algorithm",
            }),
        }
    }

    /// Whether this build can decompress it.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::None | Self::Brotli)
    }
}

/// The SHA-256 of a byte slice.
#[must_use]
pub fn digest(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compresses a payload, or declines to.
///
/// Returns the algorithm actually used alongside the bytes. Input that is
/// already compressed — a JPEG, an MP4, a ZIP, anything encrypted — does not
/// shrink, and spending a pass on it to gain half a percent would only add a
/// decompression step and a way to fail.
#[must_use]
pub fn compress(data: &[u8]) -> (Compression, Vec<u8>) {
    let mut out = Vec::new();
    let params = brotli::enc::BrotliEncoderParams {
        quality: i32::try_from(BROTLI_QUALITY).unwrap_or(11),
        lgwin: i32::try_from(BROTLI_WINDOW).unwrap_or(22),
        ..Default::default()
    };

    let mut input = data;
    if brotli::BrotliCompress(&mut input, &mut out, &params).is_err() {
        return (Compression::None, data.to_vec());
    }

    let threshold = (data.len() as f64) * (1.0 - MIN_COMPRESSION_GAIN);
    if (out.len() as f64) <= threshold {
        (Compression::Brotli, out)
    } else {
        (Compression::None, data.to_vec())
    }
}

/// Decompresses a payload, refusing to exceed `expected_len`.
///
/// The bound is load-bearing rather than defensive tidiness. A decompressor
/// asked to expand a hostile or corrupted stream will happily allocate until the
/// process dies, and the size it is told to expect arrives over the same channel
/// as the data.
///
/// # Errors
///
/// Returns [`Error::DecompressionFailed`] if the stream is invalid, if it
/// expands past `expected_len`, or if the algorithm is one this build does not
/// implement.
pub fn decompress(compression: Compression, data: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    match compression {
        Compression::None => {
            if data.len() == expected_len {
                Ok(data.to_vec())
            } else {
                Err(Error::DecompressionFailed)
            }
        }
        Compression::Brotli => {
            let mut sink = BoundedSink::new(expected_len);
            {
                let mut writer = brotli::DecompressorWriter::new(&mut sink, 4096);
                if writer.write_all(data).is_err() || writer.flush().is_err() {
                    return Err(Error::DecompressionFailed);
                }
            }
            let out = sink.into_inner();
            if out.len() == expected_len { Ok(out) } else { Err(Error::DecompressionFailed) }
        }
        Compression::Zstd => Err(Error::DecompressionFailed),
    }
}

/// A sink that refuses to grow past a limit, so a decompressor cannot be used to
/// exhaust memory.
struct BoundedSink {
    buffer: Vec<u8>,
    limit: usize,
}

impl BoundedSink {
    fn new(limit: usize) -> Self {
        Self { buffer: Vec::new(), limit }
    }

    fn into_inner(self) -> Vec<u8> {
        self.buffer
    }
}

impl Write for BoundedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.buffer.len() + buf.len() > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "decompressed output exceeds the declared size",
            ));
        }
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Everything a receiver needs to know about the transfer (`SPEC.md` §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// How the payload was packed.
    pub compression: Compression,
    /// Size of the file before compression.
    pub original_size: u64,
    /// SHA-256 of the file before compression.
    pub sha256: [u8; 32],
    /// RaptorQ Object Transmission Information, RFC 6330 byte order.
    pub oti: [u8; 12],
    /// File name, UTF-8, no path separators.
    pub name: String,
}

impl Manifest {
    /// Serialises the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] if the file name is longer than the format
    /// allows or is not a bare name.
    pub fn encode(&self) -> Result<Vec<u8>> {
        validate_name(&self.name)?;
        let name = self.name.as_bytes();

        let mut out = Vec::with_capacity(MANIFEST_FIXED_LEN + name.len());
        out.extend_from_slice(&MANIFEST_MAGIC);
        out.push(MANIFEST_VERSION);
        out.push(self.compression.as_u8());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.original_size.to_le_bytes());
        out.extend_from_slice(&self.sha256);
        out.extend_from_slice(&self.oti);
        out.push(u8::try_from(name.len()).map_err(|_| Error::Malformed {
            context: "manifest",
            detail: "file name exceeds 255 bytes",
        })?);
        out.extend_from_slice(name);
        Ok(out)
    }

    /// Parses a manifest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for a bad magic, a truncated body, an
    /// unknown version, an unregistered compression algorithm, or a file name
    /// that is not a bare, valid UTF-8 name.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let Some(fixed) = bytes.get(..MANIFEST_FIXED_LEN) else {
            return Err(Error::Malformed { context: "manifest", detail: "too short" });
        };
        if fixed[0..4] != MANIFEST_MAGIC {
            return Err(Error::Malformed { context: "manifest", detail: "bad magic" });
        }
        if fixed[4] != MANIFEST_VERSION {
            return Err(Error::Malformed { context: "manifest", detail: "unknown version" });
        }

        let compression = Compression::from_u8(fixed[5])?;
        let original_size = u64::from_le_bytes(
            fixed[8..16]
                .try_into()
                .map_err(|_| Error::Malformed { context: "manifest", detail: "truncated size" })?,
        );
        let sha256: [u8; 32] = fixed[16..48]
            .try_into()
            .map_err(|_| Error::Malformed { context: "manifest", detail: "truncated digest" })?;
        let oti: [u8; 12] = fixed[48..60].try_into().map_err(|_| Error::Malformed {
            context: "manifest",
            detail: "truncated transmission information",
        })?;

        let name_len = usize::from(fixed[60]);
        let Some(name_bytes) = bytes.get(MANIFEST_FIXED_LEN..MANIFEST_FIXED_LEN + name_len) else {
            return Err(Error::Malformed { context: "manifest", detail: "truncated file name" });
        };
        let name = core::str::from_utf8(name_bytes)
            .map_err(|_| Error::Malformed {
                context: "manifest",
                detail: "file name is not UTF-8",
            })?
            .to_owned();
        validate_name(&name)?;

        Ok(Self { compression, original_size, sha256, oti, name })
    }

    /// Bytes this manifest occupies.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        MANIFEST_FIXED_LEN + self.name.len()
    }

    /// Checks a reconstructed file against the manifest (`SPEC.md` §7.3).
    ///
    /// # Errors
    ///
    /// Returns [`Error::HashMismatch`] if the size or the digest disagrees.
    /// Nothing may be shown to the user when this fails: it is the protocol's
    /// only end-to-end check.
    pub fn verify(&self, file: &[u8]) -> Result<()> {
        if file.len() as u64 != self.original_size || digest(file) != self.sha256 {
            return Err(Error::HashMismatch);
        }
        Ok(())
    }
}

/// Rejects anything that is not a bare file name (`SPEC.md` §7.1).
///
/// The name arrives over a channel with no authentication whatsoever and is
/// destined for a filesystem, so `../../.ssh/authorized_keys` has to stop here
/// rather than at whichever caller forgets to check.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Malformed { context: "manifest", detail: "empty file name" });
    }
    if name.len() > MAX_NAME_LEN {
        return Err(Error::Malformed {
            context: "manifest",
            detail: "file name exceeds 255 bytes",
        });
    }
    if name == "." || name == ".." {
        return Err(Error::Malformed {
            context: "manifest",
            detail: "file name is a directory reference",
        });
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(Error::Malformed {
            context: "manifest",
            detail: "file name contains a path separator",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        Manifest {
            compression: Compression::Brotli,
            original_size: 1_048_576,
            sha256: digest(b"abc"),
            oti: [0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x04, 0x80, 0x01, 0x00, 0x01, 0x08],
            name: "report.pdf".to_owned(),
        }
    }

    #[test]
    fn the_digest_matches_the_published_vector() {
        // SHA-256 of "abc", the standard FIPS 180-4 example, and the value the
        // specification's manifest vector is built from.
        let expected = hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(digest(b"abc").to_vec(), expected);
    }

    #[test]
    fn the_manifest_matches_the_published_test_vector() {
        // SPEC.md 10.3, byte for byte.
        let encoded = sample_manifest().encode().unwrap();
        let expected = hex(concat!(
            "504854 4D01010000 000010000000 0000",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "000008000000048001000108",
            "0A",
            "7265706F72742E706466",
        ));
        assert_eq!(encoded.len(), 71);
        assert_eq!(encoded, expected, "encoded manifest does not match SPEC.md 10.3");

        assert_eq!(
            digest(&encoded).to_vec(),
            hex("8349301C214A9CF5B0BDF5A1F1A25CA102000C6DD64FD0AD4299E561CE723D68"),
            "manifest digest does not match SPEC.md 10.3"
        );
    }

    #[test]
    fn the_manifest_unit_matches_the_published_test_vector() {
        // SPEC.md 10.4: the same manifest wrapped in a payload unit.
        use crate::codec::{PayloadUnit, encode_units, unit_type};

        let manifest = sample_manifest().encode().unwrap();
        let unit = PayloadUnit::new(unit_type::MANIFEST, manifest);
        let encoded = encode_units(&[unit], 256).unwrap();

        assert_eq!(encoded.len(), 78);
        assert_eq!(&encoded[..8], &hex("0147005048544D01")[..]);
        assert_eq!(&encoded[70..], &hex("2E706466C8E70067")[..]);
        assert_eq!(
            digest(&encoded).to_vec(),
            hex("5E2E9845F4CBFA4DA69021CFD8B5CDF4E56647A907C75A165469E1FA93CC9799"),
            "unit digest does not match SPEC.md 10.4"
        );
    }

    #[test]
    fn manifests_round_trip() {
        let manifest = sample_manifest();
        let encoded = manifest.encode().unwrap();
        assert_eq!(Manifest::decode(&encoded).unwrap(), manifest);
        assert_eq!(manifest.encoded_len(), encoded.len());
    }

    #[test]
    fn a_truncated_manifest_is_refused_rather_than_read_past() {
        let encoded = sample_manifest().encode().unwrap();
        for cut in 0..encoded.len() {
            assert!(Manifest::decode(&encoded[..cut]).is_err(), "accepted a {cut}-byte manifest");
        }
    }

    #[test]
    fn path_traversal_in_the_file_name_is_refused() {
        // The name arrives over an unauthenticated channel and is headed for a
        // filesystem. Every one of these has to stop in the parser, not in
        // whichever caller remembers to check.
        for hostile in ["../secrets", "..\\secrets", "/etc/passwd", "a/b", "..", ".", ""] {
            let mut manifest = sample_manifest();
            manifest.name = hostile.to_owned();
            assert!(manifest.encode().is_err(), "encoder accepted {hostile:?}");
        }

        // And on the way in, where the bytes were not written by us.
        let mut encoded = sample_manifest().encode().unwrap();
        encoded[60] = 3;
        encoded.truncate(MANIFEST_FIXED_LEN);
        encoded.extend_from_slice(b"../");
        assert!(Manifest::decode(&encoded).is_err(), "decoder accepted a traversal");
    }

    #[test]
    fn a_non_utf8_file_name_is_refused() {
        let mut encoded = sample_manifest().encode().unwrap();
        encoded.truncate(MANIFEST_FIXED_LEN);
        encoded[60] = 2;
        encoded.extend_from_slice(&[0xFF, 0xFE]);
        assert!(Manifest::decode(&encoded).is_err());
    }

    #[test]
    fn compressible_input_is_compressed_and_comes_back() {
        let data: Vec<u8> = (0..20_000).map(|i| b"photon protocol "[i % 16]).collect();
        let (algorithm, packed) = compress(&data);
        assert_eq!(algorithm, Compression::Brotli);
        assert!(packed.len() < data.len() / 10, "repetitive input barely shrank");
        assert_eq!(decompress(algorithm, &packed, data.len()).unwrap(), data);
    }

    #[test]
    fn incompressible_input_is_left_alone() {
        // Most of what people actually send is already compressed. Declaring an
        // algorithm that gains nothing would only add a way to fail.
        let mut state = 0x1234_5678u32;
        let data: Vec<u8> = (0..20_000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                u8::try_from(state >> 24).unwrap_or(0)
            })
            .collect();

        let (algorithm, packed) = compress(&data);
        assert_eq!(algorithm, Compression::None);
        assert_eq!(packed, data);
        assert_eq!(decompress(algorithm, &packed, data.len()).unwrap(), data);
    }

    #[test]
    fn decompression_will_not_exceed_the_declared_size() {
        // A decompression bomb is a manifest that declares a small file and
        // carries a stream that expands without bound. The limit has to be
        // enforced while decompressing, not checked afterwards.
        let data = vec![0u8; 4_000_000];
        let (algorithm, packed) = compress(&data);
        assert_eq!(algorithm, Compression::Brotli);
        assert!(packed.len() < 10_000, "a zero-filled megabyte should compress hard");

        assert_eq!(decompress(algorithm, &packed, 1024), Err(Error::DecompressionFailed));
        assert_eq!(decompress(algorithm, &packed, data.len()).unwrap().len(), data.len());
    }

    #[test]
    fn an_unimplemented_algorithm_fails_rather_than_returning_rubbish() {
        assert!(!Compression::Zstd.is_supported());
        assert_eq!(decompress(Compression::Zstd, &[1, 2, 3], 3), Err(Error::DecompressionFailed));
    }

    #[test]
    fn verification_rejects_a_file_that_does_not_match() {
        let file = b"the original bytes".to_vec();
        let manifest = Manifest {
            compression: Compression::None,
            original_size: file.len() as u64,
            sha256: digest(&file),
            oti: [0; 12],
            name: "a.bin".to_owned(),
        };

        assert!(manifest.verify(&file).is_ok());

        let mut altered = file.clone();
        altered[0] ^= 0x01;
        assert_eq!(manifest.verify(&altered), Err(Error::HashMismatch));
        assert_eq!(manifest.verify(&file[..file.len() - 1]), Err(Error::HashMismatch));
    }

    fn hex(text: &str) -> Vec<u8> {
        let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        (0..cleaned.len() / 2)
            .map(|i| u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }
}
