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

    // --- transport.parent structural checks (E1121/E1122) ---
    // A second pass: the parent controller's board may sort after the
    // child's in `doc.controllers` (a BTreeMap), so this only runs once
    // every controller's board payload has been loaded above. Shape
    // (`controller.port`) and "known controller" are already checked by
    // the parser (E0502/E0503); a board that itself failed to load is
    // already diagnosed above and is silently skipped here.
    for (name, ctrl) in &doc.controllers {
        let Some(parent) = &ctrl.transport.parent else {
            continue;
        };
        let Some((pctrl, port)) = parent.split_once('.') else {
            continue;
        };
        let Some(parent_board) = boards.get(pctrl) else {
            continue;
        };
        match parent_board.downlinks.get(port) {
            None => {
                let available: Vec<&str> =
                    parent_board.downlinks.keys().map(String::as_str).collect();
                let mut d = Diagnostic::error(
                    "E1121",
                    format!(
                        "controller '{name}': transport.parent '{parent}' names port '{port}', which board '{}' does not declare as a downlink",
                        doc.controllers[pctrl].board
                    ),
                )
                .at(format!("controllers.{name}.transport.parent"));
                d = if available.is_empty() {
                    d.suggest(format!(
                        "'{}' declares no downlinks",
                        doc.controllers[pctrl].board
                    ))
                } else {
                    d.suggest(format!("available downlinks: {}", available.join(", ")))
                };
                diagnostics.push(d);
            }
            Some(downlink) if downlink.kind != ctrl.transport.kind => {
                diagnostics.push(
                    Diagnostic::error(
                        "E1122",
                        format!(
                            "controller '{name}': transport type '{}' disagrees with parent downlink '{parent}' type '{}'",
                            ctrl.transport.kind, downlink.kind
                        ),
                    )
                    .at(format!("controllers.{name}.transport.type")),
                );
            }
            Some(_) => {}
        }
    }

    // --- controller parent cycles (E1123): whole-graph check ---
    // Every controller has at most one parent, so a cycle is detected by
    // walking each controller's parent chain until it either terminates
    // (no `transport.parent`) or revisits a controller already seen on
    // this walk (which also catches self-parenting on the first step).
    // `reported` avoids emitting the same cycle once per member.
    let mut reported: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for start in doc.controllers.keys() {
        if reported.contains(start) {
            continue;
        }
        let mut path = vec![start.clone()];
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        seen.insert(start.as_str());
        let mut current = start.as_str();
        loop {
            let Some(ctrl) = doc.controllers.get(current) else {
                break;
            };
            let Some(parent) = &ctrl.transport.parent else {
                break;
            };
            let Some((pctrl, _)) = parent.split_once('.') else {
                break;
            };
            if !seen.insert(pctrl) {
                diagnostics.push(
                    Diagnostic::error(
                        "E1123",
                        format!(
                            "controller '{start}': transport.parent chain cycles back through '{pctrl}'"
                        ),
                    )
                    .at(format!("controllers.{start}.transport.parent")),
                );
                reported.extend(path.iter().cloned());
                reported.insert(pctrl.to_string());
                break;
            }
            path.push(pctrl.to_string());
            current = pctrl;
        }
    }

    ControllerTargets {
        boards,
        chips,
        chip_refs,
    }
}
