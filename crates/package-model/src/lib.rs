//! Package identity, manifests, dependency ranges, and local directory
//! registry loading (spec §6, §20.2 "local directory sources").
//!
//! v0.1 scope: identity/reference syntax, manifest parsing, structure
//! checks (§6.3 required files), a deterministic local registry index, and
//! portable registry provenance, and a portable full-content digest for
//! lockfile integrity (§6.6). Version *resolution* belongs to
//! `machine-resolver`; fetching, signatures, and trust policy remain future
//! package-registry work.

pub mod board;
pub mod chip;
pub mod device;
pub mod machine;
pub mod safety;

use dryer_machine_schema::{valid_identifier, Diagnostic};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

const CONTENT_HASH_DOMAIN: &[u8] = b"dryer-package-content-v1\0";
const SNAPSHOT_VERIFY_RETRIES: usize = 3;
pub const REGISTRY_DESCRIPTOR_SCHEMA: &str = "dryer.registry/v1";
pub const REGISTRY_SOURCE_SCHEMA: &str = "dryer.registry-source/v1";

/// Portable provenance for a loaded registry. `descriptor_hash` binds the
/// exact `registry.yaml` bytes; package content remains pinned separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySource {
    pub schema: String,
    pub id: String,
    pub uri: String,
    pub descriptor_hash: String,
}

impl RegistrySource {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != REGISTRY_SOURCE_SCHEMA {
            return Err(format!(
                "registry source schema '{}' is not '{}'",
                self.schema, REGISTRY_SOURCE_SCHEMA
            ));
        }
        if !valid_identifier(&self.id) {
            return Err(format!(
                "registry source id '{}' is not a valid identifier",
                self.id
            ));
        }
        if !portable_registry_uri(&self.uri) {
            return Err(format!(
                "registry source URI '{}' is not a portable absolute URI",
                self.uri
            ));
        }
        if !valid_sha256(&self.descriptor_hash) {
            return Err("registry descriptor_hash is not a lowercase sha256 digest".into());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDescriptor {
    schema: String,
    id: String,
    uri: String,
}

fn portable_registry_uri(uri: &str) -> bool {
    if uri.is_empty()
        || uri.trim() != uri
        || uri.bytes().any(|byte| byte.is_ascii_whitespace())
        || uri.contains('\\')
    {
        return false;
    }
    let Some((scheme, location)) = uri.split_once(':') else {
        return false;
    };
    !location.is_empty()
        && scheme != "file"
        && scheme.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' => true,
            b'0'..=b'9' | b'+' | b'-' | b'.' => index > 0,
            _ => false,
        })
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn registry_source(root: &Path) -> Result<RegistrySource, Box<Diagnostic>> {
    let path = root.join("registry.yaml");
    let bytes = std::fs::read(&path).map_err(|error| {
        Box::new(Diagnostic::error(
            "E0607",
            format!(
                "cannot read registry descriptor {}: {error}",
                path.display()
            ),
        ))
    })?;
    let descriptor: RegistryDescriptor = serde_yaml::from_slice(&bytes).map_err(|error| {
        Box::new(Diagnostic::error(
            "E0608",
            format!(
                "registry descriptor {} does not parse: {error}",
                path.display()
            ),
        ))
    })?;
    if descriptor.schema != REGISTRY_DESCRIPTOR_SCHEMA {
        return Err(Box::new(Diagnostic::error(
            "E0609",
            format!(
                "registry descriptor schema '{}' is not '{}'",
                descriptor.schema, REGISTRY_DESCRIPTOR_SCHEMA
            ),
        )));
    }
    let source = RegistrySource {
        schema: REGISTRY_SOURCE_SCHEMA.to_string(),
        id: descriptor.id,
        uri: descriptor.uri,
        descriptor_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
    };
    source
        .validate()
        .map_err(|error| Box::new(Diagnostic::error("E0609", error)))?;
    Ok(source)
}

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

impl LoadedPackage {
    /// Read one byte-verified snapshot of the complete package tree.
    pub fn snapshot(&self) -> io::Result<PackageSnapshot> {
        package_snapshot(&self.dir)
    }

