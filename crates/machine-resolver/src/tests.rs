use super::*;
use std::path::Path;

fn registry() -> LocalRegistry {
    LocalRegistry::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages"))
}

fn registry_with_conflicting_template() -> LocalRegistry {
    let mut registry = registry();
    let package = registry
        .packages
        .iter_mut()
        .find(|package| package.reference.to_string() == "machines/cartesian-basic@1.0.0")
        .expect("cartesian template package");
    package.dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/template-conflict");
    registry
}

fn registry_with_safety_fixture(fixture: &str) -> LocalRegistry {
    let mut registry = registry();
    let package = registry
        .packages
        .iter_mut()
        .find(|package| package.reference.to_string() == "safety-profiles/desktop-fdm@1.0.0")
        .expect("desktop FDM safety profile");
    package.dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    registry
}

fn registry_with_downlink_board() -> LocalRegistry {
    let mut registry = registry();
    let package = registry
        .packages
        .iter_mut()
        .find(|package| package.reference.to_string() == "boards/example-mainboard@1.0.0")
        .expect("example mainboard");
    package.dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mainboard-with-downlinks");
    registry
}

fn two_controller_source(child_kind: &str, parent_ref: &str) -> String {
    format!(
        r#"api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: two-controller-test
packages:
  - boards/example-mainboard@1.0.0
  - devices/tmc2209@2.1.0
  - machines/cartesian-basic@1.0.0
controllers:
  mainboard:
    board: boards/example-mainboard
    transport:
      type: usb
  child:
    board: boards/example-mainboard
    transport:
      type: {child_kind}
      parent: {parent_ref}
components:
  x_motor:
    type: stepper_motor
    driver: x_driver
    role: axis.x
  x_driver:
    type: tmc2209
    connected_to: mainboard.motor0
  hotend_heater:
    type: heater
    output: mainboard.heater0
    sensor: hotend_sensor
    current: 2 A
  hotend_sensor:
    type: thermistor
    model: generic-3950
    input: mainboard.thermistor0
kinematics:
  type: cartesian
  limits:
    max_velocity: 300 mm/s
    max_acceleration: 3000 mm/s^2
safety:
  profile: safety-profiles/desktop-fdm
"#
    )
}

fn registry_without_firmware_target() -> (LocalRegistry, std::path::PathBuf) {
    let mut registry = registry();
    let package = registry
        .packages
        .iter_mut()
        .find(|package| package.reference.to_string() == "chips/generic-mcu@1.5.0")
        .expect("latest generic MCU package");
    let source = std::fs::read_to_string(package.dir.join("package.yaml")).unwrap();
    let metadata_start = source.find("\nmemory:\n").unwrap();
    let peripherals_start = source
        .find("\n# Synthetic certified-target budgets")
        .unwrap();
    let stripped = format!(
        "{}{}",
        &source[..metadata_start],
        &source[peripherals_start..]
    );
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!(
        "dryer-resolver-no-target-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&temporary).unwrap();
    std::fs::write(temporary.join("package.yaml"), stripped).unwrap();
    package.dir = temporary.clone();
    (registry, temporary)
}

fn fixture() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/minimal-cartesian/machine.yaml"),
    )
    .unwrap()
}

#[test]
fn the_fixture_machine_resolves_with_expected_assignments() {
    let o = resolve_source(&fixture(), &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    assert_eq!(o.phases_run.len(), 11, "all implemented phases ran");
    assert_eq!(*o.phases_run.last().unwrap(), Phase::ArtifactPlanning);
    let g = o.resolved.unwrap();
    let x = &g.assignments["x_driver"][0];
    assert_eq!(x.resource.0, "mainboard.motor0");
    assert_eq!(x.connector_kind, "stepper_driver_socket");
    let h = &g.assignments["hotend_heater"][0];
    assert_eq!(h.resource.0, "mainboard.heater0");
    let s = &g.assignments["hotend_sensor"][0];
    assert_eq!(s.resource.0, "mainboard.thermistor0");
    assert!(g.explain("x_driver").unwrap().contains("explicit claim"));
}

#[test]
fn resolution_is_deterministic() {
    let a = resolve_source(&fixture(), &registry());
    let b = resolve_source(&fixture(), &registry());
    assert_eq!(a.resolved, b.resolved);
    assert_eq!(
        serde_json::to_string(&a.resolved).unwrap(),
        serde_json::to_string(&b.resolved).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&a.diagnostics).unwrap(),
        serde_json::to_string(&b.diagnostics).unwrap(),
        "diagnostic ranges and related locations are deterministic"
    );
}

#[test]
fn a_double_claim_is_a_conflict_with_actionable_suggestions() {
    let doubled = fixture().replace(
        "  x_driver:\n    type: tmc2209\n    connected_to: mainboard.motor0",
        "  x_driver:\n    type: tmc2209\n    connected_to: mainboard.motor0\n\n  y_driver:\n    type: tmc2209\n    connected_to: mainboard.motor0",
    );
    assert_ne!(doubled, fixture(), "replacement must apply");
    let o = resolve_source(&doubled, &registry());
    assert!(!o.is_ok());
    let conflict = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1200")
        .expect("conflict diagnostic");
    assert!(conflict.message.contains("x_driver") && conflict.message.contains("y_driver"));
    assert!(
        conflict.suggestions[0].contains("mainboard.motor1"),
        "should suggest the free socket: {:?}",
        conflict.suggestions
    );
    let source = conflict.source.as_ref().expect("second claim source");
    assert_eq!(
        source.path.as_deref(),
        Some("components.y_driver.connected_to")
    );
    assert_eq!((source.start.column, source.end.column), (5, 17));
    assert_eq!(conflict.related.len(), 1);
    let first = conflict.related[0]
        .source
        .as_ref()
        .expect("first claim source");
    assert_eq!(
        first.path.as_deref(),
        Some("components.x_driver.connected_to")
    );
    assert_eq!((first.start.column, first.end.column), (5, 17));
}

