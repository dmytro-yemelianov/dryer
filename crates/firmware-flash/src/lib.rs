//! Cross-platform USB discovery and deterministic flash dry-run planning
//! (spec §21.2–§21.4, §29 step 10).
//!
//! This crate deliberately cannot flash a device. It enumerates USB devices
//! through `nusb` (native Linux, macOS, and Windows backends), applies the
//! locked board package's selection rule, verifies the artifact bytes, and
//! emits a stable plan. A missing or ambiguous device and an artifact hash
//! mismatch are represented as blocking plan states. Execution is a later,
//! method-specific slice and must consume — not weaken — these checks.

use dryer_firmware_build::{
    verify_build_plan, verify_controller_image, ControllerBuildPlanArtifact,
};
use dryer_machine_lock::Lockfile;
use dryer_package_model::{board::UsbSelector, LocalRegistry, PackageRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, fs, path::Path};

pub const FLASH_PLAN_SCHEMA: &str = "dryer.flash-plan/v0.2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbSelectionRule {
    pub usb_vid: u16,
    pub usb_pid: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

impl From<&UsbSelector> for UsbSelectionRule {
    fn from(value: &UsbSelector) -> Self {
        Self {
            usb_vid: value.usb_vid,
            usb_pid: value.usb_pid,
            serial_number: value.serial_number.clone(),
            manufacturer: value.manufacturer.clone(),
            product: value.product.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredUsbDevice {
    /// `linux`, `macos`, or `windows` for native discovery. Tests and
    /// imported inventories may use another stable label.
    pub platform: String,
    pub bus_id: String,
    /// Platform-native stable location identity (sysfs path, Windows
    /// instance id, or macOS location id). It is evidence in the plan, not
    /// an implicit substitute for the package's explicit selection rule.
    pub location: String,
    pub device_address: u8,
    pub usb_vid: u16,
    pub usb_pid: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

impl DiscoveredUsbDevice {
    fn matches(&self, rule: &UsbSelectionRule) -> bool {
        self.usb_vid == rule.usb_vid
            && self.usb_pid == rule.usb_pid
            && optional_exact(&self.serial_number, &rule.serial_number)
            && optional_exact(&self.manufacturer, &rule.manufacturer)
            && optional_exact(&self.product, &rule.product)
    }
}

fn optional_exact(observed: &Option<String>, required: &Option<String>) -> bool {
    required
        .as_ref()
        .map_or(true, |required| observed.as_ref() == Some(required))
}

fn device_sort_key(
    device: &DiscoveredUsbDevice,
) -> (&str, &str, &str, u8, u16, u16, &str, &str, &str) {
    (
        &device.platform,
        &device.bus_id,
        &device.location,
        device.device_address,
        device.usb_vid,
        device.usb_pid,
        device.serial_number.as_deref().unwrap_or(""),
        device.manufacturer.as_deref().unwrap_or(""),
        device.product.as_deref().unwrap_or(""),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryError {
    message: String,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "USB discovery failed: {}", self.message)
    }
}

impl std::error::Error for DiscoveryError {}

/// Enumerate USB devices without opening or mutating them.
///
/// `nusb` uses the platform's native USB API. Basic descriptor fields can be
/// absent on some operating systems (notably manufacturer on Windows), so a
/// board recipe should only constrain portable fields it truly requires.
pub fn discover_usb_devices() -> Result<Vec<DiscoveredUsbDevice>, DiscoveryError> {
    let platform = std::env::consts::OS.to_string();
    let devices = nusb::list_devices().map_err(|error| DiscoveryError {
        message: error.to_string(),
    })?;
    let mut result: Vec<_> = devices
        .map(|device| DiscoveredUsbDevice {
            platform: platform.clone(),
            bus_id: device.bus_number().to_string(),
            location: platform_location(&device),
            device_address: device.device_address(),
            usb_vid: device.vendor_id(),
            usb_pid: device.product_id(),
            serial_number: device.serial_number().map(str::to_owned),
            manufacturer: device.manufacturer_string().map(str::to_owned),
            product: device.product_string().map(str::to_owned),
        })
        .collect();
    result.sort_by(|a, b| device_sort_key(a).cmp(&device_sort_key(b)));
    Ok(result)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn platform_location(device: &nusb::DeviceInfo) -> String {
    device.sysfs_path().to_string_lossy().into_owned()
}

#[cfg(target_os = "windows")]
fn platform_location(device: &nusb::DeviceInfo) -> String {
    device.instance_id().to_string_lossy().into_owned()
}

#[cfg(target_os = "macos")]
fn platform_location(device: &nusb::DeviceInfo) -> String {
    format!("location-id:{:08x}", device.location_id())
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows",
    target_os = "macos"
)))]
fn platform_location(device: &nusb::DeviceInfo) -> String {
    format!("{}:{}", device.bus_number(), device.device_address())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    Missing,
    Unique,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSelection {
    pub rule: UsbSelectionRule,
    pub status: MatchStatus,
    pub candidates: Vec<DiscoveredUsbDevice>,
}

/// Match and deterministically order only candidates satisfying every field
/// in the selection rule. Selection never falls back to a partial match.
pub fn match_usb_devices(
    rule: UsbSelectionRule,
    devices: &[DiscoveredUsbDevice],
) -> DeviceSelection {
    let mut candidates: Vec<_> = devices
        .iter()
        .filter(|device| device.matches(&rule))
        .cloned()
        .collect();
    candidates.sort_by(|a, b| device_sort_key(a).cmp(&device_sort_key(b)));
    let status = match candidates.len() {
        0 => MatchStatus::Missing,
        1 => MatchStatus::Unique,
        _ => MatchStatus::Ambiguous,
    };
    DeviceSelection {
        rule,
        status,
        candidates,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ArtifactSpec<'a> {
    pub path: &'a Path,
    pub signature: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct DryRunRequest<'a> {
    pub controller: &'a str,
    pub lock: &'a Lockfile,
    pub build_plan: &'a ControllerBuildPlanArtifact,
    pub registry: &'a LocalRegistry,
    pub discovered_devices: &'a [DiscoveredUsbDevice],
    pub artifact: ArtifactSpec<'a>,
    pub expected_current_firmware: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPlan {
    pub format: String,
    pub path: String,
    pub size_bytes: u64,
    pub expected_sha256: String,
    pub observed_sha256: String,
    pub hash_matches: bool,
    pub deployable: bool,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum PlanStep {
    InspectArtifact,
    SelectDevice,
    CheckCurrentFirmware { expected: String },
    EnterBootloader { instruction: String },
    FlashArtifact { method: String, transport: String },
    VerifyArtifact { method: String },
    ConfirmBoard { expected: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashPlan {
    pub schema: String,
    pub mode: String,
    pub ready: bool,
    pub controller: String,
    pub lock_hash: String,
    /// Exact locked `namespace/name@version` board identity.
    pub board: String,
    pub method: String,
    pub transport: String,
    pub device_selection: DeviceSelection,
    pub expected_current_firmware: String,
    pub artifact: ArtifactPlan,
    pub steps: Vec<PlanStep>,
    pub blocked_reasons: Vec<String>,
    pub recovery: Vec<String>,
}

impl FlashPlan {
    /// Stable human-reviewable JSON (a final newline is part of the format).
    pub fn to_pretty_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("flash plan serializes");
        text.push('\n');
        text
    }
}

#[derive(Debug)]
pub enum PlanError {
    UnknownController(String),
    InvalidLock(String),
    RegistryDrift(String),
    RegistryIo(std::io::Error),
    InvalidBoardMetadata(String),
    MissingFlashMetadata(String),
    InvalidInput(String),
    BuildOutput(String),
    ArtifactIo(std::io::Error),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownController(controller) => {
                write!(f, "controller '{controller}' is not in machine.lock")
            }
            Self::InvalidLock(message) => write!(f, "invalid machine.lock: {message}"),
            Self::RegistryDrift(message) => write!(f, "locked registry drift: {message}"),
            Self::RegistryIo(error) => write!(f, "cannot read locked registry package: {error}"),
            Self::InvalidBoardMetadata(message) => {
                write!(f, "invalid locked board metadata: {message}")
            }
            Self::MissingFlashMetadata(board) => {
                write!(f, "board '{board}' has no usable flash metadata")
            }
            Self::InvalidInput(message) => write!(f, "invalid flash-plan input: {message}"),
            Self::BuildOutput(message) => {
                write!(f, "invalid locked firmware build output: {message}")
            }
            Self::ArtifactIo(error) => write!(f, "cannot read firmware artifact: {error}"),
        }
    }
}

impl std::error::Error for PlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RegistryIo(error) | Self::ArtifactIo(error) => Some(error),
            _ => None,
        }
    }
}

/// Produce a complete, non-mutating plan from locked package metadata,
/// discovered USB inventory, and independently verified artifact bytes.
pub fn plan_dry_run(request: DryRunRequest<'_>) -> Result<FlashPlan, PlanError> {
    request.lock.validate().map_err(PlanError::InvalidLock)?;
    if request.lock.lock_version >= 5 {
        let locked_source = request
            .lock
            .registry_source
            .as_ref()
            .expect("lockfile v5 validation requires a registry source");
        let observed_source = request.registry.source.as_ref().ok_or_else(|| {
            PlanError::RegistryDrift(
                "local registry has no validated portable source descriptor".into(),
            )
        })?;
        if observed_source != locked_source {
            return Err(PlanError::RegistryDrift(format!(
                "registry source expected '{}', {} at {}; observed '{}', {} at {}",
                locked_source.id,
                locked_source.descriptor_hash,
                locked_source.uri,
                observed_source.id,
                observed_source.descriptor_hash,
                observed_source.uri
            )));
        }
    }

    if request.expected_current_firmware.trim().is_empty()
        || request.expected_current_firmware.trim() != request.expected_current_firmware
    {
        return Err(PlanError::InvalidInput(
            "expected_current_firmware must be non-empty with no surrounding whitespace".into(),
        ));
    }
    if request
        .artifact
        .signature
        .is_some_and(|signature| signature.trim().is_empty())
    {
        return Err(PlanError::InvalidInput(
            "artifact signature must not be empty when provided".into(),
        ));
    }

    let controller = request
        .lock
        .controllers
        .get(request.controller)
        .ok_or_else(|| PlanError::UnknownController(request.controller.to_string()))?;
    verify_build_plan(request.lock, request.controller, request.build_plan)
        .map_err(|error| PlanError::BuildOutput(error.to_string()))?;
    let build_plan = request.build_plan;
    let board_lock = exact_locked_board(request.lock, &controller.board)?;
    let board_ref = PackageRef::parse(&board_lock.id)
        .map_err(|error| PlanError::InvalidLock(error.to_string()))?;
    let package = request
        .registry
        .find_version(&board_ref.namespace, &board_ref.name, &board_ref.version)
        .ok_or_else(|| {
            PlanError::RegistryDrift(format!(
                "{} is absent from the local registry",
                board_lock.id
            ))
        })?;

    let manifest = fs::read(package.dir.join("package.yaml")).map_err(PlanError::RegistryIo)?;
    let observed_manifest_hash = sha256(&manifest);
    if observed_manifest_hash != board_lock.manifest_hash {
        return Err(PlanError::RegistryDrift(format!(
            "{} expected {}, observed {}",
            board_lock.id, board_lock.manifest_hash, observed_manifest_hash
        )));
    }
    if request.lock.lock_version >= 2 && board_lock.content_hash.is_empty() {
        return Err(PlanError::InvalidLock(format!(
            "{} has no package content hash in lockfile v{}",
            board_lock.id, request.lock.lock_version
        )));
    }
    if !board_lock.content_hash.is_empty() {
        let observed_content_hash = package.content_hash().map_err(PlanError::RegistryIo)?;
        if observed_content_hash != board_lock.content_hash {
            return Err(PlanError::RegistryDrift(format!(
                "{} package content expected {}, observed {}",
                board_lock.id, board_lock.content_hash, observed_content_hash
            )));
        }
    }

    let board = package.board_payload().map_err(|diagnostics| {
        PlanError::InvalidBoardMetadata(
            diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    let flash = board
        .flash
        .ok_or_else(|| PlanError::MissingFlashMetadata(board_lock.id.clone()))?;
    let method = flash
        .methods
        .get(&flash.default_method)
        .cloned()
        .ok_or_else(|| {
            PlanError::MissingFlashMetadata(format!(
                "{} (default method '{}')",
                board_lock.id, flash.default_method
            ))
        })?;

    let artifact_bytes = fs::read(request.artifact.path).map_err(PlanError::ArtifactIo)?;
    let observed_sha256 = sha256(&artifact_bytes);
    let expected_sha256 = build_plan.expected_artifact.sha256.clone();
    let hash_matches = observed_sha256 == expected_sha256;
    if hash_matches {
        verify_controller_image(
            request.lock,
            request.controller,
            build_plan,
            &artifact_bytes,
        )
        .map_err(|error| PlanError::BuildOutput(error.to_string()))?;
    }
    let artifact = ArtifactPlan {
        format: build_plan.expected_artifact.format.clone(),
        path: build_plan.expected_artifact.path.clone(),
        size_bytes: artifact_bytes.len() as u64,
        expected_sha256: expected_sha256.clone(),
        hash_matches,
        observed_sha256,
        deployable: build_plan.expected_artifact.deployable,
        signature: request.artifact.signature.map(str::to_owned),
    };
    let device_selection = match_usb_devices((&method.select).into(), request.discovered_devices);

    let mut blocked_reasons = Vec::new();
    match device_selection.status {
        MatchStatus::Missing => blocked_reasons.push(format!(
            "no USB device matches {:04x}:{:04x} and all declared string constraints",
            device_selection.rule.usb_vid, device_selection.rule.usb_pid
        )),
        MatchStatus::Ambiguous => blocked_reasons.push(format!(
            "{} USB devices match; refine the board/controller selection rule before flashing",
            device_selection.candidates.len()
        )),
        MatchStatus::Unique => {}
    }
    if !artifact.hash_matches {
        blocked_reasons.push("artifact sha256 does not match the expected build digest".into());
    }
    if !artifact.deployable {
        blocked_reasons.push(format!(
            "artifact format '{}' is an inspectable reference image, not a deployable controller executable",
            artifact.format
        ));
    }

    let mut steps = vec![PlanStep::InspectArtifact, PlanStep::SelectDevice];
    steps.push(PlanStep::CheckCurrentFirmware {
        expected: request.expected_current_firmware.to_string(),
    });
    steps.extend(
        method
            .enter_bootloader
            .iter()
            .cloned()
            .map(|instruction| PlanStep::EnterBootloader { instruction }),
    );
    steps.push(PlanStep::FlashArtifact {
        method: flash.default_method.clone(),
        transport: method.transport.clone(),
    });
    steps.push(PlanStep::VerifyArtifact {
        method: method.verify.clone(),
    });
    steps.push(PlanStep::ConfirmBoard {
        expected: board_lock.id.clone(),
    });

    Ok(FlashPlan {
        schema: FLASH_PLAN_SCHEMA.into(),
        mode: "dry_run".into(),
        ready: blocked_reasons.is_empty(),
        controller: request.controller.to_string(),
        lock_hash: request.lock.lock_hash(),
        board: board_lock.id.clone(),
        method: flash.default_method,
        transport: method.transport.clone(),
        device_selection,
        expected_current_firmware: request.expected_current_firmware.to_string(),
        artifact,
        steps,
        blocked_reasons,
        recovery: method.recovery.clone(),
    })
}

fn exact_locked_board<'a>(
    lock: &'a Lockfile,
    board: &str,
) -> Result<&'a dryer_machine_lock::LockedPackage, PlanError> {
    let prefix = format!("{board}@");
    let matches: Vec<_> = lock
        .packages
        .iter()
        .filter(|package| package.id.starts_with(&prefix))
        .collect();
    match matches.as_slice() {
        [board] => Ok(*board),
        [] => Err(PlanError::InvalidLock(format!(
            "controller board '{board}' has no exact package pin"
        ))),
        _ => Err(PlanError::InvalidLock(format!(
            "controller board '{board}' has multiple exact package pins"
        ))),
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashResult {
    pub success: bool,
    pub controller_id: String,
    pub bytes_written: usize,
    pub execution_log: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlashExecutionError {
    PlanNotReady { reason: String },
    ChecksumMismatch { expected: String, actual: String },
    DeviceNotFound { target: String },
    TransportError { message: String },
}

impl fmt::Display for FlashExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlanNotReady { reason } => write!(f, "flash plan not ready: {reason}"),
            Self::ChecksumMismatch { expected, actual } => {
                write!(f, "artifact checksum mismatch: expected {expected}, actual {actual}")
            }
            Self::DeviceNotFound { target } => write!(f, "flash target device '{target}' not found"),
            Self::TransportError { message } => write!(f, "flash transport error: {message}"),
        }
    }
}

impl std::error::Error for FlashExecutionError {}

pub trait FlashExecutor {
    fn execute_flash(
        &mut self,
        plan: &FlashPlan,
        artifact_bytes: &[u8],
    ) -> Result<FlashResult, FlashExecutionError>;
}

/// In-memory mock executor for flash testing and simulator hardware verification.
#[derive(Debug, Default)]
pub struct MockFlashExecutor {
    pub flashed_artifacts: Vec<(String, Vec<u8>)>,
}

impl FlashExecutor for MockFlashExecutor {
    fn execute_flash(
        &mut self,
        plan: &FlashPlan,
        artifact_bytes: &[u8],
    ) -> Result<FlashResult, FlashExecutionError> {
        if !plan.ready {
            let reason = plan
                .blocked_reasons
                .first()
                .cloned()
                .unwrap_or_else(|| "plan is deployable: false".into());
            return Err(FlashExecutionError::PlanNotReady { reason });
        }

        let actual_hash = format!("sha256:{:x}", Sha256::digest(artifact_bytes));

        if actual_hash != plan.artifact.expected_sha256 {
            return Err(FlashExecutionError::ChecksumMismatch {
                expected: plan.artifact.expected_sha256.clone(),
                actual: actual_hash,
            });
        }

        self.flashed_artifacts
            .push((plan.controller.clone(), artifact_bytes.to_vec()));

        Ok(FlashResult {
            success: true,
            controller_id: plan.controller.clone(),
            bytes_written: artifact_bytes.len(),
            execution_log: vec![
                format!("verified checksum {}", plan.artifact.expected_sha256),
                format!("flashed {} bytes to {}", artifact_bytes.len(), plan.controller),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(address: u8, serial: Option<&str>) -> DiscoveredUsbDevice {
        DiscoveredUsbDevice {
            platform: "test".into(),
            bus_id: "usb-test-0".into(),
            location: format!("fixture-port-{address}"),
            device_address: address,
            usb_vid: 0x1209,
            usb_pid: 0xd003,
            serial_number: serial.map(str::to_owned),
            manufacturer: Some("Example".into()),
            product: Some("Mainboard DFU".into()),
        }
    }

    #[test]
    fn matching_is_strict_and_deterministic() {
        let rule = UsbSelectionRule {
            usb_vid: 0x1209,
            usb_pid: 0xd003,
            serial_number: None,
            manufacturer: Some("Example".into()),
            product: Some("Mainboard DFU".into()),
        };
        let selection = match_usb_devices(rule, &[device(9, None), device(3, None)]);
        assert_eq!(selection.status, MatchStatus::Ambiguous);
        assert_eq!(selection.candidates[0].device_address, 3);
        assert_eq!(selection.candidates[1].device_address, 9);
    }

    #[test]
    fn a_serial_constraint_never_falls_back_to_vid_pid() {
        let rule = UsbSelectionRule {
            usb_vid: 0x1209,
            usb_pid: 0xd003,
            serial_number: Some("wanted".into()),
            manufacturer: None,
            product: None,
        };
        let selection = match_usb_devices(rule, &[device(1, Some("different"))]);
        assert_eq!(selection.status, MatchStatus::Missing);
        assert!(selection.candidates.is_empty());
    }

    #[test]
    fn mock_flash_executor_verifies_checksum_and_records_flashed_bytes() {
        let payload_bytes = b"dryer_controller_image_bytes";
        let hash = format!("sha256:{:x}", Sha256::digest(payload_bytes));

        let plan = FlashPlan {
            schema: FLASH_PLAN_SCHEMA.into(),
            mode: "dfu".into(),
            ready: true,
            controller: "mainboard".into(),
            lock_hash: "sha256:dummy".into(),
            board: "boards/cartesian-mainboard@1.0.0".into(),
            method: "dfu".into(),
            transport: "usb".into(),
            device_selection: DeviceSelection {
                rule: UsbSelectionRule {
                    usb_vid: 0x1209,
                    usb_pid: 0xd003,
                    serial_number: None,
                    manufacturer: None,
                    product: None,
                },
                status: MatchStatus::Unique,
                candidates: vec![device(1, None)],
            },
            expected_current_firmware: "1.0.0".into(),
            artifact: ArtifactPlan {
                format: "bin".into(),
                path: "images/mainboard.bin".into(),
                size_bytes: payload_bytes.len() as u64,
                expected_sha256: hash.clone(),
                observed_sha256: hash.clone(),
                hash_matches: true,
                deployable: true,
                signature: None,
            },
            steps: Vec::new(),
            blocked_reasons: Vec::new(),
            recovery: Vec::new(),
        };

        let mut executor = MockFlashExecutor::default();
        let result = executor.execute_flash(&plan, payload_bytes).unwrap();
        assert!(result.success);
        assert_eq!(result.controller_id, "mainboard");
        assert_eq!(result.bytes_written, payload_bytes.len());
        assert_eq!(executor.flashed_artifacts.len(), 1);

        // Mismatched checksum is rejected
        let bad_result = executor.execute_flash(&plan, b"corrupted_bytes");
        assert!(matches!(bad_result, Err(FlashExecutionError::ChecksumMismatch { .. })));
    }
}
