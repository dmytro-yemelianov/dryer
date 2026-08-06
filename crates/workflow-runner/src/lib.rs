//! `dryer-workflow-runner`
//!
//! Streaming workflow runner and dynamic job queue dispatcher for Dryer.
//!
//! Enqueues lowered command envelopes into a streaming job queue, monitors controller
//! session status (queue capacity and fill level), dynamically dispatches scheduled commands
//! to maintain queue horizon without underrun, and tracks completion.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub use dryer_control_client::{CommandClient, FrameSink, SendError, SendReceipt};
pub use dryer_control_protocol::{Command, CommandEnvelope, Tick};
pub use dryer_controller_daemon::ControllerSessionStatus;
pub use dryer_toolpath_auditor::{AuditLimits, AuditReport, ToolpathAuditor};

/// Configuration for `WorkflowRunner` queue management and dispatch throttling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    /// Desired maximum fill level in the controller's queue (queue horizon).
    pub target_horizon: u16,

    /// Minimum fill level threshold below which replenishment is urgent.
    pub min_threshold: u16,

    /// Maximum number of commands to dispatch in a single dispatch step.
    pub max_batch_size: usize,

    /// Time margin in microseconds between scheduled commands during dynamic dispatch.
    pub schedule_lead_margin_us: u64,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            target_horizon: 16,
            min_threshold: 4,
            max_batch_size: 8,
            schedule_lead_margin_us: 1_000,
        }
    }
}

/// Lifecycle state of the `WorkflowRunner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerState {
    /// Queue is empty and all dispatched commands have completed.
    Idle,
    /// Actively dispatching commands to controller.
    Running,
    /// Controller queue fill level is at or above target horizon; dispatch paused.
    Throttled,
    /// Controller reported an underrun condition.
    UnderrunWarning,
    /// Controller heartbeat timed out or daemon state is faulted.
    Faulted,
    /// All enqueued commands have been dispatched and confirmed completed.
    Completed,
}

/// Runtime statistics snapshot of the workflow runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerStats {
    pub total_enqueued: usize,
    pub total_dispatched: usize,
    pub total_completed: usize,
    pub pending_in_runner_queue: usize,
    pub underrun_events: u64,
    pub state: RunnerState,
}

/// Errors returned by `WorkflowRunner` operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    AuditFailed(AuditReport),
    ControllerFaulted(String),
    HeartbeatTimeout,
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuditFailed(report) => write!(
                f,
                "toolpath audit failed with {} diagnostic(s)",
                report.diagnostics.len()
            ),
            Self::ControllerFaulted(msg) => write!(f, "controller session faulted: {msg}"),
            Self::HeartbeatTimeout => write!(f, "controller heartbeat timed out"),
        }
    }
}

impl std::error::Error for RunnerError {}

/// Streaming job runner and dynamic queue horizon maintainer for Dryer control sessions.
#[derive(Debug)]
pub struct WorkflowRunner {
    config: RunnerConfig,
    auditor: Option<ToolpathAuditor>,
    job_queue: VecDeque<CommandEnvelope>,
    last_controller_status: Option<ControllerSessionStatus>,
    total_enqueued: usize,
    total_dispatched: usize,
    total_completed: usize,
    underrun_events: u64,
    last_dispatched_tick: Option<Tick>,
    state: RunnerState,
}

impl WorkflowRunner {
    /// Create a new `WorkflowRunner` with the given configuration.
    pub fn new(config: RunnerConfig) -> Self {
        Self {
            config,
            auditor: None,
            job_queue: VecDeque::new(),
            last_controller_status: None,
            total_enqueued: 0,
            total_dispatched: 0,
            total_completed: 0,
            underrun_events: 0,
            last_dispatched_tick: None,
            state: RunnerState::Idle,
        }
    }

    /// Create a new `WorkflowRunner` equipped with a pre-flight `ToolpathAuditor`.
    pub fn with_auditor(config: RunnerConfig, limits: AuditLimits) -> Self {
        let mut runner = Self::new(config);
        runner.set_auditor(limits);
        runner
    }

    /// Attach or update pre-flight audit limits for enqueued commands.
    pub fn set_auditor(&mut self, limits: AuditLimits) {
        self.auditor = Some(ToolpathAuditor::new(limits));
    }

    /// Clear current auditor.
    pub fn clear_auditor(&mut self) {
        self.auditor = None;
    }

    /// Current runner state.
    pub fn state(&self) -> RunnerState {
        self.state
    }

