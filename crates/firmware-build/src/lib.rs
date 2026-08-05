//! Deterministic controller firmware images (spec §11.2 phase 12, §21.1).
//!
//! The reference backend turns controller-local safety and target metadata
//! pinned by `machine.lock` into a byte-stable, inspectable controller-image
//! container and records its exact output hash in the build plan. The image is
//! a configuration/runtime handoff, not an executable MCU program; a native
//! target backend must consume the same locked inputs without overriding them.

use dryer_machine_lock::{
    LockedPackage, LockedSafeState, LockedSafetyConfig, Lockfile, CONTROLLER_SAFETY_SCHEMA,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

pub const MINIMUM_LOCK_VERSION: u32 = 3;
pub const BUILD_PLAN_MINIMUM_LOCK_VERSION: u32 = 4;
pub const CONTROLLER_BUILD_PLAN_SCHEMA: &str = "dryer.controller-build-plan/v2";
pub const CONTROLLER_IMAGE_SCHEMA: &str = "dryer.controller-image/v1";

/// Versioned, controller-local safety payload ready for firmware embedding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerSafetyArtifact {
    pub schema: String,
    pub controller: String,
    pub lock_hash: String,
    pub safety_profile: LockedPackage,
    pub states: Vec<LockedSafeState>,
}

/// Versioned, deterministic firmware build plan and expected backend output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerBuildPlanArtifact {
    pub schema: String,
    pub controller: String,
    pub lock_hash: String,
    pub board: String,
    pub chip: String,
    pub target_triple: String,
    pub toolchain: String,
    pub build_profile: String,
    pub protocol_version: String,
    pub abi_version: String,
    pub flash_bytes: u64,
    pub ram_bytes: u64,
    pub bootloader_offset_bytes: u64,
    pub features: Vec<String>,
    pub native_drivers: Vec<String>,
    pub resolved_resources: BTreeMap<String, String>,
    pub safety: LockedSafetyConfig,
    pub expected_artifact: ExpectedFirmwareArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFirmwareArtifact {
    pub format: String,
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// False for the inspectable reference container; a native backend must
    /// explicitly mark a target executable as deployable.
    pub deployable: bool,
}

/// Canonical payload emitted by the reference controller-image backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerImageManifest {
    pub schema: String,
    pub controller: String,
    pub lock_hash: String,
    pub board: String,
    pub chip: String,
    pub target_triple: String,
    pub toolchain: String,
    pub build_profile: String,
    pub protocol_version: String,
    pub abi_version: String,
    pub flash_bytes: u64,
    pub ram_bytes: u64,
    pub bootloader_offset_bytes: u64,
    pub features: Vec<String>,
    pub native_drivers: Vec<String>,
    pub resolved_resources: BTreeMap<String, String>,
    pub safety: LockedSafetyConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltControllerFirmware {
    pub plan: ControllerBuildPlanArtifact,
    pub image: ControllerImageManifest,
    pub bytes: Vec<u8>,
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

impl ControllerBuildPlanArtifact {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("controller build plan serializes")
    }

    pub fn artifact_hash(&self) -> String {
        format!("sha256:{:x}", Sha256::digest(self.canonical_bytes()))
    }

    pub fn to_pretty_json(&self) -> String {
        let mut json =
            serde_json::to_string_pretty(self).expect("controller build plan serializes");
        json.push('\n');
        json
    }
}

impl ControllerImageManifest {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.to_pretty_json().into_bytes()
    }

    pub fn image_hash(&self) -> String {
        format!("sha256:{:x}", Sha256::digest(self.canonical_bytes()))
    }

    pub fn to_pretty_json(&self) -> String {
        let mut json =
            serde_json::to_string_pretty(self).expect("controller image manifest serializes");
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
    MissingBuildConfig(String),
    UnsupportedBuildPlanLockVersion(u32),
    InvalidSafetyConfig(String),
    InvalidControllerImage(String),
    BuildPlanMismatch(String),
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
            Self::MissingBuildConfig(controller) => write!(
                formatter,
                "controller '{controller}' has no compiled build configuration"
            ),
            Self::UnsupportedBuildPlanLockVersion(version) => write!(
                formatter,
                "lockfile v{version} cannot produce controller build plans; re-lock with v{BUILD_PLAN_MINIMUM_LOCK_VERSION}+"
            ),
            Self::InvalidSafetyConfig(message) => {
                write!(formatter, "invalid controller safety configuration: {message}")
            }
            Self::InvalidControllerImage(message) => {
                write!(formatter, "invalid controller image: {message}")
            }
            Self::BuildPlanMismatch(message) => {
                write!(formatter, "controller build plan mismatch: {message}")
            }
        }
    }
}

