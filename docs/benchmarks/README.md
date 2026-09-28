# Benchmarks

What the protocol measured on 2026-09-28, kept so that a change meant to make
transfers faster can be shown to, against the figures from before it.

```bash
cargo build --release -p photon-cli
node tools/e2e/matrix.mjs <file> --against docs/benchmarks/baseline.json
```

Two kinds of figure are here and they must not be confused. The first is from a
real phone. The rest are from `photon film`, which is a model of one.

## A real phone

An iPhone on iOS 18.7, in Safari, pointed by hand at a 1080p monitor refreshing
at 164 Hz. The camera gave 1080x1920. The file was an 818 KB PNG. Reported by
the receiving page itself, with `?capture` in its address.

| profile | codes shown a second | pictures read a second | throughput | |
| --- | --- | --- | --- | --- |
| `P1-conservative` | 10 | 29 | 23.7 KB/s | received in 34 s, digest verified |
| `P1-conservative` | 20 | 30 | 38.2 KB/s | received in 21 s, digest verified |
| `P4-balanced` | 10 | 26 | — | code found, header read, no payload decoded |
| `P2-standard` | 10 | 26 | — | the same |
| `P3-dense` | 10 and 20 | 25 | — | the same |

The phone reads a picture in 9 to 18 ms and copies one out of the video in 11
to 16 ms, on two readers. That is quicker than the desktop slowed four times
which stood in for it below.

Of the pictures of `P1-conservative` in which the code was found, 92% were
read. Of the pictures of the other three, none were.

**Why the denser profiles do not read.** Compared against the file that was
sent, the pictures the phone could not read are wrong in the middle of the code
and right at its edges. In a picture of `P1-conservative` that failed, 1% of
cells were wrong in the outer sixth of the code and 40% to 47% in the middle;
in one of `P4-balanced`, 55% and 80%. Measured block by block against what was painted, the code's interior
sits up to 0.3 of a cell from where a homography through the four finder
patterns puts it, in a field that is zero at the perimeter and at the centre
and largest between them. The correction fitted from the timing ring does not
follow it, because it is measured at the perimeter, where there is nothing to
measure. `P1-conservative` has cells large enough to be read 0.3 of a cell out.
The others have not.

The simulated camera bends a picture about its centre, where the code is, and
so bends the code very little. The real one bends it more, and differently.

## The simulated camera

`node tools/e2e/matrix.mjs`, three seconds of each condition, a 60 KB file that
does not compress. `P1-conservative` unless it says otherwise. The throughput
is what a decoder that read every picture would have carried.

| conditions | camera px per cell | codes read a second, of shown | KB/s | median wrong cells |
| --- | --- | --- | --- | --- |
| ideal | 9.55 | 10.3 of 10 | 26.3 | 0.00% |
| good | 9.12 | 10.3 of 10 | 26.3 | 0.00% |
| typical | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| poor | 6.77 | 10.0 of 10 | 25.5 | 4.37% |
| typical, 15 codes/s | 8.27 | 15.3 of 15 | 39.1 | 0.03% |
| typical, 20 codes/s | 8.27 | 19.0 of 20 | 48.4 | 1.93% |
| typical, 144 Hz screen | 8.27 | 10.7 of 10 | 27.2 | 0.00% |
| typical, further away | 6.20 | 10.0 of 10 | 25.5 | 0.37% |
| typical, shutter rolling down | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| typical, landscape | 8.79 | 10.3 of 10 | 26.3 | 0.00% |
| typical, 720p | 6.20 | 10.0 of 10 | 25.5 | 0.45% |
| typical, 4K | 16.54 | 10.3 of 10 | 26.3 | 0.00% |
| typical, 1440p screen | 8.25 | 10.3 of 10 | 26.3 | 0.00% |
| typical, shaky | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| typical, overexposed | 8.27 | 10.3 of 10 | 26.3 | 0.00% |
| typical, bent lens | 9.73 | 10.3 of 10 | 26.3 | 0.00% |
| P4-balanced, good | 6.91 | 10.0 of 10 | 48.2 | 0.00% |
| P4-balanced, typical | 6.26 | 10.0 of 10 | 48.2 | 0.28% |
| P4-balanced, typical, 15 codes/s | 7.25 | 15.0 of 15 | 72.2 | 0.63% |
| P4-balanced, poor, close up | 7.01 | 10.0 of 10 | 48.2 | 3.37% |
| P2-standard, typical, 4K | 12.62 | 10.3 of 10 | 71.1 | 0.00% |
| P3-dense, typical, 4K | 11.51 | 10.3 of 10 | 153.8 | 0.00% |

`baseline.json` holds the same results for `--against`.

The rows for `P4-balanced`, `P2-standard` and `P3-dense` are what the model
says. A real phone says otherwise, above, and until the model is made to bend a
picture the way a real lens does those rows measure the decoder and not the
world.

## Reading a picture

`photon bench`, one core of a desktop, 1080x1920, `P1-conservative`.

| stage | before this work | now |
| --- | --- | --- |
| finding the frame | 29.0 ms | 6.3 ms |
| fitting the grid | — | 0.9 ms |
| reading the cells | 20.0 ms | 8.0 ms |
| **a picture** | **50.0 ms** | **15.2 ms** |

## The receiving page, simulated footage as its camera

Chrome 151, `tools/e2e/receive.mjs`, `typical` preset, 1080p.

| file | codes shown a second | browser slowed | pictures read a second | throughput |
| --- | --- | --- | --- | --- |
| 151 KB | 10 | — | 29 | 24.5 KB/s |
| 151 KB | 10 | 4 times | 23 | 20.9 KB/s |
| 151 KB | 10 | 6 times | 16 | 20.3 KB/s |
| 2.7 MB | 15 | 4 times | 21 | 24.6 KB/s |
