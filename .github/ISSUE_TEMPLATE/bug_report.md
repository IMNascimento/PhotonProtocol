---
name: Bug report
about: Something behaves differently from what the specification says
title: ''
labels: bug
assignees: ''
---

## What happened

<!-- What did you expect, and what did you get instead? -->

## Which part

- [ ] `photon-core` (the protocol)
- [ ] `photon-cli` (bench command line)
- [ ] `photon-wasm` / one of the web pages
- [ ] `SPEC.md` (the specification says the wrong thing, or two things)
- [ ] tooling / CI

## Reproducing it

<!-- Commands, inputs, and the exact bytes if a format is involved. -->

## If a decode failed

The decoder is required to say which stage gave up and how close it came
(`SPEC.md` §9.2). Please paste that report — the error code and any counts —
rather than only "it did not work". If it did *not* say, that is itself the bug
and worth reporting.

- Profile used:
- Emitting device and screen refresh rate:
- Recording device, resolution and frame rate:
- Roughly how much of the video frame the screen filled:

## Environment

- Version or commit:
- Operating system:
- Rust version, or browser and version:
