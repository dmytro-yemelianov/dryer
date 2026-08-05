//! Deterministic Machine Graph resolution (spec §11), v0.1 slice.
//!
//! Implements the resolver's explicit phase structure (§11.2) over the
//! Phase 0 models. Phases 1–2 delegate to `dryer-machine-parser`; phases
//! 3–4 check and load packages; phase 5 is a recorded no-op (no package
//! templates exist yet); phases 6–7 validate and allocate **explicit
//! connector claims** — a component saying `connected_to: mainboard.motor0`
//! claims that connector, and the resolver checks existence, kind
//! compatibility, and exclusivity, then records an explainable assignment
//! (§11.5).
//!
//! Slice 2 added: **search-based allocation** — a component with no
//! explicit claim whose type names a device package with a
//! `requires.connector` payload (§9) gets the first free kind-compatible
//! connector in stable order, with every candidate recorded — and the
//! first **electrical validation** check (§11.2 phase 8): a component's
//! declared `current` draw must fit the connector's `max_current`.
//!
//! Slice 3 added the **safety phase** (profile coverage, §18); slice 5
//! rebuilt phase 3 as **transitive closure resolution**: roots are the
//! machine's pins plus implicit references (boards, safety profile),
//! dependency ranges intersect per package, and each package resolves to
//! the highest satisfying version at a fixpoint — machine pins are
//! absolute. The closure is published on `ResolvedGraph::packages` and is
//! what the lockfile pins.
//!
//! Slice 6 implemented **graph expansion** (machine-class templates,
//! §5.5); slice 7 added **voltage-domain validation**; slice 8 implemented
//! **peripheral mapping** (docs/peripheral-mapping.md): board pins join the
//! chip's pin-function table into per-assignment `pin_capabilities`, board
//! wiring is checked against the chip (E1312), and a declared
//! `max_step_rate` makes step pins require exclusive timer channels —
//! E1310 for gpio-only pins, E1314 for channel conflicts, with search
//! allocation steering around both.
//!
//! Slice 9 completed peripheral mapping with **bus/signal matching**
//! (§9 `requires.bus`): a device's bus family must appear among the
//! connector's derived capabilities and the chip's bus instance must
//! declare a sufficient `max_frequency` (E1315; silence never satisfies a
//! minimum). Device packages now also drive the expected connector kind
//! for explicit claims, retiring the attribute table wherever a device
//! exists.
//!
//! Slice 12 added **DMA routing and measured timing budgets**: device bus
//! requirements may name DMA signals plus maximum worst-case latency and
//! jitter; chip targets publish explicit routes and measured bounds. All are
//! hard search/claim constraints, and accepted evidence is recorded on the
//! assignment. DMA channel ownership/exclusivity remains a firmware concern.
//!
//! Slice 13 compiles validated class policy into concrete, controller-local
//! safe-state bindings (phase 11). Lockfile generation and versioned artifact
//! encoding remain separate downstream boundaries. Slice 14 adds phase 12
//! artifact planning: exact board/chip/native-driver inputs plus validated
//! target, toolchain, memory, feature, protocol, and ABI metadata.

mod allocation;
mod artifacts;
mod capability;
mod diagnostics;
mod electrical;
mod expansion;
mod model;
mod packages;
mod requirements;
mod safety;
mod targets;
#[cfg(test)]
mod tests;

#[cfg(test)]
use capability::bus_satisfied;
use diagnostics::locate_diagnostics;
use dryer_machine_schema::{Diagnostic, MachineDoc, Severity};
use dryer_package_model::LocalRegistry;
pub use model::{
    Assignment, ControllerBuildPlan, ControllerSafeState, Phase, ResolveOutcome, ResolvedGraph,
};
#[cfg(test)]
use std::collections::BTreeMap;

/// Resolve a machine manifest source against a local package registry.
///
/// Determinism (§11.4): all iteration is over `BTreeMap`s, so identical
/// inputs produce identical outcomes including diagnostic order.
pub fn resolve_source(source: &str, registry: &LocalRegistry) -> ResolveOutcome {
    let mut phases_run = vec![Phase::Parse, Phase::SchemaValidation];
    let dryer_machine_parser::ParseOutcome {
        doc,
        mut diagnostics,
        spans,
    } = dryer_machine_parser::parse_str(source);
    let Some(doc) = doc else {
        let mut outcome = ResolveOutcome {
            resolved: None,
            diagnostics,
            phases_run,
        };
        locate_diagnostics(&mut outcome.diagnostics, &spans);
        return outcome;
    };
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        let mut outcome = ResolveOutcome {
            resolved: None,
            diagnostics,
            phases_run,
        };
        locate_diagnostics(&mut outcome.diagnostics, &spans);
        return outcome;
    }
    let mut outcome = resolve_doc(&doc, registry, &mut diagnostics, &mut phases_run);
    locate_diagnostics(&mut outcome.diagnostics, &spans);
    outcome
}

