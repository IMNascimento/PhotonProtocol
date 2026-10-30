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

The first version that has moved a file from a monitor to a phone: an 818 KB
picture to an iPhone, at 23.7 KB/s with ten codes shown a second and 38.2 KB/s
with twenty, using `P1-conservative`. The denser profiles decode through the
simulated camera and do not yet decode through the real one.
`docs/phase-2-report.md` has the measurements and `docs/benchmarks` the
figures to beat.

### The dense format, on `experiment/throughput`

A second format, for the camera there is rather than the one the first format
hoped for: black and white, the shape of the screen, read in tiles.
`SPEC.md` §13, and `docs/phase-3-report.md` for how it came about.

**Simulated, and not yet tried on a phone.** Through the simulated camera a
mebibyte that will not compress arrives in 2.14 s with a camera taking thirty
pictures a second, which is 479 KB/s, and in 1.15 s with one taking sixty,
which is 888 KB/s. Through the receiving page in Chrome, with the first of
those as its camera, in 2.2 s. The model has flattered before.

- `photon_core::dense`: the profiles `D1-swift`, `D2-rapid` and `D3-blaze`,
  carrying 5040, 8800 and 17952 bytes a frame; a transmitter, a reader that
  keeps nothing, and a receiver.
- The light of each module's neighbours is taken out of it, by a channel
  measured for each tile. Without it `D3-blaze` reads nothing at 2.5 camera
  pixels to a module.
- A code that is known is taken out of a picture that has two in it, and what
  is left is read. So a picture taken as the code changes yields both, and the
  codes can change as fast as the display refreshes.
- `photon film --profile d1|d2|d3`, and `photon decode`, `bench` and
  `profiles` for dense codes. `tools/e2e/matrix.mjs` has rows for them.
- Both pages. The sending page paints a dense code at a whole number of pixels
  to a module with the screen to itself. The receiving page tells which format
  it is looking at from the picture, asks the camera for sixty pictures a
  second and settles for thirty, and has pictures copied out of the video by
  the workers that read them rather than by the page.

### Changed, on `experiment/throughput`

- **Frames are whitened** (`SPEC.md` §5.2.4), which changes what is painted.
  Frames of draft 0.2 are not read by this decoder, and the reverse.
  `photon --unwhitened` reads and paints them as they were.
- `SPEC.md` is draft 0.3.
- The grid of a frame of cells is measured from the edges of the cells
  themselves, its alphabet from its payload, and its black and white by
  neighbourhood. Of 33 pictures of `P4-balanced` that an iPhone took and no
  decoder read, 15 now read.

### Fixed

- **No real capture ever decoded, and this is why.** The classifier decided a
  cell's shape by luma and its colour afterwards. The palette's blue has a
  quarter of the luma of its white, so through a lens that blurs at all, a blue
  cell's own pattern was fainter than what its bright neighbours spilt into it:
  27% of blue cells and 10% of red ones were read as the wrong shape, and none
  of the green or white. That put every frame at or over what its parity could
  repair. Cells are now matched whole, in colour, against templates measured in
  the same frame, and then matched again against templates measured from the
  payload itself. Wrong cells in a typical capture fell from 9.3% to under
  0.1%.
- Finder patterns were not found in a well-lit capture at small sizes. Light
  spreads into dark in a photograph, so the rings of a finder do not measure
  1:1:3:1:1; they are now judged by where their middles fall, which spreading
  does not move.
- The sampling grid followed a homography, and a lens is not a pinhole. The grid
  is now bent to fit the timing ring, which follows a lens that bends lines by
  4% to within a tenth of a cell.
- The sending page held each code for a number of display refreshes, assumed to
  be sixtieths of a second. On a 144 Hz display the default was 28 ms a code,
  which no phone camera can photograph whole. It now measures the display and
  works in codes per second.
