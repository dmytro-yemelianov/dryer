//! Deterministic simulated controller (spec §24.1, §29 step 9;
//! docs/simulator.md).
//!
//! Everything here is explicitly clocked: one tick = **1 µs** of virtual
//! time, advanced only by the test driver. There is no wall clock, no
//! ambient randomness (the transport's jitter PRNG is seeded), and no
//! float formatting in traces (temperatures serialize as milli-degrees) —
//! so a trace is a byte-stable golden artifact.
//!
//! The simulator speaks *typed command semantics*, not wire frames: the
//! §16 codec, when it arrives, must round-trip to these types rather than
//! freezing bytes before behavior. Integration is deliberately simple
//! (fixed 1 ms steps, first-order plants, bang-bang heaters); fidelity
//! beyond "makes wait-for-temp and homing mean something" is a non-goal.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// One microsecond of virtual time.
pub type Tick = u64;
pub const TICKS_PER_MS: Tick = 1_000;
/// Fixed plant-integration step (1 ms).
pub const STEP_TICKS: Tick = TICKS_PER_MS;

// ---------------------------------------------------------------------------
// Commands and events — the semantic layer (§16 without §16.3 framing)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cmd")]
pub enum Command {
    Heartbeat,
    SetHeaterTarget {
        heater: String,
        target_milli_c: i64,
    },
    Home {
        axis: String,
        rate_um_s: u64,
    },
    Move {
        axis: String,
        distance_um: i64,
        rate_um_s: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum Event {
    Accepted {
        at: Tick,
        what: String,
    },
    Rejected {
        at: Tick,
        what: String,
        reason: String,
    },
    Executed {
        at: Tick,
        what: String,
    },
    TempSample {
        at: Tick,
        heater: String,
        milli_c: i64,
    },
    Endstop {
        at: Tick,
        axis: String,
    },
    SafeState {
        at: Tick,
        output: String,
        state: String,
        cause: String,
    },
    FaultLatched {
        at: Tick,
        code: String,
    },
    Reset {
        at: Tick,
    },
}

impl Event {
    pub fn at(&self) -> Tick {
        match self {
            Event::Accepted { at, .. }
            | Event::Rejected { at, .. }
            | Event::Executed { at, .. }
            | Event::TempSample { at, .. }
            | Event::Endstop { at, .. }
            | Event::SafeState { at, .. }
            | Event::FaultLatched { at, .. }
            | Event::Reset { at } => *at,
        }
    }
}

/// Tick-stamped event log; the golden artifact.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Trace(pub Vec<Event>);

impl Trace {
    /// One JSON object per line — stable field order via serde, stable
    /// numbers via integers only.
    pub fn to_json_lines(&self) -> String {
        let mut s = String::new();
        for e in &self.0 {
            s.push_str(&serde_json::to_string(e).expect("events serialize"));
            s.push('\n');
        }
        s
    }

    pub fn from_json_lines(text: &str) -> Result<Self, serde_json::Error> {
        let mut v = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            v.push(serde_json::from_str(line)?);
        }
        Ok(Trace(v))
    }

    /// Index of the first divergent event between two traces.
    pub fn first_divergence(&self, other: &Trace) -> Option<usize> {
        let n = self.0.len().max(other.0.len());
        (0..n).find(|&i| self.0.get(i) != other.0.get(i))
    }
}

// ---------------------------------------------------------------------------
// Transport: latency, jitter, loss, duplication, link faults (§24.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub latency_ticks: Tick,
    /// Uniform jitter in `[0, jitter_ticks]`, from the seeded PRNG.
    pub jitter_ticks: Tick,
    /// Per-mille probability a command is silently lost.
    pub loss_per_mille: u16,
    /// Per-mille probability a command is delivered twice.
    pub dup_per_mille: u16,
    /// PRNG seed — part of the test definition, never ambient.
    pub seed: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            latency_ticks: 200, // 200 µs
            jitter_ticks: 0,
            loss_per_mille: 0,
            dup_per_mille: 0,
            seed: 0,
        }
    }
}

/// In-memory duplex carrying `(deliver_at, Command)`.
#[derive(Debug)]
pub struct SimTransport {
    cfg: TransportConfig,
    rng: u64,
    in_flight: VecDeque<(Tick, Command)>,
    link_up: bool,
}

