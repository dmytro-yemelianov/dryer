//! `cargo run -p forge-machine-lock --example lock <machine.yaml> [packages-dir] [-o out.lock]`
//! Prints the lockfile YAML to stdout (or writes it with -o).
//! Exit codes: 0 locked · 1 resolution errors · 2 usage/IO.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional = Vec::new();
    let mut out: Option<String> = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "-o" {
            out = it.next();
        } else {
            positional.push(a);
        }
    }
    let Some(machine) = positional.first() else {
        eprintln!("usage: lock <machine.yaml> [packages-dir] [-o out.lock]");
        return ExitCode::from(2);
    };
    let packages = positional
        .get(1)
        .cloned()
        .unwrap_or_else(|| "packages".to_string());

    let source = match std::fs::read_to_string(machine) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {machine}: {e}");
            return ExitCode::from(2);
        }
    };
    let registry = forge_package_model::LocalRegistry::load(std::path::Path::new(&packages));
    let outcome = forge_machine_resolver::resolve_source(&source, &registry);
    for d in registry.diagnostics.iter().chain(&outcome.diagnostics) {
        eprintln!("{d}");
    }
    let ok = outcome.is_ok();
    let Some(resolved) = outcome.resolved.filter(|_| ok) else {
        return ExitCode::from(1);
    };
    let lockfile = match forge_machine_lock::lock(&source, &registry, &resolved) {
        Ok(l) => l,
        Err(diags) => {
            for d in diags {
                eprintln!("{d}");
            }
            return ExitCode::from(1);
        }
    };
    let yaml = lockfile.to_yaml();
    match out {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, &yaml) {
                eprintln!("cannot write {path}: {e}");
                return ExitCode::from(2);
            }
            println!("locked: {path} ({})", lockfile.lock_hash());
        }
        None => print!("{yaml}"),
    }
    ExitCode::SUCCESS
}
