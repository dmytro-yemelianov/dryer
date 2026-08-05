//! `machine.lock` (spec §12, §29 step 7): a generated, canonical, hashed
//! capture of one successful resolution.
//!
//! v0.5 field scope, stated honestly: exact package versions + portable
//! full-content digests (§6.6), manifest hashes, the machine-source hash,
//! resolver and registry-source identity, the pinned safety profile, and
//! per-controller resolved resources plus compiled controller safety and
//! firmware build inputs. Reproducible output hashes are derived from this
//! lock in build-plan v2 rather than inserted here, avoiding a lock/output
//! hash cycle.
//!
//! Canonical form: JSON with every map a `BTreeMap`, so byte-identical
//! lockfiles for identical inputs. The on-disk file is YAML for humans;
//! `canonical_bytes`/`lock_hash` always use the JSON form.

mod generate;
mod model;
mod validate;
mod wire;

pub use generate::lock;
pub use model::{
    LockedBuildConfig, LockedController, LockedPackage, LockedSafeState, LockedSafetyConfig,
    Lockfile, CONTROLLER_BUILD_SCHEMA, CONTROLLER_SAFETY_SCHEMA, LOCK_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use dryer_package_model::LocalRegistry;
    use std::path::Path;

    fn setup() -> (String, LocalRegistry) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        (
            std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap(),
            LocalRegistry::load(&root.join("packages")),
        )
    }

    #[test]
    fn locking_the_fixture_is_deterministic_and_round_trips() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .expect("fixture resolves");
        let a = lock(&source, &registry, &resolved).unwrap();
        let b = lock(&source, &registry, &resolved).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.lock_hash(), b.lock_hash());
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());

        let back = Lockfile::from_yaml(&a.to_yaml()).unwrap();
        assert_eq!(back, a, "YAML round-trip preserves canonical identity");
        assert_eq!(back.lock_hash(), a.lock_hash());
    }

    #[test]
    fn the_lock_captures_assignments_under_the_right_controller() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let l = lock(&source, &registry, &resolved).unwrap();
        let registry_source = l.registry_source.as_ref().expect("v5 registry source");
        assert_eq!(
            registry_source.schema,
            dryer_package_model::REGISTRY_SOURCE_SCHEMA
        );
        assert_eq!(registry_source.id, "dryer-official");
        assert_eq!(
            registry_source.uri,
            "git+https://github.com/dmytro-yemelianov/dryer.git?subdir=packages"
        );
        assert!(registry_source.descriptor_hash.starts_with("sha256:"));
        registry_source.validate().unwrap();
        let main = &l.controllers["mainboard"];
        assert_eq!(main.resolved_resources["x_driver/connected_to"], "motor0");
        assert_eq!(main.resolved_resources["hotend_heater/output"], "heater0");
        let safety = main.safety.as_ref().expect("v3 safety config");
        assert_eq!(safety.schema, CONTROLLER_SAFETY_SCHEMA);
        assert_eq!(safety.states.len(), 2, "{:?}", safety.states);
        let heater = safety
            .states
            .iter()
            .find(|state| state.component == "hotend_heater")
            .unwrap();
        assert_eq!(heater.resource, "heater0");
        assert_eq!(heater.sensor.as_deref(), Some("thermistor0"));
        assert_eq!(heater.heartbeat_timeout_us, Some(500_000));
        let build = main.build.as_ref().expect("v4 build config");
        assert_eq!(build.schema, CONTROLLER_BUILD_SCHEMA);
        assert_eq!(build.board, "boards/example-mainboard@1.0.0");
        assert_eq!(build.chip, "chips/generic-mcu@1.5.0");
        assert_eq!(build.target_triple, "thumbv7em-none-eabihf");
        assert_eq!(build.flash_bytes, 524_288);
        assert_eq!(build.bootloader_offset_bytes, 16_384);
        assert_eq!(build.native_drivers, ["devices/tmc2209@2.1.0"]);
        // the full closure: 3 explicit pins + the transitive chip
        // dependency + the implicit safety profile
        assert_eq!(l.packages.len(), 5, "{:?}", l.packages);
        assert!(l.packages.iter().any(|p| p.id == "chips/generic-mcu@1.5.0"));
        // the template-expanded component locks like any other
        assert_eq!(
            l.controllers["mainboard"].resolved_resources["y_driver/requires.connector"],
            "motor1"
        );
        assert!(l
            .packages
            .iter()
            .all(|p| p.manifest_hash.starts_with("sha256:")));
        assert!(l
            .packages
            .iter()
            .all(|p| p.content_hash.starts_with("sha256:")));
        assert!(l.machine_hash.starts_with("sha256:"));
        assert_eq!(l.safety_profile.id, "safety-profiles/desktop-fdm@1.0.0");
        assert!(l.safety_profile.manifest_hash.starts_with("sha256:"));
        assert!(l.safety_profile.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn reformatting_the_manifest_changes_the_machine_hash() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let a = lock(&source, &registry, &resolved).unwrap();
        let reformatted = format!("{source}\n# trailing comment\n");
        let b = lock(&reformatted, &registry, &resolved).unwrap();
        assert_ne!(a.machine_hash, b.machine_hash);
    }

    #[test]
    fn legacy_v1_locks_without_content_hashes_still_parse() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let mut legacy = lock(&source, &registry, &resolved).unwrap();
        legacy.lock_version = 1;
        legacy.registry_source = None;
        for package in &mut legacy.packages {
            package.content_hash.clear();
        }
        legacy.safety_profile.content_hash.clear();
        for controller in legacy.controllers.values_mut() {
            controller.safety = None;
            controller.build = None;
        }
        let parsed = Lockfile::from_yaml(&legacy.to_yaml()).unwrap();
        assert_eq!(parsed.lock_version, 1);
        assert!(parsed
            .packages
            .iter()
            .all(|package| package.content_hash.is_empty()));
        assert!(parsed.safety_profile.content_hash.is_empty());
        assert!(parsed.registry_source.is_none());
        assert!(parsed
            .controllers
            .values()
            .all(|controller| controller.safety.is_none()));
    }

    #[test]
    fn legacy_v2_locks_without_compiled_safety_still_parse() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let mut legacy = lock(&source, &registry, &resolved).unwrap();
        legacy.lock_version = 2;
        legacy.registry_source = None;
        for controller in legacy.controllers.values_mut() {
            controller.safety = None;
            controller.build = None;
        }
        let parsed = Lockfile::from_yaml(&legacy.to_yaml()).unwrap();
        assert_eq!(parsed.lock_version, 2);
        assert!(parsed.registry_source.is_none());
        assert!(parsed
            .controllers
            .values()
            .all(|controller| controller.safety.is_none()));
    }

    #[test]
    fn legacy_v3_locks_without_compiled_build_inputs_still_parse() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let mut legacy = lock(&source, &registry, &resolved).unwrap();
        legacy.lock_version = 3;
        legacy.registry_source = None;
        for controller in legacy.controllers.values_mut() {
            controller.build = None;
        }
        let parsed = Lockfile::from_yaml(&legacy.to_yaml()).unwrap();
        assert_eq!(parsed.lock_version, 3);
        assert!(parsed.registry_source.is_none());
        assert!(parsed
            .controllers
            .values()
            .all(|controller| controller.safety.is_some() && controller.build.is_none()));
    }

    #[test]
    fn legacy_v4_locks_without_registry_identity_still_parse() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let mut legacy = lock(&source, &registry, &resolved).unwrap();
        legacy.lock_version = 4;
        legacy.registry_source = None;

        let parsed = Lockfile::from_yaml(&legacy.to_yaml()).unwrap();
        assert_eq!(parsed.lock_version, 4);
        assert!(parsed.registry_source.is_none());
        assert!(parsed
            .controllers
            .values()
            .all(|controller| { controller.safety.is_some() && controller.build.is_some() }));
    }

    #[test]
    fn v2_locks_require_every_content_hash_at_parse_and_serialize_boundaries() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let valid = lock(&source, &registry, &resolved).unwrap();
        let yaml = valid.to_yaml();
        let mut removed = false;
        let missing_package_hash = yaml
            .lines()
            .filter(|line| {
                if !removed && line.trim_start().starts_with("content_hash:") {
                    removed = true;
                    false
                } else {
                    true
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let error = Lockfile::from_yaml(&missing_package_hash).unwrap_err();
        assert!(error.to_string().contains("has no content_hash"), "{error}");

        let mut missing_safety_hash = valid;
        missing_safety_hash.safety_profile.content_hash.clear();
        let error = serde_yaml::to_string(&missing_safety_hash).unwrap_err();
        assert!(error.to_string().contains("has no content_hash"), "{error}");
    }

    #[test]
    fn v3_locks_require_compiled_controller_safety_at_both_boundaries() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let valid = lock(&source, &registry, &resolved).unwrap();
        let mut missing = valid.clone();
        missing.controllers.get_mut("mainboard").unwrap().safety = None;
        let error = serde_yaml::to_string(&missing).unwrap_err();
        assert!(
            error.to_string().contains("compiled safety configuration"),
            "{error}"
        );

        let mut malformed = valid.clone();
        malformed
            .controllers
            .get_mut("mainboard")
            .unwrap()
            .safety
            .as_mut()
            .unwrap()
            .states[0]
            .component = " padded".to_string();
        for error in [
            malformed.try_canonical_bytes().unwrap_err().to_string(),
            malformed.try_lock_hash().unwrap_err().to_string(),
            malformed.try_to_yaml().unwrap_err().to_string(),
        ] {
            assert!(error.contains("empty or padded"), "{error}");
        }

        let yaml = valid.to_yaml();
        let safety_start = yaml.find("    safety:\n").unwrap();
        let truncated = yaml[..safety_start].to_string();
        let error = Lockfile::from_yaml(&truncated).unwrap_err();
        assert!(
            error.to_string().contains("compiled safety configuration"),
            "{error}"
        );
    }

    #[test]
    fn v3_locks_reject_multiple_actions_for_one_physical_resource() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let mut duplicate = lock(&source, &registry, &resolved).unwrap();
        let safety = duplicate
            .controllers
            .get_mut("mainboard")
            .unwrap()
            .safety
            .as_mut()
            .unwrap();
        let mut second_owner = safety
            .states
            .iter()
            .find(|state| state.resource == "motor0")
            .unwrap()
            .clone();
        second_owner.component = "x_driver".to_string();
        second_owner.class = "tmc2209".to_string();
        second_owner.state = dryer_package_model::safety::SafeState::Off;
        safety.states.push(second_owner);

        let error = serde_yaml::to_string(&duplicate).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("repeats safety state for physical resource 'motor0'"),
            "{error}"
        );
    }

    #[test]
    fn v4_locks_require_valid_compiled_build_inputs() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let valid = lock(&source, &registry, &resolved).unwrap();

        let mut missing = valid.clone();
        missing.controllers.get_mut("mainboard").unwrap().build = None;
        let error = serde_yaml::to_string(&missing).unwrap_err();
        assert!(
            error.to_string().contains("compiled build configuration"),
            "{error}"
        );

        let mut invalid_memory = valid;
        invalid_memory
            .controllers
            .get_mut("mainboard")
            .unwrap()
            .build
            .as_mut()
            .unwrap()
            .bootloader_offset_bytes = 524_288;
        let error = serde_yaml::to_string(&invalid_memory).unwrap_err();
        assert!(
            error.to_string().contains("invalid build memory"),
            "{error}"
        );

        let mut invalid_interface = lock(&source, &registry, &resolved).unwrap();
        invalid_interface
            .controllers
            .get_mut("mainboard")
            .unwrap()
            .build
            .as_mut()
            .unwrap()
            .protocol_version = "dryer.control/v01".to_string();
        let error = serde_yaml::to_string(&invalid_interface).unwrap_err();
        assert!(error.to_string().contains("versioned interface"), "{error}");

        let mut invalid_feature = lock(&source, &registry, &resolved).unwrap();
        invalid_feature
            .controllers
            .get_mut("mainboard")
            .unwrap()
            .build
            .as_mut()
            .unwrap()
            .features = vec!["not valid".to_string()];
        let error = serde_yaml::to_string(&invalid_feature).unwrap_err();
        assert!(error.to_string().contains("invalid identifier"), "{error}");
    }

    #[test]
    fn v5_locks_require_a_valid_registry_source_identity() {
        let (source, registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let valid = lock(&source, &registry, &resolved).unwrap();

        let mut missing = valid.clone();
        missing.registry_source = None;
        let error = serde_yaml::to_string(&missing).unwrap_err();
        assert!(
            error.to_string().contains("no registry source identity"),
            "{error}"
        );

        let mut malformed = valid;
        malformed.registry_source.as_mut().unwrap().uri =
            "file:///Users/example/packages".to_string();
        let error = serde_yaml::to_string(&malformed).unwrap_err();
        assert!(
            error.to_string().contains("portable absolute URI"),
            "{error}"
        );
    }

    #[test]
    fn lock_creation_rejects_a_registry_without_portable_identity() {
        let (source, mut registry) = setup();
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        registry.source = None;

        let diagnostics = lock(&source, &registry, &resolved).unwrap_err();
        assert_eq!(diagnostics[0].code, "E1406");
        assert!(diagnostics[0]
            .message
            .contains("no validated portable source descriptor"));
    }

    #[test]
    fn lock_creation_rejects_a_missing_controller_build_plan() {
        let (source, registry) = setup();
        let mut resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        resolved.controller_build_plans.clear();

        let diagnostics = lock(&source, &registry, &resolved).unwrap_err();
        assert_eq!(diagnostics[0].code, "E1405");
        assert!(diagnostics[0]
            .message
            .contains("no compiled build configuration"));
    }

    #[test]
    fn lock_creation_reports_the_actual_safety_resource_controller() {
        let (source, registry) = setup();
        let mut resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        resolved.controller_safety.get_mut("mainboard").unwrap()[0]
            .resource
            .0 = "toolboard.heater0".to_string();

        let diagnostics = lock(&source, &registry, &resolved).unwrap_err();
        assert_eq!(diagnostics[0].code, "E1403");
        assert!(diagnostics[0]
            .message
            .contains("belongs to controller 'toolboard', not 'mainboard'"));
    }
}
