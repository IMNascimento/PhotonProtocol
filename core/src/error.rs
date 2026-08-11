//! The failure taxonomy of `SPEC.md` §9.2.
//!
//! A decoder that reports "failed" tells the user nothing they can act on. The
//! useful question after a failed transfer is always the same — should I film
//! closer, hold steadier, turn the brightness up, or record for longer? — and
//! only the decoder knows. Every variant here therefore names the stage that
//! gave up, and the ones that can say how close they came carry the numbers.

use core::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = core::result::Result<T, Error>;

/// Why a decode did not produce the file.
///
/// The variants correspond one to one with the conditions a conforming decoder
/// must be able to distinguish (`SPEC.md` §9.2).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No frame could be located in any video frame: the four finder patterns
    /// were never found together.
    NoFinders,

    /// Finder patterns were found but the orientation tag could not be resolved,
    /// so the frame's rotation is unknown.
    NoOrientation,

    /// Both copies of the frame header failed Reed-Solomon correction or the
    /// CRC check.
    HeaderUnrecoverable,

    /// The frame declares a `protocol_version` this implementation does not
    /// support.
    VersionUnsupported {
        /// The version byte read from the frame header.
        found: u8,
        /// The version byte this implementation writes and accepts.
        supported: u8,
    },

    /// The frame declares a `profile_id` this implementation does not know. A
    /// decoder must not guess at the geometry.
    ProfileUnsupported {
        /// The profile identifier read from the frame header.
        found: u8,
    },

    /// Reed-Solomon decoding failed across the whole frame payload, leaving no
    /// unit recoverable.
    FrameFecFailed,

    /// Symbols were collected but no manifest ever passed its CRC, so the
    /// transfer's parameters are unknown.
    NoManifest,

    /// The manifest is known but not enough distinct encoding symbols were
    /// captured to reconstruct the object.
    InsufficientSymbols {
        /// Distinct symbols accepted so far.
        accepted: usize,
        /// Symbols needed for the transport decoder to succeed.
        needed: usize,
    },

    /// RaptorQ decoding failed even though enough symbols appeared to be
    /// available.
    TransportDecodeFailed,

    /// Decompression failed, or produced more bytes than the manifest declared.
    DecompressionFailed,

    /// The reconstructed file does not match the digest in the manifest. This is
    /// the only end-to-end check in the protocol; nothing may be handed to the
    /// user when it fails.
    HashMismatch,

    /// A structure was malformed in a way the format forbids: a bad magic, an
    /// out-of-range length, a file name containing a path separator.
    Malformed {
        /// What was being parsed.
        context: &'static str,
        /// Why it was rejected.
        detail: &'static str,
    },
}

impl Error {
    /// Stable, machine-readable identifier, matching the names tabulated in
    /// `SPEC.md` §9.2.
    ///
    /// User interfaces localise their own prose; this is what they key off, and
    /// what belongs in logs and bug reports.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoFinders => "E_NO_FINDERS",
            Self::NoOrientation => "E_NO_ORIENTATION",
            Self::HeaderUnrecoverable => "E_HEADER_UNRECOVERABLE",
            Self::VersionUnsupported { .. } => "E_VERSION_UNSUPPORTED",
            Self::ProfileUnsupported { .. } => "E_PROFILE_UNSUPPORTED",
            Self::FrameFecFailed => "E_FRAME_FEC_FAILED",
            Self::NoManifest => "E_NO_MANIFEST",
            Self::InsufficientSymbols { .. } => "E_INSUFFICIENT_SYMBOLS",
            Self::TransportDecodeFailed => "E_TRANSPORT_DECODE_FAILED",
            Self::DecompressionFailed => "E_DECOMPRESSION_FAILED",
            Self::HashMismatch => "E_HASH_MISMATCH",
            Self::Malformed { .. } => "E_MALFORMED",
        }
    }

    /// Whether filming the same transmission for longer would plausibly fix it.
    ///
    /// This is the distinction a user interface actually needs: a shortfall of
    /// symbols is cured by recording more, whereas an unsupported profile or a
    /// digest mismatch never is.
    #[must_use]
    pub const fn is_recoverable_by_more_capture(&self) -> bool {
        matches!(
            self,
            Self::NoFinders
                | Self::NoOrientation
                | Self::HeaderUnrecoverable
                | Self::FrameFecFailed
                | Self::NoManifest
                | Self::InsufficientSymbols { .. }
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFinders => f.write_str("no code was found in the recording"),
            Self::NoOrientation => {
                f.write_str("a code was found but its orientation could not be resolved")
            }
            Self::HeaderUnrecoverable => {
                f.write_str("both copies of the frame header were unreadable")
            }
            Self::VersionUnsupported { found, supported } => write!(
                f,
                "protocol version {found} is not supported (this build implements {supported})"
            ),
            Self::ProfileUnsupported { found } => {
                write!(f, "profile 0x{found:02x} is not supported")
            }
            Self::FrameFecFailed => f.write_str("error correction failed for the whole frame"),
            Self::NoManifest => f.write_str("no readable manifest was captured"),
            Self::InsufficientSymbols { accepted, needed } => {
                write!(f, "not enough data captured: {accepted} of about {needed} blocks")
            }
            Self::TransportDecodeFailed => f.write_str("fountain code reconstruction failed"),
            Self::DecompressionFailed => f.write_str("decompression failed"),
            Self::HashMismatch => {
                f.write_str("the reconstructed file does not match its declared digest")
            }
            Self::Malformed { context, detail } => write!(f, "malformed {context}: {detail}"),
        }
    }
}

impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique() {
        let errors = [
            Error::NoFinders,
            Error::NoOrientation,
            Error::HeaderUnrecoverable,
            Error::VersionUnsupported { found: 2, supported: 1 },
            Error::ProfileUnsupported { found: 9 },
            Error::FrameFecFailed,
            Error::NoManifest,
            Error::InsufficientSymbols { accepted: 1, needed: 2 },
            Error::TransportDecodeFailed,
            Error::DecompressionFailed,
            Error::HashMismatch,
            Error::Malformed { context: "manifest", detail: "bad magic" },
        ];

        let mut codes: Vec<&str> = errors.iter().map(Error::code).collect();
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "two variants share a code");
    }

    #[test]
    fn shortfall_reports_how_close_it_came() {
        // A bare "not enough data" is exactly the message this taxonomy exists
        // to avoid: the user cannot tell a near miss from a hopeless one.
        let err = Error::InsufficientSymbols { accepted: 418, needed: 512 };
        let text = err.to_string();
        assert!(text.contains("418"), "{text}");
        assert!(text.contains("512"), "{text}");
        assert!(err.is_recoverable_by_more_capture());
    }

    #[test]
    fn digest_mismatch_is_not_cured_by_filming_longer() {
        assert!(!Error::HashMismatch.is_recoverable_by_more_capture());
        assert!(!Error::ProfileUnsupported { found: 0x7f }.is_recoverable_by_more_capture());
    }
}