#[test]
fn template_claim_conflicts_retain_package_source_ranges() {
    let o = resolve_source(&with_rate(), &registry_with_conflicting_template());
    assert!(!o.is_ok());

    let connector = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1200")
        .expect("connector conflict");
    let template_claim = connector.related[0]
        .source
        .as_ref()
        .expect("template connector source");
    assert_eq!(
        template_claim.document.as_deref(),
        Some("package:machines/cartesian-basic@1.0.0/package.yaml")
    );
    assert_eq!(
        template_claim.path.as_deref(),
        Some("template.components.a_template_driver.connected_to")
    );

    let timer = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1314")
        .expect("timer conflict");
    assert_eq!(
        timer
            .source
            .as_ref()
            .and_then(|source| source.path.as_deref()),
        Some("template.components.b_template_driver.connected_to")
    );
    assert_eq!(
        timer.related[0]
            .source
            .as_ref()
            .and_then(|source| source.path.as_deref()),
        Some("template.components.a_template_driver.connected_to")
    );
    assert!(timer.source.as_ref().unwrap().document.is_some());
    assert!(timer.related[0].source.as_ref().unwrap().document.is_some());
}

#[test]
fn wrong_connector_kind_is_rejected() {
    let wrong = fixture().replace("output: mainboard.heater0", "output: mainboard.thermistor0");
    let o = resolve_source(&wrong, &registry());
    assert!(!o.is_ok());
    assert!(o.diagnostics.iter().any(|d| d.code == "E1202"));
}

#[test]
fn unknown_connector_lists_available_ones() {
    let wrong = fixture().replace(
        "connected_to: mainboard.motor0",
        "connected_to: mainboard.motor9",
    );
    let o = resolve_source(&wrong, &registry());
    let d = o.diagnostics.iter().find(|d| d.code == "E1201").unwrap();
    assert!(d.suggestions[0].contains("motor0"));
}

#[test]
fn missing_package_and_version_mismatch_stop_resolution_in_phase_3() {
    let missing = fixture().replace("boards/example-mainboard@1.0.0", "boards/ghost-board@1.0.0");
    let o = resolve_source(&missing, &registry());
    assert!(o.diagnostics.iter().any(|d| d.code == "E1100"));
    assert_eq!(*o.phases_run.last().unwrap(), Phase::PackageDependencies);

    let mismatched = fixture().replace("devices/tmc2209@2.1.0", "devices/tmc2209@9.9.9");
    let o = resolve_source(&mismatched, &registry());
    assert!(o.diagnostics.iter().any(|d| d.code == "E1101"));
}

#[test]
fn unknown_transport_on_the_board_is_an_error() {
    let bad = fixture().replace("type: usb", "type: ethernet");
    let o = resolve_source(&bad, &registry());
    assert!(o.diagnostics.iter().any(|d| d.code == "E1120"));
}

#[test]
fn transport_parent_naming_an_undeclared_downlink_is_e1121() {
    // registry() alone: example-mainboard@1.0.0 declares no downlinks at all.
    let src = two_controller_source("can", "mainboard.can0");
    let o = resolve_source(&src, &registry());
    let d = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1121")
        .expect("E1121 diagnostic");
    assert!(d.message.contains("can0"));
    assert!(d.message.contains("boards/example-mainboard"));
}

#[test]
fn transport_type_disagreeing_with_parent_downlink_is_e1122_not_e1121() {
    let src = two_controller_source("usb", "mainboard.can0");
    let o = resolve_source(&src, &registry_with_downlink_board());
    assert!(
        !o.diagnostics.iter().any(|d| d.code == "E1121"),
        "port exists on the board, E1121 must not fire: {:?}",
        o.diagnostics
    );
    let d = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1122")
        .expect("E1122 diagnostic");
    assert!(d.message.contains("usb"));
    assert!(d.message.contains("can"));
}

#[test]
fn matching_downlink_port_and_type_raises_neither_e1121_nor_e1122() {
    let src = two_controller_source("can", "mainboard.can0");
    let o = resolve_source(&src, &registry_with_downlink_board());
    assert!(
        !o.diagnostics
            .iter()
            .any(|d| d.code == "E1121" || d.code == "E1122"),
        "matching port+type must raise neither: {:?}",
        o.diagnostics
    );
}

#[test]
fn self_parenting_controller_is_e1123() {
    // child's own transport.parent points at itself.
    let src = two_controller_source("can", "child.can0");
    let o = resolve_source(&src, &registry_with_downlink_board());
    let d = o
        .diagnostics
        .iter()
        .find(|d| d.code == "E1123")
        .expect("E1123 diagnostic for self-parenting");
    assert!(d.message.contains("child"));
}

