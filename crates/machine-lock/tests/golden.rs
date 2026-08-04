use dryer_machine_lock::{lock, CONTROLLER_SAFETY_SCHEMA, LOCK_VERSION};
use dryer_package_model::LocalRegistry;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn minimal_cartesian_lock_is_drift_gated() {
    let root = repo_root();
    let source =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
        .resolved
        .unwrap();
    let actual = lock(&source, &registry, &resolved).unwrap().to_yaml();
    let path = root.join("examples/minimal-cartesian/machine.lock");
    let expected = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        actual, expected,
        "machine.lock drifted; replace the golden deliberately with:\n{actual}"
    );
}

#[test]
fn normative_schema_tracks_the_current_lock_contract() {
    let root = repo_root();
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("schemas/lockfile.schema.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(schema["properties"]["lock_version"]["const"], LOCK_VERSION);
    assert_eq!(
        schema["$defs"]["controller_safety"]["properties"]["schema"]["const"],
        CONTROLLER_SAFETY_SCHEMA
    );
    let controller_required = schema["$defs"]["locked_controller"]["required"]
        .as_array()
        .unwrap();
    assert!(controller_required.iter().any(|field| field == "safety"));
    assert_eq!(
        schema["$defs"]["safe_state"]["properties"]["heartbeat_timeout_us"]["minimum"],
        1
    );
    assert_eq!(
        schema["$defs"]["safe_state"]["properties"]["state"]["enum"],
        serde_json::json!(["off", "disabled"])
    );

    let source =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
        .resolved
        .unwrap();
    let value = serde_json::to_value(lock(&source, &registry, &resolved).unwrap()).unwrap();
    for controller in value["controllers"].as_object().unwrap().values() {
        let safety = controller
            .get("safety")
            .expect("v3 schema requires controller safety");
        assert_eq!(safety["schema"], CONTROLLER_SAFETY_SCHEMA);
        for state in safety["states"].as_array().unwrap() {
            if let Some(timeout) = state.get("heartbeat_timeout_us") {
                assert!(timeout.as_u64().is_some_and(|timeout| timeout >= 1));
            }
        }
    }
}