    /// A portable digest of every regular file under this package root.
    pub fn content_hash(&self) -> io::Result<String> {
        Ok(self.snapshot()?.content_hash())
    }
}

/// One stable, in-memory view of every regular file in a package.
///
/// Manifest and full-content hashes derived from this value necessarily refer
/// to the same bytes. Construction compares consecutive complete reads and
/// retries when paths or bytes change, including same-length rewrites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSnapshot {
    files: BTreeMap<String, Vec<u8>>,
}

impl PackageSnapshot {
    /// Exact `package.yaml` bytes from this snapshot.
    pub fn manifest_bytes(&self) -> io::Result<&[u8]> {
        self.files
            .get("package.yaml")
            .map(Vec::as_slice)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "package snapshot contains no package.yaml",
                )
            })
    }

    /// Portable digest of all paths and bytes in this snapshot.
    pub fn content_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(CONTENT_HASH_DOMAIN);
        for (relative, bytes) in &self.files {
            hasher.update((relative.len() as u64).to_be_bytes());
            hasher.update(relative.as_bytes());
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
        format!("sha256:{:x}", hasher.finalize())
    }
}

/// Hash an entire package tree deterministically (§6.6).
///
/// Entries are sorted by UTF-8 relative path with `/` separators. The hash
/// commits to a domain tag, each path and its length, then each file's length
/// and bytes. Filesystem metadata is intentionally excluded. Symlinks and
/// non-UTF-8 paths are rejected so the result cannot depend on host traversal
/// behavior or escape the package root.
pub fn package_content_hash(root: &Path) -> io::Result<String> {
    Ok(package_snapshot(root)?.content_hash())
}

/// Read a stable package snapshot, retrying when the tree changes between
/// consecutive complete reads.
pub fn package_snapshot(root: &Path) -> io::Result<PackageSnapshot> {
    package_snapshot_with_hook(root, |_| Ok(()))
}

fn package_snapshot_with_hook(
    root: &Path,
    mut between_reads: impl FnMut(usize) -> io::Result<()>,
) -> io::Result<PackageSnapshot> {
    let mut observed = read_package_tree(root)?;
    for attempt in 0..=SNAPSHOT_VERIFY_RETRIES {
        between_reads(attempt)?;
        let verified = read_package_tree(root)?;
        if observed == verified {
            return Ok(PackageSnapshot { files: verified });
        }
        observed = verified;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "package tree '{}' remained unstable after {} verification retries",
            root.display(),
            SNAPSHOT_VERIFY_RETRIES
        ),
    ))
}

fn read_package_tree(root: &Path) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let root_type = std::fs::symlink_metadata(root)?.file_type();
    if root_type.is_symlink() || !root_type.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "package root '{}' must be a directory, not a symlink",
                root.display()
            ),
        ));
    }

    let mut files = Vec::new();
    collect_content_files(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut snapshot = BTreeMap::new();
    for (relative, path) in files {
        snapshot.insert(relative, std::fs::read(path)?);
    }
    Ok(snapshot)
}

