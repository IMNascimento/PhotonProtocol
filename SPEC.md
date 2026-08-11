# PhotonProtocol — Specification

**Version:** 0.1 (draft)
**Status:** DRAFT. The wire format is unstable until this document is tagged `1.0`.
Implementations built against a draft MUST NOT be assumed interoperable with
any other draft.
**Protocol version byte:** `0x01`

---

## 1. Scope

PhotonProtocol transfers a file from one device to another over an optical,
**simplex** channel: an emitter renders the file as a sequence of high-density
visual codes on its screen, a receiver films that screen with an ordinary
camera, and a decoder reconstructs the file from the recorded video.

There is no return channel. The receiver cannot request a retransmission, cannot
acknowledge anything, and in general does not begin recording at the start of
the transmission. Every design decision in this document follows from that
constraint.

This document specifies the wire format: what is drawn on the screen, and how
bytes map to and from it. It does not mandate a detection algorithm, a
classifier, or a user interface.

### 1.1 Non-goals for version 1

- Real-time decoding from a live camera preview.
- Bidirectional or negotiated sessions.
- Confidentiality. The channel is a screen in the open; anything requiring
  secrecy MUST be encrypted before it is handed to this protocol.

---

## 2. Conventions

The key words MUST, MUST NOT, REQUIRED, SHALL, SHOULD, SHOULD NOT, RECOMMENDED
and MAY are to be interpreted as described in RFC 2119.

- **Integer encoding.** All multi-byte integers in PhotonProtocol structures are
  **little-endian**, except inside the RaptorQ Object Transmission Information
  and the FEC Payload ID, which are big-endian because RFC 6330 defines them
  that way. Every such field is marked explicitly below.
- **Bit packing.** When a sequence of *n*-bit values is packed into bytes, the
  values are concatenated most-significant-bit first into a bit stream, and the
  bit stream is split into bytes most-significant-bit first. Trailing bits of
  the final byte that carry no value are zero.
- **Cell coordinates.** `(r, c)` with `r` the row index from the top and `c` the
  column index from the left, both 0-based, both relative to the **code area**
  (the quiet zone is outside it).
- **Colours** are given as sRGB hexadecimal triplets and are to be written to
  the framebuffer without gamma adjustment.
- `G` denotes the side of the code area in cells. `b` denotes bits per cell.
- Byte ranges are written `[start, end)` — end exclusive — unless stated
  otherwise.

### 2.1 Layer model

| Layer | Name | Unit | Defined in |
| --- | --- | --- | --- |
| L1 | Physical | cell | §4 |
| L2 | Link | frame | §5 |
| L3 | Transport | encoding symbol | §6 |
| L4 | Session | file | §7 |

Each layer depends only on the layer below it. A frame (L2) is self-describing:
it can be decoded without knowledge of any other frame.

---

## 3. Transmission model

The emitter:

1. reads the file, optionally compresses it, and computes its digest (§7);
2. encodes the compressed byte stream as a RaptorQ object (§6);
3. produces an unbounded sequence of encoding symbols;
4. packs those symbols into frames (§5) and renders them in an endless loop
   (§4.7).

The receiver films the screen for as long as it likes, then decodes offline.
Because RaptorQ is a fountain code, the receiver does not need any *particular*
frame; it needs *enough distinct* symbols. Frames lost to blur, glare, motion or
rolling-shutter tearing cost throughput, never correctness.

The emitter MUST keep emitting until stopped by the user. The emitter MUST NOT
assume the receiver started recording at symbol 0.

---

## 4. Physical layer

### 4.1 Frame geometry

A frame is a square **code area** of `G x G` cells surrounded by a **quiet
zone**. `G` is fixed by the profile (§8) and is always a multiple of 32.

Each cell is a square of `S x S` device pixels. `S` MUST be a multiple of 4
(shape masks are 4x4, §4.3.1) and SHOULD be at least 8, so that each shape
sub-cell covers at least 2x2 pixels.

The quiet zone is 4 cells wide on all four sides and is rendered pure white
(`#FFFFFF`). Nothing MAY be drawn in it.

The code area background — every pixel not covered by ink — is pure black
(`#000000`).

### 4.2 Reserved regions

The following regions are reserved. All remaining cells form the **data
region** (§4.4).

#### 4.2.1 Finder patterns

One at each corner, occupying an `8 x 8` **finder box**. The box contains a
`7 x 7` pattern anchored at the outer corner plus a one-cell white **separator**
along the two inner edges.

The `7 x 7` pattern, with `#` = `#000000` and `.` = `#FFFFFF`:

```
#######
#.....#
#.###.#
#.###.#
#.###.#
#.....#
#######
```

This is the classic 1:1:3:1:1 scanline ratio, chosen because it is invariant
under perspective projection along any line through the centre and because it
remains detectable under heavy blur.

Box positions:

