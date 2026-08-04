//! `machine.lock` (spec §12, §29 step 7): a generated, canonical, hashed
//! capture of one successful resolution.
//!
//! v0.2 field scope, stated honestly: exact package versions + portable
//! full-content digests (§6.6), manifest hashes, the machine-source hash,
//! resolver identity, the pinned safety profile, and per-controller resolved
//! resources. Deferred to later slices (each needs machinery that does not
//! exist yet): registry source identity, firmware target triples and build
//! profiles, protocol versions, and feature flags.
//!
//! Canonical form: JSON with every map a `BTreeMap`, so byte-identical
//! lockfiles for identical inputs. The on-disk file is YAML for humans;
//! `canonical_bytes`/`lock_hash` always use the JSON form.

use dryer_machine_resolver::ResolvedGraph;
use dryer_machine_schema::Diagnostic;
use dryer_package_model::LocalRegistry;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const LOCK_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Lockfile {
    pub lock_version: u32,
    /// sha256 of the exact machine-manifest bytes that resolved.
    pub machine_hash: String,
    /// The resolver that produced this (crate version; §12 'resolver version').
    pub resolver_version: String,
    pub packages: Vec<LockedPackage>,
    /// The safety profile the resolution validated against (§12 requires
    /// the lock to pin the safety-profile version).
    pub safety_profile: LockedPackage,
    pub controllers: BTreeMap<String, LockedController>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

#[derive(Serialize)]
struct LockfileRef<'a> {
    lock_version: u32,
    machine_hash: &'a str,
    resolver_version: &'a str,
    packages: &'a [LockedPackage],
    safety_profile: &'a LockedPackage,
    controllers: &'a BTreeMap<String, LockedController>,
}

#[derive(Deserialize)]
struct LockfileOwned {
    lock_version: u32,
    machine_hash: String,
    resolver_version: String,
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
            packages: wire.packages,
            safety_profile: wire.safety_profile,
            controllers: wire.controllers,
        };
        lock.validate().map_err(serde::de::Error::custom)?;
        Ok(lock)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

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

    Ok(Lockfile {
        lock_version: LOCK_VERSION,
        machine_hash: sha256_hex(source.as_bytes()),
        resolver_version: env!("CARGO_PKG_VERSION").to_string(),
        packages,
        safety_profile,
        controllers,
    })
}

impl Lockfile {
    fn validate(&self) -> Result<(), String> {
        if self.lock_version >= 2 {
            for (index, package) in self.packages.iter().enumerate() {
                if package.content_hash.is_empty() {
                    return Err(format!(
                        "lockfile v{} package[{index}] '{}' has no content_hash",
                        self.lock_version, package.id
                    ));
                }
            }
            if self.safety_profile.content_hash.is_empty() {
                return Err(format!(
                    "lockfile v{} safety_profile '{}' has no content_hash",
                    self.lock_version, self.safety_profile.id
                ));
            }
        }
        Ok(())
    }

    /// Canonical bytes: deterministic JSON. The hash of a lockfile is the
    /// hash of these bytes regardless of how the YAML file was formatted.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("lockfile serializes")
    }

    pub fn lock_hash(&self) -> String {
        sha256_hex(&self.canonical_bytes())
    }

    /// The human-facing on-disk form (`machine.lock`).
    pub fn to_yaml(&self) -> String {
        serde_yaml::to_string(self).expect("lockfile serializes")
    }

    pub fn from_yaml(text: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(text)
    }
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
        let main = &l.controllers["mainboard"];
        assert_eq!(main.resolved_resources["x_driver/connected_to"], "motor0");
        assert_eq!(main.resolved_resources["hotend_heater/output"], "heater0");
        // the full closure: 3 explicit pins + the transitive chip
        // dependency + the implicit safety profile
        assert_eq!(l.packages.len(), 5, "{:?}", l.packages);
        assert!(l.packages.iter().any(|p| p.id == "chips/generic-mcu@1.4.0"));
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
        let current = lock(&source, &registry, &resolved).unwrap().to_yaml();
        let legacy = current
            .replace("lock_version: 2", "lock_version: 1")
            .lines()
            .filter(|line| !line.trim_start().starts_with("content_hash:"))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = Lockfile::from_yaml(&legacy).unwrap();
        assert_eq!(parsed.lock_version, 1);
        assert!(parsed
            .packages
            .iter()
            .all(|package| package.content_hash.is_empty()));
        assert!(parsed.safety_profile.content_hash.is_empty());
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
}