impl SimTransport {
    pub fn new(cfg: TransportConfig) -> Self {
        let rng = cfg.seed.wrapping_mul(2).wrapping_add(1);
        SimTransport {
            cfg,
            rng,
            in_flight: VecDeque::new(),
            link_up: true,
        }
    }

    fn next_rand(&mut self) -> u64 {
        // LCG (Knuth): deterministic, dependency-free.
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.rng >> 33
    }

    /// Send a command at `now`; it arrives after latency+jitter unless the
    /// link is down or the loss roll eats it.
    pub fn send(&mut self, now: Tick, cmd: Command) {
        if !self.link_up {
            return;
        }
        if self.cfg.loss_per_mille > 0 && self.next_rand() % 1000 < self.cfg.loss_per_mille as u64 {
            return;
        }
        let jitter = if self.cfg.jitter_ticks > 0 {
            self.next_rand() % (self.cfg.jitter_ticks + 1)
        } else {
            0
        };
        let at = now + self.cfg.latency_ticks + jitter;
        self.in_flight.push_back((at, cmd.clone()));
        if self.cfg.dup_per_mille > 0 && self.next_rand() % 1000 < self.cfg.dup_per_mille as u64 {
            self.in_flight.push_back((at + 1, cmd));
        }
    }

    /// Fault injection: sever the link (in-flight commands are lost too).
    pub fn drop_link(&mut self) {
        self.link_up = false;
        self.in_flight.clear();
    }

    pub fn restore_link(&mut self) {
        self.link_up = true;
    }

    fn deliver_due(&mut self, now: Tick) -> Vec<Command> {
        let mut due: Vec<(Tick, Command)> = Vec::new();
        self.in_flight.retain(|(at, cmd)| {
            if *at <= now {
                due.push((*at, cmd.clone()));
                false
            } else {
                true
            }
        });
        due.sort_by_key(|(at, _)| *at);
        due.into_iter().map(|(_, c)| c).collect()
    }
}

// ---------------------------------------------------------------------------
// Controller: queue, plants, safety envelope (§18.1)
// ---------------------------------------------------------------------------

/// Safety policy for one output — taken from the RESOLVED safety profile,
/// never from test constants.
#[derive(Debug, Clone)]
pub struct HeaterCfg {
    pub name: String,
    /// °C above ambient at full duty (steady state).
    pub gain_milli_c: i64,
    /// Time constant in milliseconds.
    pub tau_ms: u64,
    pub safe_state: String,
    pub heartbeat_timeout: Option<Tick>,
}

#[derive(Debug, Clone)]
pub struct AxisCfg {
    pub name: String,
    pub start_position_um: i64,
}

#[derive(Debug)]
struct HeaterState {
    cfg: HeaterCfg,
    milli_c: f64,
    target_milli_c: i64,
    duty_on: bool,
    forced_safe: bool,
}

#[derive(Debug, Clone)]
struct Motion {
    what: String,
    per_ms_um: i64,
    /// Remaining distance for moves; homing runs to the endstop instead.
    remaining_um: i64,
    homing: bool,
}

#[derive(Debug)]
struct AxisState {
    cfg: AxisCfg,
    position_um: i64,
    motion: Option<Motion>,
}

#[derive(Debug)]
pub struct SimController {
    now: Tick,
    ambient_milli_c: i64,
    heaters: Vec<HeaterState>,
    axes: Vec<AxisState>,
    queue: VecDeque<Command>,
    queue_capacity: usize,
    last_heartbeat: Tick,
    latched_fault: Option<String>,
    /// Sample cadence for temperature telemetry.
    pub temp_sample_every: Tick,
    next_temp_sample: Tick,
    pub trace: Trace,
}

impl SimController {
    pub fn new(ambient_milli_c: i64, heaters: Vec<HeaterCfg>, axes: Vec<AxisCfg>) -> Self {
        SimController {
            now: 0,
            ambient_milli_c,
            heaters: heaters
                .into_iter()
                .map(|cfg| HeaterState {
                    milli_c: ambient_milli_c as f64,
                    target_milli_c: 0,
                    duty_on: false,
                    forced_safe: false,
                    cfg,
                })
                .collect(),
            axes: axes
                .into_iter()
                .map(|cfg| AxisState {
                    position_um: cfg.start_position_um,
                    motion: None,
                    cfg,
                })
                .collect(),
            queue: VecDeque::new(),
            queue_capacity: 16,
            last_heartbeat: 0,
            latched_fault: None,
            temp_sample_every: 100 * TICKS_PER_MS,
            next_temp_sample: 100 * TICKS_PER_MS,
            trace: Trace::default(),
        }
    }

