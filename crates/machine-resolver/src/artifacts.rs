use crate::model::{ControllerBuildPlan, ResolvedGraph};
use crate::packages::PackageSelection;
use crate::requirements::DeviceRequirement;
use dryer_machine_schema::{Diagnostic, MachineDoc};
use dryer_package_model::chip::ChipPackageFile;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Inputs<'a> {
    pub(super) doc: &'a MachineDoc,
    pub(super) device_reqs: &'a BTreeMap<String, DeviceRequirement>,
    pub(super) packages: &'a PackageSelection<'a>,
    pub(super) chips: &'a BTreeMap<String, ChipPackageFile>,
    pub(super) chip_refs: &'a BTreeMap<String, String>,
}

pub(super) fn plan(
    inputs: Inputs<'_>,
    resolved: &mut ResolvedGraph,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Inputs {
        doc,
        device_reqs,
        packages,
        chips,
        chip_refs,
    } = inputs;

    for (controller_name, controller) in &doc.controllers {
        let Some(chip) = chips.get(controller_name) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1600",
                    format!(
                        "controller '{controller_name}' has no resolved chip for firmware artifact planning"
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };
        let (Some(memory), Some(boot), Some(firmware), Some(chip_reference)) = (
            chip.memory.as_ref(),
            chip.boot.as_ref(),
            chip.firmware.as_ref(),
            chip_refs.get(controller_name),
        ) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1600",
                    format!(
                        "controller '{controller_name}' chip package lacks memory, boot, or firmware target metadata"
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };
        let (Some(flash_bytes), Some(ram_bytes)) = (memory.flash_bytes(), memory.ram_bytes())
        else {
            diagnostics.push(
                Diagnostic::error(
                    "E1601",
                    format!(
                        "controller '{controller_name}' chip memory cannot compile to whole bytes"
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };
        let Some(board_version) = packages.version(&controller.board) else {
            diagnostics.push(
                Diagnostic::error(
                    "E1602",
                    format!(
                        "controller '{controller_name}' board '{}' has no exact selected version",
                        controller.board
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };

        let mut features = firmware.features.clone();
        features.sort();
        features.dedup();
        let native_drivers: BTreeSet<String> = doc
            .components
            .iter()
            .filter(|(component_name, _)| {
                resolved
                    .assignments
                    .get(*component_name)
                    .into_iter()
                    .flatten()
                    .any(|assignment| {
                        assignment
                            .resource
                            .0
                            .split_once('.')
                            .is_some_and(|(candidate, _)| candidate == controller_name)
                    })
            })
            .filter_map(|(_, component)| {
                device_reqs
                    .get(&component.kind)
                    .map(|requirement| requirement.reference.clone())
            })
            .collect();

        resolved.controller_build_plans.insert(
            controller_name.clone(),
            ControllerBuildPlan {
                board: format!("{}@{board_version}", controller.board),
                chip: chip_reference.clone(),
                target_triple: firmware.target_triple.clone(),
                toolchain: firmware.toolchain.clone(),
                build_profile: firmware.build_profile.clone(),
                protocol_version: firmware.protocol_version.clone(),
                abi_version: firmware.abi_version.clone(),
                flash_bytes,
                ram_bytes,
                bootloader_offset_bytes: boot.default_bootloader_offset,
                features,
                native_drivers: native_drivers.into_iter().collect(),
            },
        );
    }
}