| corner | box rows | box cols | pattern anchor |
| --- | --- | --- | --- |
| top-left | `[0, 8)` | `[0, 8)` | rows `[0,7)`, cols `[0,7)` |
| top-right | `[0, 8)` | `[G-8, G)` | rows `[0,7)`, cols `[G-7, G)` |
| bottom-left | `[G-8, G)` | `[0, 8)` | rows `[G-7, G)`, cols `[0,7)` |
| bottom-right | `[G-8, G)` | `[G-8, G)` | rows `[G-7, G)`, cols `[G-7, G)` |

The separator cells are those in the box not covered by the pattern.

The four pattern centres give the decoder four point correspondences, which is
exactly what a homography needs.

#### 4.2.2 Orientation tag

The four finders are identical, so they leave a four-fold rotational ambiguity.
It is resolved by a single `3 x 3` block of **solid white** cells at rows
`[G-12, G-9)`, cols `[G-12, G-9)` — diagonally inward from the bottom-right
finder box, separated from it by one cell of data region.

No other region of the frame can be solid white across `3 x 3` cells: data,
calibration and timing cells are at most 50% ink by construction (§4.3.1). A
decoder therefore resolves orientation by sampling that position under each of
the four candidate rotations and selecting the brightest.

#### 4.2.3 Timing ring

Ring 0 — the outermost ring of the code area, excluding the finder boxes:

- row `0` and row `G-1`, cols `[8, G-8)`
- col `0` and col `G-1`, rows `[8, G-8)`

Each timing cell is **solid**: `#000000` if its varying coordinate (`c` for the
horizontal edges, `r` for the vertical edges) is even, `#FFFFFF` if odd. Since
the first cell of every edge is at index 8, every edge starts dark.

The timing ring lets a decoder recover `G` by counting transitions, and lets it
refine the cell grid beyond what four corner points alone provide.

#### 4.2.4 Calibration ring

Ring 1 — immediately inside the timing ring, again excluding the finder boxes:

- row `1` and row `G-2`, cols `[8, G-8)`
- col `1` and col `G-2`, rows `[8, G-8)`

Calibration cells are rendered **exactly like data cells** (§4.3). Enumerate
them clockwise starting at `(1, 8)`:

1. row `1`, cols `8 .. G-9` (left to right)
2. col `G-2`, rows `8 .. G-9` (top to bottom)
3. row `G-2`, cols `G-9 .. 8` (right to left)
4. col `1`, rows `G-9 .. 8` (bottom to top)

The cell at enumeration index `k` carries the cell value `k mod 2^b`.

This is the single most important robustness feature of the format. A camera's
white balance, exposure and colour matrix drift continuously and are unknown to
the emitter, so a decoder MUST NOT classify cells against the nominal palette of
§4.3.2. It classifies against references measured **in the same frame**: the
calibration ring provides `4(G-16)` labelled samples covering every symbol of
the alphabet, distributed around the whole perimeter so that a spatially varying
illumination can be modelled.

#### 4.2.5 Alignment marker

A `7 x 7` module at the centre of the code area, rows and cols `[G/2-3, G/2+4)`:
a one-cell white separator around the `5 x 5` pattern

```
#####
#...#
#.#.#
#...#
#####
```

with `#` = `#000000`, `.` = `#FFFFFF`.

Four corner points define a homography exactly, with no residual to detect an
error. The centre marker gives a fifth point: a decoder can measure the
reprojection error there, reject a bad detection early, and correct the mild
non-planarity of a real screen filmed at an angle.

#### 4.2.6 Header bands

The frame header (§5.1) is carried **twice**, in two bands of `Hr` rows each:

- top band: rows `[2, 2+Hr)`, cols `[8, G-8)`
- bottom band: rows `[G-2-Hr, G-2)`, cols `[8, G-8)`

with

```
W  = G - 16                     usable cells per band row
Hr = ceil(320 / W)              band height in rows, >= 20 data bytes
nh = floor(Hr * W / 8)          header codeword length in bytes
```

Header cells are **solid**: `#000000` for bit 0, `#FFFFFF` for bit 1. They do not
use the shape/colour alphabet at all.

Two consequences follow, and both are deliberate. First, the header is readable
without knowing the profile — which matters, because the header is what
*declares* the profile. Second, the header survives conditions that destroy the
payload, since one bit per cell with high-contrast solid fill is far more robust
than `b` bits per cell of shape and colour.

The header is duplicated at opposite edges because it is the frame's single
point of failure: a glare band, a finger, or a fold in the recording that wipes
out the top of the frame would otherwise cost the whole frame. A decoder MUST
attempt both copies and MAY accept either.

Any cell of a band not consumed by the `nh`-byte codeword MUST be set to bit 0
and MUST be ignored on receipt.

#### 4.2.7 Reserved cell budget

```
reserved(G, Hr) = 314 + (8 + 2*Hr) * (G - 16)
data_cells(G, Hr) = G*G - reserved(G, Hr)
```

