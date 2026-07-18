//! The hazard engine.
//!
//! For each combinational **endpoint** (a primary output or a flop data pin) we
//! aggregate, per **source** (a primary input or a flop Q), how that source reaches
//! the endpoint: the set of inversion **parities** of the paths, the **fastest and
//! slowest** path delay, and the **path count**. A source that reaches an endpoint
//! by two or more paths is a *reconvergent fanout* — the necessary structural
//! condition for a glitch. We then classify:
//!
//! - **static** — the parities differ (one path inverts the source, another
//!   doesn't); a single edge on the source can momentarily glitch the endpoint;
//! - **dynamic** — same parity but the path delays differ; the endpoint can glitch
//!   over the settling window (≈ slowest − fastest reconverging path).
//!
//! The aggregation is a memoized DP over the combinational DAG, so it is polynomial
//! in (nets × sources), never an exponential path enumeration. Combinational loops
//! are broken (and counted) rather than followed.

use std::collections::{BTreeMap, BTreeSet};

use crate::liberty::{Dir, Lib};
use crate::netlist::{Inst, Netlist};

// nominal operating point for a representative arc delay (same spirit as an SDF
// IOPATH at a fixed slew/load — relative spread is what the glitch window needs).
const SLEW: f64 = 0.05;
const LOAD: f64 = 0.005;
const EPS: f64 = 1e-6;

const EVEN: u8 = 0b01; // non-inverting parity reachable
const ODD: u8 = 0b10; // inverting parity reachable

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Static,
    Dynamic,
}

impl Kind {
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Static => "static",
            Kind::Dynamic => "dynamic",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hazard {
    pub endpoint: String,
    pub source: String,
    pub kind: Kind,
    pub paths: u64,
    /// settling window in ns (slowest − fastest reconverging path); 0 for a pure
    /// static hazard whose paths happen to be balanced in delay.
    pub window_ns: f64,
}

#[derive(Debug, Default)]
pub struct GlitchReport {
    pub hazards: Vec<Hazard>,
    pub comb_loops: usize,
}

/// Per-source aggregate of all paths from a source to the current net.
#[derive(Debug, Clone, Copy)]
struct Agg {
    parity: u8,
    min_d: f64,
    max_d: f64,
    count: u64,
}

fn net_of<'a>(inst: &'a Inst, pin: &str) -> Option<&'a str> {
    inst.conns
        .iter()
        .find(|(p, _)| p == pin)
        .map(|(_, n)| n.as_str())
}

fn apply_sense(parity: u8, sense: &str) -> u8 {
    match sense {
        "positive_unate" => parity,
        "negative_unate" => ((parity & EVEN) << 1) | ((parity & ODD) >> 1), // swap even/odd
        _ => {
            // non_unate (XOR-like) — the arc can pass or invert depending on side
            // inputs, so both parities become reachable.
            if parity == 0 {
                0
            } else {
                EVEN | ODD
            }
        }
    }
}

/// A representative delay for a delay arc at the nominal operating point.
fn arc_delay(arc: &crate::liberty::Arc) -> f64 {
    let r = arc.cell_rise.lookup(SLEW, LOAD);
    let f = arc.cell_fall.lookup(SLEW, LOAD);
    r.max(f)
}

struct Ctx<'a> {
    nl: &'a Netlist,
    lib: &'a Lib,
    /// net -> (instance index, output pin, is_sequential_driver)
    driver: BTreeMap<&'a str, (usize, &'a str, bool)>,
    inputs: BTreeSet<&'a str>,
}

fn merge(map: &mut BTreeMap<String, Agg>, start: &str, a: Agg) {
    map.entry(start.to_string())
        .and_modify(|e| {
            e.parity |= a.parity;
            e.min_d = e.min_d.min(a.min_d);
            e.max_d = e.max_d.max(a.max_d);
            e.count = e.count.saturating_add(a.count);
        })
        .or_insert(a);
}

