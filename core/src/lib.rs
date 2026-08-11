//! Reference implementation of PhotonProtocol.
//!
//! PhotonProtocol moves a file across an optical, simplex channel: an emitter
//! paints the file as a sequence of dense visual codes on a screen, a camera
//! films that screen, and a decoder reconstructs the file from the recording.
//! There is no return channel, so the receiver can never ask for anything to be
//! sent again — that single constraint shapes the whole design.
//!
//! The wire format is defined by [`SPEC.md`] at the repository root. This crate
//! implements it and nothing else: it performs no I/O, touches no filesystem,
//! decodes no container formats, and draws to no screen. Those belong to the
//! `photon-cli` and `photon-wasm` crates, which adapt this one to a terminal and
//! to a browser respectively. Keeping the protocol free of its environments is
//! what lets the same bytes be produced natively and under WebAssembly.
//!
//! [`SPEC.md`]: https://github.com/IMNascimento/PhotonProtocol/blob/main/SPEC.md
//!
//! # Layers
//!
//! The module layout mirrors the specification's layer model, and dependencies
//! only ever point downwards:
//!
//! | Layer | Concern | Module |
//! | ----- | ------- | ------ |
//! | L1 | cells, palette, frame geometry | [`profile`] |
//! | L2 | frames, headers, forward error correction | *(pending)* |
//! | L3 | RaptorQ transport | *(pending)* |
//! | L4 | manifest, compression, integrity | *(pending)* |
//!
//! # Status
//!
//! The specification is a draft and the wire format is unstable until it is
//! tagged `1.0`. The layers marked *pending* land once the draft has been
//! reviewed; implementing them earlier would only produce code that has to be
//! rewritten against a format that moved underneath it.

pub mod error;
pub mod geom;
pub mod image;
pub mod profile;
pub mod symbol;

pub use error::{Error, Result};
pub use geom::{Homography, Point};
pub use image::RgbImage;
pub use profile::{Profile, ProfileId, RsPartition};
pub use symbol::{Alphabet, CellSample, Classification, Classifier, Rgb, Rgbf};

/// Value of the `protocol_version` header field this crate implements.
///
/// See `SPEC.md` §5.1. A decoder must check this before interpreting any other
/// field, and must refuse a frame carrying anything else.
pub const PROTOCOL_VERSION: u8 = 0x01;

/// Value of the `manifest_version` field this crate implements.
///
/// See `SPEC.md` §7.1.
pub const MANIFEST_VERSION: u8 = 0x01;

/// Magic bytes that open every frame header: ASCII `PHTN`.
pub const FRAME_MAGIC: [u8; 4] = *b"PHTN";

/// Magic bytes that open every manifest: ASCII `PHTM`.
pub const MANIFEST_MAGIC: [u8; 4] = *b"PHTM";

/// Version of the specification document this crate was written against.
pub const SPEC_VERSION: &str = "0.1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magics_are_distinct_and_ascii() {
        // A decoder scanning a corrupted byte stream distinguishes a frame
        // header from a manifest by these four bytes alone, so they must not
        // collide and must not be confusable with binary noise.
        assert_ne!(FRAME_MAGIC, MANIFEST_MAGIC);
        assert!(FRAME_MAGIC.iter().all(u8::is_ascii_uppercase));
        assert!(MANIFEST_MAGIC.iter().all(u8::is_ascii_uppercase));
        assert_eq!(&FRAME_MAGIC[..3], &MANIFEST_MAGIC[..3]);
    }
}
