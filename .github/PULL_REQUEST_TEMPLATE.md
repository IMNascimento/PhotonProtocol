## What this changes

<!-- And, more importantly, why. The diff already shows what. -->

Closes #

## Type

- [ ] Fix
- [ ] Feature
- [ ] Specification change
- [ ] Documentation
- [ ] Refactor
- [ ] Performance
- [ ] Tooling / CI

## Checks

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-targets`
- [ ] Targets `develop`, not `main`

## If this touches the wire format

- [ ] `SPEC.md` is updated **in the same commit** as the code
- [ ] Test vectors added or updated, and the Node derivations in `tools/` agree
- [ ] `protocol_version` bumped, or the change is genuinely backwards compatible

## If this claims to be faster

- [ ] A `criterion` benchmark is included, and the before/after numbers are in
      the description below

<!--
Measurements, if any:
-->
