//! `cargo run -p dryer-machine-parser --example validate <machine.yaml>`
//! Exit codes: 0 valid · 1 diagnostics with errors · 2 usage/IO.

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: validate <machine.yaml>");
        return ExitCode::from(2);
    };
    let outcome = dryer_machine_parser::parse_file(std::path::Path::new(&path));
    for d in &outcome.diagnostics {
        eprintln!("{d}");
    }
    if outcome.doc.is_none() {
        return ExitCode::from(2);
    }
    if outcome.is_valid() {
        println!(
            "ok: {path} is a valid {} manifest",
            dryer_machine_schema::API_VERSION
        );
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
