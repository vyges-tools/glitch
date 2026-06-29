# vyges-glitch

Static **glitch / hazard** analysis: a gate-level **netlist** and a **Liberty** in,
the list of **reconvergent-fanout hazards** out — the spots where a signal can
momentarily glitch.

> **Vyges open EDA tools.** Commercial-grade silicon sign-off capability, built on
> open standards and plain file formats — and meant to be accessible to everyone,
> not only teams who can license a six-figure tool. `vyges-glitch` opens up hazard
> analysis.

> **Stability: experimental (v0.1.0).** Reconvergent-fanout detection with static
> (parity) and dynamic (delay-window) classification is real and tested; this is a
> structural lint, not a transient/SPICE glitch sign-off — see **Current state**.

## Why this exists

When one signal reaches a gate's output by more than one path, the paths can
disagree for a moment — and the output glitches before it settles. That is a
*hazard*, and it is exactly what a **lockstep gate-level simulator cannot see**: a
cycle-based sim samples one settled value per tick and steps right over the
intermediate glitch. Catching it is a **structural + timing** question, not a
simulation one — which puts it squarely in the deterministic-Rust lane and makes it
a clean complement to the simulators it is invisible to.

## How this is solved today

Hazard / glitch analysis lives inside **commercial** static tools (and bespoke
academic scripts); there is no small, open, embeddable engine for it. `vyges-glitch`
is a clean-room Rust engine that reads the **same Liberty / Verilog** the rest of
the Vyges flow already uses, taking cell parity from each arc's `timing_sense` and
the glitch window from the very delay tables `vyges-sta-si` times with — one
toolset, one language.

## Use it

```sh
cargo build --release            # std-only beyond the shared parsers

vyges-glitch check design.v --lib cells.lib                      # -> hazard report
vyges-glitch check design.v --lib cells.lib --json
vyges-glitch check design.v --lib cells.lib --fail-on-violation  # exit 3
# flags: --lib FILE · -o FILE · --json · --fail-on-violation · -h · -V
```

```text
vyges-glitch — 1 hazard(s): 1 static, 0 dynamic
  static  a → f   2 path(s), window 0.0976 ns
```

## How it works

- **Sources** are primary inputs and flop Q outputs; **endpoints** are primary
  outputs and flop data pins.
- For each endpoint, a memoized DP walks the combinational cone back to its sources,
  tracking per source the set of path **parities** (from `timing_sense`: positive →
  keep, negative → invert, non-unate → both), the **fastest/slowest** path delay,
  and the **path count**. It is polynomial in (nets × sources) — never an
  exponential path enumeration — and breaks (and counts) combinational loops.
- A source reaching an endpoint by **≥2 paths** is a reconvergent fanout. It is a
  **static** hazard if the path parities differ (a single edge can drive the
  endpoint the wrong way for a moment), or a **dynamic** hazard if the parities
  match but the delays differ (a glitch over the settling window ≈ slowest − fastest
  path). A balanced reconvergence (same parity, same delay) is *not* flagged.

## Current state (v0.1.0)

**Working & tested:** static-hazard detection on inverted-vs-non-inverted
reconvergence, dynamic-hazard detection on delay-skewed same-parity reconvergence,
hazards into both primary outputs and flop data pins, combinational-loop breaking,
worst-window-first ordering. Text + `--json`, a `--fail-on-violation` CI exit code.

**Depth reserved (honest):**

- it reports the **structural opportunity** for a glitch, not a proof one occurs —
  it does not solve for an input assignment that sensitizes both paths (that is the
  function-vs-logic-hazard distinction, a SAT/BDD pass);
- the glitch window uses a **single nominal-slew/load** arc delay (the same spirit
  as an SDF IOPATH at a fixed operating point), not a full timer-propagated slew;
- parity needs the Liberty's `timing_sense`; arcs without it are treated as
  non-unate (conservative — they widen, never hide, a hazard);
- multi-input *function* hazards and asynchronous (set/reset) paths are not yet
  modelled.

**Validation roadmap:** correlate flagged endpoints against a glitch-aware
event/transient simulation on representative blocks — the same oracle-backed
discipline the rest of Loom uses.
