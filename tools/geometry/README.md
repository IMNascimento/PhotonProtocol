# Geometry and test-vector tools

Independent derivations of the numbers published in `SPEC.md`. They exist so
that the specification's tables can be checked without running — or trusting —
the Rust implementation. A bug that lives in both the spec and the reference
implementation is invisible; a second, deliberately unrelated derivation makes
it visible.

## `frame-geometry.js`

Recomputes the profile tables of `SPEC.md` §8 from the formulas of §4.2.7, §4.4
and §5.2.2: reserved cell budget, header band height and codeword length, data
cell count, raw byte count, Reed-Solomon partitioning and payload capacity.

```bash
node tools/geometry/frame-geometry.js
```

## `test-vectors.js`

Computes the byte sequences of `SPEC.md` §10 — frame header, manifest, and the
manifest wrapped in a payload unit — together with their CRCs and digests. It
first verifies both CRC implementations against the published check values for
the string `123456789`, so a wrong CRC parameterisation fails loudly instead of
producing plausible garbage.

```bash
node tools/geometry/test-vectors.js
```

The Rust test suite asserts the same vectors. If the two ever disagree, the
specification is the authority.
