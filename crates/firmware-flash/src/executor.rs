use crate::{sha256, FlashExecutionError, FlashExecutor, FlashPlan, FlashResult, MatchStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

/// Supported hardware flash tools / execution methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashToolMethod {
    DfuUtil,
    Stm32Flash,
    Bossac,
    Picotool,
}

impl FlashToolMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DfuUtil => "dfu_util",
            Self::Stm32Flash => "stm32flash",
            Self::Bossac => "bossac",
            Self::Picotool => "picotool",
        }
    }

    pub fn default_binary_name(&self) -> &'static str {
        match self {
            Self::DfuUtil => "dfu-util",
            Self::Stm32Flash => "stm32flash",
            Self::Bossac => "bossac",
            Self::Picotool => "picotool",
        }
    }
}

impl FromStr for FlashToolMethod {
    type Err = FlashExecutionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase().replace('-', "_");
        match normalized.as_str() {
            "dfu" | "dfu_util" | "dfu_util_v0" => Ok(Self::DfuUtil),
            "stm32flash" | "stm32_flash" | "stm32" => Ok(Self::Stm32Flash),
            "bossac" | "bossa" => Ok(Self::Bossac),
            "picotool" | "pico" => Ok(Self::Picotool),
            _ => Err(FlashExecutionError::TransportError {
                message: format!("unsupported flash execution method '{s}'"),
            }),
        }
    }
}

impl fmt::Display for FlashToolMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Description of a generated tool command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashCommand {
    pub tool: FlashToolMethod,
    pub program: String,
    pub args: Vec<String>,
}

impl FlashCommand {
    pub fn display_command(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }
}

/// Helper to test if a string represents a serial port device path.
fn is_serial_port(location: &str) -> bool {
    let lower = location.to_lowercase();
    lower.starts_with("/dev/")
        || lower.starts_with("com")
        || lower.contains("tty")
        || lower.contains("serial")
}

/// Generate tool invocation command line for a given flash method, plan, and artifact path.
pub fn generate_command(
    method: FlashToolMethod,
    plan: &FlashPlan,
    artifact_path: &Path,
    binary_override: Option<&str>,
) -> Result<FlashCommand, FlashExecutionError> {
    let program = binary_override
        .unwrap_or_else(|| method.default_binary_name())
        .to_string();

    let candidate = plan.device_selection.candidates.first();
    let artifact_str = artifact_path.to_string_lossy().to_string();

    let args = match method {
        FlashToolMethod::DfuUtil => {
            let mut args = Vec::new();
            if let Some(cand) = candidate {
                let vid_pid = format!("{:04x}:{:04x}", cand.usb_vid, cand.usb_pid);
                args.push("-d".to_string());
                args.push(vid_pid);
                if let Some(ref serial) = cand.serial_number {
                    if !serial.trim().is_empty() {
                        args.push("-S".to_string());
                        args.push(serial.clone());
                    }
                }
            } else {
                let vid_pid = format!(
                    "{:04x}:{:04x}",
                    plan.device_selection.rule.usb_vid, plan.device_selection.rule.usb_pid
                );
                args.push("-d".to_string());
                args.push(vid_pid);
                if let Some(ref serial) = plan.device_selection.rule.serial_number {
                    if !serial.trim().is_empty() {
                        args.push("-S".to_string());
                        args.push(serial.clone());
                    }
                }
            }
            args.push("-D".to_string());
            args.push(artifact_str);
            args.push("-R".to_string());
            args
        }
        FlashToolMethod::Stm32Flash => {
            let mut args = vec![
                "-w".to_string(),
                artifact_str,
                "-v".to_string(),
                "-g".to_string(),
                "0x0".to_string(),
            ];
            if let Some(cand) = candidate {
                if is_serial_port(&cand.location) {
                    args.push(cand.location.clone());
                }
            }
            args
        }
        FlashToolMethod::Bossac => {
            let mut args = Vec::new();
            if let Some(cand) = candidate {
                if is_serial_port(&cand.location) {
                    args.push("-p".to_string());
                    args.push(cand.location.clone());
                }
            }
            args.push("-e".to_string());
            args.push("-w".to_string());
            args.push("-v".to_string());
            args.push("-b".to_string());
            args.push("-R".to_string());
            args.push(artifact_str);
            args
        }
        FlashToolMethod::Picotool => {
            let mut args = Vec::new();
            args.push("load".to_string());
            if let Some(cand) = candidate {
                if !cand.bus_id.is_empty() && cand.device_address > 0 {
                    args.push("--bus".to_string());
                    args.push(cand.bus_id.clone());
                    args.push("--address".to_string());
                    args.push(cand.device_address.to_string());
                }
            }
            args.push(artifact_str);
            args.push("-x".to_string());
            args.push("-v".to_string());
            args
        }
    };

    Ok(FlashCommand {
        tool: method,
        program,
        args,
    })
}

