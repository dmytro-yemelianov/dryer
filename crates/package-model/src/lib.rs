//! Package identity, manifests, dependency ranges, and local directory
//! registry loading (spec §6, §20.2 "local directory sources").
//!
//! v0.1 scope: identity/reference syntax, manifest parsing, structure
//! checks (§6.3 required files), and a deterministic local registry index.
//! Version *resolution*, trust policy and integrity hashes belong to the
//! future `package-registry` and `machine-resolver` crates.

use forge_machine_schema::{valid_identifier, Diagnostic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Package kinds the registry supports (spec §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageKind {
    Chip,
    Board,
    Device,
    Machine,
    Workflow,
    SafetyProfile,
    Kinematics,
    HostExtension,
    FirmwareExtension,
}

/// Fully qualified, versioned package reference: `namespace/name@1.1.0`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageRef {
    pub namespace: String,
    pub name: String,
    pub version: semver::Version,
}

impl fmt::Display for PackageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}@{}", self.namespace, self.name, self.version)
    }
}

/// Error from [`PackageRef::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefParseError(pub String);

impl fmt::Display for RefParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid package reference: {}", self.0)
    }
}

impl PackageRef {
    /// Parse `namespace/name@version` (spec §6.2).
    pub fn parse(s: &str) -> Result<Self, RefParseError> {
        let (path, version) = s
            .split_once('@')
            .ok_or_else(|| RefParseError(format!("'{s}' is missing '@version'")))?;
        let (namespace, name) = path
            .split_once('/')
            .ok_or_else(|| RefParseError(format!("'{s}' is missing 'namespace/'")))?;
        if !valid_identifier(namespace) || !valid_identifier(name) {
            return Err(RefParseError(format!(
                "'{path}' must be lowercase identifiers 'namespace/name'"
            )));
        }
        let version = semver::Version::parse(version)
            .map_err(|e| RefParseError(format!("'{version}' is not semver: {e}")))?;
        Ok(PackageRef {
            namespace: namespace.to_string(),
            name: name.to_string(),
            version,
        })
    }
}

/// The `package.yaml` manifest (spec §6.2–§6.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub package: PackageIdentity,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    pub namespace: String,
    pub name: String,
    pub version: semver::Version,
    pub kind: PackageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// Semver range, e.g. `"^1.0"` or `">=2.0,<3.0"` (spec §6.4; the
    /// comma form is normalized to semver's comma-separated AND).
    pub version: String,
}

impl Dependency {
    pub fn requirement(&self) -> Result<semver::VersionReq, semver::Error> {
        semver::VersionReq::parse(&self.version.replace(',', ", "))
    }
}

/// One package found by the local registry scan.
#[derive(Debug, Clone)]
pub struct LoadedPackage {
    pub reference: PackageRef,
    pub kind: PackageKind,
    pub dir: PathBuf,
    pub manifest: Manifest,
}

/// A deterministic index over a local `packages/` directory
/// (`packages/<namespace>/<name>/package.yaml`).
#[derive(Debug, Default)]
pub struct LocalRegistry {
    pub packages: Vec<LoadedPackage>,
    pub diagnostics: Vec<Diagnostic>,
}

impl LocalRegistry {
    /// Scan a directory tree. Deterministic: packages are returned in
    /// lexicographic `namespace/name` order regardless of filesystem order.
    /// Structural problems become diagnostics (`E06xx`), never panics.
    pub fn load(root: &Path) -> Self {
        let mut reg = LocalRegistry::default();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let namespaces = match std::fs::read_dir(root) {
            Ok(rd) => rd,
            Err(e) => {
                reg.diagnostics.push(Diagnostic::error(
                    "E0600",
                    format!("cannot read registry root {}: {e}", root.display()),
                ));
                return reg;
            }
        };
        for ns in namespaces.flatten() {
            if !ns.path().is_dir() {
                continue;
            }
            if let Ok(pkgs) = std::fs::read_dir(ns.path()) {
                for p in pkgs.flatten() {
                    if p.path().is_dir() {
                        dirs.push(p.path());
                    }
                }
            }
        }
        dirs.sort();
        for dir in dirs {
            reg.load_one(&dir);
        }
        reg
    }

