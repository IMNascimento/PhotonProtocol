# Phase 2 report — why no real transfer worked, and what reads now

**Date:** 2026-09-28
**Specification:** 0.2 (draft)
**Reproduce with:** `node tools/e2e/matrix.mjs <file>`

Every attempt to move a file from a monitor to a phone had failed. The phone
found the code, read its header, and then could not read its cells — every
picture, every time. Phase 1's channel model said the same frames should decode
comfortably.

This report is what was found when the channel model was made harsh enough to
disagree with Phase 1 and agree with the phones.

**Read this first.** Everything in this report but its last section was
measured with `photon film`, a model of a camera, and with the real pages
running in a real browser fed by that model. The model reproduced the reported
failure exactly, stage for stage, which is evidence that it models the right
things. It is not evidence that a particular phone will behave like it, and the
last section is what a particular phone did.

## The harness

Phase 1's channel distorted one painted frame at a time. That measures a
classifier and says nothing about a transfer, and nearly everything that stops
a real transfer lives in what it left out.

`photon film` renders the pictures a browser's `getUserMedia` would hand a
page, from the frames the sending page paints, with the display and the camera
each on a clock of its own:

| Modelled | Why it matters |
| --- | --- |
| rolling shutter, sideways for a phone held upright | a picture taken while the screen changes is part of one code and part of the next |
| display scan-out and pixel response | the screen changes top to bottom, and each pixel takes milliseconds to settle |
| pose, perspective and hand tremor | the pose moves between pictures, and during one |
| radial lens distortion | four corners fix a homography, and a homography cannot bend |
| lens blur and stray light | neighbours spill into each other; black is never black |
| colour filter array and demosaicing | every sensor pixel sees one colour and interpolates two |
| exposure, clipping, white balance, colour correction | the camera exposes for a face, not for a code |
| sharpening | halos at every edge |
| 4:2:0 | the browser is handed colour at half resolution |
| backlight flicker | bands across a short exposure |

Four presets — `ideal`, `good`, `typical`, `poor` — move these together, and
each can be overridden alone.

The output is a directory of PNGs for `photon decode`, and a `.y4m` file that
Chrome plays as a camera. `tools/e2e/receive.mjs` opens the real receiving page
with that as its camera, waits for the page to offer a file, and compares the
file's digest with the original's. Everything a unit test cannot reach is on
that path: the camera API, the video element, the canvas read-back, the
workers, the WebAssembly boundary, the page's own bookkeeping.

`photon decode --truth <file>` rebuilds what the sender painted and counts the
cells the decoder read wrongly, by painted colour and by region of the frame.

## What was found

### F1 — Shape was decided by luma, and blue has none to spare

The reported failure, reproduced: 755 pictures through the receiving page, the
code located in every one, the header read in every one, and no payload
decoded.

Compared against what was sent:

| | wrong cells | of which shape | of which colour |
| --- | --- | --- | --- |
| all cells | 9.30% | 9.29% | 0.04% |

| painted colour | cells read wrongly |
| --- | --- |
| red | 10.11% |
| green | 0.00% |
| blue | 27.07% |
| white | 0.00% |

The colour of a cell was read almost perfectly and its shape was not, and
which shapes failed depended entirely on the ink.

The classifier decided shape first, by correlating the cell's **luma** against
each mask, and colour afterwards. The palette's blue has 0.26 of the luma of
its white, and its red 0.41. Through any lens that blurs, a cell's bright
neighbours spill into its unpainted half; for a blue cell beside a white one,
what spills in is as bright in luma as the cell's own ink, and the brighter
half of the cell is then the wrong one.

A 4-bit cell straddles no byte boundary, so 9.3% of cells wrong is 17.7% of
bytes wrong, against a correction radius of 15.7%. Every frame was a little
over. The same captures written out by the simulator rather than by the
browser — a slightly different colour conversion — came to 6.6% of cells and
12.7% of bytes, a little under, and 38% of them decoded. The decoder was
sitting on the cliff that Phase 1 described (its F2), and which side of it a
frame fell depended on the rounding of a colour matrix.

