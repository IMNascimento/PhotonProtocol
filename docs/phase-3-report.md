# Phase 3 report: a format for the camera there is

Phase 2 ended with a file on a phone and a list of what the phone would not
read. `P1-conservative` moved an 818 KB picture to an iPhone at 23.7 KB/s, and
38.2 KB/s with the codes changing twice as fast. The three denser profiles
decoded nothing.

This is the account of what was done about it: why the pictures would not
read, what reads some of them now, and the format that came of taking what the
phone showed at its word. **Every figure for that format is from a simulated
camera.** It has been through the real pages in a real browser, with simulated
footage as the camera, and it has not been through a real phone. Where a
figure here is from a phone it says so.

## What the phone's pictures said

The receiving page sends the pictures it could not read to the development
server when it is asked to, and the sending page sends what it painted. So for
128 pictures an iPhone took there is, for every cell, what was painted and what
was seen.

**The grid is out in the middle and nowhere else.** Measured block by block,
the interior of the code sits up to 0.3 of a cell from where the homography
through the four finder patterns puts it. At the perimeter it sits where it
should. The correction fitted from the timing ring measured the perimeter.

**The calibration ring is not like the payload.** It runs next to the white
margin, and light from the margin spreads into it. A template learnt from the
ring is washed toward white and matches a cell in the middle of the frame
badly.

**Black is a fifth of white, and not everywhere the same fifth.** In the
camera's numbers black sits near 95 of 255 and white near 205, which as light
is 0.11 and 0.62, and the bottom of a screen seen from slightly above is
washed out more than the top.

**Brightness is sharp and colour is not.** An edge between black and white
goes from a tenth to nine tenths of its height in 1.6 to 2.7 pixels. Colour
takes twice that. Given the cell each picture was really of, and a classifier
that knew every template exactly, 7% to 10% of cells of `P4-balanced` at 7.9
pixels a cell were still wrong, nearly all of them in colour: shape alone was
wrong in 0.4% to 3%.

The first three can be put right in the decoder. The fourth cannot, and is why
there is a new format.

## What reads more of them

| | pictures of `P4-balanced` read, of 33 |
| --- | --- |
| as phase 2 left it | 0 |
| the grid measured from the edges of the cells | |
| the alphabet learnt from the payload | |
| black and white measured by neighbourhood | 15 |

The three were measured together, not one at a time, and the 15 is of all of
them. The median picture has 6.6% of its cells wrong, which is the colour.

The picture of `P1-conservative` that the phone could not read now reads with
0.09% of its cells wrong.

**The grid from the cells themselves.** Every cell has edges at its half-cell
boundaries. In patches of eight cells by eight the phase of those edges gives
the displacement of the patch to within a period; the timing ring says which
period at the perimeter, and the patches are unwrapped inward from there.

**The alphabet from the payload.** For profiles of four colours: shape from
which sub-cells are painted, and colour by clustering the direction of painted
less unpainted. No labels are needed, because a frame has every symbol in it
about equally often.

Which it has only if it is whitened. A file that compresses well ends in a
frame that is mostly padding, and that frame is one cell repeated. So the coded
bytes are XORed with a fixed sequence before they are painted. **This changes
what is painted**: frames of draft 0.2 are not read by this decoder.

## The dense format

`SPEC.md` §13. Three decisions, each the plain reading of something above.

1. **Black and white.** A module is a bit. At three camera pixels to a module
   a frame the shape of the screen has 600 by 336 of them.
2. **Tiles.** A frame is cut into 56, each with its own codewords, its own
   checksum, and its own note of which code it is from. A picture yields the
   tiles it caught whole.
3. **A mark in every tile**, which is found outright, with the edges of the
   modules for what is between the marks.

| profile | modules | tiles | a frame carries | on a 1080p screen |
| --- | --- | --- | --- | --- |
| `D1-swift` | 360 x 192 | 20 | 5040 B | 5 px a module |
| `D2-rapid` | 450 x 240 | 30 | 8800 B | 4 px a module |
| `D3-blaze` | 600 x 336 | 56 | 17952 B | 3 px a module |

