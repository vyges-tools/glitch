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
  --describe            print a machine-readable JSON description of the command
  -h, --help · -V, --version
";

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Emit the vyges-events causal trail — one event per glitch hazard + a completion
/// event. Written to stderr (the default sink) so it never mixes with the report
/// (stdout / -o). `code` (GLITCH-<KIND>) is the clustering key; `objects` (the
/// reconverging source net and the endpoint net) are the cross-stage co-ref keys.
fn emit_glitch_events(r: &GlitchReport) {
    use vyges_events::{Event, Severity};
    for h in &r.hazards {
        vyges_events::emit(
            &Event::new(
                "vyges-glitch",
                Severity::Warn,
                format!(
                    "{} hazard: {} → {} ({} path(s), window {:.4} ns)",
                    h.kind.tag(),
                    h.source,
                    h.endpoint,
                    h.paths,
                    h.window_ns
                ),
            )
            .with_code(format!("GLITCH-{}", h.kind.tag().to_uppercase()))
            .with_objects(vec![
                format!("net:{}", h.source),
                format!("net:{}", h.endpoint),
            ]),
        );
    }
    vyges_events::emit(
        &Event::new(
            "vyges-glitch",
            if r.hazards.is_empty() {
                Severity::Info
            } else {
                Severity::Warn
            },
            format!("glitch analysis complete: {} hazard(s)", r.hazards.len()),
        )
        .with_code("GLITCH-DONE"),
    );
}

fn render_text(r: &GlitchReport) -> String {
    let mut s = String::new();
    let stat = r
        .hazards
        .iter()
        .filter(|h| h.kind == glitch::Kind::Static)
        .count();
    let dyn_ = r.hazards.len() - stat;
    s.push_str(&format!(
        "vyges-glitch — {} hazard(s): {} static, {} dynamic\n",
        r.hazards.len(),
        stat,
        dyn_
    ));
    if r.comb_loops > 0 {
        s.push_str(&format!(
            "  note: {} combinational loop edge(s) broken\n",
            r.comb_loops
        ));
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
    // The single verdict, tri-state on purpose.
    //
    // Combinational loops are broken rather than followed, so where any were cut the
    // aggregation never traversed those edges and the search for hazards was incomplete
    // there. Finding none under those conditions is not evidence that none exist — so the
    // verdict is `null`, not a pass. Hazards that WERE found still fail regardless: an
    // incomplete search does not cast doubt on what it did find.
    let glitch_free = if !r.hazards.is_empty() {
        "false".to_string()
    } else if r.comb_loops > 0 {
        "null".to_string()
    } else {
        "true".to_string()
    };
    s.push_str(&format!("  \"glitch_free\": {glitch_free},\n"));
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

/// Add `"report_path"` to a `--json` payload so the result says where its report landed.
///
/// String surgery rather than a JSON round-trip because this crate is std-only. Inserting
/// after the opening brace keeps every existing field untouched; an empty object gets no
/// trailing comma.
fn with_report_path(json: &str, path: Option<&str>) -> String {
    let (Some(p), Some(rest)) = (path, json.trim_start().strip_prefix('{')) else {
        return json.to_string();
    };
    let esc = p.replace('\\', "\\\\").replace('"', "\\\"");
    let sep = if rest.trim_start().starts_with('}') {
        ""
    } else {
        ","
    };
    format!("{{\"report_path\": \"{esc}\"{sep}{rest}")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--describe") {
        // Machine-readable description of `check` for tooling that drives it.
        const DESCRIBE: &str = r#"{
  "name": "glitch",
  "summary": "static glitch / hazard analysis (reconvergent fanout)",
  "maturity": "structured",
  "provenance_limitations": [
      "input_hash covers the argument vector, not the content of the netlist or Liberty it names.",
      "Liberty `include` files are not enumerated."
  ],
  "invocation": {
    "args_template": ["check", "{netlist}", "--lib", "{lib}"],
    "optional": [
      { "arg": "out", "flag": "-o" }
    ],
    "emits_json": true
  },
  "inputs": {
    "type": "object",
    "required": ["netlist", "lib"],
    "properties": {
      "netlist": { "type": "string", "description": "Netlist file to analyze for reconvergent-fanout hazards" },
      "lib": { "type": "string", "description": "Liberty file (cell parity via timing_sense + delays), required" },
      "out": { "type": "string", "description": "Write the report to this file instead of stdout" }
    }
  },
  "artifacts": [ { "role": "hazard_report", "field": "report_path" } ],
  "assertion": {
    "id": "glitch-free",
    "field": "glitch_free",
    "pass_when": { "is_true": true }
  }
}
"#;
        print!("{DESCRIBE}");
        return;
    }
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
    emit_glitch_events(&report); // vyges-events causal trail on stderr; report goes to stdout / -o
    let json = args.iter().any(|a| a == "--json");
    let text = if json {
        with_report_path(&render_json(&report), opt(&args, "-o").as_deref())
    } else {
        render_text(&report)
    };
    match opt(&args, "-o") {
        Some(p) => {
            if let Err(e) = std::fs::write(&p, &text) {
                die(&format!("{p}: {e}"));
            }
            eprintln!("wrote {p}");
            // `-o` writes the report; the machine payload still goes to stdout, so asking
            // for the file does not cost the caller the parsed result.
            if json {
                print!("{text}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use vyges_glitch::glitch::{Hazard, Kind};

    fn hazard() -> Hazard {
        Hazard {
            endpoint: "y".into(),
            source: "a".into(),
            kind: Kind::Static,
            paths: 2,
            window_ns: 0.0,
        }
    }

    /// `glitch_free` distinguishes "searched everywhere, found nothing" from "could not
    /// search everywhere, found nothing" — combinational loops are broken rather than
    /// followed, so the second is not evidence of a clean design.
    #[test]
    fn glitch_free_is_tri_state() {
        let clean = GlitchReport {
            hazards: vec![],
            comb_loops: 0,
        };
        assert!(
            render_json(&clean).contains("\"glitch_free\": true"),
            "complete search, no hazards"
        );

        let cut = GlitchReport {
            hazards: vec![],
            comb_loops: 3,
        };
        assert!(
            render_json(&cut).contains("\"glitch_free\": null"),
            "loops were broken, so finding no hazards proves nothing"
        );

        let found = GlitchReport {
            hazards: vec![hazard()],
            comb_loops: 0,
        };
        assert!(
            render_json(&found).contains("\"glitch_free\": false"),
            "hazards found"
        );

        // An incomplete search does not cast doubt on what it DID find.
        let both = GlitchReport {
            hazards: vec![hazard()],
            comb_loops: 5,
        };
        assert!(
            render_json(&both).contains("\"glitch_free\": false"),
            "hazards found still fail even when loops were broken"
        );
    }
}