Phase 1 saw these errors and did not recognise them. Its table has
`P1-conservative` reading 6.9% of its cells wrongly at severity 0.5 and marks
that as inside a budget of 15.69%. The budget is in bytes and the measurement
in cells. A byte of that profile is two cells, so 6.9% of cells is 13.3% of
bytes: inside, but at the edge, in a frame of fifteen codewords that all have
to decode. It also never asked which cells were wrong, which is the question
that gives the fault away.

**Fix.** A cell is matched whole — sixteen sub-cells, three channels — against
a template of each symbol, and the nearest wins. The templates are measured
from the calibration ring of the same frame, so they already hold whatever
this lens and this camera did to a symbol. Each match is allowed a gain of its
own, so that uneven lighting costs nothing, bounded so that white and grey stay
distinct.

Wrong cells, same captures: 9.30% became 0.06%.

### F2 — The ring describes the ring

The calibration ring is a few hundred cells at the edge of the frame, with the
timing ring on one side and a header band on the other. The payload is
thousands of cells surrounded by other payload. Through a lens that blurs, a
cell's appearance depends on its neighbours, so templates from the ring
describe cells in a neighbourhood the payload does not have.

Once the payload has been read once, most of it is known. Averaging the cells
read as each value gives templates measured where they are used, from twenty
times the evidence. How much the cells of one value vary among themselves,
number by number, says which parts of a cell to trust: the rim, where the
neighbours spill in, varies more than the middle and is weighted less.

`poor` preset, median wrong cells per frame: 8.83% became 4.45%, and a preset
that decoded nothing decoded 38 of the 40 codes shown.

### F3 — A photographed finder is not 1:1:3:1:1

At 5 pixels a cell the code was not located at all. Light spreads into dark in
a photograph — more with every stop of overexposure, and a bright screen in an
ordinary room is always somewhat overexposed — so each dark ring of a finder
comes out narrower than it was painted and each light ring wider. The rings
measured nearer 0.6:1.4:2.6:1.4:0.6.

The middle of a ring does not move: its two edges shift in opposite directions
by the same amount. Candidates are now judged by where the middles of their
rings fall.

### F4 — A lens is not a pinhole

Four finder centres fix a homography, and a lens bends straight lines by about
a percent. The timing ring already carries what is needed to measure that: each
of its cells can be located to a fraction of a pixel, which says how far the
true frame departs from the homography all the way round its perimeter. The
correction inside the frame is blended from the four edges, and the centre
alignment marker supplies what the edges cannot.

In the presets this mattered less than expected, because a code that fills the
width of an upright picture occupies the middle of the lens, where there is
little distortion. With the distortion raised to 4% and the code filling the
picture, the grid is followed to within a tenth of a cell.

### F5 — A refresh is not a sixtieth of a second

The sending page held each code for four display refreshes. On a 60 Hz display
that is 67 ms. On a 144 Hz display it is 28 ms, and a picture from a phone held
upright spans 40 to 60 ms of the display's time.

Distinct codes read, by a decoder that looks at every picture from a camera at
30 frames a second, `typical` preset:

| codes shown a second | codes read a second |
| --- | --- |
| 10 | 10.3 |
| 15 | 15.3 |
| 20 | 18.7 |
| 30 | 10.5 |

The sending page now measures the display and works in codes a second, ten by
default.

### F6 — Throughput is pictures read a second

A phone that reads three pictures a second reads three codes a second at best,
whatever the sender shows. Reading a picture took 50 ms on one core of a
desktop, which is about 300 ms on a phone.

| stage | before | after |
| --- | --- | --- |
| finding the frame | 29.0 ms | 6.3 ms |
| fitting the grid | — | 0.9 ms |
| reading the cells | 20.0 ms | 8.0 ms |
| **a picture** | **50.0 ms** | **15.2 ms** |

Finding works on eight-bit brightness at half size. Sampling is in fixed point.
A code already read is dropped at its header, for about 7 ms.

