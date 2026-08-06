//! G-code transpiler lowering slicer G-code into typed `dryer_control_protocol::Command` streams.

use dryer_control_protocol::Command;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringError {
    ParseError {
        line_number: usize,
        line: String,
        reason: String,
    },
}

impl fmt::Display for LoweringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParseError {
                line_number,
                line,
                reason,
            } => {
                write!(
                    f,
                    "G-code parse error at line {line_number} '{line}': {reason}"
                )
            }
        }
    }
}

impl std::error::Error for LoweringError {}

#[derive(Debug, Clone)]
pub struct LowererConfig {
    pub default_feed_rate_um_s: u64,
    pub hotend_name: String,
    pub bed_name: String,
}

impl Default for LowererConfig {
    fn default() -> Self {
        Self {
            default_feed_rate_um_s: 10_000,
            hotend_name: "hotend_heater".into(),
            bed_name: "bed_heater".into(),
        }
    }
}

#[derive(Debug)]
pub struct GcodeLowerer {
    config: LowererConfig,
    current_feed_rate_um_s: u64,
    absolute_positioning: bool,
    positions: BTreeMap<String, i64>,
}

impl GcodeLowerer {
    pub fn new(config: LowererConfig) -> Self {
        let feed = config.default_feed_rate_um_s;
        Self {
            config,
            current_feed_rate_um_s: feed,
            absolute_positioning: true,
            positions: BTreeMap::new(),
        }
    }

    pub fn lower_source(&mut self, gcode_text: &str) -> Result<Vec<Command>, LoweringError> {
        let mut commands = Vec::new();

        for (idx, raw_line) in gcode_text.lines().enumerate() {
            let _line_no = idx + 1;
            let line = clean_line(raw_line);
            if line.is_empty() {
                continue;
            }

            let tokens = parse_tokens(&line);
            if tokens.is_empty() {
                continue;
            }

            let cmd_letter = tokens[0].0;
            let cmd_num = tokens[0].1 as u64;

            match (cmd_letter, cmd_num) {
                ('G', 90) => {
                    self.absolute_positioning = true;
                }
                ('G', 91) => {
                    self.absolute_positioning = false;
                }
                ('G', 28) => {
                    let mut homed = false;
                    for (axis, _) in &tokens[1..] {
                        let axis_lower = axis.to_lowercase().to_string();
                        if matches!(axis_lower.as_str(), "x" | "y" | "z") {
                            commands.push(Command::Home {
                                axis: axis_lower.clone(),
                                rate_um_s: self.current_feed_rate_um_s,
                            });
                            self.positions.insert(axis_lower, 0);
                            homed = true;
                        }
                    }
                    if !homed {
                        for axis in ["x", "y", "z"] {
                            commands.push(Command::Home {
                                axis: axis.into(),
                                rate_um_s: self.current_feed_rate_um_s,
                            });
                            self.positions.insert(axis.into(), 0);
                        }
                    }
                }
                ('G', 0) | ('G', 1) => {
                    let mut new_feed = None;
                    for (param, val) in &tokens[1..] {
                        if *param == 'F' {
                            let rate = (*val * 1000.0 / 60.0).max(1.0) as u64;
                            new_feed = Some(rate);
                        }
                    }
                    if let Some(f) = new_feed {
                        self.current_feed_rate_um_s = f;
                    }

                    for (param, val) in &tokens[1..] {
                        let param_lower = param.to_lowercase().to_string();
                        if matches!(param_lower.as_str(), "x" | "y" | "z" | "e") {
                            let target_um = (*val * 1000.0) as i64;
                            let current_um = self.positions.get(&param_lower).copied().unwrap_or(0);
                            let delta_um = if self.absolute_positioning {
                                target_um - current_um
                            } else {
                                target_um
                            };

                            commands.push(Command::Move {
                                axis: param_lower.clone(),
                                distance_um: delta_um,
                                rate_um_s: self.current_feed_rate_um_s,
                            });

                            if self.absolute_positioning {
                                self.positions.insert(param_lower, target_um);
                            } else {
                                self.positions.insert(param_lower, current_um + delta_um);
                            }
                        }
                    }
                }
                ('M', 104) | ('M', 109) => {
                    if let Some((_, val)) = tokens.iter().find(|(p, _)| *p == 'S') {
                        let target_milli_c = (*val * 1000.0) as i64;
                        commands.push(Command::SetHeaterTarget {
                            heater: self.config.hotend_name.clone(),
                            target_milli_c,
                        });
                    }
                }
                ('M', 140) | ('M', 190) => {
                    if let Some((_, val)) = tokens.iter().find(|(p, _)| *p == 'S') {
                        let target_milli_c = (*val * 1000.0) as i64;
                        commands.push(Command::SetHeaterTarget {
                            heater: self.config.bed_name.clone(),
                            target_milli_c,
                        });
                    }
                }
                ('M', 105) => {
                    commands.push(Command::Heartbeat);
                }
                _ => {}
            }
        }

        Ok(commands)
    }
}

fn clean_line(s: &str) -> String {
    let s = s.trim();
    if let Some(pos) = s.find(';') {
        s[..pos].trim().to_string()
    } else if let Some(pos) = s.find('(') {
        s[..pos].trim().to_string()
    } else {
        s.to_string()
    }
}

fn parse_tokens(line: &str) -> Vec<(char, f64)> {
    let mut tokens = Vec::new();
    for word in line.split_whitespace() {
        let mut chars = word.chars();
        if let Some(letter) = chars.next() {
            let letter = letter.to_ascii_uppercase();
            let rest: String = chars.collect();
            if let Ok(num) = rest.parse::<f64>() {
                tokens.push((letter, num));
            }
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcode_lowering_converts_linear_moves_and_temperatures() {
        let gcode = r#"
            ; Slicer output header
            M104 S210 ; Set hotend temperature
            M140 S60  ; Set bed temperature
            G28 ; Home all axes
            G1 F3000 X10.5 Y20.0 ; Move at 3000 mm/min
            G1 X15.5 ; Move X by 5 mm relative to previous position
        "#;

        let mut lowerer = GcodeLowerer::new(LowererConfig::default());
        let cmds = lowerer.lower_source(gcode).unwrap();

        assert_eq!(cmds.len(), 8); // M104, M140, G28 (x,y,z), G1 X, G1 Y, G1 X
        assert_eq!(
            cmds[0],
            Command::SetHeaterTarget {
                heater: "hotend_heater".into(),
                target_milli_c: 210_000
            }
        );
        assert_eq!(
            cmds[1],
            Command::SetHeaterTarget {
                heater: "bed_heater".into(),
                target_milli_c: 60_000
            }
        );
        assert_eq!(
            cmds[2],
            Command::Home {
                axis: "x".into(),
                rate_um_s: 10_000
            }
        );

        // F3000 mm/min = 3000 * 1000 / 60 = 50000 um/s
        assert_eq!(
            cmds[5],
            Command::Move {
                axis: "x".into(),
                distance_um: 10_500,
                rate_um_s: 50_000
            }
        );
        assert_eq!(
            cmds[7],
            Command::Move {
                axis: "x".into(),
                distance_um: 5_000,
                rate_um_s: 50_000
            }
        );
    }
}