#[test]
fn mutual_parent_cycle_is_e1123() {
    let src = r#"api_version: dryer.machine/v0.1
kind: Machine
metadata:
  name: cycle-test
packages:
  - boards/example-mainboard@1.0.0
  - devices/tmc2209@2.1.0
  - machines/cartesian-basic@1.0.0
controllers:
  a:
    board: boards/example-mainboard
    transport:
      type: can
      parent: b.can0
  b:
    board: boards/example-mainboard
    transport:
      type: can
      parent: a.can0
components:
  x_motor:
    type: stepper_motor
    driver: x_driver
    role: axis.x
  x_driver:
    type: tmc2209
    connected_to: a.motor0
  hotend_heater:
    type: heater
    output: a.heater0
    sensor: hotend_sensor
    current: 2 A
  hotend_sensor:
    type: thermistor
    model: generic-3950
    input: a.thermistor0
kinematics:
  type: cartesian
  limits:
    max_velocity: 300 mm/s
    max_acceleration: 3000 mm/s^2
safety:
  profile: safety-profiles/desktop-fdm
"#;
    let o = resolve_source(src, &registry_with_downlink_board());
    let cycles: Vec<_> = o.diagnostics.iter().filter(|d| d.code == "E1123").collect();
    assert_eq!(cycles.len(), 1, "one cycle reported once, not twice: {:?}", o.diagnostics);
}

