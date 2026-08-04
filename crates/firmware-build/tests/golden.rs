use dryer_firmware_build::compile_controller;
use dryer_machine_lock::lock;
use dryer_package_model::LocalRegistry;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn minimal_cartesian_controller_safety_is_drift_gated() {
    let root = repo_root();
    let source =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
        .resolved
        .unwrap();
    let lock = lock(&source, &registry, &resolved).unwrap();
    let artifact = compile_controller(&lock, "mainboard").unwrap();
    let actual = artifact.to_pretty_json();
    let path = root.join("examples/minimal-cartesian/controller-safety.golden.json");
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {}\n\n{actual}", path.display()));
    assert_eq!(
        actual, expected,
        "controller safety artifact drifted; inspect and update the golden deliberately"
    );
}
