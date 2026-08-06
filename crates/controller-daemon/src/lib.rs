//! `dryer-controller-daemon`
//!
//! Host-side controller state service, connection manager, and heartbeat guard for Dryer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use dryer_control_client::{decode_queue_status_frame, MultiControllerClockSync};
pub use dryer_control_protocol::{DecodeError, QueueStatus};

/// State of the daemon connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonState {
    Uninitialized,
    Connecting,
    Connected,
    Running,
    Faulted,
    Disconnected,
}

/// Runtime status snapshot for a registered controller session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerSessionStatus {
    pub controller_id: String,
    pub heartbeat_timeout_us: u64,
    pub last_seen_host_us: u64,
    pub queue_capacity: u16,
    pub queue_fill: u16,
    pub earliest_accepted_tick: u64,
    pub latest_accepted_tick: u64,
    pub underrun: bool,
    pub heartbeat_ok: bool,
}

/// Managed controller session state within the daemon.
#[derive(Debug)]
pub struct ControllerSession {
    pub controller_id: String,
    pub heartbeat_timeout_us: u64,
    pub last_seen_host_us: u64,
    pub last_status: Option<QueueStatus>,
}

impl ControllerSession {
    pub fn new(controller_id: impl Into<String>, heartbeat_timeout_us: u64) -> Self {
        Self {
            controller_id: controller_id.into(),
            heartbeat_timeout_us,
            last_seen_host_us: 0,
            last_status: None,
        }
    }

    pub fn record_heartbeat(&mut self, current_host_us: u64) {
        self.last_seen_host_us = current_host_us;
    }

    pub fn is_heartbeat_alive(&self, current_host_us: u64) -> bool {
        if self.last_seen_host_us == 0 {
            return true;
        }
        current_host_us.saturating_sub(self.last_seen_host_us) <= self.heartbeat_timeout_us
    }

    pub fn status(&self, current_host_us: u64) -> ControllerSessionStatus {
        let (cap, fill, earliest, latest, underrun) = match self.last_status {
            Some(ref status) => (
                status.capacity,
                status.fill,
                status.earliest_accepted,
                status.latest_accepted,
                status.underrun,
            ),
            None => (0, 0, 0, 0, false),
        };

        ControllerSessionStatus {
            controller_id: self.controller_id.clone(),
            heartbeat_timeout_us: self.heartbeat_timeout_us,
            last_seen_host_us: self.last_seen_host_us,
            queue_capacity: cap,
            queue_fill: fill,
            earliest_accepted_tick: earliest,
            latest_accepted_tick: latest,
            underrun,
            heartbeat_ok: self.is_heartbeat_alive(current_host_us),
        }
    }
}

/// Host-side Controller Daemon managing multiple sessions, clock sync, and heartbeat safety.
#[derive(Debug)]
pub struct ControllerDaemon {
    state: DaemonState,
    cluster_sync: MultiControllerClockSync,
    sessions: BTreeMap<String, ControllerSession>,
}

impl ControllerDaemon {
    pub fn new() -> Self {
        Self {
            state: DaemonState::Uninitialized,
            cluster_sync: MultiControllerClockSync::new(0, 100_000),
            sessions: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> DaemonState {
        self.state
    }

    pub fn register_controller(
        &mut self,
        controller_id: impl Into<String>,
        heartbeat_timeout_us: u64,
    ) {
        let id = controller_id.into();
        let _ = self.cluster_sync.add_controller(&id);
        self.sessions
            .insert(id.clone(), ControllerSession::new(id, heartbeat_timeout_us));
        if self.state == DaemonState::Uninitialized {
            self.state = DaemonState::Connected;
        }
    }

    pub fn record_heartbeat(&mut self, controller_id: &str, current_host_us: u64) -> bool {
        if let Some(session) = self.sessions.get_mut(controller_id) {
            session.record_heartbeat(current_host_us);
            true
        } else {
            false
        }
    }

    pub fn update_queue_status(
        &mut self,
        controller_id: &str,
        status_frame: &[u8],
        current_host_us: u64,
    ) -> Result<QueueStatus, DecodeError> {
        let status_decoded = decode_queue_status_frame(status_frame)?;
        let status = status_decoded.status;
        if let Some(session) = self.sessions.get_mut(controller_id) {
            session.last_status = Some(status.clone());
            session.record_heartbeat(current_host_us);
        }
        Ok(status)
    }

    pub fn audit_heartbeats(&mut self, current_host_us: u64) -> Vec<String> {
        let mut dead = Vec::new();
        for (id, session) in &self.sessions {
            if !session.is_heartbeat_alive(current_host_us) {
                dead.push(id.clone());
            }
        }
        if !dead.is_empty() {
            self.state = DaemonState::Faulted;
        }
        dead
    }

    pub fn active_controller_ids(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    pub fn session_status(
        &self,
        controller_id: &str,
        current_host_us: u64,
    ) -> Option<ControllerSessionStatus> {
        self.sessions
            .get(controller_id)
            .map(|s| s.status(current_host_us))
    }

    pub fn daemon_status_summary(&self, current_host_us: u64) -> Vec<ControllerSessionStatus> {
        self.sessions
            .values()
            .map(|s| s.status(current_host_us))
            .collect()
    }
}

impl Default for ControllerDaemon {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_registers_and_tracks_controller_sessions() {
        let mut daemon = ControllerDaemon::new();
        assert_eq!(daemon.state(), DaemonState::Uninitialized);

        daemon.register_controller("mcu1", 50_000);
        assert_eq!(daemon.state(), DaemonState::Connected);
        assert_eq!(daemon.active_controller_ids(), vec!["mcu1"]);

        let status = daemon.session_status("mcu1", 1000).unwrap();
        assert_eq!(status.controller_id, "mcu1");
        assert_eq!(status.heartbeat_timeout_us, 50_000);
        assert!(status.heartbeat_ok);
    }

    #[test]
    fn daemon_detects_heartbeat_timeout_and_faults() {
        let mut daemon = ControllerDaemon::new();
        daemon.register_controller("mcu1", 10_000);

        daemon.record_heartbeat("mcu1", 1_000);
        assert!(daemon.audit_heartbeats(5_000).is_empty());
        assert_eq!(daemon.state(), DaemonState::Connected);

        let dead = daemon.audit_heartbeats(20_000);
        assert_eq!(dead, vec!["mcu1"]);
        assert_eq!(daemon.state(), DaemonState::Faulted);
    }
}
