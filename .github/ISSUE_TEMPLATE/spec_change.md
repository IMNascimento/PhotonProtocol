---
name: Format change proposal
about: Propose a change to the wire format
title: ''
labels: spec
assignees: ''
---

<!--
Open this before writing code. A format change costs everyone who has already
implemented against the specification, so the case has to be made first.
-->

## What you want changed

<!-- Which section of SPEC.md, and what it should say instead. -->

## What it costs

<!--
In cells per frame, in payload capacity, in decoder complexity. Numbers, not
adjectives.
-->

## What it buys

<!--
Robustness against which specific failure? Density under which conditions?
-->

## How you would measure the trade

<!--
The project settles these by measurement. Describe the experiment: which
severities of the synthetic channel, or which real recordings, and what result
would make you abandon the proposal.
-->

## Compatibility

- [ ] Backwards compatible (a new unit type or a new profile)
- [ ] Requires a `protocol_version` bump

## Related open question

<!-- If this addresses one of Q1–Q7 in SPEC.md §12, say which. -->