where `314 = 4*64` (finder boxes) `+ 9` (orientation tag) `+ 49` (alignment
module), and `(8 + 2*Hr)` counts the two rings plus the two header bands, each
`G-16` cells per row or edge.

### 4.3 Symbol alphabet

A data cell carries a value in `[0, 2^b)` encoded as a **shape** painted in an
**ink colour** over the black cell background:

```
cell_value = (colour_index << log2(num_shapes)) | shape_index
b          = log2(num_shapes) + log2(num_colours)
```

Both alphabet sizes are powers of two in every profile defined here.

#### 4.3.1 Shapes

A shape is a `4 x 4` binary mask. Sub-cell `(y, x)` of the mask, `y` the row and
`x` the column, corresponds to bit `4y + x` of the 16-bit mask value, and is
painted with the ink colour when the bit is set. Each mask has exactly 8 of 16
sub-cells set, so every shape carries the same amount of colour energy and the
colour classifier is not perturbed by the shape classifier.

**4-shape alphabet** (profiles with `num_shapes = 4`):

| index | mask | pattern |
| --- | --- | --- |
| 0 | `0x00FF` | `####` / `####` / `....` / `....` |
| 1 | `0x3333` | `##..` / `##..` / `##..` / `##..` |
| 2 | `0xCCCC` | `..##` / `..##` / `..##` / `..##` |
| 3 | `0xFF00` | `....` / `....` / `####` / `####` |

**8-shape alphabet** (profiles with `num_shapes = 8`):

| index | mask | pattern |
| --- | --- | --- |
| 0 | `0x00FF` | `####` / `####` / `....` / `....` |
| 1 | `0x1DF0` | `....` / `####` / `#.##` / `#...` |
| 2 | `0x3333` | `##..` / `##..` / `##..` / `##..` |
| 3 | `0x662E` | `.###` / `.#..` / `.##.` / `.##.` |
| 4 | `0x8CCE` | `.###` / `..##` / `..##` / `...#` |
| 5 | `0xC837` | `###.` / `##..` / `...#` / `..##` |
| 6 | `0xF710` | `....` / `#...` / `###.` / `####` |
| 7 | `0xFC88` | `...#` / `...#` / `..##` / `####` |

These alphabets are not hand-drawn. They are the output of a search that scores
candidates on the feature vector a decoder actually measures — a 4x4 average
taken after Gaussian blur, under adversarial sub-cell registration offsets — and
maximises the worst case over the alphabet. The search is committed at
`tools/symbol-design/shape-search.js`; see its README for the objective
function and the reference results. Implementations MUST use the tables above
rather than re-deriving them, because the search is a design tool and its output
is only guaranteed stable for a fixed set of parameters.

#### 4.3.2 Colours

**4-colour palette:**

| index | name | sRGB |
| --- | --- | --- |
| 0 | red | `#FF2A2A` |
| 1 | green | `#2AFF2A` |
| 2 | blue | `#2A2AFF` |
| 3 | white | `#FFFFFF` |

**8-colour palette:**

| index | name | sRGB |
| --- | --- | --- |
| 0 | red | `#FF2A2A` |
| 1 | yellow | `#FFFF2A` |
| 2 | green | `#2AFF2A` |
| 3 | cyan | `#2AFFFF` |
| 4 | blue | `#2A2AFF` |
| 5 | magenta | `#FF2AFF` |
| 6 | white | `#FFFFFF` |
| 7 | grey | `#8C8C8C` |

No channel is driven fully to zero. A hard zero invites chroma subsampling and
demosaicing to overshoot at edges, and costs more in ringing than the extra
saturation buys in separation.

White is a member of the palette rather than a reserved colour: in a
chromaticity plane it sits at the centroid, maximally far from every saturated
hue, which makes it the cheapest reliable symbol available.

Grey is the weakest entry of the 8-colour palette — it differs from white only
in luminance, which is precisely the axis the camera's auto-exposure attacks.
This is recorded as open question **Q2** (§12).

### 4.4 Data region scan order

Data cells are enumerated in raster order — `r` ascending, and within a row `c`
ascending — skipping every reserved cell of §4.2. The cell value of the `i`-th
data cell in this order carries bits `[i*b, (i+1)*b)` of the frame's cell bit
stream (§2, bit packing).

The number of whole bytes recoverable from a frame is

```
raw_bytes = floor(data_cells * b / 8)
```

Bits beyond `raw_bytes * 8` MUST be set to zero by the emitter and MUST be
ignored by the decoder.

### 4.5 Rendering requirements

An emitter MUST:

- render at exactly one code pixel per device pixel, with no scaling, no
  filtering and no antialiasing;
- write the palette values of §4.3.2 into the framebuffer unmodified;
- draw the quiet zone.

An emitter SHOULD:

- run the display at maximum brightness with adaptive brightness, night-shade
  and colour-temperature adjustment disabled;
