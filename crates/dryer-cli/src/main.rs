//! Unified `dryer` command-line interface.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use dryer_control_protocol::*;
use dryer_firmware_build::plan_controller;
use dryer_firmware_flash::{discover_usb_devices, plan_dry_run, ArtifactSpec, DryRunRequest};
use dryer_machine_lock::{lock as generate_lock, Lockfile};
use dryer_machine_resolver::resolve_source;
use dryer_package_model::LocalRegistry;
use dryer_simulator::*;
use dryer_toolpath_auditor::*;

fn print_usage() {
    println!(
        r#"Dryer Unified Machine Control Platform CLI

USAGE:
    dryer <SUBCOMMAND> [ARGS]

SUBCOMMANDS:
    check <machine.yaml>            Parse, validate, and resolve a machine document
    lock <machine.yaml> [out.lock]  Generate canonical machine.lock file (v5 schema)
    verify-lock <machine.lock>      Verify lockfile package content digests against disk
    flash-plan <machine.lock>       Discover USB devices and generate dry-run flash plan
    audit <job.json>                Run pre-flight kinematics, feed rate & thermal audit
    sim <job.json>                  Execute job in simulator and output execution trace
    daemon <machine.lock> [mcu]     Run host controller daemon state service
    ui                              Open Dryer OS Web Dashboard in browser
    help                            Show this help message
"#
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    let subcommand = args[1].as_str();
    match subcommand {
        "check" => {
            if args.len() < 3 {
                eprintln!("Error: missing <machine.yaml> path");
                std::process::exit(1);
            }
            cmd_check(Path::new(&args[2]))?;
        }
        "lock" => {
            if args.len() < 3 {
                eprintln!("Error: missing <machine.yaml> path");
                std::process::exit(1);
            }
            let out_path = args.get(3).map(PathBuf::from);
            cmd_lock(Path::new(&args[2]), out_path.as_deref())?;
        }
        "verify-lock" => {
            if args.len() < 3 {
                eprintln!("Error: missing <machine.lock> path");
                std::process::exit(1);
            }
            cmd_verify_lock(Path::new(&args[2]))?;
        }
        "ui" => {
            cmd_ui()?;
        }
        "flash-plan" => {
            if args.len() < 3 {
                eprintln!("Error: missing <machine.lock> path");
                std::process::exit(1);
            }
            let controller = args.get(3).map(|s| s.as_str()).unwrap_or("mainboard");
            cmd_flash_plan(Path::new(&args[2]), controller)?;
        }
        "audit" => {
            if args.len() < 3 {
                eprintln!("Error: missing <job.json> path");
                std::process::exit(1);
            }
            cmd_audit(Path::new(&args[2]))?;
        }
        "sim" => {
            if args.len() < 3 {
                eprintln!("Error: missing <job.json> path");
                std::process::exit(1);
            }
            cmd_sim(Path::new(&args[2]))?;
        }
        "daemon" => {
            if args.len() < 3 {
                eprintln!("Error: missing <machine.lock> path");
                std::process::exit(1);
            }
            let controller = args.get(3).map(|s| s.as_str()).unwrap_or("mainboard");
            cmd_daemon(Path::new(&args[2]), controller)?;
        }
        "help" | "-h" | "--help" => {
            print_usage();
        }
        other => {
            eprintln!("Error: unknown subcommand '{other}'");
            print_usage();
            std::process::exit(1);
        }
    }

    Ok(())
}

fn find_registry() -> LocalRegistry {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let candidates = [
        cwd.join("packages"),
        cwd.join("../packages"),
        cwd.join("../../packages"),
    ];
    for cand in &candidates {
        if cand.exists() {
            return LocalRegistry::load(cand);
        }
    }
    LocalRegistry::load(&cwd.join("packages"))
}

fn cmd_check(machine_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(machine_path)?;
    let registry = find_registry();
    let outcome = resolve_source(&source, &registry);

    if outcome.is_ok() {
        println!("✅ Machine document '{:?}' resolved cleanly!", machine_path);
        println!("   Phases run: {:?}", outcome.phases_run);
        if let Some(resolved) = &outcome.resolved {
            println!("   Assignments: {}", resolved.assignments.len());
            println!("   Packages:    {}", resolved.packages.len());
        }
    } else {
        eprintln!(
            "❌ Resolution failed with {} diagnostics:",
            outcome.diagnostics.len()
        );
        for d in &outcome.diagnostics {
            eprintln!("   [{}] {}", d.code, d.message);
        }
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_lock(
    machine_path: &Path,
    out_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(machine_path)?;
    let registry = find_registry();
    let outcome = resolve_source(&source, &registry);

    if !outcome.is_ok() {
        eprintln!("❌ Machine resolution failed. Cannot generate lockfile.");
        for d in &outcome.diagnostics {
            eprintln!("   [{}] {}", d.code, d.message);
        }
        std::process::exit(1);
    }

    let resolved = outcome.resolved.unwrap();
    let lockfile = generate_lock(&source, &registry, &resolved)
        .map_err(|errs| format!("lock generation diagnostics: {:?}", errs))?;
    let lock_yaml = lockfile.to_yaml();

    let target_path = out_path
        .map(PathBuf::from)
        .unwrap_or_else(|| machine_path.with_extension("lock"));
    fs::write(&target_path, &lock_yaml)?;
    println!("✅ Canonical lockfile generated at '{:?}'", target_path);
    println!("   Lock hash: {}", lockfile.lock_hash());
    Ok(())
}

fn cmd_verify_lock(lock_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let lock_bytes = fs::read_to_string(lock_path)?;
    let lockfile = Lockfile::from_yaml(&lock_bytes)?;
    let registry = find_registry();

    println!("🔍 Verifying lockfile '{:?}'", lock_path);
    println!("   Lock hash: {}", lockfile.lock_hash());

    let mut mismatched = 0;
    for locked_pkg in &lockfile.packages {
        let pkg_ref_str = &locked_pkg.id;
        let parsed_ref = dryer_package_model::PackageRef::parse(pkg_ref_str)
            .map_err(|e| format!("invalid locked package ref '{pkg_ref_str}': {e}"))?;

        if let Some(loaded_pkg) =
            registry.find_version(&parsed_ref.namespace, &parsed_ref.name, &parsed_ref.version)
        {
            let disk_hash = loaded_pkg.content_hash().map_err(|e| e.to_string())?;
            if disk_hash == locked_pkg.content_hash {
                println!(
                    "   [OK] {} matches content digest {}",
                    pkg_ref_str, locked_pkg.content_hash
                );
            } else {
                eprintln!(
                    "   [DRIFT] {} content digest mismatch! Expected {}, found {}",
                    pkg_ref_str, locked_pkg.content_hash, disk_hash
                );
                mismatched += 1;
            }
        } else {
            eprintln!("   [MISSING] {} not found in local registry!", pkg_ref_str);
            mismatched += 1;
        }
    }

    if mismatched > 0 {
        eprintln!(
            "❌ Lockfile verification failed with {} package mismatches/drift.",
            mismatched
        );
        std::process::exit(1);
    } else {
        println!(
            "✅ Lockfile verification passed! All {} locked packages matched.",
            lockfile.packages.len()
        );
    }

    Ok(())
}

fn cmd_flash_plan(lock_path: &Path, controller: &str) -> Result<(), Box<dyn std::error::Error>> {
    let lock_bytes = fs::read_to_string(lock_path)?;
    let lockfile = Lockfile::from_yaml(&lock_bytes)?;
    let registry = find_registry();

    let build_plan = plan_controller(&lockfile, controller)?;

    let dummy_img = b"dummy_controller_image_bytes";
    let img_path = env::temp_dir().join("dummy_image.bin");
    fs::write(&img_path, dummy_img)?;

    let discovered = discover_usb_devices().unwrap_or_default();
    let req = DryRunRequest {
        controller,
        lock: &lockfile,
        build_plan: &build_plan,
        registry: &registry,
        discovered_devices: &discovered,
        artifact: ArtifactSpec {
            path: &img_path,
            signature: None,
        },
        expected_current_firmware: "1.0.0",
    };

    let plan = plan_dry_run(req)?;
    println!("{}", plan.to_pretty_json());
    let _ = fs::remove_file(img_path);
    Ok(())
}

fn load_job_commands(job_path: &Path) -> Result<Vec<Command>, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(job_path)?;
    if let Ok(cmds) = serde_json::from_str::<Vec<Command>>(&content) {
        Ok(cmds)
    } else {
        let mut lowerer =
            dryer_gcode_lowerer::GcodeLowerer::new(dryer_gcode_lowerer::LowererConfig::default());
        let cmds = lowerer.lower_source(&content)?;
        Ok(cmds)
    }
}

fn cmd_audit(job_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let commands = load_job_commands(job_path)?;

    let mut axes = BTreeMap::new();
    axes.insert(
        "x".into(),
        AxisLimit {
            min_um: 0,
            max_um: 200_000,
        },
    );
    axes.insert(
        "y".into(),
        AxisLimit {
            min_um: 0,
            max_um: 200_000,
        },
    );

    let mut heaters = BTreeMap::new();
    heaters.insert("hotend_heater".into(), 300_000);

    let auditor = ToolpathAuditor::new(AuditLimits {
        axes,
        max_feed_rate_um_s: 50_000,
        heater_ceilings_milli_c: heaters,
    });

    let report = auditor.audit(&commands);
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.passed {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_sim(job_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let commands = load_job_commands(job_path)?;

    let heater_cfg = HeaterCfg {
        name: "hotend_heater".into(),
        tau_ms: 10_000,
        gain_milli_c: 400_000,
        safe_state: "off".into(),
        heartbeat_timeout: Some(5_000_000),
    };
    let axis_cfg = AxisCfg {
        name: "x".into(),
        start_position_um: 0,
    };
    let mut sim = SimController::new(25_000, vec![heater_cfg], vec![axis_cfg]);

    for (seq, cmd) in commands.into_iter().enumerate() {
        let frame = CommandFrame {
            sequence: seq as u32 + 1,
            envelope: CommandEnvelope {
                execute_at: None,
                command: cmd,
            },
        };
        let mut buf = vec![0u8; MAX_FRAME_LEN];
        let len = encode_command(&frame, &mut buf).unwrap();
        buf.truncate(len);

        sim.process_wire_frame(&buf)
            .map_err(std::io::Error::other)?;
    }

    let mut transport = SimTransport::new(TransportConfig::default());
    sim.run(&mut transport, 5_000 * TICKS_PER_MS);

    println!("{}", sim.trace.to_json_lines());
    Ok(())
}

fn cmd_daemon(lock_path: &Path, controller_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use dryer_controller_daemon::*;

    let lock_bytes = fs::read_to_string(lock_path)?;
    let lockfile = Lockfile::from_yaml(&lock_bytes)?;

    let mut daemon = ControllerDaemon::new();
    daemon.register_controller(controller_id, 50_000);

    println!(
        "⚡ Dryer Controller Daemon initialized for lock '{}'",
        lockfile.lock_hash()
    );
    println!(
        "   Active Controllers: {:?}",
        daemon.active_controller_ids()
    );
    println!("   Daemon State:       {:?}", daemon.state());

    let summary = daemon.daemon_status_summary(1_000);
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn cmd_ui() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let candidates = [
        cwd.join("ui/index.html"),
        cwd.join("../ui/index.html"),
        cwd.join("../../ui/index.html"),
    ];
    let ui_path = candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .unwrap_or_else(|| cwd.join("ui/index.html"));

    println!("🌐 Dryer OS Web Dashboard launched!");
    println!("   Dashboard URI: file://{}", ui_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn cli_check_command_validates_minimal_cartesian_fixture() {
        let root = workspace_root();
        let machine_path = root.join("examples/minimal-cartesian/machine.yaml");
        cmd_check(&machine_path).unwrap();
    }

    #[test]
    fn cli_lock_command_generates_lockfile() {
        let root = workspace_root();
        let machine_path = root.join("examples/minimal-cartesian/machine.yaml");
        let temp_lock =
            env::temp_dir().join(format!("test-dryer-lock-{}.lock", std::process::id()));
        cmd_lock(&machine_path, Some(&temp_lock)).unwrap();
        assert!(temp_lock.exists());
        let content = fs::read_to_string(&temp_lock).unwrap();
        assert!(!content.is_empty(), "lockfile content must not be empty");

        // Verify the newly generated lockfile
        cmd_verify_lock(&temp_lock).unwrap();

        // Verify daemon command
        cmd_daemon(&temp_lock, "mainboard").unwrap();

        let _ = fs::remove_file(temp_lock);
    }

    #[test]
    fn cli_audit_and_sim_accept_gcode_files() {
        let gcode_content = "M104 S200\nG28\nG1 F3000 X10 Y20\n";
        let temp_gcode = env::temp_dir().join(format!("test-gcode-{}.gcode", std::process::id()));
        fs::write(&temp_gcode, gcode_content).unwrap();

        cmd_audit(&temp_gcode).unwrap();
        cmd_sim(&temp_gcode).unwrap();

        let _ = fs::remove_file(temp_gcode);
    }

    #[test]
    fn cli_ui_command_launches_dashboard() {
        cmd_ui().unwrap();
    }
}
