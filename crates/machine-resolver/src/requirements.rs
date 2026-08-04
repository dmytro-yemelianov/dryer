use crate::packages::PackageSelection;
use dryer_machine_schema::MachineDoc;
use dryer_package_model::device::BusReq;
use std::collections::BTreeMap;

pub(super) struct DeviceRequirement {
    pub(super) reference: String,
    pub(super) connector: Option<String>,
    pub(super) domains: Vec<String>,
    pub(super) bus: Option<BusReq>,
}

/// Collect device requirements once per component type over the expanded
/// graph. Compatibility comes from the device package; the component
/// attribute supplies only the explicit-claim syntax.
pub(super) fn collect(
    doc: &MachineDoc,
    packages: &PackageSelection<'_>,
) -> BTreeMap<String, DeviceRequirement> {
    let mut device_reqs = BTreeMap::new();
    for comp in doc.components.values() {
        if device_reqs.contains_key(&comp.kind) {
            continue;
        }
        let Some(dev) = packages.select(&format!("devices/{}", comp.kind)) else {
            continue;
        };
        let Ok(payload) = dev.device_payload() else {
            continue; // payload errors surface when the device is used
        };
        let requirement = payload.requires;
        device_reqs.insert(
            comp.kind.clone(),
            DeviceRequirement {
                reference: dev.reference.to_string(),
                connector: requirement
                    .as_ref()
                    .and_then(|requirement| requirement.connector.clone()),
                domains: requirement
                    .as_ref()
                    .map(|requirement| requirement.voltage_domains.clone())
                    .unwrap_or_default(),
                bus: requirement.and_then(|requirement| requirement.bus),
            },
        );
    }
    device_reqs
}
