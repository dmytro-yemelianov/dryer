use dryer_machine_lock::{lock, CONTROLLER_BUILD_SCHEMA, CONTROLLER_SAFETY_SCHEMA, LOCK_VERSION};
use dryer_package_model::{LocalRegistry, REGISTRY_SOURCE_SCHEMA};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const EXAMPLES: &[&str] = &["minimal-cartesian", "corexy", "multi-mcu-toolhead"];

#[test]
fn example_locks_are_drift_gated() {
    let root = repo_root();
    for example in EXAMPLES {
        let dir = root.join("examples").join(example);
        let source = std::fs::read_to_string(dir.join("machine.yaml")).unwrap();
        let registry = LocalRegistry::load(&root.join("packages"));
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap_or_else(|| panic!("{example}: does not resolve"));
        let actual = lock(&source, &registry, &resolved).unwrap().to_yaml();
        let path = dir.join("machine.lock");
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!("{example}: missing golden {}\n\n{actual}", path.display())
        });
        assert_eq!(
            actual, expected,
            "{example}: machine.lock drifted; replace the golden deliberately with:\n{actual}"
        );
    }
}

#[test]
fn normative_schema_tracks_the_current_lock_contract() {
    let root = repo_root();
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("schemas/lockfile.schema.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(schema["properties"]["lock_version"]["const"], LOCK_VERSION);
    let lock_required = schema["required"].as_array().unwrap();
    assert!(lock_required.iter().any(|field| field == "registry_source"));
    assert_eq!(
        schema["$defs"]["registry_source"]["properties"]["schema"]["const"],
        REGISTRY_SOURCE_SCHEMA
    );
    assert_eq!(
        schema["$defs"]["registry_source"]["properties"]["descriptor_hash"]["$ref"],
        "#/$defs/sha256"
    );
    assert_eq!(
        schema["$defs"]["controller_safety"]["properties"]["schema"]["const"],
        CONTROLLER_SAFETY_SCHEMA
    );
    let controller_required = schema["$defs"]["locked_controller"]["required"]
        .as_array()
        .unwrap();
    assert!(controller_required.iter().any(|field| field == "safety"));
    assert!(controller_required.iter().any(|field| field == "build"));
    assert_eq!(
        schema["$defs"]["safe_state"]["properties"]["heartbeat_timeout_us"]["minimum"],
        1
    );
    assert_eq!(
        schema["$defs"]["safe_state"]["properties"]["state"]["enum"],
        serde_json::json!(["off", "disabled"])
    );
    assert_eq!(
        schema["$defs"]["controller_build"]["properties"]["schema"]["const"],
        CONTROLLER_BUILD_SCHEMA
    );
    assert_eq!(
        schema["$defs"]["controller_build"]["properties"]["flash_bytes"]["minimum"],
        1
    );
    assert_eq!(
        schema["$defs"]["controller_build"]["properties"]["protocol_version"]["pattern"],
        "^[a-z.]+/v[1-9][0-9]*$"
    );
    assert_eq!(
        schema["$defs"]["controller_build"]["properties"]["features"]["items"]["pattern"],
        "^[a-z][a-z0-9_-]*$"
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
            .expect("v5 schema retains controller safety");
        assert_eq!(safety["schema"], CONTROLLER_SAFETY_SCHEMA);
        for state in safety["states"].as_array().unwrap() {
            if let Some(timeout) = state.get("heartbeat_timeout_us") {
                assert!(timeout.as_u64().is_some_and(|timeout| timeout >= 1));
            }
        }
        let build = controller
            .get("build")
            .expect("v5 schema requires controller build inputs");
        assert_eq!(build["schema"], CONTROLLER_BUILD_SCHEMA);
        assert!(build["flash_bytes"].as_u64().is_some_and(|bytes| bytes > 0));
        assert!(build["ram_bytes"].as_u64().is_some_and(|bytes| bytes > 0));
    }
}