impl std::error::Error for BuildError {}

fn sort_safety_states(states: &mut [LockedSafeState]) {
    states.sort_by(|left, right| {
        (&left.component, &left.resource).cmp(&(&right.component, &right.resource))
    });
}

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
    let mut states = safety.states.clone();
    sort_safety_states(&mut states);

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

fn controller_image_manifest(
    lock: &Lockfile,
    controller_name: &str,
) -> Result<ControllerImageManifest, BuildError> {
    lock.validate().map_err(BuildError::InvalidLock)?;
    if lock.lock_version < BUILD_PLAN_MINIMUM_LOCK_VERSION {
        return Err(BuildError::UnsupportedBuildPlanLockVersion(
            lock.lock_version,
        ));
    }
    let controller = lock
        .controllers
        .get(controller_name)
        .ok_or_else(|| BuildError::UnknownController(controller_name.to_string()))?;
    let build = controller
        .build
        .as_ref()
        .ok_or_else(|| BuildError::MissingBuildConfig(controller_name.to_string()))?;
    let mut safety = controller
        .safety
        .as_ref()
        .ok_or_else(|| BuildError::MissingSafetyConfig(controller_name.to_string()))?
        .clone();
    sort_safety_states(&mut safety.states);

    Ok(ControllerImageManifest {
        schema: CONTROLLER_IMAGE_SCHEMA.to_string(),
        controller: controller_name.to_string(),
        lock_hash: lock.lock_hash(),
        board: build.board.clone(),
        chip: build.chip.clone(),
        target_triple: build.target_triple.clone(),
        toolchain: build.toolchain.clone(),
        build_profile: build.build_profile.clone(),
        protocol_version: build.protocol_version.clone(),
        abi_version: build.abi_version.clone(),
        flash_bytes: build.flash_bytes,
        ram_bytes: build.ram_bytes,
        bootloader_offset_bytes: build.bootloader_offset_bytes,
        features: build.features.clone(),
        native_drivers: build.native_drivers.clone(),
        resolved_resources: controller.resolved_resources.clone(),
        safety,
    })
}

/// Build one deterministic, inspectable controller-image container without
/// consulting the source manifest or package registry.
pub fn build_controller(
    lock: &Lockfile,
    controller_name: &str,
) -> Result<BuiltControllerFirmware, BuildError> {
    let image = controller_image_manifest(lock, controller_name)?;
    let bytes = image.canonical_bytes();
    let expected_artifact = ExpectedFirmwareArtifact {
        format: CONTROLLER_IMAGE_SCHEMA.to_string(),
        path: format!("build/{controller_name}/firmware.dryer.json"),
        size_bytes: bytes.len() as u64,
        sha256: format!("sha256:{:x}", Sha256::digest(&bytes)),
        deployable: false,
    };
    let plan = ControllerBuildPlanArtifact {
        schema: CONTROLLER_BUILD_PLAN_SCHEMA.to_string(),
        controller: image.controller.clone(),
        lock_hash: image.lock_hash.clone(),
        board: image.board.clone(),
        chip: image.chip.clone(),
        target_triple: image.target_triple.clone(),
        toolchain: image.toolchain.clone(),
        build_profile: image.build_profile.clone(),
        protocol_version: image.protocol_version.clone(),
        abi_version: image.abi_version.clone(),
        flash_bytes: image.flash_bytes,
        ram_bytes: image.ram_bytes,
        bootloader_offset_bytes: image.bootloader_offset_bytes,
        features: image.features.clone(),
        native_drivers: image.native_drivers.clone(),
        resolved_resources: image.resolved_resources.clone(),
        safety: image.safety.clone(),
        expected_artifact,
    };
    Ok(BuiltControllerFirmware { plan, image, bytes })
}