- choose `S` so the code area fills as much of the physical screen as possible.

### 4.6 Frame identity

Every frame carries `session_id` and `frame_seq` in its header (§5.1). A decoder
MUST ignore frames whose `session_id` differs from the one it has locked onto,
and SHOULD lock onto the `session_id` of the first frame it successfully
decodes.

Because a camera samples the screen asynchronously, the same displayed frame
usually appears in several video frames, and some video frames straddle two
displayed frames. `frame_seq` lets a decoder discard duplicates cheaply and
detect straddling.

### 4.7 Display timing

An emitter MUST hold each frame for at least two display refresh intervals, and
MUST switch between frames instantaneously — no cross-fade, no transition, no
partial update.

The emission rate MUST NOT exceed half the display refresh rate. On a 60 Hz
display this caps emission at 30 frames per second.

An emitter MUST loop indefinitely, and MUST set the `LOOP_RESTART` flag (§5.1)
on the first frame of each pass so a decoder can report coverage.

---

## 5. Link layer

### 5.1 Frame header

20 bytes, little-endian.

| offset | size | field | description |
| --- | --- | --- | --- |
| 0 | 4 | `magic` | ASCII `PHTN` = `50 48 54 4E` |
| 4 | 1 | `protocol_version` | `0x01` for this document |
| 5 | 1 | `profile_id` | §8 |
| 6 | 1 | `flags` | §5.1.1 |
| 7 | 1 | `unit_count` | number of payload units in this frame |
| 8 | 4 | `session_id` | u32, identifies the transmission |
| 12 | 4 | `frame_seq` | u32, index of this frame in the emission sequence |
| 16 | 2 | `payload_len` | u16, bytes of payload actually used (§5.3) |
| 18 | 2 | `header_crc16` | CRC-16/CCITT-FALSE over bytes `[0, 18)` |

`session_id` MUST be drawn from a source with at least 32 bits of entropy per
transmission. It exists to stop a decoder from mixing frames of two different
transfers, not to authenticate anything.

`frame_seq` starts at 0 and increments by one per emitted frame. It does not
wrap in any practical transfer; if it does, it wraps modulo 2^32.

`payload_len` counts the bytes of the payload byte stream (§5.3) that carry
units, including each unit's header and CRC. It MUST NOT exceed the profile's
payload capacity (§8).

**CRC-16/CCITT-FALSE**: polynomial `0x1021`, initial value `0xFFFF`, input not
reflected, output not reflected, final XOR `0x0000`. Check value for the ASCII
string `123456789` is `0x29B1`.

#### 5.1.1 Flags

| bit | name | meaning |
| --- | --- | --- |
| 0 | `HAS_MANIFEST` | the frame contains a `MANIFEST` unit |
| 1 | `LOOP_RESTART` | the frame is the first of an emission pass |
| 2-7 | — | reserved, MUST be 0 on send, MUST be ignored on receipt |

#### 5.1.2 Header protection and placement

The 20 header bytes are protected by a systematic Reed-Solomon code over
GF(2^8) (§5.2.1) with `nh` total bytes and 20 data bytes, `RS(nh, 20)`, where
`nh` is defined in §4.2.6. The codeword corrects up to `(nh - 20) / 2` byte
errors, or twice that many erasures.

The codeword is written bit by bit (§2, bit packing) into the header band's
cells in raster order — `r` ascending, then `c` ascending within a row. Both
bands carry the same codeword.

A decoder MUST reject the frame if neither copy yields a codeword whose
`magic` is `PHTN` and whose `header_crc16` verifies.

### 5.2 Payload protection

#### 5.2.1 Reed-Solomon code

Systematic Reed-Solomon over GF(2^8) with:

- field generator polynomial `x^8 + x^4 + x^3 + x^2 + 1` (`0x11D`),
- generator element `alpha = 0x02`,
- first consecutive root `alpha^0`,
- codeword layout: `k` data bytes followed by `n - k` parity bytes.

This is the conventional "RS over GF(256), fcr = 0" configuration.

#### 5.2.2 Codeword partitioning

Let `raw_bytes` be as in §4.4, `k` the profile's data length and `p = 255 - k`
the parity length. Then

```
full   = floor(raw_bytes / 255)          full RS(255, k) codewords
rem    = raw_bytes - full * 255
```

If `rem >= p + 1`, a final **shortened** codeword `RS(rem, rem - p)` is appended,
carrying `rem - p` data bytes with the same `p` parity bytes. Otherwise the
trailing `rem` bytes of the frame are unused, MUST be set to zero and MUST be
ignored.

The payload capacity of the frame is

```
payload_capacity = full * k + max(0, rem - p)   when rem >= p + 1
                 = full * k                     otherwise
```

#### 5.2.3 Interleaving

Codewords are interleaved across the cell stream so that a spatially local
defect — glare, a fingertip, a scratch, a compression block — is spread thinly
over many codewords instead of destroying one.

