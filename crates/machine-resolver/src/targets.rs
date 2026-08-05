use crate::packages::PackageSelection;
use dryer_machine_schema::{Diagnostic, MachineDoc};
use dryer_package_model::{board::BoardPackageFile, chip::ChipPackageFile};
use std::collections::BTreeMap;

pub(super) struct ControllerTargets {
    pub(super) boards: BTreeMap<String, BoardPackageFile>,
    pub(super) chips: BTreeMap<String, ChipPackageFile>,
    pub(super) chip_refs: BTreeMap<String, String>,
}

pub(super) fn load(
    doc: &MachineDoc,
    packages: &PackageSelection<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ControllerTargets {
    let mut boards = BTreeMap::new();
    let mut chips = BTreeMap::new();
    let mut chip_refs = BTreeMap::new();
    for (controller_name, controller) in &doc.controllers {
        let Some((namespace, name)) = controller.board.split_once('/') else {
            diagnostics.push(
                Diagnostic::error(
                    "E1110",
                    format!(
                        "controller '{controller_name}': board '{}' must be 'namespace/name'",
                        controller.board
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            );
            continue;
        };
        let _ = (namespace, name); // Shape validated above; selection is closure-aware.
        match packages.select(&controller.board) {
            None => diagnostics.push(
                Diagnostic::error(
                    "E1111",
                    format!(
                        "controller '{controller_name}': board package '{}' is not in the registry",
                        controller.board
                    ),
                )
                .at(format!("controllers.{controller_name}.board")),
            ),
            Some(package) => match package.board_payload() {
                Ok(payload) => {
                    if !payload.transports.contains_key(&controller.transport.kind) {
                        diagnostics.push(
                            Diagnostic::error(
                                "E1120",
                                format!(
                                    "controller '{controller_name}': board '{}' has no '{}' transport",
                                    controller.board, controller.transport.kind
                                ),
                            )
                            .at(format!("controllers.{controller_name}.transport")),
                        );
                    }
                    // Peripheral mapping joins board pins to the selected chip's
                    // pin-function table. The fallback in PackageSelection is
                    // intentional for legacy board manifests whose chip is not
                    // declared in the dependency closure (E1311).
                    if let Some(chip_reference) = payload.chip.clone() {
                        let chip_package = packages.select(&chip_reference);
                        if let Some(chip_package) = chip_package {
                            if !packages.contains(&chip_reference) {
                                diagnostics.push(Diagnostic::warning(
                                    "E1311",
                                    format!(
                                        "board '{}' chip '{chip_reference}' is not in the dependency closure; using {} (highest available)",
                                        controller.board, chip_package.reference.version
                                    ),
                                ));
                            }
                            match chip_package.chip_payload() {
                                Ok(chip) => {
                                    if !chip.pin_functions.is_empty() {
                                        for (connector_id, connector) in &payload.connectors {
                                            for pin in
                                                connector.pins.values().chain(connector.pin.iter())
                                            {
                                                if !chip.pin_functions.contains_key(pin) {
                                                    diagnostics.push(Diagnostic::error(
                                                        "E1312",
                                                        format!(
                                                            "board '{}' connector '{connector_id}' wires pin {pin}, which chip '{chip_reference}' does not declare",
                                                            controller.board
                                                        ),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                    chip_refs.insert(
                                        controller_name.clone(),
                                        chip_package.reference.to_string(),
                                    );
                                    chips.insert(controller_name.clone(), chip);
                                }
                                Err(errors) => diagnostics.extend(errors),
                            }
                        } else {
                            diagnostics.push(Diagnostic::error(
                                "E1313",
                                format!(
                                    "board '{}' references chip '{chip_reference}', which is not in the registry",
                                    controller.board
                                ),
                            ));
                        }
                    }
                    boards.insert(controller_name.clone(), payload);
                }
                Err(errors) => diagnostics.extend(errors),
            },
        }
    }
    ControllerTargets {
        boards,
        chips,
        chip_refs,
    }
}
