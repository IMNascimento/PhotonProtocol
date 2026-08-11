# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Two version numbers move independently and should not be confused:

- the **crate version**, which is what the tags below record;
- the **protocol version**, the `protocol_version` byte in every frame header,
  which stays at `1` until the wire format changes incompatibly.

While the specification is a draft, the wire format may change in any release.
Drafts are not interoperable with one another.

## [Unreleased]

## [0.2.0] — 2026-08-11

Phase 1 complete. A file survives the whole pipeline — compress, fountain-code,
frame, paint, distort, locate, sample, classify, repair, reassemble, decompress,
verify — through a distorted synthetic channel, with the receiver finding the
code area in the picture itself.

The wire format is still a draft and may change in any release.

### Added

- Complete encoder and decoder in `photon-core`: cell alphabet and per-frame
  classifier, frame geometry with rendering and sampling, Reed-Solomon over
  GF(256) with erasures and interleaving, frame headers and payload units, the
  session manifest with Brotli and SHA-256, the RaptorQ symbol stream, and the
  transmitter and receiver that tie them together.
- Frame detection: local thresholding, a finder-pattern sweep verified radially
  in eight directions, corner selection over the convex hull, and a homography
  checked against the centre alignment marker and the orientation tag.
  `Receiver::accept_image` takes video frames rather than located ones.
- A configurable, seeded channel simulator, and `photon simulate` to sweep it.
- `docs/phase-1-report.md`: the first measurements of the physical layer,
  including a negative result about erasure decoding.

### Changed

- Compression is Brotli rather than Zstandard. Zstandard's reference library is
  C and both web pages run as WebAssembly; Brotli is pure Rust and, measured on
  a source-and-documentation corpus, 8.2% smaller. Zstandard keeps a registered
  identifier as an optional algorithm.
- The declared minimum supported Rust version is 1.89, which `raptorq` sets
  rather than this code.

### Fixed

- The alignment module was rendered one cell up and to the left of where
  `SPEC.md` §4.2.5 places it. The encoder and decoder shared the error, so no
  round trip could detect it; only a third-party implementation would have.
- Two half-pixel errors, one between the renderer and the sampler and one in the
  detector's run-centre arithmetic. Each shifted every sample in a frame by a
  third of a sub-cell.
- Forney's error evaluator was built from a syndrome polynomial missing its
  degree-zero term, producing error magnitudes that were individually plausible
  and collectively wrong.

## [0.1.0] — 2026-08-11

Phase 0: the specification, and the scaffolding needed to work on it.

### Added

- `SPEC.md` 0.1 (draft): the complete wire format across all four layers, with
  binary layouts, scan orders, error-correction parameters and test vectors, so
  a third-party implementation can validate compatibility without reading the
  reference code. Seven decisions that should be settled by measurement rather
  than argument are recorded as open questions instead of guessed at.
- `photon-core`: the protocol crate. Profile registry with frame geometry
  derived from the specification's formulas, and the decoder failure taxonomy of
  `SPEC.md` §9.2. No I/O, no platform dependencies.
- `photon-cli`: bench command line, reporting the profile registry and versions.
- `photon-wasm`: WebAssembly bindings, an adapter over `photon-core`.
- `tools/`: independent derivations of the constants the specification
  publishes — the blur-aware shape alphabet search, the frame geometry, and the
  test vectors — so the numbers can be reproduced or contested rather than
  trusted.
- CI covering formatting, lint, tests on three platforms, the minimum supported
  Rust version, the WebAssembly build, and a cross-check of the specification's
  numbers against the Node derivations.

### Changed

- Relicensed from the bootstrap template's proprietary licence to the customary
  Rust dual MIT / Apache-2.0, which an open protocol requires.
- Replaced the Python-oriented template documentation and ignore rules.

[Unreleased]: https://github.com/IMNascimento/PhotonProtocol/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/IMNascimento/PhotonProtocol/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/IMNascimento/PhotonProtocol/releases/tag/v0.1.0