/// Backward aggregation for a net, memoized. A combinational loop edge is skipped
/// and counted via `loops`.
fn compute(
    net: &str,
    ctx: &Ctx,
    memo: &mut BTreeMap<String, BTreeMap<String, Agg>>,
    visiting: &mut BTreeSet<String>,
    loops: &mut usize,
) -> BTreeMap<String, Agg> {
    if let Some(m) = memo.get(net) {
        return m.clone();
    }
    // a source: primary input, undriven net, or a sequential (flop Q) output
    let driver = ctx.driver.get(net);
    let is_source =
        ctx.inputs.contains(net) || driver.is_none() || driver.map(|d| d.2).unwrap_or(false);
    if is_source {
        let mut m = BTreeMap::new();
        m.insert(
            net.to_string(),
            Agg {
                parity: EVEN,
                min_d: 0.0,
                max_d: 0.0,
                count: 1,
            },
        );
        memo.insert(net.to_string(), m.clone());
        return m;
    }
    if !visiting.insert(net.to_string()) {
        *loops += 1; // comb cycle — break it
        return BTreeMap::new();
    }
    let &(idx, out_pin, _) = driver.unwrap();
    let inst = &ctx.nl.insts[idx];
    let mut res: BTreeMap<String, Agg> = BTreeMap::new();
    if let Some(cell) = ctx.lib.cells.get(&inst.cell) {
        if let Some(pin) = cell.pins.get(out_pin) {
            for arc in &pin.arcs {
                let Some(fanin) = net_of(inst, &arc.related_pin) else {
                    continue;
                };
                let d = arc_delay(arc);
                let sub = compute(fanin, ctx, memo, visiting, loops);
                for (start, a) in sub {
                    let agg = Agg {
                        parity: apply_sense(a.parity, &arc.sense),
                        min_d: a.min_d + d,
                        max_d: a.max_d + d,
                        count: a.count,
                    };
                    merge(&mut res, &start, agg);
                }
            }
        }
    }
    visiting.remove(net);
    memo.insert(net.to_string(), res.clone());
    res
}

