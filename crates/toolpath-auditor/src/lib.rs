//! Pre-flight job and toolpath auditor (§23.6).
//!
//! Validates sequences of control commands against machine kinematics bounds,
//! maximum feed rates, and thermal safety ceilings prior to execution.

use std::collections::BTreeMap;
use dryer_control_protocol::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisLimit {
    pub min_um: i64,
    pub max_um: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditLimits {
    pub axes: BTreeMap<String, AxisLimit>,
    pub max_feed_rate_um_s: u64,
    pub heater_ceilings_milli_c: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditDiagnostic {
    pub code: String,
    pub command_index: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuditReport {
    pub passed: bool,
    pub commands_audited: usize,
    pub diagnostics: Vec<AuditDiagnostic>,
}

#[derive(Debug)]
pub struct ToolpathAuditor {
    limits: AuditLimits,
}

impl ToolpathAuditor {
    pub fn new(limits: AuditLimits) -> Self {
        Self { limits }
    }

    pub fn audit(&self, commands: &[Command]) -> AuditReport {
        let mut diagnostics = Vec::new();
        let mut positions: BTreeMap<String, i64> = BTreeMap::new();

        for (index, cmd) in commands.iter().enumerate() {
            match cmd {
                Command::Heartbeat => {}
                Command::SetHeaterTarget {
                    heater,
                    target_milli_c,
                } => {
                    let Some(&ceiling) = self.limits.heater_ceilings_milli_c.get(heater) else {
                        diagnostics.push(AuditDiagnostic {
                            code: "A004".into(),
                            command_index: index,
                            message: format!("unknown heater '{heater}' in command"),
                        });
                        continue;
                    };
                    if *target_milli_c > ceiling {
                        diagnostics.push(AuditDiagnostic {
                            code: "A003".into(),
                            command_index: index,
                            message: format!(
                                "heater '{heater}' target {target_milli_c} mC exceeds ceiling of {ceiling} mC"
                            ),
                        });
                    }
                }
                Command::Home { axis, rate_um_s } => {
                    if self.limits.max_feed_rate_um_s > 0 && *rate_um_s > self.limits.max_feed_rate_um_s {
                        diagnostics.push(AuditDiagnostic {
                            code: "A002".into(),
                            command_index: index,
                            message: format!(
                                "homing rate {rate_um_s} um/s exceeds maximum feed rate {} um/s",
                                self.limits.max_feed_rate_um_s
                            ),
                        });
                    }
                    positions.insert(axis.clone(), 0);
                }
                Command::Move {
                    axis,
                    distance_um,
                    rate_um_s,
                } => {
                    if self.limits.max_feed_rate_um_s > 0 && *rate_um_s > self.limits.max_feed_rate_um_s {
                        diagnostics.push(AuditDiagnostic {
                            code: "A002".into(),
                            command_index: index,
                            message: format!(
                                "move rate {rate_um_s} um/s exceeds maximum feed rate {} um/s",
                                self.limits.max_feed_rate_um_s
                            ),
                        });
                    }

                    let current_pos = positions.get(axis).copied().unwrap_or(0);
                    let new_pos = current_pos.saturating_add(*distance_um);
                    positions.insert(axis.clone(), new_pos);

                    if let Some(limit) = self.limits.axes.get(axis) {
                        if new_pos < limit.min_um || new_pos > limit.max_um {
                            diagnostics.push(AuditDiagnostic {
                                code: "A001".into(),
                                command_index: index,
                                message: format!(
                                    "axis '{axis}' position {new_pos} um is outside bounds [{min}, {max}] um",
                                    min = limit.min_um,
                                    max = limit.max_um
                                ),
                            });
                        }
                    }
                }
            }
        }

        let passed = diagnostics.is_empty();
        AuditReport {
            passed,
            commands_audited: commands.len(),
            diagnostics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_limits() -> AuditLimits {
        let mut axes = BTreeMap::new();
        axes.insert("x".into(), AxisLimit { min_um: 0, max_um: 200_000 });
        axes.insert("y".into(), AxisLimit { min_um: 0, max_um: 200_000 });

        let mut heaters = BTreeMap::new();
        heaters.insert("hotend_heater".into(), 300_000);

        AuditLimits {
            axes,
            max_feed_rate_um_s: 50_000,
            heater_ceilings_milli_c: heaters,
        }
    }

    #[test]
    fn valid_toolpath_passes_audit() {
        let auditor = ToolpathAuditor::new(sample_limits());
        let cmds = vec![
            Command::Home { axis: "x".into(), rate_um_s: 10_000 },
            Command::SetHeaterTarget { heater: "hotend_heater".into(), target_milli_c: 200_000 },
            Command::Move { axis: "x".into(), distance_um: 50_000, rate_um_s: 20_000 },
        ];
        let report = auditor.audit(&cmds);
        assert!(report.passed);
        assert_eq!(report.commands_audited, 3);
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn out_of_bounds_move_emits_a001() {
        let auditor = ToolpathAuditor::new(sample_limits());
        let cmds = vec![
            Command::Home { axis: "x".into(), rate_um_s: 10_000 },
            Command::Move { axis: "x".into(), distance_um: 250_000, rate_um_s: 20_000 },
        ];
        let report = auditor.audit(&cmds);
        assert!(!report.passed);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].code, "A001");
        assert!(report.diagnostics[0].message.contains("250000 um is outside bounds"));
    }

    #[test]
    fn excessive_feed_rate_emits_a002() {
        let auditor = ToolpathAuditor::new(sample_limits());
        let cmds = vec![
            Command::Move { axis: "x".into(), distance_um: 10_000, rate_um_s: 100_000 },
        ];
        let report = auditor.audit(&cmds);
        assert!(!report.passed);
        assert_eq!(report.diagnostics[0].code, "A002");
    }

    #[test]
    fn excessive_temperature_target_emits_a003() {
        let auditor = ToolpathAuditor::new(sample_limits());
        let cmds = vec![
            Command::SetHeaterTarget { heater: "hotend_heater".into(), target_milli_c: 350_000 },
        ];
        let report = auditor.audit(&cmds);
        assert!(!report.passed);
        assert_eq!(report.diagnostics[0].code, "A003");
    }
}