fn collect_content_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "package content may not contain symlink '{}'",
                    path.display()
                ),
            ));
        }
        if file_type.is_dir() {
            collect_content_files(root, &path, files)?;
        } else if file_type.is_file() {
            files.push((portable_relative_path(root, &path)?, path));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "package content contains unsupported entry '{}'",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn portable_relative_path(root: &Path, path: &Path) -> io::Result<String> {
    path.strip_prefix(root)
        .expect("collected path is under package root")
        .components()
        .map(|component| {
            component.as_os_str().to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("package path '{}' is not valid UTF-8", path.display()),
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

/// A deterministic index over a local `packages/` directory.
///
/// Two layouts per package, mixable within one registry (spec §20.2
/// local sources):
/// - single-version: `packages/<ns>/<name>/package.yaml`
/// - multi-version:  `packages/<ns>/<name>/<version>/package.yaml`
///   (the directory name must equal the manifest version, E0606)
#[derive(Debug, Default)]
pub struct LocalRegistry {
    pub source: Option<RegistrySource>,
    pub packages: Vec<LoadedPackage>,
    pub diagnostics: Vec<Diagnostic>,
}

impl LocalRegistry {
    /// Scan a directory tree. Deterministic: packages are returned in
    /// lexicographic `namespace/name` then ascending-version order,
    /// regardless of filesystem order. Structural problems become
    /// diagnostics (`E06xx`), never panics.
    pub fn load(root: &Path) -> Self {
        let mut reg = LocalRegistry::default();
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
        match registry_source(root) {
            Ok(source) => reg.source = Some(source),
            Err(diagnostic) => reg.diagnostics.push(*diagnostic),
        }
        let mut name_dirs: Vec<(String, String, PathBuf)> = Vec::new();
        for ns in namespaces.flatten() {
            if !ns.path().is_dir() {
                continue;
            }
            let ns_name = ns.file_name().to_string_lossy().into_owned();
            if let Ok(pkgs) = std::fs::read_dir(ns.path()) {
                for p in pkgs.flatten() {
                    if p.path().is_dir() {
                        name_dirs.push((
                            ns_name.clone(),
                            p.file_name().to_string_lossy().into_owned(),
                            p.path(),
                        ));
                    }
                }
            }
        }
        name_dirs.sort();
        for (ns, name, dir) in name_dirs {
            if dir.join("package.yaml").is_file() {
                reg.load_one(&dir, &ns, &name, None);
            } else {
                // multi-version layout: each subdirectory is one version
                let mut versions: Vec<PathBuf> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                if versions.is_empty() {
                    reg.diagnostics.push(
                        Diagnostic::error(
                            "E0601",
                            format!("{} has no package.yaml", dir.display()),
                        )
                        .suggest("every package requires package.yaml, README.md and LICENSE"),
                    );
                    continue;
                }
                versions.sort();
                for vdir in versions {
                    let v = vdir.file_name().map(|s| s.to_string_lossy().into_owned());
                    reg.load_one(&vdir, &ns, &name, v.as_deref());
                }
            }
        }
        reg.packages.sort_by(|a, b| a.reference.cmp(&b.reference));
        reg
    }

    fn load_one(&mut self, dir: &Path, ns: &str, name: &str, dir_version: Option<&str>) {
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
        let actual = format!("{ns}/{name}");
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
        // In the multi-version layout the directory name is the version.
        if let Some(dv) = dir_version {
            if dv != manifest.package.version.to_string() {
                self.diagnostics.push(Diagnostic::error(
                    "E0606",
                    format!(
                        "{}: version directory '{dv}' but the manifest declares {}",
                        dir.display(),
                        manifest.package.version
                    ),
                ));
                return;
            }
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

    /// The highest available version of a package (deterministic default;
    /// the resolver's constraint intersection narrows from here).
    pub fn find(&self, namespace: &str, name: &str) -> Option<&LoadedPackage> {
        self.packages
            .iter()
            .filter(|p| p.reference.namespace == namespace && p.reference.name == name)
            .max_by(|a, b| a.reference.version.cmp(&b.reference.version))
    }

    /// One exact version.
    pub fn find_version(
        &self,
        namespace: &str,
        name: &str,
        version: &semver::Version,
    ) -> Option<&LoadedPackage> {
        self.packages.iter().find(|p| {
            p.reference.namespace == namespace
                && p.reference.name == name
                && p.reference.version == *version
        })
    }

    /// All available versions of a package, ascending.
    pub fn versions(&self, namespace: &str, name: &str) -> Vec<&semver::Version> {
        let mut v: Vec<&semver::Version> = self
            .packages
            .iter()
            .filter(|p| p.reference.namespace == namespace && p.reference.name == name)
            .map(|p| &p.reference.version)
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_package_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "dryer-package-content-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(path.join("nested")).unwrap();
        path
    }

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
    fn multi_version_layout_loads_all_versions_and_find_prefers_highest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = LocalRegistry::load(&root);
        let versions = reg.versions("chips", "generic-mcu");
        assert_eq!(
            versions.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            vec!["1.0.0", "1.2.0", "1.3.0", "1.4.0", "1.5.0"]
        );
        assert_eq!(
            reg.find("chips", "generic-mcu").unwrap().reference.version,
            semver::Version::new(1, 5, 0),
            "find() returns the highest version"
        );
        assert!(reg
            .find_version("chips", "generic-mcu", &semver::Version::new(1, 0, 0))
            .is_some());
        assert!(reg
            .find_version("chips", "generic-mcu", &semver::Version::new(9, 9, 9))
            .is_none());
    }

    #[test]
    fn local_registry_loads_the_committed_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
        let reg = LocalRegistry::load(&root);
        let errors: Vec<_> = reg
            .diagnostics
            .iter()
            .filter(|d| d.severity == dryer_machine_schema::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let pkg = reg.find("devices", "tmc2209").expect("fixture package");
        assert_eq!(pkg.kind, PackageKind::Device);
        assert_eq!(pkg.reference.to_string(), "devices/tmc2209@2.1.0");
        let source = reg.source.as_ref().expect("registry provenance");
        assert_eq!(source.schema, REGISTRY_SOURCE_SCHEMA);
        assert_eq!(source.id, "dryer-official");
        assert_eq!(
            source.uri,
            "git+https://github.com/dmytro-yemelianov/dryer.git?subdir=packages"
        );
        assert!(source.descriptor_hash.starts_with("sha256:"));
        source.validate().unwrap();
    }

    #[test]
    fn invalid_registry_provenance_is_diagnosed_without_accepting_it() {
        let root = temporary_package_dir("invalid-registry-source");
        std::fs::write(
            root.join("registry.yaml"),
            "schema: dryer.registry/v1\nid: Invalid\nuri: file:///tmp/packages\n",
        )
        .unwrap();

        let registry = LocalRegistry::load(&root);
        assert!(registry.source.is_none());
        assert!(registry
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0609"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_hash_is_order_independent_and_covers_companion_files() {
        let first = temporary_package_dir("first");
        let second = temporary_package_dir("second");

        std::fs::write(first.join("package.yaml"), b"package: fixture\n").unwrap();
        std::fs::write(first.join("README.md"), b"documentation\n").unwrap();
        std::fs::write(first.join("nested/data.bin"), [0_u8, 1, 2, 3]).unwrap();

        // Same paths and bytes, deliberately created in a different order.
        std::fs::write(second.join("nested/data.bin"), [0_u8, 1, 2, 3]).unwrap();
        std::fs::write(second.join("README.md"), b"documentation\n").unwrap();
        std::fs::write(second.join("package.yaml"), b"package: fixture\n").unwrap();

        let expected = package_content_hash(&first).unwrap();
        assert_eq!(expected, package_content_hash(&second).unwrap());
        assert!(expected.starts_with("sha256:"));

        std::fs::write(second.join("README.md"), b"changed documentation\n").unwrap();
        assert_ne!(expected, package_content_hash(&second).unwrap());

        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn snapshot_retries_an_intervening_same_length_manifest_rewrite() {
        let package = temporary_package_dir("same-length-rewrite");
        std::fs::write(package.join("package.yaml"), b"package: alpha\n").unwrap();
        std::fs::write(package.join("README.md"), b"stable\n").unwrap();
        let before = package_snapshot(&package).unwrap();

        let mut rewrote = false;
        let after = package_snapshot_with_hook(&package, |_| {
            if !rewrote {
                std::fs::write(package.join("package.yaml"), b"package: bravo\n")?;
                rewrote = true;
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(after.manifest_bytes().unwrap(), b"package: bravo\n");
        assert_ne!(before.content_hash(), after.content_hash());
        std::fs::remove_dir_all(package).unwrap();
    }
}