/// Build every locked controller in stable controller-id order.
pub fn build_all(lock: &Lockfile) -> Result<BTreeMap<String, BuiltControllerFirmware>, BuildError> {
    lock.controllers
        .keys()
        .map(|controller| {
            build_controller(lock, controller).map(|built| (controller.clone(), built))
        })
        .collect()
}

/// Materialize one controller's locked §21.1 build plan, including the exact
/// expected reference-backend output, without rereading source packages.
pub fn plan_controller(
    lock: &Lockfile,
    controller_name: &str,
) -> Result<ControllerBuildPlanArtifact, BuildError> {
    build_controller(lock, controller_name).map(|built| built.plan)
}

/// Parse a controller image and require its exact canonical wire encoding.
pub fn inspect_controller_image(bytes: &[u8]) -> Result<ControllerImageManifest, BuildError> {
    let image: ControllerImageManifest = serde_json::from_slice(bytes)
        .map_err(|error| BuildError::InvalidControllerImage(error.to_string()))?;
    if image.schema != CONTROLLER_IMAGE_SCHEMA {
        return Err(BuildError::InvalidControllerImage(format!(
            "schema '{}' is not '{}'",
            image.schema, CONTROLLER_IMAGE_SCHEMA
        )));
    }
    if image.canonical_bytes() != bytes {
        return Err(BuildError::InvalidControllerImage(
            "bytes are not the canonical JSON encoding".into(),
        ));
    }
    Ok(image)
}

/// Require a persisted build plan to be exactly reproducible from the lock.
pub fn verify_build_plan(
    lock: &Lockfile,
    controller_name: &str,
    plan: &ControllerBuildPlanArtifact,
) -> Result<(), BuildError> {
    let expected = plan_controller(lock, controller_name)?;
    if plan != &expected {
        return Err(BuildError::BuildPlanMismatch(format!(
            "controller '{controller_name}' plan does not match the locked inputs"
        )));
    }
    Ok(())
}

/// Verify an externally supplied plan and image against one locked controller.
pub fn verify_controller_image(
    lock: &Lockfile,
    controller_name: &str,
    plan: &ControllerBuildPlanArtifact,
    bytes: &[u8],
) -> Result<ControllerImageManifest, BuildError> {
    verify_build_plan(lock, controller_name, plan)?;
    let expected = build_controller(lock, controller_name)?;
    let image = inspect_controller_image(bytes)?;
    if bytes != expected.bytes {
        return Err(BuildError::InvalidControllerImage(format!(
            "controller '{controller_name}' expected {} bytes at {}, observed {} bytes at sha256:{:x}",
            expected.plan.expected_artifact.size_bytes,
            expected.plan.expected_artifact.sha256,
            bytes.len(),
            Sha256::digest(bytes)
        )));
    }
    if image != expected.image {
        return Err(BuildError::InvalidControllerImage(format!(
            "controller '{controller_name}' manifest does not match the locked inputs"
        )));
    }
    Ok(image)
}

