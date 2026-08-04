//! Deterministic controller-safety artifacts (spec §11.2 phase 12, §18.2).
//!
//! This crate is deliberately a build-input boundary, not a firmware
//! toolchain: it takes the controller-local safety projection pinned by a v3
//! `machine.lock` and emits byte-stable JSON that a future firmware build must
//! embed unchanged. No host-only initialization can supply or override it.

use dryer_machine_lock::{LockedPackage, LockedSafeState, Lockfile, CONTROLLER_SAFETY_SCHEMA};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const MINIMUM_LOCK_VERSION: u32 = 3;

/// Versioned, controller-local safety payload ready for firmware embedding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerSafetyArtifact {
    pub schema: String,
    pub controller: String,
    pub lock_hash: String,
    pub safety_profile: LockedPackage,
    pub states: Vec<LockedSafeState>,
}

impl ControllerSafetyArtifact {
    /// Canonical artifact bytes. All maps upstream are ordered and states are
    /// sorted by `(component, resource)` during compilation.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("controller safety artifact serializes")
    }

    pub fn artifact_hash(&self) -> String {
        format!("sha256:{:x}", Sha256::digest(self.canonical_bytes()))
    }

    pub fn to_pretty_json(&self) -> String {
        let mut json =
            serde_json::to_string_pretty(self).expect("controller safety artifact serializes");
        json.push('\n');
        json
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    InvalidLock(String),
    UnsupportedLockVersion(u32),
    UnknownController(String),
    MissingSafetyConfig(String),
    InvalidSafetyConfig(String),
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLock(message) => write!(formatter, "invalid machine lock: {message}"),
            Self::UnsupportedLockVersion(version) => write!(
                formatter,
                "lockfile v{version} cannot produce controller safety artifacts; re-lock with v{MINIMUM_LOCK_VERSION}+"
            ),
            Self::UnknownController(controller) => {
                write!(formatter, "unknown locked controller '{controller}'")
            }
            Self::MissingSafetyConfig(controller) => write!(
                formatter,
                "controller '{controller}' has no compiled safety configuration"
            ),
            Self::InvalidSafetyConfig(message) => {
                write!(formatter, "invalid controller safety configuration: {message}")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Compile one controller's locked safety projection into the artifact ABI.
pub fn compile_controller(
    lock: &Lockfile,
    controller_name: &str,
) -> Result<ControllerSafetyArtifact, BuildError> {
    lock.validate().map_err(BuildError::InvalidLock)?;
    if lock.lock_version < MINIMUM_LOCK_VERSION {
        return Err(BuildError::UnsupportedLockVersion(lock.lock_version));
    }
    let controller = lock
        .controllers
        .get(controller_name)
        .ok_or_else(|| BuildError::UnknownController(controller_name.to_string()))?;
    let safety = controller
        .safety
        .as_ref()
        .ok_or_else(|| BuildError::MissingSafetyConfig(controller_name.to_string()))?;
    if safety.schema != CONTROLLER_SAFETY_SCHEMA {
        return Err(BuildError::InvalidSafetyConfig(format!(
            "controller '{controller_name}' uses schema '{}' instead of '{}'",
            safety.schema, CONTROLLER_SAFETY_SCHEMA
        )));
    }

    let resources: BTreeSet<&str> = controller
        .resolved_resources
        .values()
        .map(String::as_str)
        .collect();
    let mut states = safety.states.clone();
    states.sort_by(|left, right| {
        (&left.component, &left.resource).cmp(&(&right.component, &right.resource))
    });
    let mut resources_with_safety = BTreeSet::new();
    for state in &states {
        if state.component.trim().is_empty()
            || state.component.trim() != state.component
            || state.class.trim().is_empty()
            || state.class.trim() != state.class
        {
            return Err(BuildError::InvalidSafetyConfig(format!(
                "controller '{controller_name}' has an empty or padded component/class"
            )));
        }
        if !resources.contains(state.resource.as_str()) {
            return Err(BuildError::InvalidSafetyConfig(format!(
                "controller '{controller_name}' resource '{}' is not in resolved_resources",
                state.resource
            )));
        }
        if state
            .sensor
            .as_deref()
            .is_some_and(|sensor| !resources.contains(sensor))
        {
            return Err(BuildError::InvalidSafetyConfig(format!(
                "controller '{controller_name}' sensor '{}' is not in resolved_resources",
                state.sensor.as_deref().unwrap_or_default()
            )));
        }
        if state.heartbeat_timeout_us == Some(0) {
            return Err(BuildError::InvalidSafetyConfig(format!(
                "controller '{controller_name}' has a zero heartbeat timeout"
            )));
        }
        if !resources_with_safety.insert(state.resource.as_str()) {
            return Err(BuildError::InvalidSafetyConfig(format!(
                "controller '{controller_name}' repeats physical resource '{}'",
                state.resource
            )));
        }
    }

    Ok(ControllerSafetyArtifact {
        schema: CONTROLLER_SAFETY_SCHEMA.to_string(),
        controller: controller_name.to_string(),
        lock_hash: lock.lock_hash(),
        safety_profile: lock.safety_profile.clone(),
        states,
    })
}

/// Compile all controllers in stable controller-id order.
pub fn compile_all(
    lock: &Lockfile,
) -> Result<BTreeMap<String, ControllerSafetyArtifact>, BuildError> {
    lock.controllers
        .keys()
        .map(|controller| {
            compile_controller(lock, controller).map(|artifact| (controller.clone(), artifact))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dryer_machine_lock::lock;
    use dryer_package_model::LocalRegistry;
    use std::path::Path;

    fn fixture_lock() -> Lockfile {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source =
            std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap();
        let registry = LocalRegistry::load(&root.join("packages"));
        let resolved = dryer_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        lock(&source, &registry, &resolved).unwrap()
    }

    #[test]
    fn compilation_is_deterministic_hashed_and_round_trips() {
        let lock = fixture_lock();
        let first = compile_controller(&lock, "mainboard").unwrap();
        let second = compile_controller(&lock, "mainboard").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert!(first.artifact_hash().starts_with("sha256:"));
        let round_trip: ControllerSafetyArtifact =
            serde_json::from_slice(&first.canonical_bytes()).unwrap();
        assert_eq!(round_trip, first);
        assert_eq!(first.states.len(), 2);
        assert_eq!(first.states[0].component, "hotend_heater");
        assert_eq!(first.states[1].component, "x_motor");
    }

    #[test]
    fn legacy_locks_cannot_silently_omit_edge_safety() {
        let mut lock = fixture_lock();
        lock.lock_version = 2;
        for controller in lock.controllers.values_mut() {
            controller.safety = None;
        }
        let error = compile_controller(&lock, "mainboard").unwrap_err();
        assert_eq!(error, BuildError::UnsupportedLockVersion(2));
    }

    #[test]
    fn all_controllers_are_emitted_in_stable_order() {
        let lock = fixture_lock();
        let artifacts = compile_all(&lock).unwrap();
        assert_eq!(artifacts.keys().collect::<Vec<_>>(), vec!["mainboard"]);
    }
}
