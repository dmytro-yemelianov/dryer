use dryer_machine_schema::{Diagnostic, Severity, SourceSpan};
use dryer_resource_model::ResourceId;
use serde::Serialize;
use std::collections::BTreeMap;

/// Resolver phases (§11.2). `phases_run` in the outcome records how far
/// resolution progressed before stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Parse,
    SchemaValidation,
    PackageDependencies,
    PackageLoading,
    GraphExpansion,
    CapabilityMatching,
    ResourceAllocation,
    ElectricalValidation,
    SafetyValidation,
    FirmwarePartitioning,
    ArtifactPlanning,
}

/// One explainable assignment (§11.5): which requirement asked, what was
/// considered, what was chosen.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assignment {
    /// The component that claimed the resource.
    pub requested_by: String,
    /// The attribute the claim came from (`connected_to`, `output`, `input`).
    pub via: String,
    /// The chosen concrete resource (`controller.connector`).
    pub resource: ResourceId,
    /// Connector kind of the chosen resource.
    pub connector_kind: String,
    /// Candidates considered. Explicit claims consider exactly one; the
    /// future search-based allocator will list every kind-compatible
    /// connector here.
    pub candidates_considered: Vec<String>,
    /// Human-readable constraints applied to accept the claim.
    pub constraints_applied: Vec<String>,
    /// Signal → chip capability tokens for the connector's pins, derived by
    /// joining board wiring with the chip's pin-function table
    /// (docs/peripheral-mapping.md). Empty when the board names no chip or
    /// the chip carries no table.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_capabilities: BTreeMap<String, Vec<String>>,
}

/// One concrete edge-enforced safety action assigned to a controller during
/// firmware partitioning (§11.2 phase 11, §18.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ControllerSafeState {
    pub component: String,
    pub class: String,
    /// Concrete controller resource (`controller.connector`).
    pub resource: ResourceId,
    pub state: dryer_package_model::safety::SafeState,
    /// Compiled to the controller's 1 us time quantum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_timeout_us: Option<u64>,
    /// Concrete sensor resource on the same controller when policy requires it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensor: Option<ResourceId>,
}

/// Deterministic controller build inputs selected during artifact planning
/// (§11.2 phase 12, §21.1). Package references are exact and all physical
/// capacities are compiled to integer bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ControllerBuildPlan {
    pub board: String,
    pub chip: String,
    pub target_triple: String,
    pub toolchain: String,
    pub build_profile: String,
    pub protocol_version: String,
    pub abi_version: String,
    pub flash_bytes: u64,
    pub ram_bytes: u64,
    pub bootloader_offset_bytes: u64,
    pub features: Vec<String>,
    pub native_drivers: Vec<String>,
}

/// The resolved graph, v0.1: deterministic assignments keyed by component,
/// plus the full package closure the machine uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ResolvedGraph {
    pub assignments: BTreeMap<String, Vec<Assignment>>,
    /// Controller id → deterministic local safety configuration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub controller_safety: BTreeMap<String, Vec<ControllerSafeState>>,
    /// Controller id → deterministic firmware target/build inputs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub controller_build_plans: BTreeMap<String, ControllerBuildPlan>,
    /// Every package the resolution selected — explicit pins, implicit
    /// roots (boards, safety profile) and transitive dependencies — as
    /// `namespace/name@version`, sorted. This is what the lockfile pins.
    pub packages: Vec<String>,
}

impl ResolvedGraph {
    /// Explain one component's assignments (the CLI `explain` seed, §11.5).
    pub fn explain(&self, component: &str) -> Option<String> {
        let list = self.assignments.get(component);
        let safety: Vec<&ControllerSafeState> = self
            .controller_safety
            .values()
            .flatten()
            .filter(|binding| binding.component == component)
            .collect();
        if list.is_none() && safety.is_empty() {
            return None;
        }
        let mut s = String::new();
        for a in list.into_iter().flatten() {
            s.push_str(&format!(
                "{} --{}--> {} (kind {})\n  candidates: {}\n  constraints: {}\n",
                a.requested_by,
                a.via,
                a.resource.0,
                a.connector_kind,
                a.candidates_considered.join(", "),
                a.constraints_applied.join("; "),
            ));
            for (signal, funcs) in &a.pin_capabilities {
                s.push_str(&format!("  {signal}: {}\n", funcs.join(" | ")));
            }
        }
        for binding in safety {
            s.push_str(&format!(
                "{} --safe_state--> {} = {}",
                binding.component,
                binding.resource.0,
                binding.state.as_str()
            ));
            if let Some(timeout) = binding.heartbeat_timeout_us {
                s.push_str(&format!(" (heartbeat {} us)", timeout));
            }
            if let Some(sensor) = &binding.sensor {
                s.push_str(&format!(" (sensor {})", sensor.0));
            }
            s.push('\n');
        }
        Some(s)
    }
}

/// Everything a resolution run produces (§11.1, v0.1 subset).
#[derive(Debug)]
pub struct ResolveOutcome {
    pub resolved: Option<ResolvedGraph>,
    pub diagnostics: Vec<Diagnostic>,
    pub phases_run: Vec<Phase>,
}

impl ResolveOutcome {
    pub fn is_ok(&self) -> bool {
        self.resolved.is_some()
            && !self
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RequirementConstraint {
    pub(super) required_by: String,
    pub(super) requirement: semver::VersionReq,
    pub(super) source: Option<SourceSpan>,
}

#[derive(Debug, Clone)]
pub(super) struct ResourceClaim {
    pub(super) component: String,
    pub(super) path: String,
    pub(super) source: Option<SourceSpan>,
}

#[derive(Debug, Clone)]
pub(super) struct TimerClaim {
    pub(super) component: String,
    pub(super) pin: String,
    pub(super) path: String,
    pub(super) source: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BusMatch {
    pub(super) instance: String,
    pub(super) latency: Option<String>,
    pub(super) jitter: Option<String>,
    pub(super) dma_routes: BTreeMap<String, String>,
}

impl BusMatch {
    pub(super) fn constraints(
        &self,
        requirement: &dryer_package_model::device::BusReq,
    ) -> Vec<String> {
        let mut constraints = vec![format!(
            "{} bus via {}{}",
            requirement.kind,
            self.instance,
            requirement
                .min_frequency
                .as_deref()
                .map(|frequency| format!(" (>= {frequency})"))
                .unwrap_or_default()
        )];
        if let (Some(actual), Some(limit)) = (&self.latency, &requirement.max_latency) {
            constraints.push(format!("worst-case bus latency {actual} <= {limit}"));
        }
        if let (Some(actual), Some(limit)) = (&self.jitter, &requirement.max_jitter) {
            constraints.push(format!("worst-case bus jitter {actual} <= {limit}"));
        }
        constraints.extend(self.dma_routes.iter().map(|(signal, channel)| {
            format!("DMA route {}.{signal} via {channel}", self.instance)
        }));
        constraints
    }
}