/// Native hardware flash executor that generates and invokes hardware flashing tools
/// (`dfu-util`, `stm32flash`, `bossac`, `picotool`).
#[derive(Debug, Default)]
pub struct NativeFlashExecutor {
    pub dry_run: bool,
    pub custom_temp_dir: Option<PathBuf>,
    pub binary_overrides: HashMap<FlashToolMethod, String>,
}

impl NativeFlashExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dry_run() -> Self {
        Self {
            dry_run: true,
            ..Default::default()
        }
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn with_temp_dir(mut self, path: PathBuf) -> Self {
        self.custom_temp_dir = Some(path);
        self
    }

    pub fn with_binary_override(
        mut self,
        method: FlashToolMethod,
        program_path: impl Into<String>,
    ) -> Self {
        self.binary_overrides.insert(method, program_path.into());
        self
    }

    /// Build the tool invocation command for the given flash plan and artifact path.
    pub fn build_command(
        &self,
        plan: &FlashPlan,
        artifact_path: &Path,
    ) -> Result<FlashCommand, FlashExecutionError> {
        let method: FlashToolMethod = plan.method.parse()?;
        let binary_override = self.binary_overrides.get(&method).map(|s| s.as_str());
        generate_command(method, plan, artifact_path, binary_override)
    }

    /// Validate the flash plan pre-flight checks before attempting execution.
    pub fn validate_plan(
        &self,
        plan: &FlashPlan,
        artifact_bytes: &[u8],
    ) -> Result<FlashToolMethod, FlashExecutionError> {
        if !plan.ready || !plan.blocked_reasons.is_empty() {
            let reason = plan
                .blocked_reasons
                .first()
                .cloned()
                .unwrap_or_else(|| "flash plan is not ready".into());
            return Err(FlashExecutionError::PlanNotReady { reason });
        }

        if plan.device_selection.status != MatchStatus::Unique
            || plan.device_selection.candidates.is_empty()
        {
            return Err(FlashExecutionError::DeviceNotFound {
                target: plan.controller.clone(),
            });
        }

        let actual_hash = sha256(artifact_bytes);
        if actual_hash != plan.artifact.expected_sha256 {
            return Err(FlashExecutionError::ChecksumMismatch {
                expected: plan.artifact.expected_sha256.clone(),
                actual: actual_hash,
            });
        }

        let method: FlashToolMethod = plan.method.parse()?;
        Ok(method)
    }
}