pub fn analyze(nl: &Netlist, lib: &Lib) -> Result<GlitchReport, String> {
    if lib.cells.is_empty() {
        return Err("no cells in the Liberty".into());
    }
    // driver map: net -> the output pin that drives it
    let mut driver: BTreeMap<&str, (usize, &str, bool)> = BTreeMap::new();
    for (i, inst) in nl.insts.iter().enumerate() {
        let Some(cell) = lib.cells.get(&inst.cell) else {
            continue;
        };
        for (pin, net) in &inst.conns {
            if cell.pins.get(pin).map(|p| p.direction) == Some(Dir::Out) {
                driver.insert(net.as_str(), (i, pin.as_str(), cell.is_seq));
            }
        }
    }
    let ctx = Ctx {
        nl,
        lib,
        driver,
        inputs: nl.inputs.iter().map(String::as_str).collect(),
    };

    // endpoints: primary outputs + flop data pins (pins carrying a setup constraint)
    let mut endpoints: Vec<String> = nl.outputs.clone();
    for inst in &nl.insts {
        let Some(cell) = lib.cells.get(&inst.cell) else {
            continue;
        };
        if !cell.is_seq {
            continue;
        }
        for (pin, p) in &cell.pins {
            if !p.setup.is_empty() {
                if let Some(n) = net_of(inst, pin) {
                    endpoints.push(n.to_string());
                }
            }
        }
    }
    endpoints.sort();
    endpoints.dedup();

    let mut memo = BTreeMap::new();
    let mut loops = 0usize;
    let mut hazards = Vec::new();
    for ep in &endpoints {
        let mut visiting = BTreeSet::new();
        let agg = compute(ep, &ctx, &mut memo, &mut visiting, &mut loops);
        for (source, a) in agg {
            if a.count < 2 || source == *ep {
                continue; // not reconvergent (or a trivial self-endpoint)
            }
            let window = (a.max_d - a.min_d).max(0.0);
            let static_hazard = a.parity == (EVEN | ODD);
            if static_hazard {
                hazards.push(Hazard {
                    endpoint: ep.clone(),
                    source,
                    kind: Kind::Static,
                    paths: a.count,
                    window_ns: window,
                });
            } else if window > EPS {
                hazards.push(Hazard {
                    endpoint: ep.clone(),
                    source,
                    kind: Kind::Dynamic,
                    paths: a.count,
                    window_ns: window,
                });
            }
            // else: balanced reconvergence (same parity, same delay) — no glitch.
        }
    }
    // worst (widest window, then static) first
    hazards.sort_by(|x, y| {
        y.window_ns
            .partial_cmp(&x.window_ns)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then((x.kind == Kind::Dynamic).cmp(&(y.kind == Kind::Dynamic)))
            .then(x.endpoint.cmp(&y.endpoint))
            .then(x.source.cmp(&y.source))
    });
    Ok(GlitchReport {
        hazards,
        comb_loops: loops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Lib {
        Lib::load("examples/cells.lib").expect("cells.lib")
    }

    #[test]
    fn static_hazard_on_reconvergent_inverted_path() {
        // f = a*b + a'*c : `a` reconverges at f through a non-inverting path (a*b)
        // and an inverting one (a'*c) — the classic static-1 hazard.
        let nl = crate::netlist::parse(
            "module t(a,b,c,f);\ninput a,b,c; output f;\nwire an,t1,t2;\n\
             INV i1(.A(a),.Y(an));\nAND2 g1(.A(a),.B(b),.Z(t1));\n\
             AND2 g2(.A(an),.B(c),.Z(t2));\nOR2 g3(.A(t1),.B(t2),.Z(f));\nendmodule\n",
        )
        .unwrap();
        let r = analyze(&nl, &lib()).unwrap();
        let h: Vec<_> = r
            .hazards
            .iter()
            .filter(|h| h.source == "a" && h.endpoint == "f")
            .collect();
        assert_eq!(h.len(), 1, "exactly one a->f hazard: {:?}", r.hazards);
        assert_eq!(h[0].kind, Kind::Static);
        assert_eq!(h[0].paths, 2);
        // b and c each reach f by a single path -> not hazards
        assert!(!r.hazards.iter().any(|h| h.source == "b" || h.source == "c"));
    }

    #[test]
    fn dynamic_hazard_on_unbalanced_same_parity_reconvergence() {
        // y = x AND buf(x): both paths non-inverting, but the buffered path is slower
        // -> a transition hazard with a non-zero window.
        let nl = crate::netlist::parse(
            "module t(x,y);\ninput x; output y;\nwire xb;\n\
             BUF b1(.A(x),.Y(xb));\nAND2 g(.A(x),.B(xb),.Z(y));\nendmodule\n",
        )
        .unwrap();
        let r = analyze(&nl, &lib()).unwrap();
        let h: Vec<_> = r
            .hazards
            .iter()
            .filter(|h| h.source == "x" && h.endpoint == "y")
            .collect();
        assert_eq!(h.len(), 1, "{:?}", r.hazards);
        assert_eq!(h[0].kind, Kind::Dynamic);
        assert!(
            h[0].window_ns > 0.0,
            "buffered path is slower -> non-zero window"
        );
    }

    #[test]
    fn no_reconvergence_no_hazard() {
        // a plain chain: in -> INV -> BUF -> out. No source reconverges.
        let nl = crate::netlist::parse(
            "module t(i,o);\ninput i; output o;\nwire n;\nINV i1(.A(i),.Y(n));\nBUF b1(.A(n),.Y(o));\nendmodule\n",
        )
        .unwrap();
        let r = analyze(&nl, &lib()).unwrap();
        assert!(r.hazards.is_empty(), "no reconvergence: {:?}", r.hazards);
    }

    #[test]
    fn hazard_into_a_flop_data_pin_is_found() {
        // same static-hazard cone, but captured by a DFF.D instead of a primary output.
        let nl = crate::netlist::parse(
            "module t(a,b,c,clk,q);\ninput a,b,c,clk; output q;\nwire an,t1,t2,d;\n\
             INV i1(.A(a),.Y(an));\nAND2 g1(.A(a),.B(b),.Z(t1));\n\
             AND2 g2(.A(an),.B(c),.Z(t2));\nOR2 g3(.A(t1),.B(t2),.Z(d));\n\
             DFF r(.CK(clk),.D(d),.Q(q));\nendmodule\n",
        )
        .unwrap();
        let r = analyze(&nl, &lib()).unwrap();
        assert!(
            r.hazards
                .iter()
                .any(|h| h.source == "a" && h.endpoint == "d" && h.kind == Kind::Static),
            "static hazard at the flop data pin d: {:?}",
            r.hazards
        );
    }
}
