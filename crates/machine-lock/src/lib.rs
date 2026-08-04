//! `machine.lock` (spec §12, §29 step 7): a generated, canonical, hashed
//! capture of one successful resolution.
//!
//! v0.1 field scope, stated honestly: exact package versions + manifest
//! hashes, the machine-source hash, resolver identity, the pinned safety
//! profile, and per-controller resolved resources. Deferred to later
//! slices (each needs machinery that does not exist yet): registry source
//! identity, firmware target triples and build profiles, protocol
//! versions, feature flags — and the package hash covers `package.yaml`
//! only, not the full content tree (§6.6's content digest arrives with
//! the registry).
//!
//! Canonical form: JSON with every map a `BTreeMap`, so byte-identical
//! lockfiles for identical inputs. The on-disk file is YAML for humans;
//! `canonical_bytes`/`lock_hash` always use the JSON form.

use forge_machine_resolver::ResolvedGraph;
use forge_machine_schema::Diagnostic;
use forge_package_model::LocalRegistry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const LOCK_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// sha256 of the package's `package.yaml` bytes (manifest-only; see
    /// the module docs for the §6.6 full-content-digest deferral).
    pub manifest_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LockedController {
    pub board: String,
    /// `component/via` → connector id on this controller.
    pub resolved_resources: BTreeMap<String, String>,
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
    let parsed = forge_machine_parser::parse_str(source);
    let Some(doc) = parsed.doc else {
        return Err(parsed.diagnostics);
    };

    // The lock pins the resolver's full closure — explicit pins, implicit
    // roots and transitive dependencies — not merely the manifest's list.
    let mut packages = Vec::new();
    for pkg in &resolved.packages {
        let Ok(r) = forge_package_model::PackageRef::parse(pkg) else {
            continue; // the resolver produced these; malformed would be its bug
        };
        let Some(found) = registry.find_version(&r.namespace, &r.name, &r.version) else {
            return Err(vec![Diagnostic::error(
                "E1402",
                format!("resolved package '{pkg}' is no longer in the registry"),
            )]);
        };
        let manifest_bytes = std::fs::read(found.dir.join("package.yaml")).map_err(|e| {
            vec![Diagnostic::error(
                "E1400",
                format!("cannot hash {}: {e}", found.reference),
            )]
        })?;
        packages.push(LockedPackage {
            id: found.reference.to_string(),
            manifest_hash: sha256_hex(&manifest_bytes),
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
        let resolved = forge_machine_resolver::resolve_source(&source, &registry)
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
        let resolved = forge_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let l = lock(&source, &registry, &resolved).unwrap();
        let main = &l.controllers["mainboard"];
        assert_eq!(main.resolved_resources["x_driver/connected_to"], "motor0");
        assert_eq!(main.resolved_resources["hotend_heater/output"], "heater0");
        // the full closure: 3 explicit pins + the transitive chip
        // dependency + the implicit safety profile
        assert_eq!(l.packages.len(), 5, "{:?}", l.packages);
        assert!(l.packages.iter().any(|p| p.id == "chips/generic-mcu@1.2.0"));
        // the template-expanded component locks like any other
        assert_eq!(
            l.controllers["mainboard"].resolved_resources["y_driver/requires.connector"],
            "motor1"
        );
        assert!(l
            .packages
            .iter()
            .all(|p| p.manifest_hash.starts_with("sha256:")));
        assert!(l.machine_hash.starts_with("sha256:"));
        assert_eq!(l.safety_profile.id, "safety-profiles/desktop-fdm@1.0.0");
        assert!(l.safety_profile.manifest_hash.starts_with("sha256:"));
    }

    #[test]
    fn reformatting_the_manifest_changes_the_machine_hash() {
        let (source, registry) = setup();
        let resolved = forge_machine_resolver::resolve_source(&source, &registry)
            .resolved
            .unwrap();
        let a = lock(&source, &registry, &resolved).unwrap();
        let reformatted = format!("{source}\n# trailing comment\n");
        let b = lock(&reformatted, &registry, &resolved).unwrap();
        assert_ne!(a.machine_hash, b.machine_hash);
    }
}
