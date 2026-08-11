# Phase 1 report — the physical layer under a synthetic channel

**Date:** 2026-08-11
**Specification:** 0.1 (draft)
**Reproduce with:** `cargo run --release -p photon-cli -- simulate`

Phase 1 measures the physical layer against a modelled camera, with ground truth
on both sides, so that a result can be attributed to the classifier rather than
to error correction hiding the damage. It answers "at what point does it stop
working" rather than "did it work".

Everything here comes from `core/src/simulate.rs`. The severity ladder is a
model, not a measurement of any real camera; Phase 2 replaces its parameters
with values fitted to actual footage, and every number below should be re-read
then.

## What the ladder varies

Severity runs from 0 (the frame comes back exactly as painted) to 1 (barely a
recording). All six axes move together, because real conditions do not degrade
one at a time.

| Axis | At severity 0.5 | At severity 1.0 |
| --- | --- | --- |
| perspective (corner displacement) | 5% | 10% |
| resampling (output pixels per input pixel) | 0.725 | 0.45 |
| blur (Gaussian sigma, output pixels) | 1.1 px | 2.2 px |
| white balance gain (R, G, B) | 1.15, 0.97, 0.86 | 1.30, 0.94, 0.72 |
| black lift (glare) | 0.05 | 0.10 |
| sensor noise (standard deviation) | 0.028 | 0.055 |
| block quantisation | mild | heavy |

Chroma subsampling at 2×2 is applied unconditionally, because 4:2:0 is what
every phone records.

## Results at 12 pixels per cell

Budget is the profile's error-correction radius. Above it the frame is lost
whatever else happens.

### `P1-conservative` — budget 15.69%

| severity | cell errors | doubtful | errors caught | |
| --- | --- | --- | --- | --- |
| 0.0–0.3 | 0.000% | 0.000% | — | ok |
| 0.4 | 2.519% | 1.417% | 26.0% | ok |
| 0.5 | 6.914% | 1.338% | 9.9% | ok |
| 0.6 | 16.019% | 1.522% | 8.0% | **over** |
| 0.8 | 30.104% | 17.612% | 37.2% | over |
| 1.0 | 71.845% | 61.834% | 64.5% | over |

### `P2-standard` — budget 10.98%

| severity | cell errors | doubtful | errors caught | |
| --- | --- | --- | --- | --- |
| 0.0–0.2 | 0.000% | 0.000% | — | ok |
| 0.3 | 0.214% | 0.655% | 67.7% | ok |
| 0.4 | 5.503% | 1.131% | 13.9% | ok |
| 0.5 | 13.040% | 3.055% | 15.4% | **over** |
| 0.7 | 31.906% | 8.902% | 17.4% | over |
| 1.0 | 84.733% | 48.973% | 51.5% | over |

### `P3-dense` — budget 6.27%

| severity | cell errors | doubtful | errors caught | |
| --- | --- | --- | --- | --- |
| 0.0–0.3 | 0.000% | 0.004% | — | ok |
| 0.4 | 0.391% | 0.473% | 48.4% | ok |
| 0.5 | 6.450% | 3.240% | 25.6% | **over** |
| 0.7 | 33.631% | 14.078% | 24.3% | over |
| 1.0 | 86.300% | 47.177% | 49.9% | over |

## Findings

### F1 — The confidence margin does not track error, so erasure decoding is not earning its place

This is the clearest result, and it is a negative one.

Erasure decoding repairs twice as many bytes as error decoding, which is why
`SPEC.md` §5.2.3 recommends it and why the classifier reports a confidence
margin at all. For that to pay off, the cells the margin flags have to be the
cells that are actually wrong.

They are not. In the severity band that decides whether a transfer succeeds —
0.4 to 0.6, where error rates cross the correction budget — the margin catches
**8% to 26%** of the wrong cells. At severity 0.5 on `P2-standard`, 3.1% of
cells are flagged and only 15.4% of the wrong ones are among them.

The margin only becomes informative at severities where the frame is already
long lost: 64.5% caught at severity 1.0 on `P1-conservative`, where 72% of cells
are wrong and nothing can be recovered.

This is a direct answer to open question **Q5**, and the answer is that the
current approach does not work. Either the confidence estimate needs to be
built from something better than a nearest-versus-second-nearest margin, or the
format needs to help — a scattering of known-value cells through the data
region would let a decoder calibrate its threshold against measured error rather
than against an arbitrary constant. Until one of those lands, the erasure path
is complexity that buys close to nothing.

