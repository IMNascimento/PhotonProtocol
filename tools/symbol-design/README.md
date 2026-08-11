# Symbol design tools

Scripts that *derive* the constants baked into `SPEC.md`. They are not part of
the protocol implementation; they exist so that every magic number in the
specification can be reproduced and challenged by anyone.

## `shape-search.js`

Selects the shape alphabet used by the physical layer (`SPEC.md` §4.3).

A shape is a 4x4 binary mask painted in the cell's ink colour over a black cell
background. The alphabet must survive what a camera actually does to it, so the
search does not optimise raw Hamming distance. Instead each candidate mask is:

1. rendered at 4x supersampling (16x16),
2. blurred with a Gaussian of sigma = 0.5 sub-cell,
3. resampled to a 4x4 feature vector — the same measurement a decoder makes —
   under five sub-cell registration offsets (0, +/-0.35 sub-cell in x and y).

The score of a pair of masks is the *worst case* Euclidean distance between
their feature vectors over every combination of registration offsets. The
score of an alphabet is the minimum over all its pairs, and the search
maximises that (greedy seeding from every candidate, followed by swap-based
local improvement).

Two constraints prune the 65536-mask space first:

- **balanced ink** (exactly 8 of 16 sub-cells set) so every shape carries the
  same amount of colour energy, which keeps the colour classifier independent
  of the shape classifier;
- **low perimeter** (at most 10 differing 4-neighbour adjacencies) so shapes
  are chunky; high-frequency masks such as checkerboards wash out under blur
  and are useless in practice even though their Hamming distance looks good.

### Running

```bash
node tools/symbol-design/shape-search.js [max_perimeter]
```

Default `max_perimeter` is 10, which is the value used to produce the alphabets
in `SPEC.md`. The output is deterministic.

Reference result (`max_perimeter = 10`):

| alphabet | masks | worst-case min feature distance |
| -------- | ----- | ------------------------------- |
| 4 shapes | `0x00FF 0x3333 0xCCCC 0xFF00`                                           | 2.3075 |
| 8 shapes | `0x00FF 0x1DF0 0x3333 0x662E 0x8CCE 0xC837 0xF710 0xFC88`                | 1.7803 |

These are provisional. Phase 1 (synthetic channel) and Phase 2 (real camera
capture) will re-run the search with a blur sigma and registration error
measured from actual footage rather than assumed.
