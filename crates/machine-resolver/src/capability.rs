use crate::model::{BusMatch, ResolvedGraph};
use dryer_machine_schema::{Component, Dimension, Quantity};
use dryer_resource_model::ResourceId;
use std::collections::BTreeMap;

/// Resolve the physical resources governed by a component's safety policy.
/// Most components own resources directly; logical actuators such as a
/// `stepper_motor` inherit the connector assigned to their declared driver.
pub(super) fn safety_target_resources(
    resolved: &ResolvedGraph,
    component_name: &str,
    component: &Component,
) -> Result<Vec<ResourceId>, String> {
    let direct = resolved
        .assignments
        .get(component_name)
        .filter(|assignments| !assignments.is_empty());
    let assignments = match direct {
        Some(assignments) => assignments,
        None => {
            let Some(driver) = component
                .attributes
                .get("driver")
                .and_then(|value| value.as_str())
            else {
                return Ok(Vec::new());
            };
            let Some(assignments) = resolved
                .assignments
                .get(driver)
                .filter(|assignments| !assignments.is_empty())
            else {
                return Ok(Vec::new());
            };
            if let Some(assignment) = assignments
                .iter()
                .find(|assignment| assignment.connector_kind != "stepper_driver_socket")
            {
                return Err(format!(
                    "component '{component_name}' names '{driver}' as its driver, but '{}' is a '{}' connector rather than a stepper driver socket",
                    assignment.resource.0, assignment.connector_kind
                ));
            }
            assignments
        }
    };
    let mut resources: Vec<ResourceId> = assignments
        .iter()
        .map(|assignment| assignment.resource.clone())
        .collect();
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    resources.dedup();
    Ok(resources)
}

pub(super) fn is_sensor_connector_kind(connector_kind: &str) -> bool {
    matches!(connector_kind, "analog_input" | "digital_input")
}

pub(super) fn sensor_resource_on_controller(
    resolved: &ResolvedGraph,
    component: &Component,
    controller: &str,
) -> Option<ResourceId> {
    let sensor = component
        .attributes
        .get("sensor")
        .and_then(|value| value.as_str())?;
    resolved
        .assignments
        .get(sensor)?
        .iter()
        .filter(|assignment| is_sensor_connector_kind(&assignment.connector_kind))
        .find_map(|assignment| {
            assignment
                .resource
                .0
                .split_once('.')
                .filter(|(candidate, _)| *candidate == controller)
                .map(|_| assignment.resource.clone())
        })
}

/// Join a connector's pins to the chip's pin-function table
/// (docs/peripheral-mapping.md). No chip / no table ⇒ empty: absence of
/// data disables capability checks, it never fakes them.
pub(super) fn derive_pin_capabilities(
    chip: Option<&dryer_package_model::chip::ChipPackageFile>,
    connector: &dryer_package_model::board::Connector,
) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let Some(chip) = chip else { return out };
    if chip.pin_functions.is_empty() {
        return out;
    }
    for (signal, pin) in &connector.pins {
        if let Some(f) = chip.pin_functions.get(pin) {
            out.insert(signal.clone(), f.clone());
        }
    }
    if let Some(pin) = &connector.pin {
        if let Some(f) = chip.pin_functions.get(pin) {
            out.insert("pin".to_string(), f.clone());
        }
    }
    out
}

/// Does the connector (via its derived capabilities) satisfy a §9 bus
/// requirement? Returns the matched bus instance and its verified evidence,
/// or the reason it fails. Undeclared frequency, timing, or DMA data never
/// satisfies a corresponding hard requirement — silence is not compatibility.
pub(super) fn bus_satisfied(
    chip: Option<&dryer_package_model::chip::ChipPackageFile>,
    caps: &BTreeMap<String, Vec<String>>,
    req: &dryer_package_model::device::BusReq,
) -> Result<BusMatch, String> {
    let instance = caps.values().flatten().find_map(|tok| {
        let inst = tok.split('.').next().unwrap_or(tok);
        let family = inst.trim_end_matches(|c: char| c.is_ascii_digit());
        (family == req.kind).then(|| inst.to_string())
    });
    let Some(instance) = instance else {
        return Err(format!("no {} function on any connector pin", req.kind));
    };
    let needs_metadata = req.min_frequency.is_some()
        || req.max_latency.is_some()
        || req.max_jitter.is_some()
        || !req.dma_signals.is_empty();
    let declared = chip.and_then(|chip| chip.buses().find(|bus| bus.id == instance));
    if needs_metadata && declared.is_none() {
        return Err(format!(
            "bus '{instance}' has no declared capability metadata"
        ));
    }

    if let Some(min_raw) = &req.min_frequency {
        let min = Quantity::parse_as(min_raw, Dimension::Frequency).map_err(|e| e.to_string())?;
        let frequency = declared.and_then(|bus| bus.max_frequency.as_ref());
        let Some(frequency) = frequency else {
            return Err(format!(
                "bus '{instance}' declares no max_frequency (required >= {min_raw})"
            ));
        };
        let actual =
            Quantity::parse_as(frequency, Dimension::Frequency).map_err(|e| e.to_string())?;
        if actual.value < min.value {
            return Err(format!(
                "bus '{instance}' max_frequency {frequency} < required {min_raw}"
            ));
        }
    }

    let check_time = |field: &str,
                      actual: Option<&String>,
                      limit: Option<&String>|
     -> Result<Option<String>, String> {
        let Some(limit) = limit else { return Ok(None) };
        let Some(actual) = actual else {
            return Err(format!(
                "bus '{instance}' declares no {field} (required <= {limit})"
            ));
        };
        let actual_quantity =
            Quantity::parse_as(actual, Dimension::Time).map_err(|e| e.to_string())?;
        let limit_quantity =
            Quantity::parse_as(limit, Dimension::Time).map_err(|e| e.to_string())?;
        if actual_quantity.value > limit_quantity.value {
            return Err(format!(
                "bus '{instance}' {field} {actual} > required maximum {limit}"
            ));
        }
        Ok(Some(actual.clone()))
    };
    let latency = check_time(
        "worst_case_latency",
        declared.and_then(|bus| bus.worst_case_latency.as_ref()),
        req.max_latency.as_ref(),
    )?;
    let jitter = check_time(
        "worst_case_jitter",
        declared.and_then(|bus| bus.worst_case_jitter.as_ref()),
        req.max_jitter.as_ref(),
    )?;

    let mut dma_routes = BTreeMap::new();
    if !req.dma_signals.is_empty() {
        let chip = chip.ok_or_else(|| "no chip table to verify DMA routing against".to_string())?;
        for signal in &req.dma_signals {
            let route = format!("{instance}.{signal}");
            let Some(channel) = chip.dma_channel_for_route(&route) else {
                return Err(format!("no DMA channel routes '{route}'"));
            };
            dma_routes.insert(signal.clone(), channel.id.clone());
        }
    }
    Ok(BusMatch {
        instance,
        latency,
        jitter,
        dma_routes,
    })
}

/// v0.1 claim-compatibility table: which connector kind each claiming
/// attribute requires. This is a deliberate stopgap — once device packages
/// carry requirement payloads (§9), compatibility comes from the claimed
/// component's device package, not from the attribute name.
pub(super) fn claim_kind(attr: &str) -> Option<&'static str> {
    match attr {
        "connected_to" => Some("stepper_driver_socket"),
        "output" => Some("power_output"),
        "input" => Some("analog_input"),
        _ => None,
    }
}