/// Search-based allocation: a tmc2209 with no explicit claim gets the
/// first FREE stepper socket (motor0 is claimed by x_driver), and its
/// provenance lists every kind-compatible candidate.
#[test]
fn an_unclaimed_device_is_search_allocated_with_full_candidates() {
    // The template's y_driver is itself the search-allocated component:
    // motor0 is explicitly claimed, so it lands on motor1 with every
    // kind-compatible connector recorded.
    let o = resolve_source(&fixture(), &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let g = o.resolved.unwrap();
    let y = &g.assignments["y_driver"][0];
    assert_eq!(
        y.resource.0, "mainboard.motor1",
        "motor0 is already claimed"
    );
    assert_eq!(
        y.candidates_considered,
        vec![
            "mainboard.motor0".to_string(),
            "mainboard.motor1".to_string(),
            "mainboard.motor2".to_string(),
            "mainboard.motor3".to_string()
        ],
        "all kind-compatible connectors are recorded"
    );
    assert!(y.via.contains("devices/tmc2209"));
}

#[test]
fn exhausting_the_sockets_is_a_typed_error() {
    // four sockets: x explicit + template y + z + e fill them; w exhausts
    let with_three = fixture().replace(
        "kinematics:",
        "  z_driver:\n    type: tmc2209\n\n  e_driver:\n    type: tmc2209\n\n  w_driver:\n    type: tmc2209\n\nkinematics:",
    );
    let o = resolve_source(&with_three, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1205").unwrap();
    assert!(d.message.contains("no free stepper_driver_socket"));
}

/// The fixture heater declares `current: 2 A` against a 5 A connector
/// and passes; raising the draw beyond the connector limit fails.
#[test]
fn electrical_validation_compares_draw_against_connector_limit() {
    let over = fixture().replace("current: 2 A", "current: 6 A");
    assert_ne!(over, fixture());
    let o = resolve_source(&over, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1300").unwrap();
    assert!(d.message.contains("6 A") && d.message.contains("5 A"));
    assert_eq!(*o.phases_run.last().unwrap(), Phase::ElectricalValidation);
}

#[test]
fn a_malformed_current_declaration_is_its_own_error() {
    let bad = fixture().replace("current: 2 A", "current: quite a lot");
    let o = resolve_source(&bad, &registry());
    assert!(o.diagnostics.iter().any(|d| d.code == "E1301"));
}

/// The safety profile is an implicit dependency root, so a missing one
/// now fails in phase 3 (E1100) — before any allocation work — rather
/// than surviving to the safety phase.
#[test]
fn a_missing_safety_profile_fails_resolution_at_the_package_phase() {
    let bad = fixture().replace("safety-profiles/desktop-fdm", "safety-profiles/ghost");
    let o = resolve_source(&bad, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1100").unwrap();
    assert!(d.message.contains("safety-profiles/ghost"));
    assert_eq!(*o.phases_run.last().unwrap(), Phase::PackageDependencies);
}

/// A component driving a power output whose class the profile does not
/// cover is the §30 "unresolved safety default" — a hard error.
#[test]
fn an_uncovered_hazardous_output_is_rejected() {
    let laser = fixture().replace("    type: heater", "    type: laser");
    assert_ne!(laser, fixture());
    let o = resolve_source(&laser, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1501").unwrap();
    assert!(d.message.contains("laser"));
}

/// Graph expansion: the pinned machine class contributes `y_driver`
/// (search-allocated to the free socket) and a default limit, each
/// surfaced as an Info diagnostic; the source graph is never mutated.
#[test]
fn the_machine_template_expands_components_and_default_limits() {
    let o = resolve_source(&fixture(), &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let g = o.resolved.as_ref().unwrap();
    let y = &g.assignments["y_driver"][0];
    assert_eq!(y.resource.0, "mainboard.motor1");
    assert!(y.via.contains("devices/tmc2209"));
    let infos: Vec<&str> = o
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Info)
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        infos.iter().any(|m| m.contains("y_driver")),
        "component contribution surfaced: {infos:?}"
    );
    assert!(
        infos.iter().any(|m| m.contains("max_z_velocity")),
        "limit default surfaced: {infos:?}"
    );
}

/// Source-wins: a source component with the template's name shadows it.
#[test]
fn a_source_component_shadows_the_template() {
    let with_own_y = fixture().replace(
        "kinematics:",
        "  y_driver:\n    type: tmc2209\n    connected_to: mainboard.motor1\n\nkinematics:",
    );
    let o = resolve_source(&with_own_y, &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let g = o.resolved.as_ref().unwrap();
    let y = &g.assignments["y_driver"][0];
    assert_eq!(y.via, "connected_to", "the explicit source claim won");
    assert!(o
        .diagnostics
        .iter()
        .any(|d| d.code == "I1132" && d.message.contains("y_driver")));
}

/// A kinematics-type mismatch between source and machine class is
/// surfaced as a warning, never silently overridden.
#[test]
fn a_kinematics_mismatch_with_the_machine_class_warns() {
    let corexy = fixture().replace("  type: cartesian", "  type: corexy");
    let o = resolve_source(&corexy, &registry());
    assert!(o.is_ok(), "warning, not error: {:#?}", o.diagnostics);
    let d = o.diagnostics.iter().find(|d| d.code == "E1130").unwrap();
    assert!(d.message.contains("cartesian") && d.message.contains("corexy"));
}

/// §9 bus matching: the adxl345 (spi >= 1 MHz, logic_3v3) is
/// search-allocated to accel0, whose pins reach spi1 (42 MHz); the
/// gpio-only accel1 is skipped as a hard filter.
#[test]
fn a_bus_requirement_steers_search_to_a_capable_socket() {
    let with_accel = fixture().replace("kinematics:", "  imu:\n    type: adxl345\n\nkinematics:");
    let o = resolve_source(&with_accel, &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let g = o.resolved.unwrap();
    let imu = &g.assignments["imu"][0];
    assert_eq!(imu.resource.0, "mainboard.accel0");
    assert!(
        imu.constraints_applied
            .iter()
            .any(|c| c.contains("spi bus via spi1")),
        "constraints: {:?}",
        imu.constraints_applied
    );
}

/// An explicit claim onto a socket with no SPI function is E1315.
#[test]
fn an_explicit_claim_without_the_required_bus_is_rejected() {
    let bad = fixture().replace(
        "kinematics:",
        "  imu:\n    type: adxl345\n    connected_to: mainboard.accel1\n\nkinematics:",
    );
    let o = resolve_source(&bad, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1315").unwrap();
    assert!(d.message.contains("no spi function"), "{}", d.message);
}

/// A bus-frequency demand above what the chip declares fails with
/// both numbers in the message.
#[test]
fn a_bus_frequency_above_the_chips_ceiling_is_rejected() {
    let bad = fixture().replace(
        "kinematics:",
        "  cam:\n    type: fast-cam\n    connected_to: mainboard.accel0\n\nkinematics:",
    );
    let o = resolve_source(&bad, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1315").unwrap();
    assert!(
        d.message.contains("42 MHz") && d.message.contains("80 MHz"),
        "{}",
        d.message
    );
}

/// DMA routes and measured timing bounds are hard requirements and
/// become human-readable assignment provenance when accepted.
#[test]
fn dma_and_timing_budgets_are_matched_and_recorded() {
    let source = fixture().replace(
        "kinematics:",
        "  stream_sensor:\n    type: dma-stream-sensor\n\nkinematics:",
    );
    let outcome = resolve_source(&source, &registry());
    assert!(outcome.is_ok(), "diagnostics: {:#?}", outcome.diagnostics);
    let graph = outcome.resolved.unwrap();
    let assignment = &graph.assignments["stream_sensor"][0];
    assert_eq!(assignment.resource.0, "mainboard.accel0");
    for expected in [
        "worst-case bus latency 20 us <= 50 us",
        "worst-case bus jitter 3 us <= 10 us",
        "DMA route spi1.rx via dma1.ch0",
    ] {
        assert!(
            assignment
                .constraints_applied
                .iter()
                .any(|constraint| constraint == expected),
            "missing '{expected}' in {:?}",
            assignment.constraints_applied
        );
    }
}

#[test]
fn latency_jitter_and_dma_failures_name_the_missing_budget() {
    let registry = registry();
    let chip = registry
        .find("chips", "generic-mcu")
        .unwrap()
        .chip_payload()
        .unwrap();
    let caps = BTreeMap::from([
        ("sck".to_string(), vec!["spi2.sck".to_string()]),
        ("miso".to_string(), vec!["spi2.miso".to_string()]),
    ]);
    let requirement = |max_latency, max_jitter, dma_signals| dryer_package_model::device::BusReq {
        kind: "spi".to_string(),
        min_frequency: None,
        dma_signals,
        max_latency,
        max_jitter,
    };

    let latency = bus_satisfied(
        Some(&chip),
        &caps,
        &requirement(Some("50 us".to_string()), None, Vec::new()),
    )
    .unwrap_err();
    assert!(latency.contains("worst_case_latency 80 us"), "{latency}");

    let jitter = bus_satisfied(
        Some(&chip),
        &caps,
        &requirement(None, Some("10 us".to_string()), Vec::new()),
    )
    .unwrap_err();
    assert!(jitter.contains("worst_case_jitter 20 us"), "{jitter}");

    let dma = bus_satisfied(
        Some(&chip),
        &caps,
        &requirement(None, None, vec!["rx".to_string()]),
    )
    .unwrap_err();
    assert!(dma.contains("no DMA channel routes 'spi2.rx'"), "{dma}");
}

/// Pin capabilities ride every assignment: the explicit x_driver claim
/// carries the chip functions behind motor0's pins.
#[test]
fn assignments_carry_derived_pin_capabilities() {
    let o = resolve_source(&fixture(), &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let g = o.resolved.unwrap();
    let x = &g.assignments["x_driver"][0];
    assert_eq!(x.pin_capabilities["step"], vec!["tim1.ch2", "gpio"]);
    assert_eq!(x.pin_capabilities["dir"], vec!["gpio"]);
    assert!(g.explain("x_driver").unwrap().contains("tim1.ch2"));
}

fn with_rate() -> String {
    fixture().replace("  limits:", "  limits:\n    max_step_rate: 100 kHz")
}

/// With a step-rate budget, search allocation steers around sockets
/// whose step pin lacks a timer and reserves the chosen channel.
#[test]
fn a_step_rate_budget_steers_search_to_timer_backed_sockets() {
    let o = resolve_source(&with_rate(), &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let g = o.resolved.unwrap();
    let y = &g.assignments["y_driver"][0];
    assert_eq!(
        y.resource.0, "mainboard.motor1",
        "motor1's PD5 carries tim3.ch3; motor2 shares x's tim1.ch2"
    );
    assert!(y.constraints_applied.iter().any(|c| c.contains("tim3.ch3")));
}

/// An explicit claim onto a gpio-only step pin violates the budget.
#[test]
fn a_declared_step_rate_rejects_gpio_only_step_pins() {
    let bad = with_rate().replace(
        "kinematics:",
        "  z_driver:\n    type: tmc2209\n    connected_to: mainboard.motor3\n\nkinematics:",
    );
    let o = resolve_source(&bad, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1310").unwrap();
    assert!(d.message.contains("PD7"), "{}", d.message);
}

/// Two sockets multiplexed onto one timer channel cannot both step:
/// the spec's own E1204-style conflict, now with real operands.
#[test]
fn a_shared_timer_channel_is_a_conflict_naming_both_components() {
    let bad = with_rate().replace(
        "kinematics:",
        "  z_driver:\n    type: tmc2209\n    connected_to: mainboard.motor2\n\nkinematics:",
    );
    let o = resolve_source(&bad, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1314").unwrap();
    assert!(
        d.message.contains("x_driver") && d.message.contains("tim1.ch2"),
        "{}",
        d.message
    );
    assert_eq!(d.related.len(), 1);
    assert_eq!(
        d.source.as_ref().and_then(|s| s.path.as_deref()),
        Some("components.z_driver.connected_to")
    );
    assert_eq!(
        d.related[0].source.as_ref().and_then(|s| s.path.as_deref()),
        Some("components.x_driver.connected_to")
    );
}

/// Voltage domains are checked on explicit claims: legacy-probe
/// requires logic_5v, and thermistor0 declares no domain — silence
/// never satisfies an electrical requirement.
#[test]
fn a_voltage_domain_requirement_rejects_an_undeclared_connector() {
    let probed = fixture().replace(
        "  hotend_sensor:\n    type: thermistor\n    model: generic-3950\n    input: mainboard.thermistor0",
        "  hotend_sensor:\n    type: thermistor\n    model: generic-3950\n\n  z_probe:\n    type: legacy-probe\n    input: mainboard.thermistor0",
    );
    assert_ne!(probed, fixture(), "replacement must apply");
    let o = resolve_source(&probed, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1302").unwrap();
    assert!(
        d.message.contains("logic_5v") && d.message.contains("none"),
        "{}",
        d.message
    );
}

/// And on search allocation the domain is a hard candidate filter:
/// the tmc2209 requirement (logic_3v3) matches the fixture sockets,
/// and the constraint is recorded in the assignment provenance.
#[test]
fn search_allocation_records_the_voltage_domain_constraint() {
    let o = resolve_source(&fixture(), &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let g = o.resolved.unwrap();
    let y = &g.assignments["y_driver"][0];
    assert!(
        y.constraints_applied
            .iter()
            .any(|c| c.contains("logic_3v3")),
        "constraints: {:?}",
        y.constraints_applied
    );
}

/// The closure pulls tmc2209's chip dependency at the highest version
/// satisfying its range, and records implicit roots (board, safety
/// profile) alongside explicit pins.
#[test]
fn the_closure_selects_transitive_dependencies_at_max_satisfying_version() {
    let o = resolve_source(&fixture(), &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let pkgs = o.resolved.unwrap().packages;
    assert!(
        pkgs.contains(&"chips/generic-mcu@1.5.0".to_string()),
        "transitive dep at max satisfying (1.5.0 supersedes 1.4.0): {pkgs:?}"
    );
    assert!(pkgs.contains(&"boards/example-mainboard@1.0.0".to_string()));
    assert!(pkgs.contains(&"safety-profiles/desktop-fdm@1.0.0".to_string()));
    // the unpinned implicit safety profile carries a warning, not an error
    assert!(o.diagnostics.iter().any(|d| d.code == "E1106"));
}

/// A machine pin that a dependency range excludes is an error — pins
/// are absolute, never silently overridden (§11.4).
#[test]
fn a_machine_pin_excluded_by_a_dependency_range_is_rejected() {
    let pinned_old = fixture().replace(
        "  - devices/tmc2209@2.1.0",
        "  - devices/tmc2209@2.1.0\n  - chips/generic-mcu@1.0.0",
    );
    let o = resolve_source(&pinned_old, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1103").unwrap();
    assert!(d.message.contains("devices/tmc2209") && d.message.contains("1.0.0"));
    assert_eq!(
        d.source.as_ref().and_then(|s| s.path.as_deref()),
        Some("packages[2]")
    );
    assert_eq!(d.related.len(), 1);
    let requirement = d.related[0]
        .source
        .as_ref()
        .expect("dependency requirement source");
    assert_eq!(
        requirement.document.as_deref(),
        Some("package:devices/tmc2209@2.1.0/package.yaml")
    );
    assert_eq!(
        requirement.path.as_deref(),
        Some("dependencies.chips/generic-mcu")
    );
}

/// Two requirers with disjoint ranges on one package: no version can
/// satisfy the intersection — a conflict naming both requirers.
#[test]
fn disjoint_dependency_ranges_are_a_conflict_naming_both_requirers() {
    let both = fixture().replace(
        "  - devices/tmc2209@2.1.0",
        "  - devices/tmc2209@2.1.0\n  - devices/legacy-probe@1.0.0",
    );
    let o = resolve_source(&both, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1104").unwrap();
    assert!(
        d.message.contains("tmc2209") && d.message.contains("legacy-probe"),
        "both requirers named: {}",
        d.message
    );
    assert!(d.message.contains("available: 1.0.0, 1.2.0"));
    let mut sources: Vec<_> = d
        .source
        .iter()
        .chain(d.related.iter().filter_map(|r| r.source.as_ref()))
        .map(|source| {
            (
                source.document.clone().unwrap(),
                source.path.clone().unwrap(),
            )
        })
        .collect();
    sources.sort();
    assert_eq!(
        sources,
        vec![
            (
                "package:devices/legacy-probe@1.0.0/package.yaml".to_string(),
                "dependencies.chips/generic-mcu".to_string(),
            ),
            (
                "package:devices/tmc2209@2.1.0/package.yaml".to_string(),
                "dependencies.chips/generic-mcu".to_string(),
            ),
        ]
    );
    let repeated = resolve_source(&both, &registry());
    assert_eq!(
        serde_json::to_string(&o.diagnostics).unwrap(),
        serde_json::to_string(&repeated.diagnostics).unwrap(),
        "multi-source conflict output must be byte-stable"
    );
}

#[test]
fn safe_states_are_partitioned_to_concrete_controller_resources() {
    let outcome = resolve_source(&fixture(), &registry());
    assert!(outcome.is_ok(), "diagnostics: {:#?}", outcome.diagnostics);
    assert_eq!(outcome.phases_run.last(), Some(&Phase::ArtifactPlanning));
    let graph = outcome.resolved.unwrap();
    let safety = &graph.controller_safety["mainboard"];
    assert_eq!(safety.len(), 2, "{safety:#?}");

    let heater = safety
        .iter()
        .find(|binding| binding.component == "hotend_heater")
        .unwrap();
    assert_eq!(heater.resource.0, "mainboard.heater0");
    assert_eq!(heater.state, dryer_package_model::safety::SafeState::Off);
    assert_eq!(heater.heartbeat_timeout_us, Some(500_000));
    assert_eq!(
        heater.sensor.as_ref().map(|resource| resource.0.as_str()),
        Some("mainboard.thermistor0")
    );

    let motor = safety
        .iter()
        .find(|binding| binding.component == "x_motor")
        .unwrap();
    assert_eq!(motor.resource.0, "mainboard.motor0");
    assert_eq!(
        motor.state,
        dryer_package_model::safety::SafeState::Disabled
    );
    assert!(graph
        .explain("x_motor")
        .unwrap()
        .contains("safe_state--> mainboard.motor0 = disabled"));
}

#[test]
fn controller_build_inputs_are_planned_from_exact_target_packages() {
    let outcome = resolve_source(&fixture(), &registry());
    assert!(outcome.is_ok(), "diagnostics: {:#?}", outcome.diagnostics);
    let graph = outcome.resolved.unwrap();
    let plan = &graph.controller_build_plans["mainboard"];
    assert_eq!(plan.board, "boards/example-mainboard@1.0.0");
    assert_eq!(plan.chip, "chips/generic-mcu@1.5.0");
    assert_eq!(plan.target_triple, "thumbv7em-none-eabihf");
    assert_eq!(plan.toolchain, "rustc-1.85.0");
    assert_eq!(plan.build_profile, "release");
    assert_eq!(plan.protocol_version, "dryer.control/v1");
    assert_eq!(plan.abi_version, "dryer.controller/v1");
    assert_eq!(plan.flash_bytes, 524_288);
    assert_eq!(plan.ram_bytes, 131_072);
    assert_eq!(plan.bootloader_offset_bytes, 16_384);
    assert_eq!(
        plan.features,
        ["adc", "pwm", "step_scheduler", "usb_transport"]
    );
    assert_eq!(plan.native_drivers, ["devices/tmc2209@2.1.0"]);
}

#[test]
fn artifact_planning_rejects_a_chip_without_firmware_target_metadata() {
    let (registry, temporary) = registry_without_firmware_target();
    let outcome = resolve_source(&fixture(), &registry);
    std::fs::remove_dir_all(temporary).unwrap();
    assert!(!outcome.is_ok());
    assert_eq!(outcome.phases_run.last(), Some(&Phase::ArtifactPlanning));
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E1600")
        .unwrap();
    assert!(
        diagnostic.message.contains("memory, boot, or firmware"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn an_unresolved_required_sensor_cannot_enter_controller_firmware() {
    let bad = fixture().replace("    input: mainboard.thermistor0\n", "");
    assert_ne!(bad, fixture(), "replacement must apply");
    let outcome = resolve_source(&bad, &registry());
    assert!(!outcome.is_ok());
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E1503")
        .unwrap();
    assert!(
        diagnostic.message.contains("hotend_sensor"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn a_driver_backed_actuator_requires_profile_coverage() {
    let outcome = resolve_source(
        &fixture(),
        &registry_with_safety_fixture("safety-no-stepper"),
    );
    assert!(!outcome.is_ok());
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E1501")
        .unwrap();
    assert!(
        diagnostic.message.contains("x_motor") && diagnostic.message.contains("stepper_motor"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn an_actuator_driver_must_resolve_to_a_driver_socket() {
    let bad = fixture().replace("    driver: x_driver\n", "    driver: hotend_sensor\n");
    assert_ne!(bad, fixture(), "replacement must apply");
    let outcome = resolve_source(&bad, &registry());
    assert!(!outcome.is_ok());
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E1506")
        .unwrap();
    assert!(
        diagnostic.message.contains("thermistor0") && diagnostic.message.contains("analog_input"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn a_required_sensor_must_resolve_to_an_input_connector() {
    let bad = fixture().replace("    sensor: hotend_sensor\n", "    sensor: x_driver\n");
    assert_ne!(bad, fixture(), "replacement must apply");
    let outcome = resolve_source(&bad, &registry());
    assert!(!outcome.is_ok());
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E1507")
        .unwrap();
    assert!(
        diagnostic.message.contains("x_driver"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn one_physical_resource_cannot_receive_two_safety_actions() {
    let outcome = resolve_source(
        &fixture(),
        &registry_with_safety_fixture("safety-driver-conflict"),
    );
    assert!(!outcome.is_ok());
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E1508")
        .unwrap();
    assert!(
        diagnostic.message.contains("x_motor")
            && diagnostic.message.contains("x_driver")
            && diagnostic.message.contains("mainboard.motor0"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn a_safe_actuator_without_a_controller_resource_is_rejected() {
    let bad = fixture().replace("    driver: x_driver\n", "");
    assert_ne!(bad, fixture(), "replacement must apply");
    let outcome = resolve_source(&bad, &registry());
    assert!(!outcome.is_ok());
    let diagnostic = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E1505")
        .unwrap();
    assert!(
        diagnostic.message.contains("x_motor"),
        "{}",
        diagnostic.message
    );
}

#[test]
fn a_sensorless_heater_violates_the_profile() {
    let sensorless = fixture().replace("    sensor: hotend_sensor\n", "");
    assert_ne!(sensorless, fixture());
    let o = resolve_source(&sensorless, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1502").unwrap();
    assert!(d.message.contains("hotend_heater"));
}

#[test]
fn invalid_safety_profile_name_syntax_emits_e1500() {
    let bad = fixture().replace(
        "profile: safety-profiles/desktop-fdm",
        "profile: invalid_profile_without_slash",
    );
    let o = resolve_source(&bad, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1500").unwrap();
    assert!(d.message.contains("must be 'namespace/name'"));
}

#[test]
fn unpinned_safety_profile_name_with_invalid_chars_emits_e1500() {
    let bad = fixture().replace(
        "profile: safety-profiles/desktop-fdm",
        "profile: bad space/desktop-fdm",
    );
    let o = resolve_source(&bad, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1500").unwrap();
    assert!(d.message.contains("must be 'namespace/name'"));
}

#[test]
fn referenced_workflow_packages_are_included_in_resolution_closure() {
    let with_workflow = format!("{}\nworkflows:\n  start: workflows/print-start\n", fixture());
    let o = resolve_source(&with_workflow, &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
    let pkgs = o.resolved.unwrap().packages;
    assert!(
        pkgs.contains(&"workflows/print-start@1.0.0".to_string()),
        "workflow package must resolve into package closure: {pkgs:?}"
    );
}

#[test]
fn valid_workflow_parameter_invocation_resolves_cleanly() {
    let with_params = format!(
        "{}\nworkflows:\n  start:\n    package: workflows/print-start\n    parameters:\n      bed_temperature: \"60 degC\"\n",
        fixture()
    );
    let o = resolve_source(&with_params, &registry());
    assert!(o.is_ok(), "diagnostics: {:#?}", o.diagnostics);
}

#[test]
fn workflow_invocation_with_unknown_parameter_emits_e1701() {
    let bad_params = format!(
        "{}\nworkflows:\n  start:\n    package: workflows/print-start\n    parameters:\n      unknown_param: 42\n",
        fixture()
    );
    let o = resolve_source(&bad_params, &registry());
    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1701").unwrap();
    assert!(
        d.message.contains("unknown_param") && d.message.contains("workflows/print-start"),
        "{}",
        d.message
    );
}

#[test]
fn workflow_requiring_missing_capability_emits_e1702() {
    let mut registry = registry();
    let pkg = registry
        .packages
        .iter_mut()
        .find(|p| p.reference.to_string() == "workflows/print-start@1.0.0")
        .expect("print-start workflow package");
    let source = std::fs::read_to_string(pkg.dir.join("package.yaml")).unwrap();
    let replaced = source.replace("- heater.set_target", "- light.set_color");

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!("dryer-resolver-wf-cap-{nonce}"));
    std::fs::create_dir_all(&temporary).unwrap();
    std::fs::write(temporary.join("package.yaml"), replaced).unwrap();
    pkg.dir = temporary.clone();

    let with_workflow = format!("{}\nworkflows:\n  start: workflows/print-start\n", fixture());
    let o = resolve_source(&with_workflow, &registry);
    std::fs::remove_dir_all(temporary).unwrap();

    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1702").unwrap();
    assert!(
        d.message.contains("light.set_color") && d.message.contains("light"),
        "{}",
        d.message
    );
}

#[test]
fn workflow_with_unresolved_lock_resource_emits_e1703() {
    let mut registry = registry();
    let pkg = registry
        .packages
        .iter_mut()
        .find(|p| p.reference.to_string() == "workflows/print-start@1.0.0")
        .expect("print-start workflow package");
    let source = std::fs::read_to_string(pkg.dir.join("package.yaml")).unwrap();
    let replaced = source.replace("mainboard.heater0", "ghost_controller.ghost_resource");

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!("dryer-resolver-wf-lock-{nonce}"));
    std::fs::create_dir_all(&temporary).unwrap();
    std::fs::write(temporary.join("package.yaml"), replaced).unwrap();
    pkg.dir = temporary.clone();

    let with_workflow = format!("{}\nworkflows:\n  start: workflows/print-start\n", fixture());
    let o = resolve_source(&with_workflow, &registry);
    std::fs::remove_dir_all(temporary).unwrap();

    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1703").unwrap();
    assert!(
        d.message.contains("ghost_controller.ghost_resource"),
        "{}",
        d.message
    );
}

#[test]
fn workflow_step_calling_unsupported_action_emits_e1704() {
    let mut registry = registry();
    let pkg = registry
        .packages
        .iter_mut()
        .find(|p| p.reference.to_string() == "workflows/print-start@1.0.0")
        .expect("print-start workflow package");
    let source = std::fs::read_to_string(pkg.dir.join("package.yaml")).unwrap();
    let replaced = source.replace("call: heater.set_target", "call: unsupported_device.fire");

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temporary = std::env::temp_dir().join(format!("dryer-resolver-wf-step-{nonce}"));
    std::fs::create_dir_all(&temporary).unwrap();
    std::fs::write(temporary.join("package.yaml"), replaced).unwrap();
    pkg.dir = temporary.clone();

    let with_workflow = format!("{}\nworkflows:\n  start: workflows/print-start\n", fixture());
    let o = resolve_source(&with_workflow, &registry);
    std::fs::remove_dir_all(temporary).unwrap();

    assert!(!o.is_ok());
    let d = o.diagnostics.iter().find(|d| d.code == "E1704").unwrap();
    assert!(
        d.message.contains("unsupported_device.fire"),
        "{}",
        d.message
    );
}

#[test]
fn corexy_delta_and_toolchanger_templates_resolve_cleanly() {
    let reg = registry();

    // 1. CoreXY template machine — swap the machine-class package and the
    // kinematics type, and drop the explicit limits entirely so all three
    // template-supplied limits (including the buggy max_acceleration) get
    // quantity-parsed and validated.
    let corexy_src = fixture()
        .replace(
            "- machines/cartesian-basic@1.0.0",
            "- machines/corexy-standard@1.0.0",
        )
        .replace(
            "kinematics:\n  type: cartesian\n  limits:\n    max_velocity: 300 mm/s\n    max_acceleration: 3000 mm/s^2",
            "kinematics:\n  type: corexy",
        );
    let o = resolve_source(&corexy_src, &reg);
    assert!(o.is_ok(), "corexy resolution diagnostics: {:?}", o.diagnostics);

    // 2. Delta template machine
    let delta_src = fixture()
        .replace(
            "- machines/cartesian-basic@1.0.0",
            "- machines/delta-basic@1.0.0",
        )
        .replace(
            "kinematics:\n  type: cartesian\n  limits:\n    max_velocity: 300 mm/s\n    max_acceleration: 3000 mm/s^2",
            "kinematics:\n  type: delta",
        );
    let o = resolve_source(&delta_src, &reg);
    assert!(o.is_ok(), "delta resolution diagnostics: {:?}", o.diagnostics);

    // 3. Toolchanger template machine
    let toolchanger_src = fixture()
        .replace(
            "- machines/cartesian-basic@1.0.0",
            "- machines/toolchanger-corexy@1.0.0",
        )
        .replace(
            "kinematics:\n  type: cartesian\n  limits:\n    max_velocity: 300 mm/s\n    max_acceleration: 3000 mm/s^2",
            "kinematics:\n  type: corexy",
        );
    let o = resolve_source(&toolchanger_src, &reg);
    assert!(o.is_ok(), "toolchanger resolution diagnostics: {:?}", o.diagnostics);
}

