//! `cargo run -p dryer-machine-resolver --example resolve <machine.yaml> [packages-dir]`
//! Exit codes: 0 resolved · 1 diagnostics with errors · 2 usage/IO.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(machine) = args.next() else {
        eprintln!("usage: resolve <machine.yaml> [packages-dir]");
        return ExitCode::from(2);
    };
    let packages = args.next().unwrap_or_else(|| "packages".to_string());

    let source = match std::fs::read_to_string(&machine) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {machine}: {e}");
            return ExitCode::from(2);
        }
    };
    let registry = dryer_package_model::LocalRegistry::load(std::path::Path::new(&packages));
    let outcome = dryer_machine_resolver::resolve_source(&source, &registry);

    for d in registry.diagnostics.iter().chain(&outcome.diagnostics) {
        eprintln!("{d}");
    }
    match outcome.resolved {
        Some(graph) if outcome.is_ok() => {
            println!("resolved: {machine}");
            for (component, assignments) in &graph.assignments {
                for a in assignments {
                    println!("  {component} --{}--> {}", a.via, a.resource.0);
                }
            }
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(1),
    }
}