    pub fn now(&self) -> Tick {
        self.now
    }

    pub fn latched_fault(&self) -> Option<&str> {
        self.latched_fault.as_deref()
    }

    pub fn heater_milli_c(&self, name: &str) -> Option<i64> {
        self.heaters
            .iter()
            .find(|h| h.cfg.name == name)
            .map(|h| h.milli_c.round() as i64)
    }

    pub fn axis_position_um(&self, name: &str) -> Option<i64> {
        self.axes
            .iter()
            .find(|a| a.cfg.name == name)
            .map(|a| a.position_um)
    }

    /// Fault injection: controller reset — outputs enter their safe state,
    /// a fault latches, queue and targets clear (§18.1 boot/reset safety).
    pub fn reset(&mut self) {
        let at = self.now;
        self.trace.0.push(Event::Reset { at });
        for h in &mut self.heaters {
            h.target_milli_c = 0;
            h.duty_on = false;
            h.forced_safe = false;
            self.trace.0.push(Event::SafeState {
                at,
                output: h.cfg.name.clone(),
                state: h.cfg.safe_state.clone(),
                cause: "controller-reset".to_string(),
            });
        }
        for a in &mut self.axes {
            a.motion = None;
        }
        self.queue.clear();
        self.latched_fault = Some("controller-reset".to_string());
        self.trace.0.push(Event::FaultLatched {
            at,
            code: "controller-reset".to_string(),
        });
    }

    fn accept(&mut self, cmd: Command) {
        let what = summarize(&cmd);
        if let Some(code) = &self.latched_fault {
            self.trace.0.push(Event::Rejected {
                at: self.now,
                what,
                reason: format!("fault latched: {code}"),
            });
            return;
        }
        if matches!(cmd, Command::Heartbeat) {
            self.last_heartbeat = self.now;
            return; // heartbeats are not queued and not traced (volume)
        }
        if self.queue.len() >= self.queue_capacity {
            self.trace.0.push(Event::Rejected {
                at: self.now,
                what,
                reason: "queue full".to_string(),
            });
            return;
        }
        self.trace.0.push(Event::Accepted { at: self.now, what });
        self.queue.push_back(cmd);
    }

    fn start_queued(&mut self) {
        while let Some(cmd) = self.queue.pop_front() {
            match cmd {
                Command::Heartbeat => {}
                Command::SetHeaterTarget {
                    heater,
                    target_milli_c,
                } => {
                    if let Some(h) = self.heaters.iter_mut().find(|h| h.cfg.name == heater) {
                        h.target_milli_c = target_milli_c;
                        h.forced_safe = false;
                        self.trace.0.push(Event::Executed {
                            at: self.now,
                            what: format!("heater {heater} target {target_milli_c} mC"),
                        });
                    }
                }
                Command::Home { axis, rate_um_s } => {
                    if let Some(a) = self.axes.iter_mut().find(|a| a.cfg.name == axis) {
                        a.motion = Some(Motion {
                            what: format!("home {axis}"),
                            per_ms_um: -((rate_um_s as i64) / 1000),
                            remaining_um: 0,
                            homing: true,
                        });
                    }
                }
                Command::Move {
                    axis,
                    distance_um,
                    rate_um_s,
                } => {
                    if let Some(a) = self.axes.iter_mut().find(|a| a.cfg.name == axis) {
                        a.motion = Some(Motion {
                            what: format!("move {axis} {distance_um} um"),
                            per_ms_um: distance_um.signum() * (rate_um_s as i64) / 1000,
                            remaining_um: distance_um,
                            homing: false,
                        });
                    }
                }
            }
        }
    }