/// Materialize all build plans in stable controller-id order.
pub fn plan_all(
    lock: &Lockfile,
) -> Result<BTreeMap<String, ControllerBuildPlanArtifact>, BuildError> {
    lock.controllers
        .keys()
        .map(|controller| plan_controller(lock, controller).map(|plan| (controller.clone(), plan)))
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

        let plans = plan_all(&lock).unwrap();
        assert_eq!(plans.keys().collect::<Vec<_>>(), vec!["mainboard"]);

        let images = build_all(&lock).unwrap();
        assert_eq!(images.keys().collect::<Vec<_>>(), vec!["mainboard"]);
    }

    #[test]
    fn build_plan_is_deterministic_complete_and_round_trips() {
        let lock = fixture_lock();
        let first = plan_controller(&lock, "mainboard").unwrap();
        let second = plan_controller(&lock, "mainboard").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert!(first.artifact_hash().starts_with("sha256:"));
        let round_trip: ControllerBuildPlanArtifact =
            serde_json::from_slice(&first.canonical_bytes()).unwrap();
        assert_eq!(round_trip, first);
        assert_eq!(first.schema, CONTROLLER_BUILD_PLAN_SCHEMA);
        assert_eq!(first.target_triple, "thumbv7em-none-eabihf");
        assert_eq!(first.flash_bytes, 524_288);
        assert_eq!(first.safety.states.len(), 2);
        assert_eq!(first.native_drivers, ["devices/tmc2209@2.1.0"]);
        assert_eq!(first.expected_artifact.format, CONTROLLER_IMAGE_SCHEMA);
        assert_eq!(
            first.expected_artifact.path,
            "build/mainboard/firmware.dryer.json"
        );
        assert!(first.expected_artifact.size_bytes > 0);
        assert!(first.expected_artifact.sha256.starts_with("sha256:"));
        assert!(!first.expected_artifact.deployable);

        let mut reordered = lock;
        reordered
            .controllers
            .get_mut("mainboard")
            .unwrap()
            .safety
            .as_mut()
            .unwrap()
            .states
            .reverse();
        let reordered = plan_controller(&reordered, "mainboard").unwrap();
        assert!(reordered.safety.states.windows(2).all(|states| {
            (&states[0].component, &states[0].resource)
                < (&states[1].component, &states[1].resource)
        }));
    }

    #[test]
    fn reference_backend_is_deterministic_inspectable_and_lock_bound() {
        let lock = fixture_lock();
        let first = build_controller(&lock, "mainboard").unwrap();
        let second = build_controller(&lock, "mainboard").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.image.schema, CONTROLLER_IMAGE_SCHEMA);
        assert_eq!(
            first.bytes.len() as u64,
            first.plan.expected_artifact.size_bytes
        );
        assert_eq!(
            first.image.image_hash(),
            first.plan.expected_artifact.sha256
        );
        assert_eq!(inspect_controller_image(&first.bytes).unwrap(), first.image);
        assert_eq!(
            verify_controller_image(&lock, "mainboard", &first.plan, &first.bytes).unwrap(),
            first.image
        );

        let mut noncanonical = first.bytes.clone();
        noncanonical.push(b'\n');
        let error = inspect_controller_image(&noncanonical).unwrap_err();
        assert!(error.to_string().contains("canonical JSON"), "{error}");

        let mut drifted_plan = first.plan.clone();
        drifted_plan.expected_artifact.sha256 =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into();
        let error =
            verify_controller_image(&lock, "mainboard", &drifted_plan, &first.bytes).unwrap_err();
        assert!(matches!(error, BuildError::BuildPlanMismatch(_)));
    }

    #[test]
    fn legacy_v3_locks_cannot_produce_build_plans() {
        let mut lock = fixture_lock();
        lock.lock_version = 3;
        lock.registry_source = None;
        for controller in lock.controllers.values_mut() {
            controller.build = None;
        }
        let error = plan_controller(&lock, "mainboard").unwrap_err();
        assert_eq!(error, BuildError::UnsupportedBuildPlanLockVersion(3));
    }

    #[test]
    fn legacy_v4_locks_without_registry_identity_can_still_build() {
        let mut lock = fixture_lock();
        lock.lock_version = 4;
        lock.registry_source = None;

        compile_controller(&lock, "mainboard").unwrap();
        plan_controller(&lock, "mainboard").unwrap();
    }

    #[test]
    fn v5_locks_without_registry_identity_cannot_build() {
        let mut lock = fixture_lock();
        lock.registry_source = None;

        for error in [
            compile_controller(&lock, "mainboard").unwrap_err(),
            plan_controller(&lock, "mainboard").unwrap_err(),
        ] {
            match error {
                BuildError::InvalidLock(message) => {
                    assert!(message.contains("registry source identity"), "{message}")
                }
                other => panic!("expected invalid v5 registry identity, got {other}"),
            }
        }
    }
}