- The sending page chose the densest profile the screen could paint. What
  limits a transfer is the camera, which the sender cannot see. It now defaults
  to `P1-conservative`.
- The receiving page asked the camera for 4K. It now asks for 1080p, which is
  a quarter of the work for every picture, and reads only the part of the
  picture the code is in.
- Sending a photograph of a few megabytes froze the sending page for many
  seconds while Brotli searched it for savings it did not contain. Whether
  input will shrink is now judged from three samples first.

### Added

- `P4-balanced`, profile `0x04`: the alphabet and parity of `P1-conservative`
  on a 128-cell grid, carrying 4931 bytes a frame. The profiles were cut for
  cameras that record 4K and a browser is usually given 1080p, at which
  `P1-conservative` was the only one that read. This is the rung that was
  missing.
- `photon film`: a simulated phone camera pointed at a simulated monitor, with
  the display and the camera on separate clocks. Writes the pictures a browser
  would have been handed, as PNGs and as a `.y4m` file Chrome plays as a
  camera.
- `photon decode --truth`: given the file that was sent, counts the cells read
  wrongly, by painted colour and by region of the frame.
- `photon bench`: how long each stage of reading a picture takes.
- `photon inspect`: what can be said of a picture nobody has the file for —
  where the grid landed, and how well the calibration ring reads itself.
- `tools/e2e`: the real pages driven in a real browser. `receive.mjs` runs the
  receiving page with simulated footage as its camera and checks the file it
  offers; `send.mjs` photographs what the sending page paints; `matrix.mjs`
  films a table of conditions and says how each went.
- The receiving page reads several pictures at once, in as many workers as the
  device has cores to spare, and drops a code it already has at its header
  rather than after reading its payload.
- The receiving page outlines the code it has found, says how long is left, and
  offers the file by itself when it is complete.
- Both pages in Portuguese as well as English, chosen from the browser's
  language.
- With `?capture`, both pages report what they measure to the development
  server, so that what a real phone did can be read from a terminal.

### Changed

- `SPEC.md` is draft 0.2. The wire format of the existing profiles is
  unchanged. `S_min` falls to 4 and 6 pixels, the default profile becomes
  `P1-conservative`, display timing is given in time rather than refreshes, and
  the decoder guidance of §9.1 describes what was found to be necessary.
- Reading a picture takes 15 ms on a desktop core, from 50 ms.

## [0.3.0] — 2026-08-11

Phases 2, 3 and 4. There is now something to point a camera at, and something
to give the recording to.

Live at <https://imnascimento.github.io/PhotonProtocol/>.

### Added

- `photon encode` and `photon decode`: a file becomes numbered PNGs and,
  optionally, a lossless video; a recording — or a directory of extracted
  frames — becomes the file again. The decoder reports frames located, survived
  and duplicated, the share of doubtful cells, and throughput both over the
  whole recording and over the frames it actually needed.
- The sending and receiving pages, deployed to GitHub Pages from `develop`.
  Both run entirely on the device; nothing is uploaded.
- WebAssembly bindings for the real codec: an `Emitter` that paints frames on
  demand, a `Receiver` that takes video frames, and a cheap frame locator.
- `tools/wasm-smoke.mjs`, which drives the same round trip through the generated
  JavaScript, covering the boundary the Rust tests cannot reach.
- `tools/build-site.mjs`, so the site can be assembled and served locally rather
  than only inside a workflow.

### Changed

- `Receiver` splits into `examine` and `absorb`. Reading a frame depends on no
  other frame, so it now runs across every core in the CLI and off the main
  thread in the browser.
- The command line takes subcommands and flags through `clap`, which the phase 0
  commit said would be worth its dependency once there was a command surface to
  parse.

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

[Unreleased]: https://github.com/IMNascimento/PhotonProtocol/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/IMNascimento/PhotonProtocol/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/IMNascimento/PhotonProtocol/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/IMNascimento/PhotonProtocol/releases/tag/v0.1.0
