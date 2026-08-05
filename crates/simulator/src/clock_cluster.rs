use crate::{ClockTransportConfig, SimClockFrame, SimClockTransport, SimClockTransportError};
use dryer_control_protocol::Tick;
use std::{collections::BTreeMap, fmt};

pub type ControllerId = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimClockClusterError {
    DuplicateController {
        controller: ControllerId,
    },
    UnknownController {
        controller: ControllerId,
    },
    Configuration(crate::ClockTransportConfigError),
    Transport {
        controller: ControllerId,
        source: SimClockTransportError,
    },
}

impl fmt::Display for SimClockClusterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateController { controller } => write!(
                formatter,
                "controller {controller} is configured more than once"
            ),
            Self::UnknownController { controller } => {
                write!(formatter, "controller {controller} is not in the cluster")
            }
            Self::Configuration(error) => {
                write!(formatter, "invalid controller transport: {error}")
            }
            Self::Transport { controller, source } => write!(
                formatter,
                "controller {controller} transport failed: {source}"
            ),
        }
    }
}
impl std::error::Error for SimClockClusterError {}

#[derive(Debug)]
pub struct SimClockCluster {
    controllers: BTreeMap<ControllerId, SimClockTransport>,
}

impl SimClockCluster {
    pub fn new(
        configurations: impl IntoIterator<Item = (ControllerId, ClockTransportConfig)>,
    ) -> Result<Self, SimClockClusterError> {
        let mut controllers = BTreeMap::new();
        for (controller, configuration) in configurations {
            if controllers.contains_key(&controller) {
                return Err(SimClockClusterError::DuplicateController { controller });
            }
            let transport = SimClockTransport::new(configuration)
                .map_err(SimClockClusterError::Configuration)?;
            controllers.insert(controller, transport);
        }
        Ok(Self { controllers })
    }
    pub fn controller_ids(&self) -> impl Iterator<Item = ControllerId> + '_ {
        self.controllers.keys().copied()
    }
    pub fn send_request(
        &mut self,
        controller: ControllerId,
        host_send: Tick,
        frame: &[u8],
    ) -> Result<(), SimClockClusterError> {
        let transport = self
            .controllers
            .get_mut(&controller)
            .ok_or(SimClockClusterError::UnknownController { controller })?;
        transport
            .send_request(host_send, frame)
            .map_err(|source| SimClockClusterError::Transport { controller, source })
    }
    pub fn receive_due(
        &mut self,
        controller: ControllerId,
        host_now: Tick,
    ) -> Result<Option<SimClockFrame>, SimClockClusterError> {
        let transport = self
            .controllers
            .get_mut(&controller)
            .ok_or(SimClockClusterError::UnknownController { controller })?;
        Ok(transport.receive_due(host_now))
    }
    pub fn drop_link(&mut self, controller: ControllerId) -> Result<(), SimClockClusterError> {
        let transport = self
            .controllers
            .get_mut(&controller)
            .ok_or(SimClockClusterError::UnknownController { controller })?;
        transport.drop_link();
        Ok(())
    }
    pub fn restore_link(&mut self, controller: ControllerId) -> Result<(), SimClockClusterError> {
        let transport = self
            .controllers
            .get_mut(&controller)
            .ok_or(SimClockClusterError::UnknownController { controller })?;
        transport.restore_link();
        Ok(())
    }
    pub fn pending_len(&self, controller: ControllerId) -> Result<usize, SimClockClusterError> {
        self.controllers
            .get(&controller)
            .map(SimClockTransport::pending_len)
            .ok_or(SimClockClusterError::UnknownController { controller })
    }
}