    /// Advance to `until`, pulling deliveries from the transport and
    /// integrating plants in fixed 1 ms steps.
    pub fn run(&mut self, transport: &mut SimTransport, until: Tick) {
        while self.now < until {
            self.now += STEP_TICKS;
            for cmd in transport.deliver_due(self.now) {
                self.accept(cmd);
            }
            self.start_queued();

            // Safety envelope: heartbeat loss forces declared safe states
            // (§18.1) — enforced HERE, independent of any host logic.
            for h in &mut self.heaters {
                if let Some(timeout) = h.cfg.heartbeat_timeout {
                    if !h.forced_safe
                        && h.target_milli_c > 0
                        && self.now.saturating_sub(self.last_heartbeat) > timeout
                    {
                        h.target_milli_c = 0;
                        h.duty_on = false;
                        h.forced_safe = true;
                        self.trace.0.push(Event::SafeState {
                            at: self.now,
                            output: h.cfg.name.clone(),
                            state: h.cfg.safe_state.clone(),
                            cause: "heartbeat-loss".to_string(),
                        });
                    }
                }
            }

            // First-order thermal plant, explicit Euler, dt = 1 ms.
            for h in &mut self.heaters {
                h.duty_on = !h.forced_safe && (h.milli_c as i64) < h.target_milli_c;
                let drive = if h.duty_on {
                    h.cfg.gain_milli_c as f64
                } else {
                    0.0
                };
                let ambient = self.ambient_milli_c as f64;
                let dt_over_tau = 1.0 / h.cfg.tau_ms as f64;
                h.milli_c += (drive - (h.milli_c - ambient)) * dt_over_tau;
            }

            // Motion: constant-rate displacement, clamped exactly — homing
            // completes on the endstop, moves on remaining distance zero.
            for a in &mut self.axes {
                if let Some(m) = &mut a.motion {
                    if m.homing {
                        a.position_um += m.per_ms_um;
                        if a.position_um <= 0 {
                            a.position_um = 0;
                            let what = m.what.clone();
                            self.trace.0.push(Event::Endstop {
                                at: self.now,
                                axis: a.cfg.name.clone(),
                            });
                            self.trace.0.push(Event::Executed { at: self.now, what });
                            a.motion = None;
                        }
                    } else {
                        let step = if m.per_ms_um.abs() >= m.remaining_um.abs() {
                            m.remaining_um
                        } else {
                            m.per_ms_um
                        };
                        a.position_um += step;
                        m.remaining_um -= step;
                        if m.remaining_um == 0 {
                            let what = m.what.clone();
                            self.trace.0.push(Event::Executed { at: self.now, what });
                            a.motion = None;
                        }
                    }
                }
            }

            if self.now >= self.next_temp_sample {
                self.next_temp_sample += self.temp_sample_every;
                for h in &self.heaters {
                    self.trace.0.push(Event::TempSample {
                        at: self.now,
                        heater: h.cfg.name.clone(),
                        milli_c: h.milli_c.round() as i64,
                    });
                }
            }
        }
    }
}