Let there be `W` codewords, codeword `w` having length `n_w` bytes. The output
byte stream is produced by

```
for j in 0 .. max(n_w) - 1:
    for w in 0 .. W - 1:
        if j < n_w: emit codeword[w][j]
```

The decoder reverses this exactly. Because the mapping from cell index to
codeword position is fixed and known, a decoder that tracks per-cell
classification confidence MAY declare low-confidence cells as **erasures** and
use erasure decoding, which doubles the correction capacity. Doing so is
RECOMMENDED.

### 5.3 Payload units

After RS decoding and de-interleaving, the payload is a byte stream of
`payload_capacity` bytes, of which the first `payload_len` carry a sequence of
`unit_count` **units**:

| offset | size | field |
| --- | --- | --- |
| 0 | 1 | `type` |
| 1 | 2 | `length` — u16, size of `data` |
| 3 | `length` | `data` |
| 3 + `length` | 4 | `crc32c` — u32, CRC-32C over bytes `[0, 3 + length)` |

Units are packed back to back with no padding. Bytes after `payload_len` MUST be
set to zero and MUST be ignored.

**CRC-32C** (Castagnoli): reflected polynomial `0x82F63B78`, initial value
`0xFFFFFFFF`, input and output reflected, final XOR `0xFFFFFFFF`. Check value for
the ASCII string `123456789` is `0xE3069283`.

Every unit carries its own CRC so that a frame whose RS decoding partially
failed is not thrown away wholesale: a decoder MUST evaluate each unit
independently, accept those whose CRC verifies, and discard the rest. If a
unit's `length` runs past `payload_len`, the decoder MUST stop parsing that
frame and keep the units already accepted.

#### 5.3.1 Unit types

| type | name | data |
| --- | --- | --- |
| `0x00` | `PADDING` | ignored; terminates parsing of the frame |
| `0x01` | `MANIFEST` | a session manifest (§7.1) |
| `0x02` | `RQ_SYMBOL` | a 4-byte FEC Payload ID followed by `T` symbol bytes (§6) |

A decoder MUST ignore units whose `type` it does not recognise, and MUST continue
parsing after them. This is the extension point of the format.

---

## 6. Transport layer

The compressed byte stream produced by §7 is a single RaptorQ **object**, encoded
per RFC 6330.

The parameters of that object — its Object Transmission Information (OTI) — are
carried in the manifest (§7.1) as the 12-byte encoding of RFC 6330 §3.3.2 and
§3.3.3: the 8-byte Common FEC OTI (40-bit transfer length `F`, 1 reserved byte,
16-bit symbol size `T`) followed by the 4-byte Scheme-Specific FEC OTI (8-bit
`Z`, 16-bit `N`, 8-bit `Al`). All fields are big-endian, as RFC 6330 defines
them.

An `RQ_SYMBOL` unit's data is the 4-byte **FEC Payload ID** of RFC 6330 §3.2
(8-bit Source Block Number, 24-bit Encoding Symbol ID, big-endian) followed by
exactly `T` bytes of encoding symbol.

The emitter MUST choose `T` as a multiple of `Al` and SHOULD choose it to
maximise frame fill: with a per-unit overhead of 7 bytes (3 header + 4 CRC) plus
4 bytes of FEC Payload ID, the wasted tail of a frame is
`payload_capacity mod (T + 11)` minus whatever the manifest occupies. `T`
SHOULD be in `[512, 4096]`.

The emitter MUST emit source symbols before repair symbols on its first pass, so
that a receiver who records from the beginning of a pass can often decode
without any repair at all. Subsequent passes emit repair symbols with
monotonically increasing Encoding Symbol IDs; the emitter MUST NOT repeat an
Encoding Symbol ID within a session unless it has exhausted the ESI space.

A decoder MUST feed accepted symbols to a RaptorQ decoder and MUST report
progress as symbols accumulate. RaptorQ decoding of a source block succeeds with
very high probability once `K + 2` symbols are available for a block of `K`
source symbols, which makes "`X` of about `Y` symbols" a meaningful and honest
progress figure.

---

## 7. Session layer

### 7.1 Manifest

The manifest describes the transfer. It is small, and it is required before any
payload byte means anything, so it is sent redundantly: an emitter MUST include
a `MANIFEST` unit in at least one frame in every eight, and SHOULD include one in
every frame when the profile's payload capacity exceeds 4096 bytes.

`61 + name_len` bytes, little-endian except where noted.

| offset | size | field | description |
| --- | --- | --- | --- |
| 0 | 4 | `magic` | ASCII `PHTM` = `50 48 54 4D` |
| 4 | 1 | `manifest_version` | `0x01` for this document |
| 5 | 1 | `compression` | §7.2 |
| 6 | 2 | `flags` | reserved, MUST be 0 |
| 8 | 8 | `original_size` | u64, size in bytes of the file before compression |
| 16 | 32 | `sha256` | SHA-256 of the file before compression |
| 48 | 12 | `raptorq_oti` | RaptorQ OTI, big-endian per RFC 6330 (§6) |
| 60 | 1 | `name_len` | length in bytes of `name`, 0 to 255 |
| 61 | `name_len` | `name` | file name, UTF-8, no path separators |