`P3-dense`, which needed a 4K camera, carried 15244.

## What reading it took

In the order it was found, against the simulated camera at its `typical`
setting, which blurs by a pixel and sharpens afterwards as a phone does.

**Nothing read.** `D3-blaze` at 2.5 camera pixels to a module, each module
held against the middle of its neighbourhood's black and white: 6.5% of
modules wrong in the median picture, and not one tile in 3304 within reach of
its parity. A lone white module among black ones is grey.

**The channel taken out: 68% of tiles.** What a lens and a sensor do to a
module is the same for every module of a tile, and a tile has 3536 of them to
measure it from. It is measured as nine taps by least squares, against what
the modules are first taken to be, and each module is then decided with the
light of its neighbours removed. The taps are measured for each tile rather
than for the frame because they also take up what is left of the grid being
out: a grid a fifth of a module to the left is a channel with more of the
right-hand neighbour in it.

**In light: 80%.** The same, with the camera's numbers raised to the power 2.2
first. A lens adds light. It does not add the numbers light is written down
in.

**The code under the code.** A display takes milliseconds to change and a
shutter is open for milliseconds, so at thirty codes a second a fifth of the
tiles of a picture have the light of two codes in them, and at sixty most of
them have. Neither can be read from the sum. But once a tile is read, what it
painted is known exactly: it is fitted to what was seen and taken out, and
what is left is read as a tile of another code. And what was seen of a tile
that would not read is kept, and the same is done to it when a picture before
or after has yielded the code that was in it.

As each was added, on the same two seconds of footage: `D3-blaze` at 2.5
camera pixels to a module, thirty codes a second, a camera taking 29.5
pictures a second, the `typical` setting.

| | tiles read | KB/s |
| --- | --- | --- |
| held against a threshold | 0% | 0 |
| the channel taken out | 68% | 372 |
| the grid unwrapped outward from the marks, and a mark's search centred | 76% | 399 |
| in light | 80% | 421 |

At thirty codes a second there is little under a code to find. With sixty
codes a second shown and sixty pictures a second taken, which is where there
is, the table of conditions in `docs/benchmarks` has `D3-blaze` at about 890
KB/s.

**Bent by a lens.** With lines bent by 4%, no mark was found: the homography
is right at the corners, where it was fitted, and four modules out in the
middle, and a search of three and a half modules round where it said a mark
would be found none. Marks are now found from the corners inward, each looked
for where the ones already found say it will be. They are scored by how much
lighter the border is than the core *less how far either is from being one
shade*, without which a square that happens to be lighter round its edge is
found in any data. And the grid is pinned at the four finder patterns: between
the last mark and a corner the bend comes back to nothing, and nothing but the
finder pattern says so.

One thing made it worse and was taken out again. A median over each patch and
its neighbours, to outvote a patch measured wrongly, also flattens a slope,
and at the edge of a frame, where every neighbour is on the same side, it
moved right patches a module. A patch is now held against the line through
its neighbours rather than their middle.

## What does not work

**Overexposure.** With white half as bright again as the sensor can measure,
`D3-blaze` carries 165 KB/s where it carried 475. A dark module among light
ones is as white as they are, and nothing that reads the picture afterwards
can know it was there. The pictures the iPhone took are not overexposed: their
white is near 205 of 255.

**The `poor` camera.** Nothing of `D3-blaze` reads through it. `D1-swift` is
for that.

**More than about 2.5 camera pixels to a module is needed.** At 1080p that is
a code filling four fifths of the width of the picture, held on its side.

## What is not known

Whether a phone gives a browser sixty pictures a second at 1080p, and how long
its shutter is open when it does. How long a real display takes to change.
Whether what an iPhone does to a picture before a browser sees it, which is
tuned for faces, leaves light adding up as it should. How many pictures a
second a phone can read: a desktop reads one of `D3-blaze` in 47 ms, and
through WebAssembly in 65.

The model was wrong about the lens before. It may be wrong about any of these.
