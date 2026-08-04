use dryer_package_model::RegistrySource;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const LOCK_VERSION: u32 = 5;
pub const CONTROLLER_SAFETY_SCHEMA: &str = "dryer.controller-safety/v1";
pub const CONTROLLER_BUILD_SCHEMA: &str = "dryer.controller-build-plan/v1";

#[derive(Debug, Clone, PartialEq)]
pub struct Lockfile {
    pub lock_version: u32,
    /// sha256 of the exact machine-manifest bytes that resolved.
    pub machine_hash: String,
    /// The resolver that produced this (crate version; §12 'resolver version').
    pub resolver_version: String,
    /// Present and required in lockfile v5+; absent in legacy v1-v4 locks.
    pub registry_source: Option<RegistrySource>,
    pub packages: Vec<LockedPackage>,
    /// The safety profile the resolution validated against (§12 requires
    /// the lock to pin the safety-profile version).
    pub safety_profile: LockedPackage,
    pub controllers: BTreeMap<String, LockedController>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackage {
    /// `namespace/name@version`.
    pub id: String,
    /// sha256 of the package's `package.yaml` bytes, retained for focused
    /// manifest drift diagnostics.
    pub manifest_hash: String,
    /// Portable sha256 over every path and regular-file byte in the package.
    /// Empty only when reading a legacy v1 lockfile.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LockedController {
    pub board: String,
    /// `component/via` → connector id on this controller.
    pub resolved_resources: BTreeMap<String, String>,
    /// Present and required in lockfile v3+; absent in legacy v1/v2 locks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<LockedSafetyConfig>,
    /// Present and required in lockfile v4+; absent in legacy v1-v3 locks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<LockedBuildConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedBuildConfig {
    pub schema: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedSafetyConfig {
    pub schema: String,
    pub states: Vec<LockedSafeState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedSafeState {
    pub component: String,
    pub class: String,
    /// Controller-local connector/resource id.
    pub resource: String,
    pub state: dryer_package_model::safety::SafeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_timeout_us: Option<u64>,
    /// Controller-local sensor resource when required by policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensor: Option<String>,
}
