# Benchmarks

What the protocol measured on 2026-09-28, kept so that a change meant to make
transfers faster can be shown to, against the figures from before it.

```bash
cargo build --release -p photon-cli
node tools/e2e/matrix.mjs <file> --against docs/benchmarks/baseline.json
```

Two kinds of figure are here and they must not be confused. The first is from a
real phone. The rest are from `photon film`, which is a model of one.

`baseline.json` is `develop` as phase 2 left it. `dense.json` is the branch
`experiment/throughput`, which has the dense format of `SPEC.md` §13.

## The figure to beat

[Decimen](https://github.com/bashalarmistalt/decimen-optical-transfer) sends
fountain-coded QR codes, four of them side by side, and records in
`benchmarks/records.json` what it has done between real devices:

| | file | time | sustained | peak |
| --- | --- | --- | --- | --- |
| a 49-inch monitor to an iPhone 17 Pro Max | 1 MiB | 2.45 s | 418.5 KB/s | 601.5 KB/s |
| an iPhone 17 Pro Max to another | 1 MiB | 5.14 s | 199.2 KB/s | 340.8 KB/s |

The same mebibyte, of bytes that will not compress, through `D3-blaze`:

| | camera | codes shown a second | time | throughput |
| --- | --- | --- | --- | --- |
| **simulated** camera, decoder reading every picture | 1080p at 29.5 a second | 30 | 2.14 s | 479.5 KB/s |
| | 1080p at 29.5 a second | 60 | 2.78 s | 368.4 KB/s |
| | 1080p at 59 a second | 60 | 1.15 s | 888.5 KB/s |
| **simulated** camera, the receiving page in Chrome | 1080p at 29.5 a second | 30 | 2.2 s | 462 to 473 KB/s |

**These are not the same kind of figure, and the comparison is not made
yet.** Theirs is two devices in a room. Ours is a model of a camera, which has
been wrong before in the direction that flatters: it said the denser profiles
of phase 2 read, and the phone said they did not. What the table says is that
the format and the decoder have the capacity. Whether a phone has is for a
phone to say, and the row that will settle it is the one below that is still
empty.

| | camera | codes shown a second | time | throughput |
| --- | --- | --- | --- | --- |
| a 1080p monitor to an iPhone, `D3-blaze` | | | | not yet measured |

The simulated camera of these rows is the `typical` setting with the code
filling 88% of the width of the picture: 2.7 camera pixels to a module, a lens
that blurs by a pixel, a shutter open for 12 ms at thirty pictures a second and
6 ms at sixty, and a display whose pixels take 6 ms to get most of the way to
what they are changing to.

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

### The dense profiles

The branch `experiment/throughput`, three seconds of each condition, a file
that does not compress and is too long to finish. A picture of a dense code
yields tiles, so what is counted is what the tiles that read carried, and the
codes read a second is that many codes' worth.

| conditions | camera px per module | codes' worth a second, of shown | KB/s |
| --- | --- | --- | --- |
| `D1-swift`, typical | 4.14 | 30.8 of 30 | 151.5 |
| `D2-rapid`, typical | 3.32 | 30.5 of 30 | 262.2 |
| `D3-blaze`, good | 2.75 | 30.4 of 30 | 532.6 |
| `D3-blaze`, typical | 2.74 | 27.1 of 30 | 474.7 |
| `D3-blaze`, typical, further away | 2.49 | 20.3 of 30 | 356.4 |
| `D3-blaze`, typical, bent lens | 2.68 | 25.0 of 30 | 438.7 |
| `D3-blaze`, typical, shaky | 2.74 | 27.1 of 30 | 475.6 |
| `D3-blaze`, typical, overexposed | 2.74 | 9.4 of 30 | 165.3 |
| `D3-blaze`, typical, 60 codes/s | 2.74 | 21.9 of 60 | 384.5 |
| `D2-rapid`, typical, 60 codes/s, camera at 60 | 3.32 | 59.8 of 60 | 514.2 |
| `D3-blaze`, typical, 60 codes/s, camera at 60 | 2.74 | 50.8 of 60 | 890.0 |
| `D3-blaze`, typical, 165 Hz screen, camera at 60 | 2.74 | 55.0 of 55 | 964.4 |
| `D3-blaze`, poor | — | 0 of 30 | 0 |

More than thirty codes' worth of thirty is a picture yielding tiles of the
code before or after the one it was mostly of.

On the same branch the rows for the profiles of cells are within 3% of
`baseline.json` either way. Their median of wrong cells is higher in several,
and that is pictures being read that were not: a picture taken as the code
changed, with a tenth of its cells wrong, used to be given up at its header
and is now read.

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

A picture of `D3-blaze`, 1920x1080, which has 201,600 modules in it: 47 ms.
About 6 of them are finding the frame, 10 measuring the grid, 6 sampling the
modules and their black and white, 7 taking the channel out, 7 error
correction, and the rest looking under the tiles that were read for another
code. Through WebAssembly in Chrome it is 65 ms, copying the picture out of
the video included.

## The receiving page, simulated footage as its camera

Chrome 151, `tools/e2e/receive.mjs`, `typical` preset, 1080p.

| file | codes shown a second | browser slowed | pictures read a second | throughput |
| --- | --- | --- | --- | --- |
| 151 KB | 10 | — | 29 | 24.5 KB/s |
| 151 KB | 10 | 4 times | 23 | 20.9 KB/s |
| 151 KB | 10 | 6 times | 16 | 20.3 KB/s |
| 2.7 MB | 15 | 4 times | 21 | 24.6 KB/s |

`D3-blaze`, the branch `experiment/throughput`, the same browser and setting,
thirty codes shown a second:

| file | browser slowed | pictures read a second | throughput |
| --- | --- | --- | --- |
| 1 MiB | — | 30 | 462 to 473 KB/s |
| 1 MiB | 2 times | 30 | 430 KB/s |
| 1 MiB | 3 times | 18 | 298 KB/s |
| 1 MiB | 4 times | 13 | 208 KB/s |

Six readers. What a slower device costs is pictures, and with them tiles: the
throughput of the page is how many pictures a second it gets through, as it
was in phase 2. The iPhone of the first table read a picture of
`P1-conservative` about as fast as this desktop did, not slowed.

Chrome plays a recording as a camera at no more than thirty pictures a second,
so the rows of the table above with a camera at sixty have not been through
the page.
