//! vyges-glitch — static **glitch / hazard** analysis.
//!
//! A gate-level **netlist** + a **Liberty** in, the list of **reconvergent-fanout
//! hazards** out — the spots where one signal reaches a combinational endpoint by
//! more than one path and can therefore produce a transient glitch. This is a
//! purely *structural + timing* question, and exactly the blind spot of a lockstep
//! gate-level simulator: it samples one settled value per tick and never sees the
//! intermediate glitch at all.
//!
//! Two classes are reported:
//! - a **static hazard** — a source reconverges through paths of *different
//!   inversion parity* (one path inverts the signal, another doesn't), so a single
//!   input edge can momentarily drive the endpoint to the wrong value;
//! - a **dynamic / transition hazard** — a source reconverges through paths of the
//!   *same parity but different delay*, so the endpoint can glitch during the
//!   settling window (≈ the slowest-minus-fastest reconverging path).
//!
//! Parity comes from each Liberty arc's `timing_sense` (unateness); the delay
//! window from the same delay tables `vyges-sta-si` times with. Pure std beyond the
//! shared parsers.

// The crate's own name inside it: the command-line entry ([`cli_main`]) is built into the
// library too, and its `vyges_glitch::…` paths resolve here.
extern crate self as vyges_glitch;

pub use vyges_loom::{liberty, netlist};

pub mod glitch;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COPYRIGHT: &str = "© 2026 Vyges. All Rights Reserved.  https://vyges.com";

/// The `vyges-glitch` command-line entry (the binary's `main`), built into the library too so a
/// suite binary can link every engine and run the one it is invoked as.
#[cfg(feature = "suite")]
#[doc(hidden)]
#[path = "main.rs"]
pub mod cli_main;