### F2 — The failure is a cliff, not a slope

Every profile goes from essentially zero errors to well past its budget within
two severity steps. `P2-standard` reads 0.214% at severity 0.3 and 13.040% at
0.5.

A cliff is good news for a user interface — there is little middle ground where
a transfer half-works — but it means the "doubtful cell rate" a decoder reports
will not give much warning. It also means the profile ladder is separating
conditions less than intended: `P2-standard` and `P3-dense` both break at
severity 0.5.

### F3 — The profile ladder is real but narrow

`P1-conservative` survives to severity 0.6 while the other two break at 0.5.
That is the right ordering and confirms the ladder is not decorative, but one
step of separation is less than three profiles ought to buy.

`P3-dense` is the interesting case: it holds cleaner numbers than `P2-standard`
up to severity 0.4 (0.391% against 5.503%) and then loses because its budget is
smaller. Its larger grid gives it more pixels per cell at a fixed capture size,
which the ladder's resampling axis rewards. This suggests the profiles are
varying the wrong parameter — the difference that matters may be grid size
rather than bits per cell.

### F4 — White balance is fully absorbed, as designed

A strong colour cast with nothing else wrong costs **zero** cell errors on
`P3-dense`, the profile with the tightest palette. The per-frame calibration
ring does exactly what it exists for, and the cost of carrying it is justified.

### F5 — Noise is a non-issue at plausible levels

Additive sensor noise has to reach a standard deviation of about 0.6 — visually
destroyed footage — before it produces a single error. Each sub-cell is the mean
of four taps and each colour decision the mean of eight sub-cells, so the
classifier averages it away. Noise is not what limits this format.

### F6 — Detection is free, and half a pixel is not

Locating the frame costs nothing that knowing its position would have saved. An
end-to-end transfer through the same channel needs the same number of frames and
loses the same number whether the receiver is told where the code area is or
finds it for itself, so the detector is not the limiting stage.

Getting there took three corrections, and the pattern in them is worth
recording.

The finder ratio alone does not identify a finder. A frame's own header band is
alternating solid cells, and a row across it reproduces 1:1:3:1:1 exactly; one
P2 frame produced 154 candidates, eight of them its own header, and crowded a
real corner out of the shortlist. A finder is distinguished by having that
profile along *all eight* directions, which a stripe does not. A field of
identical payload cells — which is what most of a frame is when the payload is
mostly padding — needed one further condition: the dark centre has to be
visibly wider than the rings, or 1:1:1 passes as 1.5:1:1.

Selection by popularity is the wrong rule. Ranking candidates by how many scan
lines saw them lets clutter out-vote the corners. The four finders are always
convex-hull vertices and payload artefacts never are, so the search runs over
the hull instead.

And then the accuracy bug, which is the one that generalises. A run covering
pixel indices `[start, start+len)` has its centre at `start + (len-1)/2`; using
`len/2` puts every measurement half a pixel late. That is 0.08 cells at six
pixels per module — invisible to any plausible tolerance, uniform across all
four corners, and enough to drag every sample a third of a sub-cell towards its
neighbour. Transfers that succeeded with a known transform failed outright with
a detected one, and the symptom looked like a bad channel rather than a bad
measurement.

This is the second half-pixel error in this codebase; the first was between the
renderer and the sampler. Both came from the same confusion between a pixel's
extent and a pixel's centre, and neither was caught by round-tripping, because
both sides of a round trip can share a convention and still be wrong. Detection
error is now bounded at 0.03 cells in the tests, which is tighter than either
mistake.

## What this changes

Nothing in the specification yet, deliberately. Two of these findings bear
directly on open questions and both need Phase 2 to settle:

- **Q5** now has evidence against the current design (F1). Recorded in
  `SPEC.md` §12.
- **Q1** and **Q3** are affected by F3: if grid size dominates bits per cell,
  the profile ladder should be re-cut around pixels per cell rather than around
  density. That is a change to the profile table, and making it against a
  modelled camera rather than a real one would be exactly the mistake this
  project is trying to avoid.

## Reproducing

```bash
cargo run --release -p photon-cli -- simulate
cargo test -p photon-core simulate
cargo test -p photon-core --test roundtrip
```

The channel is seeded and deterministic. A result that cannot be reproduced
exactly is a bug in the harness.