    /// Returns `true` if the runner is currently throttled due to target queue horizon.
    pub fn is_throttled(&self) -> bool {
        self.state == RunnerState::Throttled
    }

    /// Returns `true` if all enqueued commands have been dispatched and completed.
    pub fn is_completed(&self) -> bool {
        self.state == RunnerState::Completed
    }

    /// Returns number of underrun events detected.
    pub fn underrun_count(&self) -> u64 {
        self.underrun_events
    }

    /// Returns count of commands currently buffered in the runner's job queue.
    pub fn pending_count(&self) -> usize {
        self.job_queue.len()
    }

    /// Returns current runtime statistics.
    pub fn stats(&self) -> RunnerStats {
        RunnerStats {
            total_enqueued: self.total_enqueued,
            total_dispatched: self.total_dispatched,
            total_completed: self.total_completed,
            pending_in_runner_queue: self.job_queue.len(),
            underrun_events: self.underrun_events,
            state: self.state,
        }
    }

    /// Active runner configuration.
    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }

    /// Last received controller session status.
    pub fn last_controller_status(&self) -> Option<&ControllerSessionStatus> {
        self.last_controller_status.as_ref()
    }

    /// Enqueue a single lowered command envelope into the streaming job queue.
    ///
    /// If an auditor is attached, the command is audited before enqueuing.
    pub fn enqueue(&mut self, envelope: CommandEnvelope) -> Result<(), RunnerError> {
        if let Some(ref auditor) = self.auditor {
            let report = auditor.audit(std::slice::from_ref(&envelope.command));
            if !report.passed {
                return Err(RunnerError::AuditFailed(report));
            }
        }

        self.job_queue.push_back(envelope);
        self.total_enqueued += 1;

        if self.state == RunnerState::Idle || self.state == RunnerState::Completed {
            self.state = RunnerState::Running;
        }

        Ok(())
    }

    /// Enqueue a batch of lowered command envelopes into the streaming job queue.
    ///
    /// If an auditor is attached, all commands are audited together. If audit fails,
    /// no commands are enqueued and an error containing the audit report is returned.
    pub fn enqueue_batch(
        &mut self,
        envelopes: impl IntoIterator<Item = CommandEnvelope>,
    ) -> Result<usize, RunnerError> {
        let items: Vec<CommandEnvelope> = envelopes.into_iter().collect();
        if items.is_empty() {
            return Ok(0);
        }

        if let Some(ref auditor) = self.auditor {
            let raw_cmds: Vec<Command> = items.iter().map(|e| e.command.clone()).collect();
            let report = auditor.audit(&raw_cmds);
            if !report.passed {
                return Err(RunnerError::AuditFailed(report));
            }
        }

        let count = items.len();
        for envelope in items {
            self.job_queue.push_back(envelope);
        }
        self.total_enqueued += count;

        if self.state == RunnerState::Idle || self.state == RunnerState::Completed {
            self.state = RunnerState::Running;
        }

        Ok(count)
    }

    /// Update controller session status (queue capacity, fill level, tick bounds, heartbeats).
    ///
    /// Evaluates controller queue room, underruns, heartbeat status, and updates completion metrics.
    pub fn update_controller_status(&mut self, status: ControllerSessionStatus) {
        if !status.heartbeat_ok {
            self.state = RunnerState::Faulted;
        } else if status.underrun {
            self.underrun_events += 1;
            if self.state != RunnerState::Faulted {
                self.state = RunnerState::UnderrunWarning;
            }
        }

        // Completion calculation:
        let active_in_controller = status.queue_fill as usize;
        self.total_completed = self.total_dispatched.saturating_sub(active_in_controller);

        if self.job_queue.is_empty()
            && active_in_controller == 0
            && self.total_dispatched == self.total_enqueued
            && self.total_enqueued > 0
            && self.state != RunnerState::Faulted
            && self.state != RunnerState::UnderrunWarning
        {
            self.state = RunnerState::Completed;
        } else if self.state == RunnerState::Throttled
            && status.queue_fill < self.config.target_horizon
        {
            self.state = RunnerState::Running;
        }

        self.last_controller_status = Some(status);
    }

    /// Compute and extract the next batch of command envelopes to dispatch.
    ///
    /// Enforces `target_horizon` throttling, available queue capacity, and dynamic timestamp scheduling.
    pub fn next_dispatch_batch(&mut self) -> Vec<CommandEnvelope> {
        if self.state == RunnerState::Faulted {
            return Vec::new();
        }

        if self.job_queue.is_empty() {
            if self.total_completed == self.total_dispatched && self.total_dispatched > 0 {
                self.state = RunnerState::Completed;
            } else if self.state != RunnerState::Faulted
                && self.state != RunnerState::UnderrunWarning
            {
                self.state = RunnerState::Idle;
            }
            return Vec::new();
        }

        // Determine capacity limits from controller status or config defaults
        let (capacity, fill, earliest_accepted, latest_accepted) =
            match &self.last_controller_status {
                Some(status) => {
                    if !status.heartbeat_ok {
                        self.state = RunnerState::Faulted;
                        return Vec::new();
                    }
                    (
                        status.queue_capacity,
                        status.queue_fill,
                        status.earliest_accepted_tick,
                        status.latest_accepted_tick,
                    )
                }
                None => (self.config.target_horizon, 0, 0, 0),
            };

        let target_horizon = self.config.target_horizon.min(capacity);

        if fill >= target_horizon {
            self.state = RunnerState::Throttled;
            return Vec::new();
        }

        let headroom = (target_horizon - fill) as usize;
        let batch_count = headroom
            .min(self.config.max_batch_size)
            .min(self.job_queue.len());

        if batch_count == 0 {
            return Vec::new();
        }

        let mut batch = Vec::with_capacity(batch_count);
        for _ in 0..batch_count {
            if let Some(mut envelope) = self.job_queue.pop_front() {
                // Dynamic timestamp scheduling to prevent past execution or window violation:
                let target_tick = match envelope.execute_at {
                    Some(specified) => {
                        let mut t = specified.max(earliest_accepted);
                        if let Some(last) = self.last_dispatched_tick {
                            t = t.max(last.saturating_add(self.config.schedule_lead_margin_us));
                        }
                        if latest_accepted > 0 && t > latest_accepted {
                            t = latest_accepted;
                        }
                        t
                    }
                    None => {
                        let mut t = earliest_accepted;
                        if let Some(last) = self.last_dispatched_tick {
                            t = t.max(last.saturating_add(self.config.schedule_lead_margin_us));
                        }
                        t
                    }
                };

                envelope.execute_at = Some(target_tick);
                self.last_dispatched_tick = Some(target_tick);
                batch.push(envelope);
            }
        }

        self.total_dispatched += batch.len();

        if (self.state == RunnerState::Throttled && fill < target_horizon)
            || (self.state != RunnerState::UnderrunWarning && self.state != RunnerState::Faulted)
        {
            self.state = RunnerState::Running;
        }

        batch
    }

    /// Convenience method to dispatch due commands directly into a `CommandClient`.
    pub fn dispatch_to_client<S: FrameSink>(
        &mut self,
        client: &mut CommandClient<S>,
    ) -> Result<Vec<SendReceipt>, SendError<S::Error>> {
        let batch = self.next_dispatch_batch();
        let mut receipts = Vec::with_capacity(batch.len());
        for envelope in batch {
            let receipt = match envelope.execute_at {
                Some(tick) => client.send_scheduled(tick, envelope.command)?,
                None => client.send(envelope.command)?,
            };
            receipts.push(receipt);
        }
        Ok(receipts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dryer_toolpath_auditor::AxisLimit;
    use std::collections::BTreeMap;

    fn sample_limits() -> AuditLimits {
        let mut axes = BTreeMap::new();
        axes.insert(
            "x".into(),
            AxisLimit {
                min_um: 0,
                max_um: 100_000,
            },
        );
        let mut heaters = BTreeMap::new();
        heaters.insert("hotend".into(), 250_000);

        AuditLimits {
            axes,
            max_feed_rate_um_s: 20_000,
            heater_ceilings_milli_c: heaters,
        }
    }

    fn sample_controller_status(capacity: u16, fill: u16) -> ControllerSessionStatus {
        ControllerSessionStatus {
            controller_id: "mcu1".into(),
            heartbeat_timeout_us: 50_000,
            last_seen_host_us: 10_000,
            queue_capacity: capacity,
            queue_fill: fill,
            earliest_accepted_tick: 1_000,
            latest_accepted_tick: 1_000_000,
            underrun: false,
            heartbeat_ok: true,
        }
    }

    #[test]
    fn runner_enqueues_and_audits_valid_commands() {
        let mut runner = WorkflowRunner::with_auditor(RunnerConfig::default(), sample_limits());
        assert_eq!(runner.state(), RunnerState::Idle);

        let cmds = vec![
            CommandEnvelope {
                execute_at: None,
                command: Command::Home {
                    axis: "x".into(),
                    rate_um_s: 10_000,
                },
            },
            CommandEnvelope {
                execute_at: None,
                command: Command::Move {
                    axis: "x".into(),
                    distance_um: 50_000,
                    rate_um_s: 15_000,
                },
            },
        ];

        let enqueued = runner.enqueue_batch(cmds).expect("batch enqueues cleanly");
        assert_eq!(enqueued, 2);
        assert_eq!(runner.pending_count(), 2);
        assert_eq!(runner.state(), RunnerState::Running);
    }

    #[test]
    fn runner_rejects_invalid_toolpath_during_enqueue() {
        let mut runner = WorkflowRunner::with_auditor(RunnerConfig::default(), sample_limits());

        let cmds = vec![CommandEnvelope {
            execute_at: None,
            command: Command::Move {
                axis: "x".into(),
                distance_um: 200_000, // exceeds max_um 100_000
                rate_um_s: 10_000,
            },
        }];

        let res = runner.enqueue_batch(cmds);
        assert!(matches!(res, Err(RunnerError::AuditFailed(_))));
        assert_eq!(runner.pending_count(), 0);
    }

    #[test]
    fn queue_replenishment_throttling_honors_target_horizon() {
        let config = RunnerConfig {
            target_horizon: 10,
            min_threshold: 3,
            max_batch_size: 5,
            schedule_lead_margin_us: 1_000,
        };
        let mut runner = WorkflowRunner::new(config);

        // Enqueue 20 commands
        let cmds: Vec<_> = (0..20)
            .map(|i| CommandEnvelope {
                execute_at: None,
                command: Command::Move {
                    axis: "x".into(),
                    distance_um: 1_000,
                    rate_um_s: 1_000 + i as u64,
                },
            })
            .collect();
        runner.enqueue_batch(cmds).unwrap();

        // Controller queue fill is 0 -> dispatch max batch size 5
        runner.update_controller_status(sample_controller_status(32, 0));
        let batch1 = runner.next_dispatch_batch();
        assert_eq!(batch1.len(), 5);
        assert_eq!(runner.pending_count(), 15);

        // Update status: fill is now 8. Target horizon is 10, headroom is 2 -> dispatch 2
        runner.update_controller_status(sample_controller_status(32, 8));
        let batch2 = runner.next_dispatch_batch();
        assert_eq!(batch2.len(), 2);
        assert_eq!(runner.pending_count(), 13);

        // Update status: fill is 10 (at horizon) -> dispatch 0, runner becomes Throttled
        runner.update_controller_status(sample_controller_status(32, 10));
        let batch3 = runner.next_dispatch_batch();
        assert_eq!(batch3.len(), 0);
        assert!(runner.is_throttled());

        // Fill drops to 4 (below horizon) -> runner resumes dispatching up to target horizon (6 items)
        runner.update_controller_status(sample_controller_status(32, 4));
        assert!(!runner.is_throttled());
        let batch4 = runner.next_dispatch_batch();
        assert_eq!(batch4.len(), 5); // capped by max_batch_size 5
    }

    #[test]
    fn completion_tracking_and_underrun_detection() {
        let mut runner = WorkflowRunner::new(RunnerConfig::default());
        let cmds = vec![
            CommandEnvelope {
                execute_at: None,
                command: Command::Heartbeat,
            },
            CommandEnvelope {
                execute_at: None,
                command: Command::Heartbeat,
            },
        ];
        runner.enqueue_batch(cmds).unwrap();

        runner.update_controller_status(sample_controller_status(16, 0));
        let dispatched = runner.next_dispatch_batch();
        assert_eq!(dispatched.len(), 2);
        assert_eq!(runner.stats().total_dispatched, 2);

        // Controller fill is still 2 -> completed is 0
        runner.update_controller_status(sample_controller_status(16, 2));
        assert_eq!(runner.stats().total_completed, 0);

        // Controller fill drops to 0 -> completed is 2 and runner state becomes Completed
        runner.update_controller_status(sample_controller_status(16, 0));
        assert_eq!(runner.stats().total_completed, 2);
        assert!(runner.is_completed());

        // Underrun status update
        let mut underrun_status = sample_controller_status(16, 0);
        underrun_status.underrun = true;
        runner.update_controller_status(underrun_status);
        assert_eq!(runner.underrun_count(), 1);
        assert_eq!(runner.state(), RunnerState::UnderrunWarning);
    }
}