The receiving page asked the camera for 4K, which on a recent phone is four
times the pixels for cells that were already large enough; it now asks for
1080p, and reads only the part of the picture the code is in. It runs as many
readers as the device has cores to spare.

The receiving page in Chrome, 151 KB file, ten codes shown a second:

| | pictures read a second | throughput |
| --- | --- | --- |
| before, desktop speed | 9 | never finished |
| after, desktop speed | 29 | 24.5 KB/s |
| after, slowed 4 times | 23 | 20.9 KB/s |
| after, slowed 6 times | 16 | 20.3 KB/s |

A 2.7 MB photograph, fifteen codes shown a second, browser slowed four times:
107 seconds, 24.6 KB/s, digest verified. The simulated footage was 85 seconds
long and began to repeat before the end, which a real sender never does;
before it repeated, the rate was 29.5 KB/s.

### F7 — The ladder was cut for 4K, and browsers are given 1080p

| profile | cells | camera pixels per cell to read | which needs |
| --- | --- | --- | --- |
| `P1-conservative` | 96, 4 shapes | 6 | 720p from close up |
| `P2-standard` | 128, 8 shapes | 8 | 4K |
| `P3-dense` | 160, 8 shapes | 8 | 4K |

The 8-shape alphabet's finest feature is a quarter of a cell and the 4-shape
alphabet's is half of one. At 6.4 pixels a cell through the `typical` preset,
`P2-standard` reads 8.2% of its cells wrongly where its parity repairs 7.0%; at 8.3
it reads 0.8%.

So at 1080p `P1-conservative` was the only profile that read, and the sending
page — which chose the densest profile the *screen* could paint — chose one the
camera could not.

`P4-balanced` is the alphabet and parity of `P1-conservative` on the grid of
`P2-standard`: 4931 bytes a frame against 2611, reading at 6.5 pixels a cell.

## The table

`node tools/e2e/matrix.mjs`, three seconds of each, a 60 KB file.
`P1-conservative` unless it says otherwise.

| conditions | camera px per cell | codes read a second, of shown | KB/s | median wrong cells |
| --- | --- | --- | --- | --- |
| ideal | 9.55 | 10.3 of 10 | 26.3 | 0.00% |
| good | 9.12 | 10.3 of 10 | 26.3 | 0.00% |
| typical | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| poor | 6.77 | 10.0 of 10 | 25.5 | 4.70% |
| typical, 15 codes/s | 8.27 | 15.3 of 15 | 39.1 | 0.17% |
| typical, 20 codes/s | 8.27 | 18.7 of 20 | 47.6 | 2.01% |
| typical, 144 Hz screen | 8.27 | 10.7 of 10 | 27.2 | 0.00% |
| typical, further away | 6.20 | 10.3 of 10 | 26.3 | 0.42% |
| typical, shutter rolling down | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| typical, landscape | 8.79 | 10.3 of 10 | 26.3 | 0.00% |
| typical, 720p | 6.20 | 10.3 of 10 | 26.3 | 0.51% |
| typical, 4K | 16.54 | 10.3 of 10 | 26.3 | 0.00% |
| typical, 1440p screen | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| typical, shaky | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| typical, overexposed | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| typical, lens bent 4% | 9.73 | 10.3 of 10 | 26.3 | 0.00% |
| `P4-balanced`, good | 6.89 | 10.0 of 10 | 48.2 | 0.00% |
| `P4-balanced`, typical | 6.26 | 10.0 of 10 | 48.2 | 0.31% |
| `P4-balanced`, typical, 15 codes/s | 7.20 | 14.3 of 15 | 69.0 | 0.61% |
| `P4-balanced`, poor, close up | 6.98 | 9.7 of 10 | 46.5 | 3.29% |
| `P2-standard`, typical, 4K | 12.59 | 10.3 of 10 | 71.1 | 0.00% |
| `P3-dense`, typical, 4K | 11.51 | 10.3 of 10 | 153.8 | 0.00% |

