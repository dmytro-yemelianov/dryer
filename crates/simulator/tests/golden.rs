//! End-to-end golden (§29 step 9): resolve the fixture machine, build the
//! simulator FROM the resolution (safety policy from the resolved profile,
//! never test constants), run a small job, and compare the trace byte-for-
//! byte against the committed golden.
//!
//! Regenerate deliberately with:
//!   UPDATE_TRACE=1 cargo test -p dryer-simulator --test golden

use dryer_package_model::LocalRegistry;
use dryer_simulator::*;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Build heater configs from the RESOLVED machine: components whose class
/// the safety profile covers, with the profile's safe state and
/// heartbeat timeout (parsed as a typed Time quantity → ticks).
fn rig_from_resolution() -> (SimController, String) {
    let root = repo_root();
    let source =
        std::fs::read_to_string(root.join("examples/minimal-cartesian/machine.yaml")).unwrap();
    let registry = LocalRegistry::load(&root.join("packages"));
    let outcome = dryer_machine_resolver::resolve_source(&source, &registry);
    assert!(
        outcome.is_ok(),
        "fixture resolves: {:#?}",
        outcome.diagnostics
    );

    let doc = dryer_machine_parser::parse_str(&source).doc.unwrap();
    let (ns, name) = doc.safety.profile.split_once('/').unwrap();
    let profile = registry
        .find(ns, name)
        .unwrap()
        .safety_profile_payload()
        .unwrap();

    let mut heaters = Vec::new();
    let mut heater_name = String::new();
    for (cname, comp) in &doc.components {
        let Some(policy) = profile.classes.get(&comp.kind) else {
            continue;
        };
        if comp.kind != "heater" {
            continue;
        }
        let timeout = policy.heartbeat_timeout.as_deref().map(|q| {
            let t =
                dryer_machine_schema::Quantity::parse_as(q, dryer_machine_schema::Dimension::Time)
                    .expect("profile validated at load");
            (t.value * 1_000_000.0).round() as Tick // seconds → µs ticks
        });
        heater_name = cname.clone();
        heaters.push(HeaterCfg {
            name: cname.clone(),
            gain_milli_c: 200_000,
            tau_ms: 2_000,
            safe_state: policy.safe_state.clone(),
            heartbeat_timeout: timeout,
        });
    }
    assert!(!heaters.is_empty(), "fixture has a covered heater");

    let sim = SimController::new(
        25_000,
        heaters,
        vec![AxisCfg {
            name: "x".into(),
            start_position_um: 5_000,
        }],
    );
    (sim, heater_name)
}

#[test]
fn the_fixture_job_trace_matches_the_golden() {
    let (mut sim, heater) = rig_from_resolution();
    let mut tx = SimTransport::new(TransportConfig::default());

    // The job: home X, heat to 60 °C, then a 2 mm move — with heartbeats
    // every 100 ms for the whole run.
    tx.send(0, Command::Heartbeat);
    tx.send(
        0,
        Command::Home {
            axis: "x".into(),
            rate_um_s: 10_000,
        },
    );
    tx.send(
        0,
        Command::SetHeaterTarget {
            heater: heater.clone(),
            target_milli_c: 60_000,
        },
    );
    tx.send(
        800 * TICKS_PER_MS,
        Command::Move {
            axis: "x".into(),
            distance_um: 2_000,
            rate_um_s: 10_000,
        },
    );
    for ms in (0..2_000).step_by(100) {
        tx.send(ms * TICKS_PER_MS, Command::Heartbeat);
    }
    sim.run(&mut tx, 2_000 * TICKS_PER_MS);

    // The job semantics hold...
    assert!(sim.heater_milli_c(&heater).unwrap() >= 60_000, "heated");
    assert_eq!(sim.axis_position_um("x"), Some(2_000), "homed then moved");
    assert!(sim.latched_fault().is_none());

    // ...and the trace is the byte-stable golden.
    let golden_path = repo_root().join("examples/minimal-cartesian/job-trace.golden");
    let actual = sim.trace.to_json_lines();
    if std::env::var("UPDATE_TRACE").is_ok() {
        std::fs::write(&golden_path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&golden_path)
        .expect("golden exists — regenerate with UPDATE_TRACE=1");
    if expected != actual {
        let g = Trace::from_json_lines(&expected).unwrap();
        let i = sim.trace.first_divergence(&g);
        panic!(
            "trace drifted from the golden (first divergent event index: {i:?}).\n\
             If the change is intended, regenerate with UPDATE_TRACE=1."
        );
    }
}

/// Fault golden: heartbeats stop mid-heat; the resolved profile's timeout
/// (500 ms for the fixture) forces the heater to its declared safe state.
#[test]
fn heartbeat_loss_mid_heat_enters_the_profiles_safe_state() {
    let (mut sim, heater) = rig_from_resolution();
    let mut tx = SimTransport::new(TransportConfig::default());
    tx.send(0, Command::Heartbeat);
    tx.send(
        0,
        Command::SetHeaterTarget {
            heater: heater.clone(),
            target_milli_c: 60_000,
        },
    );
    // heartbeats only until 300 ms; loss detected at ~800 ms
    for ms in (0..300).step_by(100) {
        tx.send(ms * TICKS_PER_MS, Command::Heartbeat);
    }
    sim.run(&mut tx, 1_500 * TICKS_PER_MS);
    let safe_at = sim
        .trace
        .0
        .iter()
        .find_map(|e| match e {
            Event::SafeState {
                at,
                cause,
                output,
                state,
            } if cause == "heartbeat-loss" && *output == heater => {
                assert_eq!(state, "off", "the PROFILE's safe state");
                Some(*at)
            }
            _ => None,
        })
        .expect("safe state engaged");
    // last heartbeat lands ~200 ms + latency; timeout 500 ms ⇒ ≤ ~702 ms
    assert!(
        safe_at <= 702 * TICKS_PER_MS,
        "engaged within the profile timeout: {safe_at}"
    );
}
