use dryer_firmware_build::{
    build_controller, compile_controller, plan_controller, CONTROLLER_BUILD_PLAN_SCHEMA,
    CONTROLLER_IMAGE_SCHEMA,
};
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

#[test]
fn minimal_cartesian_controller_build_plan_is_drift_gated() {
    let root = repo_root();
    let source =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
        .resolved
        .unwrap();
    let lock = lock(&source, &registry, &resolved).unwrap();
    let plan = plan_controller(&lock, "mainboard").unwrap();
    let actual = plan.to_pretty_json();
    let path = root.join("examples/minimal-cartesian/controller-build-plan.golden.json");
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {}\n\n{actual}", path.display()));
    assert_eq!(
        actual, expected,
        "controller build plan drifted; inspect and update the golden deliberately"
    );
}

#[test]
fn minimal_cartesian_controller_image_is_drift_gated() {
    let root = repo_root();
    let source =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
        .resolved
        .unwrap();
    let lock = lock(&source, &registry, &resolved).unwrap();
    let built = build_controller(&lock, "mainboard").unwrap();
    let path = root.join("examples/minimal-cartesian/controller-image.golden.json");
    let expected = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}\n\n{}",
            path.display(),
            built.image.to_pretty_json()
        )
    });
    assert_eq!(
        built.bytes, expected,
        "controller image drifted; rebuild the golden deliberately"
    );
}

#[test]
fn normative_schemas_track_the_build_outputs() {
    let root = repo_root();
    let build_plan: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("schemas/controller-build-plan.schema.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        build_plan["properties"]["schema"]["const"],
        CONTROLLER_BUILD_PLAN_SCHEMA
    );
    assert_eq!(
        build_plan["properties"]["expected_artifact"]["properties"]["format"]["const"],
        CONTROLLER_IMAGE_SCHEMA
    );
    assert_eq!(
        build_plan["properties"]["expected_artifact"]["properties"]["sha256"]["$ref"],
        "#/$defs/sha256"
    );

    let image: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("schemas/controller-image.schema.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        image["properties"]["schema"]["const"],
        CONTROLLER_IMAGE_SCHEMA
    );
    assert_eq!(image["properties"]["lock_hash"]["$ref"], "#/$defs/sha256");
}
