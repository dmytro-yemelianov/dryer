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

mod model;
mod validate;
mod wire;

pub use model::{
    LockedBuildConfig, LockedController, LockedPackage, LockedSafeState, LockedSafetyConfig,
    Lockfile, CONTROLLER_BUILD_SCHEMA, CONTROLLER_SAFETY_SCHEMA, LOCK_VERSION,
};

use dryer_machine_resolver::ResolvedGraph;
use dryer_machine_schema::Diagnostic;
use dryer_package_model::LocalRegistry;
use std::collections::BTreeMap;
use wire::sha256_hex;

/// Build a lockfile from a successful resolution.
///
/// `source` must be the exact manifest text that was resolved — the hash
/// binds the lock to those bytes, so a reformatted manifest re-locks.
pub fn lock(
    source: &str,
    registry: &LocalRegistry,
    resolved: &ResolvedGraph,
) -> Result<Lockfile, Vec<Diagnostic>> {
    let parsed = dryer_machine_parser::parse_str(source);
    let Some(doc) = parsed.doc else {
        return Err(parsed.diagnostics);
    };
    let registry_source = registry.source.clone().ok_or_else(|| {
        vec![Diagnostic::error(
            "E1406",
            "registry has no validated portable source descriptor",
        )]
    })?;

    // The lock pins the resolver's full closure — explicit pins, implicit
    // roots and transitive dependencies — not merely the manifest's list.
    let mut packages = Vec::new();
    for pkg in &resolved.packages {
        let Ok(r) = dryer_package_model::PackageRef::parse(pkg) else {
            continue; // the resolver produced these; malformed would be its bug
        };
        let Some(found) = registry.find_version(&r.namespace, &r.name, &r.version) else {
            return Err(vec![Diagnostic::error(
                "E1402",
                format!("resolved package '{pkg}' is no longer in the registry"),
            )]);
        };
        let snapshot = found.snapshot().map_err(|e| {
            vec![Diagnostic::error(
                "E1400",
                format!(
                    "cannot snapshot package content for {}: {e}",
                    found.reference
                ),
            )]
        })?;
        let manifest_bytes = snapshot.manifest_bytes().map_err(|e| {
            vec![Diagnostic::error(
                "E1400",
                format!("cannot hash manifest for {}: {e}", found.reference),
            )]
        })?;
        packages.push(LockedPackage {
            id: found.reference.to_string(),
            manifest_hash: sha256_hex(manifest_bytes),
            content_hash: snapshot.content_hash(),
        });
    }
    packages.sort_by(|a, b| a.id.cmp(&b.id));

    let mut controllers: BTreeMap<String, LockedController> = doc
        .controllers
        .iter()
        .map(|(name, c)| {
            (
                name.clone(),
                LockedController {
                    board: c.board.clone(),
                    resolved_resources: BTreeMap::new(),
                    safety: Some(LockedSafetyConfig {
                        schema: CONTROLLER_SAFETY_SCHEMA.to_string(),
                        states: Vec::new(),
                    }),
                    build: None,
                },
            )
        })
        .collect();
    for (component, assignments) in &resolved.assignments {
        for a in assignments {
            let Some((ctrl, port)) = a.resource.0.split_once('.') else {
                continue;
            };
            if let Some(entry) = controllers.get_mut(ctrl) {
                // Keys stay terse: a search allocation's via carries its
                // provenance suffix ("requires.connector (devices/x@1.0)");
                // the lock keeps the mechanism, `explain` keeps the story.
                let via_short = a.via.split_whitespace().next().unwrap_or(&a.via);
                entry
                    .resolved_resources
                    .insert(format!("{component}/{via_short}"), port.to_string());
            }
        }
    }
    for (controller_name, bindings) in &resolved.controller_safety {
        let Some(controller) = controllers.get_mut(controller_name) else {
            return Err(vec![Diagnostic::error(
                "E1403",
                format!(
                    "resolved safety configuration names unknown controller '{controller_name}'"
                ),
            )]);
        };
        let states = &mut controller
            .safety
            .as_mut()
            .expect("v3 lock construction initializes safety")
            .states;
        for binding in bindings {
            let Some((resource_controller, resource)) = binding.resource.0.split_once('.') else {
                return Err(vec![Diagnostic::error(
                    "E1403",
                    format!(
                        "safety resource '{}' is not 'controller.resource'",
                        binding.resource.0
                    ),
                )]);
            };
            if resource_controller != controller_name {
                return Err(vec![Diagnostic::error(
                    "E1403",
                    format!(
                        "safety resource '{}' belongs to controller '{resource_controller}', not '{controller_name}'",
                        binding.resource.0,
                    ),
                )]);
            }
            let sensor = binding
                .sensor
                .as_ref()
                .map(|sensor| {
                    sensor
                        .0
                        .strip_prefix(&format!("{controller_name}."))
                        .map(str::to_string)
                        .ok_or_else(|| {
                            vec![Diagnostic::error(
                                "E1403",
                                format!(
                                    "safety sensor '{}' is not local to controller '{controller_name}'",
                                    sensor.0
                                ),
                            )]
                        })
                })
                .transpose()?;
            states.push(LockedSafeState {
                component: binding.component.clone(),
                class: binding.class.clone(),
                resource: resource.to_string(),
                state: binding.state,
                heartbeat_timeout_us: binding.heartbeat_timeout_us,
                sensor,
            });
        }
        states.sort_by(|left, right| {
            (&left.component, &left.resource).cmp(&(&right.component, &right.resource))
        });
    }
    for (controller_name, plan) in &resolved.controller_build_plans {
        let Some(controller) = controllers.get_mut(controller_name) else {
            return Err(vec![Diagnostic::error(
                "E1404",
                format!("resolved build plan names unknown controller '{controller_name}'"),
            )]);
        };
        controller.build = Some(LockedBuildConfig {
            schema: CONTROLLER_BUILD_SCHEMA.to_string(),
            board: plan.board.clone(),
            chip: plan.chip.clone(),
            target_triple: plan.target_triple.clone(),
            toolchain: plan.toolchain.clone(),
            build_profile: plan.build_profile.clone(),
            protocol_version: plan.protocol_version.clone(),
            abi_version: plan.abi_version.clone(),
            flash_bytes: plan.flash_bytes,
            ram_bytes: plan.ram_bytes,
            bootloader_offset_bytes: plan.bootloader_offset_bytes,
            features: plan.features.clone(),
            native_drivers: plan.native_drivers.clone(),
        });
    }

    // The safety profile is part of the closure; surface it as its own
    // field too (§12 pins it visibly), at the closure-selected version.
    let profile_prefix = format!("{}@", doc.safety.profile);
    let safety_profile = packages
        .iter()
        .find(|p| p.id.starts_with(&profile_prefix))
        .cloned()
        .ok_or_else(|| {
            vec![Diagnostic::error(
                "E1401",
                format!(
                    "safety profile '{}' is not in the resolved closure",
                    doc.safety.profile
                ),
            )]
        })?;

    let lockfile = Lockfile {
        lock_version: LOCK_VERSION,
        machine_hash: sha256_hex(source.as_bytes()),
        resolver_version: env!("CARGO_PKG_VERSION").to_string(),
        registry_source: Some(registry_source),
        packages,
        safety_profile,
        controllers,
    };
    lockfile.validate().map_err(|error| {
        vec![Diagnostic::error(
            "E1405",
            format!("generated lockfile violates its v{LOCK_VERSION} contract: {error}"),
        )]
    })?;
    Ok(lockfile)
}

#[cfg(test)]
mod tests {
    use super::*;
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