The manifest has no CRC of its own; it is always carried inside a unit whose
CRC-32C covers it (§5.3).

`name` MUST be a bare file name. It MUST NOT contain `/`, `\`, or a NUL byte,
MUST NOT be `.` or `..`, and MUST be valid UTF-8. A decoder MUST reject a
manifest that violates this, and MUST treat the name as untrusted input when
writing to disk.

The transfer length of the RaptorQ object is `F` inside the OTI, so it is not
repeated here.

### 7.2 Compression

| value | algorithm | support |
| --- | --- | --- |
| `0x00` | none | REQUIRED |
| `0x01` | Brotli (RFC 7932) | REQUIRED |
| `0x02` | Zstandard (RFC 8878) | OPTIONAL |

An emitter SHOULD compress with Brotli, and MUST fall back to `0x00` when the
compressed form is not at least 2% smaller than the original — which is the
normal outcome for input that is already compressed, such as JPEG, MP4, ZIP or
an encrypted blob.

Brotli rather than Zstandard is a decision about where this protocol runs. Both
pages execute as WebAssembly on a phone, and the reference Zstandard library is
C: building it for `wasm32-unknown-unknown` needs a C toolchain targeting Wasm,
which every contributor and every CI job would then have to carry. Brotli has a
mature implementation in pure Rust, so the same code compiles for every target
without one.

It also compresses better here. On a mixed source-and-documentation corpus of
123088 bytes, Brotli at quality 11 produced 36363 bytes against 39618 for
Zstandard at level 19 — 8.2% smaller. Compression took 126 ms against 51 ms, and
decompression was comparable at well under a millisecond. Encoder time is not a
constraint in this protocol: a frame carries a few kilobytes and the display
emits perhaps thirty frames a second, so the compressor finishes long before the
channel does.

Zstandard keeps a registered identifier so that an implementation which can
already link it may use it, but a decoder is not required to understand it.

A decoder MUST reject a manifest declaring an algorithm it does not implement,
and MUST bound the decompressed size by `original_size` so that a corrupt or
hostile manifest cannot drive it into unbounded allocation.

### 7.3 Integrity

After RaptorQ decoding and decompression, a decoder MUST verify that the result
is `original_size` bytes long and that its SHA-256 equals `sha256`. It MUST NOT
present the file to the user if either check fails.

This is the only end-to-end check in the protocol. Everything above it — RS,
CRCs, RaptorQ — protects throughput; only this protects correctness.

---

## 8. Profiles

A profile fixes every physical-layer parameter. The `profile_id` in the frame
header declares which one is in use.

| profile | id | grid `G` | shapes | colours | `b` | header | payload RS | data cells | payload capacity |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `P1-conservative` | `0x01` | 96 | 4 | 4 | 4 | `RS(40,20)` x2 | `RS(255,175)` | 7622 | 2611 B |
| `P2-standard` | `0x02` | 128 | 8 | 4 | 5 | `RS(42,20)` x2 | `RS(255,199)` | 14502 | 7047 B |
| `P3-dense` | `0x03` | 160 | 8 | 8 | 6 | `RS(54,20)` x2 | `RS(255,223)` | 23270 | 15244 B |

Derived quantities, for cross-checking an implementation:

| profile | `W = G-16` | `Hr` | `nh` | reserved cells | `raw_bytes` | RS partition | parity `p/255` | link overhead |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `P1-conservative` | 80 | 4 | 40 | 1594 (17.3%) | 3811 | 14 x `RS(255,175)` + `RS(241,161)` | 31.4% | 31.5% |
| `P2-standard` | 112 | 3 | 42 | 1882 (11.5%) | 9063 | 35 x `RS(255,199)` + `RS(138,82)` | 22.0% | 22.2% |
| `P3-dense` | 144 | 3 | 54 | 2330 (9.1%) | 17452 | 68 x `RS(255,223)` + `RS(112,80)` | 12.5% | 12.7% |

The two rightmost columns measure different things and must not be conflated.
`p/255` is the parity fraction of the code and describes its correction
strength. **Link overhead** is the fraction of the frame's raw bytes that never
reach the transport layer, and it is slightly larger because the shortened
trailing codeword pays full parity over fewer data bytes. Throughput is governed
by the second.

`P2-standard` is the default. An emitter MUST implement `P2-standard`; a decoder
MUST implement all three.

The parity rates run opposite to intuition on purpose: the conservative profile
spends *more* of its smaller frame on parity. A profile is a single point on a
robustness curve, not a density knob, and `P1-conservative` exists to survive
conditions — a shaky hand, a dim screen, a cheap camera — where `P3-dense` would
not decode at all.

These parity rates are provisional; see open question **Q3** (§12).

A decoder encountering an unknown `profile_id` MUST reject the frame with
`E_PROFILE_UNSUPPORTED` and MUST NOT guess.

---

## 9. Decoder requirements

### 9.1 Pipeline

This section is informative except where it uses MUST. A conforming decoder may
work any way it likes provided it accepts every frame a conforming emitter
produces.

1. Locate four finder patterns in the video frame.
2. Compute the homography from their centres to the nominal grid.
3. Verify it against the alignment marker; reject the detection if the
   reprojection error is large.
4. Resolve orientation from the orientation tag (§4.2.2).
5. Recover `G` from the timing ring and check it against the profile once the
   header is read.
6. Read both header bands, RS-decode, verify `magic` and `header_crc16`.
7. Sample the calibration ring and fit a per-frame classifier.
8. Classify data cells, optionally with per-cell confidence.
9. De-interleave, RS-decode the payload, with erasures where confidence is low.
10. Parse units, verify each CRC-32C, keep those that pass.
11. Feed `RQ_SYMBOL` units to the RaptorQ decoder; take the manifest from the
    first `MANIFEST` unit that verifies.
12. On success, decompress and verify SHA-256 (§7.3).

### 9.2 Failure reporting

A decoder MUST NOT report a bare failure. When it cannot produce the file it
MUST report which stage failed and how close it came. At minimum it MUST
distinguish:

| condition | meaning |
| --- | --- |
| `E_NO_FINDERS` | no frame located in any video frame |
| `E_NO_ORIENTATION` | finders found, orientation tag not resolvable |
| `E_HEADER_UNRECOVERABLE` | both header copies failed RS or CRC |
| `E_VERSION_UNSUPPORTED` | `protocol_version` not implemented |
| `E_PROFILE_UNSUPPORTED` | `profile_id` not implemented |
| `E_FRAME_FEC_FAILED` | payload RS decoding failed for the whole frame |
| `E_NO_MANIFEST` | symbols collected, no manifest ever verified |
| `E_INSUFFICIENT_SYMBOLS` | manifest known, not enough symbols to decode |
| `E_TRANSPORT_DECODE_FAILED` | RaptorQ decoding failed despite enough symbols |
| `E_DECOMPRESSION_FAILED` | decompression failed or exceeded `original_size` |
| `E_HASH_MISMATCH` | reconstructed file's SHA-256 does not match the manifest |

`E_INSUFFICIENT_SYMBOLS` MUST be accompanied by the number of distinct symbols
accepted and the number needed. A decoder SHOULD also report, per video frame,
the estimated cell error rate and the reason any frame was dropped, since that
is what tells a user whether to re-film closer, steadier, or brighter.

### 9.3 Robustness requirements

A decoder MUST NOT trust any length or size field before bounding it against the
capacity implied by the profile. A decoder MUST treat the video file, every
frame, the manifest and the file name as hostile input.

---

## 10. Test vectors

All values below are byte sequences in hexadecimal, most significant byte first
as written.

### 10.1 CRC check values

| algorithm | input | result |
| --- | --- | --- |
| CRC-16/CCITT-FALSE | ASCII `123456789` | `0x29B1` |
| CRC-32C | ASCII `123456789` | `0xE3069283` |

### 10.2 Frame header

Fields: `protocol_version` = 1, `profile_id` = `0x02` (`P2-standard`), `flags` =
`0x01` (`HAS_MANIFEST`), `unit_count` = 7, `session_id` = `0x0BADC0DE`,
`frame_seq` = 1, `payload_len` = 7047.

Bytes `[0, 18)`:

```
50 48 54 4E 01 02 01 07 DE C0 AD 0B 01 00 00 00 87 1B
```

`header_crc16` = `0xB817`. Complete 20-byte header:

```
50 48 54 4E 01 02 01 07 DE C0 AD 0B 01 00 00 00 87 1B 17 B8
```

### 10.3 Manifest

Fields: `manifest_version` = 1, `compression` = `0x01` (Brotli), `flags` = 0,
`original_size` = 1048576, `sha256` = SHA-256 of the ASCII string `abc`, OTI with
`F` = 524288, `T` = 1152, `Z` = 1, `N` = 1, `Al` = 8, `name` = `report.pdf`.

71 bytes:

```
50 48 54 4D 01 01 00 00 00 00 10 00 00 00 00 00
BA 78 16 BF 8F 01 CF EA 41 41 40 DE 5D AE 22 23
B0 03 61 A3 96 17 7A 9C B4 10 FF 61 F2 00 15 AD
00 00 08 00 00 00 04 80 01 00 01 08 0A 72 65 70
6F 72 74 2E 70 64 66
```

SHA-256 of the manifest:
`8349301C214A9CF5B0BDF5A1F1A25CA102000C6DD64FD0AD4299E561CE723D68`

### 10.4 Manifest wrapped in a payload unit

`type` = `0x01`, `length` = 71, data as §10.3. `crc32c` = `0x6700E7C8`. The unit
is 78 bytes; first 8 and last 8:

```
first: 01 47 00 50 48 54 4D 01
last:  2E 70 64 66 C8 E7 00 67
```

SHA-256 of the complete unit:
`5E2E9845F4CBFA4DA69021CFD8B5CDF4E56647A907C75A165469E1FA93CC9799`

### 10.5 Derived geometry

An implementation MUST reproduce the numbers in the tables of §8 from the
formulas of §4.2.7, §4.4 and §5.2.2. `tools/geometry/frame-geometry.js`
recomputes them independently of the Rust implementation.

---

## 11. Versioning and extensibility

- `protocol_version` is the first field after the magic and is never removed. A
  decoder MUST check it before interpreting any other field.
- New unit types (§5.3.1) extend the format without a version bump. Decoders skip
  what they do not know.
- New profiles (§8) extend the physical layer without a version bump, because
  every physical parameter is a function of `profile_id`.
- Reserved bits and reserved fields MUST be zero on send and ignored on receipt.
- A change to any existing field, offset, table or scan order requires a
  `protocol_version` bump.

No change to the wire format may be merged without updating this document in the
same commit.

---

## 12. Open questions

These are deliberately unresolved in 0.1. Each is a decision that should be made
with measurements from Phase 1 (synthetic channel) or Phase 2 (real camera
capture) rather than from argument.

**Q1 — Grid sizes.** `G` is 96 / 128 / 160. The right value depends on how many
camera pixels land on a cell, which depends on how much of the video frame the
screen occupies. If a 4K recording of a phone screen gives a code area of about
1800 px, `P2-standard` gets 14 px per cell — comfortable, possibly wasteful.
Phase 2 should establish the minimum viable pixels per cell and the profiles
should be re-cut around it.

**Q2 — The eighth colour.** The 8-colour palette pairs white with grey, and they
differ only in luminance, which the camera's auto-exposure is actively varying.
Alternatives: replace grey with an eighth hue at 45-degree spacing and rely
purely on chromaticity; or drop to 7 colours and accept a non-power-of-two
alphabet; or drop `P3-dense` to 4 colours and buy density from a larger grid
instead.

**Q3 — Parity rates.** 31.4% / 22.0% / 12.5% are guesses. Phase 1 must produce a
cell-error-rate curve against channel severity, and the parity rates should be
set so each profile decodes at a chosen point on that curve with margin.

**Q4 — Alignment markers.** One centre marker corrects the mild non-planarity of
a flat screen. If Phase 2 shows meaningful residual distortion — lens barrel
distortion at short range is the likely culprit — the format needs a lattice of
markers, which is a wire-format change and therefore must be decided before 1.0.

**Q5 — Erasure signalling.** Erasure decoding roughly doubles RS correction
capacity, and §5.2.3 permits it, but the format gives the decoder no help in
deciding which cells are unreliable. A per-frame quality region, or a
deliberately known-value cell scattered through the data region, would let a
decoder calibrate its confidence threshold. Both cost cells.

*Phase 1 measured this and the result is negative.* Across the severity band
that decides whether a transfer succeeds, a nearest-versus-second-nearest
confidence margin flags only 8% to 25% of the cells that are actually wrong; it
becomes informative only at severities where the frame is already unrecoverable.
As it stands the erasure path is complexity that buys close to nothing. See
`docs/phase-1-report.md` finding F1. Either the confidence estimate needs a
better basis, or the format must carry known-value cells for a decoder to
calibrate against — which is a wire-format change and therefore has to be
settled before 1.0.

**Q6 — Frame rate versus rolling shutter.** §4.7 caps emission at half the
refresh rate, which is a safe assumption, not a measured one. Rolling shutter
means a video frame can contain the top of one code and the bottom of the next.
The header duplication of §4.2.6 makes such a frame detectable — the two copies
will disagree — but the format does not yet let a decoder *use* the good half.

**Q7 — Symbol ordering across passes.** §6 requires source symbols first and
distinct ESIs afterwards, but does not specify the repair schedule. If a receiver
films a random 30-second window, the ideal schedule spreads coverage evenly over
source blocks. This is an emitter policy question that may deserve to become a
requirement.

---

## 13. References

- RFC 2119 — Key words for use in RFCs to Indicate Requirement Levels
- RFC 6330 — RaptorQ Forward Error Correction Scheme for Object Delivery
- RFC 7932 — Brotli Compressed Data Format
- RFC 8878 — Zstandard Compression and the `application/zstd` Media Type
- FIPS 180-4 — Secure Hash Standard (SHA-256)
- ISO/IEC 18004 — QR Code bar code symbology (finder and alignment pattern
  geometry, used here as prior art rather than as a normative dependency)
