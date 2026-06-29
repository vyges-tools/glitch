//! vyges-glitch CLI.
//!
//!   vyges-glitch check NETLIST --lib L.lib [-o OUT] [--json] [--fail-on-violation]
//!
//! Reports reconvergent-fanout hazards (static + dynamic). Exit codes:
//! 0 clean · 1 runtime error · 2 usage · 3 hazard(s) found (only with
//! --fail-on-violation).

use std::process::exit;

use vyges_glitch::glitch::{self, GlitchReport};
use vyges_glitch::{liberty::Lib, netlist};

const USAGE: &str = "\
vyges-glitch — static glitch / hazard analysis (reconvergent fanout)

usage:
  vyges-glitch check NETLIST --lib L.lib [-o OUT] [--json] [--fail-on-violation]

flags:
  --lib FILE            Liberty (cell parity via timing_sense + delays) — required
  -o FILE               write the report to FILE (default: stdout)
  --json                machine-readable JSON instead of text
  --fail-on-violation   exit 3 if any hazard is found (CI gate)
  -h, --help · -V, --version
";

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn render_text(r: &GlitchReport) -> String {
    let mut s = String::new();
    let stat = r.hazards.iter().filter(|h| h.kind == glitch::Kind::Static).count();
    let dyn_ = r.hazards.len() - stat;
    s.push_str(&format!(
        "vyges-glitch — {} hazard(s): {} static, {} dynamic\n",
        r.hazards.len(),
        stat,
        dyn_
    ));
    if r.comb_loops > 0 {
        s.push_str(&format!("  note: {} combinational loop edge(s) broken\n", r.comb_loops));
    }
    if r.hazards.is_empty() {
        s.push_str("  no reconvergent-fanout hazards.\n");
        return s;
    }
    for h in r.hazards.iter().take(200) {
        s.push_str(&format!(
            "  {:7} {} → {}   {} path(s), window {:.4} ns\n",
            h.kind.tag(),
            h.source,
            h.endpoint,
            h.paths,
            h.window_ns
        ));
    }
    if r.hazards.len() > 200 {
        s.push_str(&format!("  … {} more\n", r.hazards.len() - 200));
    }
    s
}

fn render_json(r: &GlitchReport) -> String {
    let mut s = String::from("{\n");
    s.push_str(&format!("  \"hazards\": {},\n", r.hazards.len()));
    s.push_str(&format!("  \"comb_loops\": {},\n", r.comb_loops));
    s.push_str("  \"items\": [\n");
    for (i, h) in r.hazards.iter().enumerate() {
        let comma = if i + 1 < r.hazards.len() { "," } else { "" };
        s.push_str(&format!(
            "    {{\"kind\": \"{}\", \"source\": \"{}\", \"endpoint\": \"{}\", \"paths\": {}, \"window_ns\": {:.6}}}{}\n",
            h.kind.tag(),
            h.source,
            h.endpoint,
            h.paths,
            h.window_ns,
            comma
        ));
    }
    s.push_str("  ]\n}\n");
    s
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") || args.is_empty() {
        print!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("vyges-glitch {}", vyges_glitch::VERSION);
        return;
    }
    if args[0] != "check" {
        eprintln!("error: unknown command {:?}\n{USAGE}", args[0]);
        exit(2);
    }
    let Some(net) = args.get(1).filter(|a| !a.starts_with('-')) else {
        eprintln!("error: `check` needs a NETLIST path\n{USAGE}");
        exit(2);
    };
    let Some(libp) = opt(&args, "--lib") else {
        eprintln!("error: `check` needs --lib\n{USAGE}");
        exit(2);
    };

    let nl = netlist::load(net).unwrap_or_else(|e| die(&format!("{net}: {e}")));
    let lib = Lib::load(&libp).unwrap_or_else(|e| die(&format!("{libp}: {e}")));

    let report = glitch::analyze(&nl, &lib).unwrap_or_else(|e| die(&e));
    let json = args.iter().any(|a| a == "--json");
    let text = if json { render_json(&report) } else { render_text(&report) };
    match opt(&args, "-o") {
        Some(p) => {
            if let Err(e) = std::fs::write(&p, &text) {
                die(&format!("{p}: {e}"));
            }
        }
        None => print!("{text}"),
    }
    if args.iter().any(|a| a == "--fail-on-violation") && !report.hazards.is_empty() {
        exit(3);
    }
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1);
}