fn summarize(cmd: &Command) -> String {
    match cmd {
        Command::Heartbeat => "heartbeat".to_string(),
        Command::SetHeaterTarget {
            heater,
            target_milli_c,
        } => format!("set {heater} -> {target_milli_c} mC"),
        Command::Home { axis, .. } => format!("home {axis}"),
        Command::Move {
            axis, distance_um, ..
        } => format!("move {axis} {distance_um} um"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotend() -> HeaterCfg {
        HeaterCfg {
            name: "hotend_heater".into(),
            gain_milli_c: 200_000,
            tau_ms: 2_000,
            safe_state: "off".into(),
            heartbeat_timeout: Some(500 * TICKS_PER_MS),
        }
    }

    fn rig() -> (SimController, SimTransport) {
        (
            SimController::new(
                25_000,
                vec![hotend()],
                vec![AxisCfg {
                    name: "x".into(),
                    start_position_um: 5_000,
                }],
            ),
            SimTransport::new(TransportConfig::default()),
        )
    }

    #[test]
    fn heating_reaches_target_and_homing_hits_the_endstop() {
        let (mut sim, mut tx) = rig();
        tx.send(0, Command::Heartbeat);
        tx.send(
            0,
            Command::SetHeaterTarget {
                heater: "hotend_heater".into(),
                target_milli_c: 60_000,
            },
        );
        tx.send(
            0,
            Command::Home {
                axis: "x".into(),
                rate_um_s: 10_000,
            },
        );
        // keep heartbeats flowing
        for ms in (0..2_000).step_by(100) {
            tx.send(ms * TICKS_PER_MS, Command::Heartbeat);
        }
        sim.run(&mut tx, 2_000 * TICKS_PER_MS);
        assert!(sim.heater_milli_c("hotend_heater").unwrap() >= 60_000);
        assert_eq!(sim.axis_position_um("x"), Some(0));
        assert!(sim
            .trace
            .0
            .iter()
            .any(|e| matches!(e, Event::Endstop { axis, .. } if axis == "x")));
    }

    #[test]
    fn heartbeat_loss_forces_the_declared_safe_state_within_the_timeout() {
        let (mut sim, mut tx) = rig();
        tx.send(0, Command::Heartbeat);
        tx.send(
            0,
            Command::SetHeaterTarget {
                heater: "hotend_heater".into(),
                target_milli_c: 60_000,
            },
        );
        // no further heartbeats: safe state must engage by 500 ms + one step
        sim.run(&mut tx, 1_000 * TICKS_PER_MS);
        let safe = sim
            .trace
            .0
            .iter()
            .find_map(|e| match e {
                Event::SafeState {
                    at, cause, state, ..
                } if cause == "heartbeat-loss" => Some((*at, state.clone())),
                _ => None,
            })
            .expect("safe state engaged");
        // last heartbeat registers on the 1 ms step after its 200 µs
        // delivery; the strict > comparison adds one more step: 502 ms.
        assert!(safe.0 <= 502 * TICKS_PER_MS, "engaged at {} ticks", safe.0);
        assert_eq!(safe.1, "off");
        // and the plant decays afterwards
        let t_end = sim.heater_milli_c("hotend_heater").unwrap();
        assert!(
            t_end < 60_000,
            "temperature decays after forced-off: {t_end}"
        );
    }

    #[test]
    fn a_reset_latches_a_fault_and_rejects_further_commands() {
        let (mut sim, mut tx) = rig();
        tx.send(0, Command::Heartbeat);
        sim.run(&mut tx, 10 * TICKS_PER_MS);
        sim.reset();
        assert_eq!(sim.latched_fault(), Some("controller-reset"));
        tx.send(
            sim.now(),
            Command::Home {
                axis: "x".into(),
                rate_um_s: 10_000,
            },
        );
        sim.run(&mut tx, sim.now() + 10 * TICKS_PER_MS);
        assert!(sim.trace.0.iter().any(
            |e| matches!(e, Event::Rejected { reason, .. } if reason.contains("controller-reset"))
        ));
    }

    #[test]
    fn identical_seeds_produce_identical_traces_under_jitter_and_loss() {
        let cfg = TransportConfig {
            jitter_ticks: 5_000,
            loss_per_mille: 100,
            dup_per_mille: 50,
            seed: 42,
            ..TransportConfig::default()
        };
        let run = |cfg: TransportConfig| {
            let mut sim = SimController::new(25_000, vec![hotend()], vec![]);
            let mut tx = SimTransport::new(cfg);
            for ms in (0..1_000).step_by(50) {
                tx.send(ms * TICKS_PER_MS, Command::Heartbeat);
                tx.send(
                    ms * TICKS_PER_MS,
                    Command::SetHeaterTarget {
                        heater: "hotend_heater".into(),
                        target_milli_c: 50_000,
                    },
                );
            }
            sim.run(&mut tx, 1_000 * TICKS_PER_MS);
            sim.trace
        };
        let a = run(cfg.clone());
        let b = run(cfg);
        assert_eq!(a.first_divergence(&b), None);
        assert_eq!(a.to_json_lines(), b.to_json_lines());
    }

    #[test]
    fn traces_round_trip_through_json_lines() {
        let (mut sim, mut tx) = rig();
        tx.send(0, Command::Heartbeat);
        tx.send(
            0,
            Command::Move {
                axis: "x".into(),
                distance_um: 1_000,
                rate_um_s: 10_000,
            },
        );
        sim.run(&mut tx, 300 * TICKS_PER_MS);
        let text = sim.trace.to_json_lines();
        let back = Trace::from_json_lines(&text).unwrap();
        assert_eq!(back, sim.trace);
    }
}
