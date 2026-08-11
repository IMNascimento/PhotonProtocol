# Contributing to PhotonProtocol

Thank you for wanting to help. This document is short and specific; please read
it before opening a pull request.

## The two rules that matter most

**1. No change to the wire format without updating `SPEC.md` in the same
commit.** The specification is the product. A repository whose code has quietly
moved ahead of its specification is not a protocol, it is one program with a
document attached.

**2. No optimisation without a benchmark showing the gain.** The open questions
in `SPEC.md` §12 are open precisely because they should be settled by
measurement. An argument, however good, is not a measurement.

## Branches

- `main` holds released, reviewed work. Nothing is pushed to it directly.
- `develop` is the integration branch. Branch from it, and target it with pull
  requests.
- Working branches are named `feature/…`, `fix/…`, `docs/…` or `spec/…`.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/), with the scope
being the crate or area: `feat(core):`, `fix(cli):`, `docs(spec):`, `ci:`,
`build:`, `test:`, `refactor:`, `perf:`, `chore:`.

Keep commits small and self-contained. A commit that changes one thing and
explains why is worth more than a commit that changes six things and says
"updates". Every commit on `develop` should build and pass its tests, so that
`git bisect` stays usable.

**The subject line says what changed; the body says why.** What changed is
already visible in the diff. Why it changed — the constraint you were under,
the alternative you rejected, the failure mode you were avoiding — is not, and
in six months it is the only part anyone needs.

## Before opening a pull request

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

If you touched anything the specification publishes, also run the independent
derivations:

```bash
node tools/geometry/frame-geometry.js
node tools/geometry/test-vectors.js
```

CI runs all of this on Linux, Windows and macOS. The protocol is byte-exact by
definition, so a platform-dependent result is a bug, not an inconvenience.

## Testing expectations

- Every binary format — header, manifest, unit framing — gets round-trip tests
  and fixed vectors published in `SPEC.md`, so third-party implementations can
  check themselves against the same bytes.
- Decoder failures get tests too. `SPEC.md` §9.2 requires a decoder to say
  *which* stage failed and how close it came; a decoder that returns a bare
  error is not conforming, and the tests should catch that.
- Performance claims get a `criterion` benchmark alongside them.

## Proposing a change to the format

Open an issue first, before writing code. Say what the change costs in cells,
what it buys in robustness or density, and how you would measure the trade.
Changes that add reserved regions or alter the scan order need a
`protocol_version` bump and should be batched rather than trickled.

If you found the specification ambiguous — two readings, both defensible — that
is a defect in the specification even if the reference implementation does
something sensible. Please report it as one.

## Style

- Code, comments, documentation and the specification are in **English**. The
  README is also translated to Portuguese; other translations are welcome.
- `rustfmt` and `clippy` settle formatting and lint questions. Their
  configuration is in the repository, so please change the configuration rather
  than sprinkling `#[allow]`.
- An `#[allow]` that is genuinely warranted needs a comment saying why.
- Comments explain *why*. The code already says what.

## Licensing of contributions

By contributing, you agree that your work is licensed under both the MIT
licence and the Apache License 2.0, matching the project's dual licence. You
also confirm you have the right to contribute it.

## Code of conduct

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
