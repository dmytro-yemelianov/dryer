use crate::{LockedController, LockedPackage, Lockfile};
use dryer_package_model::RegistrySource;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Serialize)]
struct LockfileRef<'a> {
    lock_version: u32,
    machine_hash: &'a str,
    resolver_version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    registry_source: &'a Option<RegistrySource>,
    packages: &'a [LockedPackage],
    safety_profile: &'a LockedPackage,
    controllers: &'a BTreeMap<String, LockedController>,
}

#[derive(Deserialize)]
struct LockfileOwned {
    lock_version: u32,
    machine_hash: String,
    resolver_version: String,
    #[serde(default)]
    registry_source: Option<RegistrySource>,
    packages: Vec<LockedPackage>,
    safety_profile: LockedPackage,
    controllers: BTreeMap<String, LockedController>,
}

impl Serialize for Lockfile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        LockfileRef {
            lock_version: self.lock_version,
            machine_hash: &self.machine_hash,
            resolver_version: &self.resolver_version,
            registry_source: &self.registry_source,
            packages: &self.packages,
            safety_profile: &self.safety_profile,
            controllers: &self.controllers,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Lockfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = LockfileOwned::deserialize(deserializer)?;
        let lock = Self {
            lock_version: wire.lock_version,
            machine_hash: wire.machine_hash,
            resolver_version: wire.resolver_version,
            registry_source: wire.registry_source,
            packages: wire.packages,
            safety_profile: wire.safety_profile,
            controllers: wire.controllers,
        };
        lock.validate().map_err(serde::de::Error::custom)?;
        Ok(lock)
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Lockfile {
    /// Canonical bytes: deterministic JSON. The hash of a lockfile is the
    /// hash of these bytes regardless of how the YAML file was formatted.
    pub fn try_canonical_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Infallible convenience wrapper for already validated lockfiles.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.try_canonical_bytes().expect("lockfile serializes")
    }

    /// Fallible lock hash for callers handling untrusted or mutated lock data.
    pub fn try_lock_hash(&self) -> Result<String, serde_json::Error> {
        self.try_canonical_bytes().map(|bytes| sha256_hex(&bytes))
    }

    /// Infallible convenience wrapper for already validated lockfiles.
    pub fn lock_hash(&self) -> String {
        self.try_lock_hash().expect("lockfile serializes")
    }

    /// The human-facing on-disk form (`machine.lock`).
    pub fn try_to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }

    /// Infallible convenience wrapper for already validated lockfiles.
    pub fn to_yaml(&self) -> String {
        self.try_to_yaml().expect("lockfile serializes")
    }

    pub fn from_yaml(text: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(text)
    }
}
