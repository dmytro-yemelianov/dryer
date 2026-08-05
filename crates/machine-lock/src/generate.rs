use crate::wire::sha256_hex;
use crate::{
    LockedBuildConfig, LockedController, LockedPackage, LockedSafeState, LockedSafetyConfig,
    Lockfile, CONTROLLER_BUILD_SCHEMA, CONTROLLER_SAFETY_SCHEMA, LOCK_VERSION,
};
use dryer_machine_resolver::ResolvedGraph;
use dryer_machine_schema::{Diagnostic, MachineDoc};
use dryer_package_model::{LocalRegistry, PackageRef};
use std::collections::BTreeMap;

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

    let packages = pin_packages(registry, resolved)?;
    let mut controllers = initialize_controllers(&doc);
    apply_assignments(&mut controllers, resolved);
    apply_safety(&mut controllers, resolved)?;
    apply_build_inputs(&mut controllers, resolved)?;
    let safety_profile = select_safety_profile(&packages, &doc.safety.profile)?;

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

fn pin_packages(
    registry: &LocalRegistry,
    resolved: &ResolvedGraph,
) -> Result<Vec<LockedPackage>, Vec<Diagnostic>> {
    // The lock pins the resolver's full closure — explicit pins, implicit
    // roots and transitive dependencies — not merely the manifest's list.
    let mut packages = Vec::new();
    for pkg in &resolved.packages {
        let Ok(r) = PackageRef::parse(pkg) else {
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
    Ok(packages)
}

fn initialize_controllers(doc: &MachineDoc) -> BTreeMap<String, LockedController> {
    doc.controllers
        .iter()
        .map(|(name, controller)| {
            (
                name.clone(),
                LockedController {
                    board: controller.board.clone(),
                    resolved_resources: BTreeMap::new(),
                    safety: Some(LockedSafetyConfig {
                        schema: CONTROLLER_SAFETY_SCHEMA.to_string(),
                        states: Vec::new(),
                    }),
                    build: None,
                },
            )
        })
        .collect()
}

fn apply_assignments(
    controllers: &mut BTreeMap<String, LockedController>,
    resolved: &ResolvedGraph,
) {
    for (component, assignments) in &resolved.assignments {
        for assignment in assignments {
            let Some((controller_name, port)) = assignment.resource.0.split_once('.') else {
                continue;
            };
            if let Some(controller) = controllers.get_mut(controller_name) {
                // Keys stay terse: a search allocation's via carries its
                // provenance suffix ("requires.connector (devices/x@1.0)");
                // the lock keeps the mechanism, `explain` keeps the story.
                let via_short = assignment
                    .via
                    .split_whitespace()
                    .next()
                    .unwrap_or(&assignment.via);
                controller
                    .resolved_resources
                    .insert(format!("{component}/{via_short}"), port.to_string());
            }
        }
    }
}

fn apply_safety(
    controllers: &mut BTreeMap<String, LockedController>,
    resolved: &ResolvedGraph,
) -> Result<(), Vec<Diagnostic>> {
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
    Ok(())
}

fn apply_build_inputs(
    controllers: &mut BTreeMap<String, LockedController>,
    resolved: &ResolvedGraph,
) -> Result<(), Vec<Diagnostic>> {
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
    Ok(())
}

fn select_safety_profile(
    packages: &[LockedPackage],
    profile: &str,
) -> Result<LockedPackage, Vec<Diagnostic>> {
    // The safety profile is part of the closure; surface it as its own
    // field too (§12 pins it visibly), at the closure-selected version.
    let profile_prefix = format!("{profile}@");
    packages
        .iter()
        .find(|package| package.id.starts_with(&profile_prefix))
        .cloned()
        .ok_or_else(|| {
            vec![Diagnostic::error(
                "E1401",
                format!("safety profile '{profile}' is not in the resolved closure"),
            )]
        })
}