    fn load_one(&mut self, dir: &Path) {
        let manifest_path = dir.join("package.yaml");
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(t) => t,
            Err(_) => {
                self.diagnostics.push(
                    Diagnostic::error("E0601", format!("{} has no package.yaml", dir.display()))
                        .suggest("every package requires package.yaml, README.md and LICENSE"),
                );
                return;
            }
        };
        let manifest: Manifest = match serde_yaml::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                self.diagnostics.push(Diagnostic::error(
                    "E0602",
                    format!("{}: package.yaml does not parse: {e}", dir.display()),
                ));
                return;
            }
        };
        // §6.3: required companion files. Missing ones are warnings during
        // development, enforced at publish time.
        for required in ["README.md", "LICENSE"] {
            if !dir.join(required).is_file() {
                self.diagnostics.push(Diagnostic::warning(
                    "E0603",
                    format!("{}: missing {required}", dir.display()),
                ));
            }
        }
        // Directory layout must agree with the declared identity.
        let declared = format!("{}/{}", manifest.package.namespace, manifest.package.name);
        let actual = format!(
            "{}/{}",
            dir.parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        if declared != actual {
            self.diagnostics.push(Diagnostic::error(
                "E0604",
                format!(
                    "{}: manifest declares '{declared}' but lives at '{actual}'",
                    dir.display()
                ),
            ));
            return;
        }
        // Dependency ranges must parse now, not at resolve time.
        for (dep, d) in &manifest.dependencies {
            if let Err(e) = d.requirement() {
                self.diagnostics.push(Diagnostic::error(
                    "E0605",
                    format!(
                        "{declared}: dependency '{dep}' has invalid range '{}': {e}",
                        d.version
                    ),
                ));
            }
        }
        self.packages.push(LoadedPackage {
            reference: PackageRef {
                namespace: manifest.package.namespace.clone(),
                name: manifest.package.name.clone(),
                version: manifest.package.version.clone(),
            },
            kind: manifest.package.kind,
            dir: dir.to_path_buf(),
            manifest,
        });
    }

    pub fn find(&self, namespace: &str, name: &str) -> Option<&LoadedPackage> {
        self.packages
            .iter()
            .find(|p| p.reference.namespace == namespace && p.reference.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_parse_and_display_round_trip() {
        let r = PackageRef::parse("boards/btt-octopus-pro@1.1.0").unwrap();
        assert_eq!(r.namespace, "boards");
        assert_eq!(r.name, "btt-octopus-pro");
        assert_eq!(r.version, semver::Version::new(1, 1, 0));
        assert_eq!(r.to_string(), "boards/btt-octopus-pro@1.1.0");
    }

    #[test]
    fn bad_refs_are_rejected_with_reasons() {
        for bad in [
            "no-version",
            "no-namespace@1.0.0",
            "Boards/x@1.0.0",
            "boards/x@not-semver",
            "boards/X@1.0.0",
        ] {
            assert!(PackageRef::parse(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn dependency_ranges_accept_both_spec_forms() {
        let caret = Dependency {
            version: "^1.0".into(),
        };
        assert!(caret
            .requirement()
            .unwrap()
            .matches(&semver::Version::new(1, 9, 3)));

        let window = Dependency {
            version: ">=2.0,<3.0".into(),
        };
        let req = window.requirement().unwrap();
        assert!(req.matches(&semver::Version::new(2, 5, 0)));
        assert!(!req.matches(&semver::Version::new(3, 0, 0)));
    }

    #[test]
    fn local_registry_loads_the_committed_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = LocalRegistry::load(&root);
        let errors: Vec<_> = reg
            .diagnostics
            .iter()
            .filter(|d| d.severity == forge_machine_schema::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let pkg = reg.find("devices", "tmc2209").expect("fixture package");
        assert_eq!(pkg.kind, PackageKind::Device);
        assert_eq!(pkg.reference.to_string(), "devices/tmc2209@2.1.0");
    }
}