impl FlashExecutor for NativeFlashExecutor {
    fn execute_flash(
        &mut self,
        plan: &FlashPlan,
        artifact_bytes: &[u8],
    ) -> Result<FlashResult, FlashExecutionError> {
        let method = self.validate_plan(plan, artifact_bytes)?;

        // Ensure artifact file exists on disk or write to temp file
        let existing_file = Path::new(&plan.artifact.path);
        let (artifact_path, temp_file_to_clean) = if existing_file.is_file() {
            if let Ok(content) = fs::read(existing_file) {
                if sha256(&content) == plan.artifact.expected_sha256 {
                    (existing_file.to_path_buf(), None)
                } else {
                    let temp_path =
                        create_temp_artifact(artifact_bytes, self.custom_temp_dir.as_deref())?;
                    (temp_path.clone(), Some(temp_path))
                }
            } else {
                let temp_path =
                    create_temp_artifact(artifact_bytes, self.custom_temp_dir.as_deref())?;
                (temp_path.clone(), Some(temp_path))
            }
        } else {
            let temp_path = create_temp_artifact(artifact_bytes, self.custom_temp_dir.as_deref())?;
            (temp_path.clone(), Some(temp_path))
        };

        let binary_override = self.binary_overrides.get(&method).map(|s| s.as_str());
        let flash_cmd = generate_command(method, plan, &artifact_path, binary_override)?;

        let mut execution_log = Vec::new();
        execution_log.push(format!(
            "verified checksum {}",
            plan.artifact.expected_sha256
        ));
        execution_log.push(format!(
            "generated tool command: {}",
            flash_cmd.display_command()
        ));

        let result = if self.dry_run {
            execution_log.push(format!(
                "[dry-run] simulated execution of {} for controller '{}'",
                flash_cmd.program, plan.controller
            ));
            execution_log.push(format!(
                "flashed {} bytes to {}",
                artifact_bytes.len(),
                plan.controller
            ));

            Ok(FlashResult {
                success: true,
                controller_id: plan.controller.clone(),
                bytes_written: artifact_bytes.len(),
                execution_log,
            })
        } else {
            let output = Command::new(&flash_cmd.program)
                .args(&flash_cmd.args)
                .output()
                .map_err(|err| FlashExecutionError::TransportError {
                    message: format!("failed to execute tool '{}': {}", flash_cmd.program, err),
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(FlashExecutionError::TransportError {
                    message: format!(
                        "tool '{}' exited with status {}: {}",
                        flash_cmd.program,
                        output.status,
                        stderr.trim()
                    ),
                })
            } else {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.trim().is_empty() {
                    execution_log.push(format!("stdout: {}", stdout.trim()));
                }
                execution_log.push(format!(
                    "flashed {} bytes to {}",
                    artifact_bytes.len(),
                    plan.controller
                ));

                Ok(FlashResult {
                    success: true,
                    controller_id: plan.controller.clone(),
                    bytes_written: artifact_bytes.len(),
                    execution_log,
                })
            }
        };

        if let Some(path) = temp_file_to_clean {
            let _ = fs::remove_file(path);
        }

        result
    }
}

fn create_temp_artifact(
    bytes: &[u8],
    parent_dir: Option<&Path>,
) -> Result<PathBuf, FlashExecutionError> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = parent_dir
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!(
        "dryer-flash-artifact-{}-{nonce}.bin",
        std::process::id()
    ));
    fs::write(&path, bytes).map_err(|err| FlashExecutionError::TransportError {
        message: format!("cannot write temporary flash artifact: {err}"),
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiscoveredUsbDevice;
    use crate::{ArtifactPlan, DeviceSelection, UsbSelectionRule, FLASH_PLAN_SCHEMA};

    fn make_test_device(address: u8, serial: Option<&str>, location: &str) -> DiscoveredUsbDevice {
        DiscoveredUsbDevice {
            platform: "test".into(),
            bus_id: "1".into(),
            location: location.into(),
            device_address: address,
            usb_vid: 0x1209,
            usb_pid: 0xd003,
            serial_number: serial.map(str::to_owned),
            manufacturer: Some("Example".into()),
            product: Some("Mainboard DFU".into()),
        }
    }

    fn make_test_plan(method: &str, device: DiscoveredUsbDevice, payload: &[u8]) -> FlashPlan {
        let hash = sha256(payload);
        FlashPlan {
            schema: FLASH_PLAN_SCHEMA.into(),
            mode: "dry_run".into(),
            ready: true,
            controller: "mainboard".into(),
            lock_hash: "sha256:dummy".into(),
            board: "boards/cartesian-mainboard@1.0.0".into(),
            method: method.into(),
            transport: "usb".into(),
            device_selection: DeviceSelection {
                rule: UsbSelectionRule {
                    usb_vid: device.usb_vid,
                    usb_pid: device.usb_pid,
                    serial_number: device.serial_number.clone(),
                    manufacturer: None,
                    product: None,
                },
                status: MatchStatus::Unique,
                candidates: vec![device],
            },
            expected_current_firmware: "1.0.0".into(),
            artifact: ArtifactPlan {
                format: "bin".into(),
                path: "test-firmware.bin".into(),
                size_bytes: payload.len() as u64,
                expected_sha256: hash.clone(),
                observed_sha256: hash.clone(),
                hash_matches: true,
                deployable: true,
                signature: None,
            },
            steps: Vec::new(),
            blocked_reasons: Vec::new(),
            recovery: Vec::new(),
        }
    }

    #[test]
    fn test_flash_tool_method_parsing() {
        assert_eq!(
            "dfu".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::DfuUtil
        );
        assert_eq!(
            "dfu-util".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::DfuUtil
        );
        assert_eq!(
            "dfu_util".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::DfuUtil
        );

        assert_eq!(
            "stm32flash".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::Stm32Flash
        );
        assert_eq!(
            "stm32_flash".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::Stm32Flash
        );
        assert_eq!(
            "stm32".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::Stm32Flash
        );

        assert_eq!(
            "bossac".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::Bossac
        );
        assert_eq!(
            "bossa".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::Bossac
        );

        assert_eq!(
            "picotool".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::Picotool
        );
        assert_eq!(
            "pico".parse::<FlashToolMethod>().unwrap(),
            FlashToolMethod::Picotool
        );

        assert!(matches!(
            "invalid_method".parse::<FlashToolMethod>(),
            Err(FlashExecutionError::TransportError { .. })
        ));
    }

    #[test]
    fn test_dfu_util_command_building() {
        let payload = b"dfu_payload_bytes";
        let dev = make_test_device(1, Some("SER123"), "location-1");
        let plan = make_test_plan("dfu_util", dev, payload);

        let executor = NativeFlashExecutor::dry_run();
        let cmd = executor.build_command(&plan, Path::new("fw.bin")).unwrap();

        assert_eq!(cmd.tool, FlashToolMethod::DfuUtil);
        assert_eq!(cmd.program, "dfu-util");
        assert_eq!(
            cmd.args,
            vec!["-d", "1209:d003", "-S", "SER123", "-D", "fw.bin", "-R"]
        );
    }

    #[test]
    fn test_stm32flash_command_building() {
        let payload = b"stm32_payload_bytes";
        let dev = make_test_device(1, None, "/dev/ttyUSB0");
        let plan = make_test_plan("stm32flash", dev, payload);

        let executor = NativeFlashExecutor::dry_run();
        let cmd = executor.build_command(&plan, Path::new("fw.bin")).unwrap();

        assert_eq!(cmd.tool, FlashToolMethod::Stm32Flash);
        assert_eq!(cmd.program, "stm32flash");
        assert_eq!(
            cmd.args,
            vec!["-w", "fw.bin", "-v", "-g", "0x0", "/dev/ttyUSB0"]
        );
    }

    #[test]
    fn test_bossac_command_building() {
        let payload = b"bossac_payload_bytes";
        let dev = make_test_device(1, None, "/dev/ttyACM0");
        let plan = make_test_plan("bossac", dev, payload);

        let executor = NativeFlashExecutor::dry_run();
        let cmd = executor.build_command(&plan, Path::new("fw.bin")).unwrap();

        assert_eq!(cmd.tool, FlashToolMethod::Bossac);
        assert_eq!(cmd.program, "bossac");
        assert_eq!(
            cmd.args,
            vec!["-p", "/dev/ttyACM0", "-e", "-w", "-v", "-b", "-R", "fw.bin"]
        );
    }

    #[test]
    fn test_picotool_command_building() {
        let payload = b"picotool_payload_bytes";
        let dev = make_test_device(4, None, "location-id:0001");
        let plan = make_test_plan("picotool", dev, payload);

        let executor = NativeFlashExecutor::dry_run();
        let cmd = executor.build_command(&plan, Path::new("fw.uf2")).unwrap();

        assert_eq!(cmd.tool, FlashToolMethod::Picotool);
        assert_eq!(cmd.program, "picotool");
        assert_eq!(
            cmd.args,
            vec!["load", "--bus", "1", "--address", "4", "fw.uf2", "-x", "-v"]
        );
    }

    #[test]
    fn test_binary_override_and_dry_run_execution() {
        let payload = b"override_bytes";
        let dev = make_test_device(1, None, "loc1");
        let plan = make_test_plan("dfu", dev, payload);

        let mut executor = NativeFlashExecutor::dry_run()
            .with_binary_override(FlashToolMethod::DfuUtil, "/usr/local/bin/custom-dfu-util");

        let result = executor.execute_flash(&plan, payload).unwrap();
        assert!(result.success);
        assert_eq!(result.controller_id, "mainboard");
        assert_eq!(result.bytes_written, payload.len());
        assert!(result
            .execution_log
            .iter()
            .any(|line| line.contains("/usr/local/bin/custom-dfu-util")));
    }

    #[test]
    fn test_unready_plan_rejection() {
        let payload = b"payload";
        let dev = make_test_device(1, None, "loc1");
        let mut plan = make_test_plan("dfu", dev, payload);
        plan.ready = false;
        plan.blocked_reasons.push("device missing".into());

        let mut executor = NativeFlashExecutor::dry_run();
        let err = executor.execute_flash(&plan, payload).unwrap_err();
        assert!(matches!(err, FlashExecutionError::PlanNotReady { .. }));
    }

    #[test]
    fn test_checksum_mismatch_rejection() {
        let payload = b"payload_bytes";
        let dev = make_test_device(1, None, "loc1");
        let plan = make_test_plan("dfu", dev, payload);

        let mut executor = NativeFlashExecutor::dry_run();
        let err = executor.execute_flash(&plan, b"corrupted").unwrap_err();
        assert!(matches!(err, FlashExecutionError::ChecksumMismatch { .. }));
    }

    #[test]
    fn test_device_not_found_rejection() {
        let payload = b"payload";
        let dev = make_test_device(1, None, "loc1");
        let mut plan = make_test_plan("dfu", dev, payload);
        plan.device_selection.status = MatchStatus::Missing;
        plan.device_selection.candidates.clear();

        let mut executor = NativeFlashExecutor::dry_run();
        let err = executor.execute_flash(&plan, payload).unwrap_err();
        assert!(matches!(err, FlashExecutionError::DeviceNotFound { .. }));
    }
}
