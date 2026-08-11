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

[Unreleased]: https://github.com/IMNascimento/PhotonProtocol/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/IMNascimento/PhotonProtocol/releases/tag/v0.1.0
