//! `cargo run -p dryer-simulator --example replay -- <expected> <actual>`
//!
//! Compares two JSON-lines traces and emits a structured replay report.
//! Exit codes: 0 match · 1 divergence · 2 usage/IO/parse error.

use dryer_simulator::Trace;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(expected_path), Some(actual_path), None) = (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: replay <expected-trace> <actual-trace>");
        return ExitCode::from(2);
    };

    let expected = match read_trace(&expected_path) {
        Ok(trace) => trace,
        Err(error) => return fail(error),
    };
    let actual = match read_trace(&actual_path) {
        Ok(trace) => trace,
        Err(error) => return fail(error),
    };
    let report = actual.replay_report(&expected);
    print!("{}", report.to_pretty_json());
    if report.matched {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn read_trace(path: &str) -> Result<Trace, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read trace {path}: {error}"))?;
    Trace::from_json_lines(&text).map_err(|error| format!("cannot parse trace {path}: {error}"))
}

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}