fn resolve_doc(
    doc: &MachineDoc,
    registry: &LocalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
    phases_run: &mut Vec<Phase>,
) -> ResolveOutcome {
    let fail = |diagnostics: Vec<Diagnostic>, phases_run: Vec<Phase>| ResolveOutcome {
        resolved: None,
        diagnostics,
        phases_run,
    };

    // --- Phase 3: package dependency resolution (transitive closure) ---
    //
    // Roots are the machine's explicit pins plus its implicit package
    // references (controller boards, the safety profile). Dependencies are
    // resolved to a fixpoint: each package's version is the HIGHEST
    // registry version satisfying the intersection of every requirer's
    // range (§11.4 stable selection rule); a machine pin is absolute and
    // conflicts with it are errors, never silent overrides.
    phases_run.push(Phase::PackageDependencies);
    let packages = packages::PackageSelection::resolve(doc, registry, diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 4: package loading (board payloads per controller) ------
    phases_run.push(Phase::PackageLoading);
    let targets::ControllerTargets {
        boards,
        chips,
        chip_refs,
    } = targets::load(doc, &packages, diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 5: graph expansion (§5.5) --------------------------------
    // Every machine-kind package in the closure contributes its template,
    // in sorted package order. The SOURCE graph is never mutated: expansion
    // produces `expanded`, and all later phases read that. Source wins —
    // a template never overrides a user declaration; every contribution
    // and shadowing is surfaced as an Info diagnostic so the expanded
    // graph stays explainable.
    phases_run.push(Phase::GraphExpansion);
    let expansion::ExpandedGraph {
        doc: expanded,
        sources: expanded_sources,
    } = expansion::expand(doc, &packages, diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }
    let doc = &expanded;

    // Device requirements (§9), resolved once per component type over the
    // EXPANDED graph. This is what replaces the v0 attribute→kind table
    // wherever a device package exists: compatibility comes from the
    // device, the attribute only supplies the claim syntax.
    let device_reqs = requirements::collect(doc, &packages);

    let Some(mut resolved) = allocation::allocate(
        allocation::Inputs {
            doc,
            expanded_sources: &expanded_sources,
            device_reqs: &device_reqs,
            boards: &boards,
            chips: &chips,
            packages: &packages,
        },
        diagnostics,
        phases_run,
    ) else {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    };

    // --- Phase 8: electrical validation ---------------------------------
    // (a) a component's declared `current` draw must fit the connector's
    //     `max_current`; (b) a device's required voltage domains must
    //     include the assigned connector's; (c) bus frequency, measured
    //     latency/jitter, and DMA routes must satisfy the device package.
    //     Silence never satisfies a declared requirement.
    phases_run.push(Phase::ElectricalValidation);
    electrical::validate(
        electrical::Inputs {
            doc,
            device_reqs: &device_reqs,
            boards: &boards,
            chips: &chips,
        },
        &resolved,
        diagnostics,
    );

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 10: safety validation (coverage check) --------------------
    // The profile must exist as a safety-profile package, and every
    // component that resolved a hazardous output (a power_output connector)
    // or delegates an actuator output to a driver must belong to a class the
    // profile covers (§18.2). Delegated resources must be real driver sockets.
    // Classes may add structural requirements (requires_sensor, §18.3). A
    // required sensor must resolve through an input connector on the same
    // controller so edge enforcement never depends on a host or link.
    phases_run.push(Phase::SafetyValidation);
    let safety_profile = safety::validate(doc, &resolved, &packages, diagnostics);

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 11: firmware partitioning --------------------------------
    // Convert policy strings/quantities into concrete controller-local
    // resources and integer controller time. `machine-lock` pins this
    // projection and `firmware-build` wraps it in a versioned artifact.
    phases_run.push(Phase::FirmwarePartitioning);
    safety::partition(doc, &mut resolved, safety_profile.as_ref(), diagnostics);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }

    // --- Phase 12: artifact planning ------------------------------------
    // Select exact board/chip packages and compile target quantities before
    // lock generation. Firmware-build consumes only this locked projection;
    // it never guesses a target or rereads package metadata.
    phases_run.push(Phase::ArtifactPlanning);
    artifacts::plan(
        artifacts::Inputs {
            doc,
            device_reqs: &device_reqs,
            packages: &packages,
            chips: &chips,
            chip_refs: &chip_refs,
        },
        &mut resolved,
        diagnostics,
    );
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return fail(std::mem::take(diagnostics), phases_run.clone());
    }
    ResolveOutcome {
        resolved: Some(resolved),
        diagnostics: std::mem::take(diagnostics),
        phases_run: phases_run.clone(),
    }
}