The throughput column is what a decoder reading every picture would have
carried. A phone that reads fewer carries less; see F6.

Where it stops: `P1-conservative` at 5.2 pixels a cell reads 7.8% of its cells
wrongly and decodes two codes in sixty pictures, and at 4.4 the code is rarely
found. `P4-balanced` does not read at 720p.

## What a real phone did

After everything above, an iPhone in Safari was pointed by hand at a 1080p
monitor refreshing at 164 Hz.

`P1-conservative` transferred an 818 KB picture in 34 seconds at ten codes a
second, and in 21 seconds at twenty: 23.7 and 38.2 KB/s. The phone read 29
pictures a second, 92% of those in which it found the code.

`P4-balanced`, `P2-standard` and `P3-dense` did not decode a single picture,
at 7.8, 7.5 and 6 camera pixels a cell. The table above says the first should
have, comfortably.

The pictures the phone could not read were kept, and the file that was sent was
to hand, so they could be compared with what was painted. The cells are wrong
in the middle of the code and right at its edges. Measured block by block, the
interior sits up to 0.3 of a cell from where the homography puts it, in a field
that is zero round the perimeter and at the centre and largest between them,
pointing inwards.

F4 does not follow that field, and makes it worse. It measures the perimeter,
where the departure is small, and blends inwards. In a picture of
`P1-conservative` that failed, 21% of cells were wrong with the fitted grid and
8% with the homography alone; in the pictures of `P4-balanced`, 66% and 58%.

The model did not show this because it bends a picture about the middle of the
lens, which is where the code is, and so bends the code hardly at all. F4 was
tested against a distortion it handles, and the row of the table that says so
is a test of the model.

What this needs is a measurement inside the code, not only round it. That is
SPEC.md Q4, and the answer recorded there after the simulation — that no
lattice of alignment marks was needed — was wrong.

## What is not modelled

- **How a real lens bends the picture.** See above. The most important entry
  in this list, and the one that was missing from it.

- **Moiré.** The beat between the screen's pixels and the sensor's. Worked
  through, it is under one percent of modulation whenever the camera's pixels
  are as large as the screen's, which they are with the code filling a 1080p
  picture. It would matter to a camera much closer, or of much higher
  resolution, than was tested.
- **Autofocus hunting, and auto-exposure settling.** The model is in focus and
  exposed from the first picture.
- **Whatever the phone's own processing does beyond sharpening and a tone
  curve.** Noise reduction, local tone mapping and HDR merging all vary by
  manufacturer and none are documented.
- **A frame rate that drops.** A camera in a dim room lengthens its exposure
  and halves its frame rate. A bright screen filling the picture should keep
  the exposure short, but that is an expectation and not a measurement.
- **Reflections.**

## What this changes in the specification

Draft 0.2. The wire format of the existing profiles is unchanged.

- §4.1, §8: `S_min` is 4 and 6 pixels, not 6 and 8. The old figures measured
  the old classifier.
- §4.7: a frame is held for a time, about 100 ms, not for a number of refreshes.
- §8: `P4-balanced` is added. The default is `P1-conservative`.
- §9.1: the decoder guidance describes what F1, F3 and F4 found necessary.
- §12: Q1, Q4 and Q6 have evidence, all of it simulated.

Phase 1's F1 — that the classifier's confidence does not track its errors —
was measured on the classifier this report replaces, and has not been measured
again.

## What would raise throughput from here

In order of what each is worth:

1. **A code that is not square.** A monitor is 16:9 and the code uses a square
   of it; a phone turned on its side gives a 16:9 picture and the code fills
   56% of that. A code the shape of the screen carries 1.8 times as much at the
   same cell size.
2. **Codewords kept within bands of the frame**, so that a picture which spans
   a change of code still yields the bands that were whole. Most of the
   pictures lost at 20 codes a second are lost this way.
3. **Reading the camera's own planes** instead of a canvas. The colour a canvas
   hands over has been through 4:2:0 and back, and the copy out of the video
   is a third of the time a slow phone spends on a picture.
