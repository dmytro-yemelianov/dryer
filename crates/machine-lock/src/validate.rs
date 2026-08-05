use crate::{Lockfile, CONTROLLER_BUILD_SCHEMA, CONTROLLER_SAFETY_SCHEMA};

impl Lockfile {
    /// Validate version-specific invariants before downstream artifact work.
    pub fn validate(&self) -> Result<(), String> {
        if self.lock_version >= 2 {
            for (index, package) in self.packages.iter().enumerate() {
                if package.content_hash.is_empty() {
                    return Err(format!(
                        "lockfile v{} package[{index}] '{}' has no content_hash",
                        self.lock_version, package.id
                    ));
                }
            }
            if self.safety_profile.content_hash.is_empty() {
                return Err(format!(
                    "lockfile v{} safety_profile '{}' has no content_hash",
                    self.lock_version, self.safety_profile.id
                ));
            }
        }
        if self.lock_version >= 3 {
            for (name, controller) in &self.controllers {
                let safety = controller.safety.as_ref().ok_or_else(|| {
                    format!(
                        "lockfile v{} controller '{name}' has no compiled safety configuration",
                        self.lock_version
                    )
                })?;
                if safety.schema != CONTROLLER_SAFETY_SCHEMA {
                    return Err(format!(
                        "lockfile v{} controller '{name}' safety schema '{}' is not '{}'",
                        self.lock_version, safety.schema, CONTROLLER_SAFETY_SCHEMA
                    ));
                }
                let resources: std::collections::BTreeSet<&str> = controller
                    .resolved_resources
                    .values()
                    .map(String::as_str)
                    .collect();
                let mut resources_with_safety = std::collections::BTreeSet::new();
                for state in &safety.states {
                    if state.component.trim().is_empty()
                        || state.component.trim() != state.component
                        || state.class.trim().is_empty()
                        || state.class.trim() != state.class
                    {
                        return Err(format!(
                            "lockfile v{} controller '{name}' has an empty or padded safety component/class",
                            self.lock_version
                        ));
                    }
                    if !resources_with_safety.insert(state.resource.as_str()) {
                        return Err(format!(
                            "lockfile v{} controller '{name}' repeats safety state for physical resource '{}'",
                            self.lock_version, state.resource
                        ));
                    }
                    if !resources.contains(state.resource.as_str()) {
                        return Err(format!(
                            "lockfile v{} controller '{name}' safety resource '{}' is not resolved",
                            self.lock_version, state.resource
                        ));
                    }
                    if state
                        .sensor
                        .as_deref()
                        .is_some_and(|sensor| !resources.contains(sensor))
                    {
                        return Err(format!(
                            "lockfile v{} controller '{name}' safety sensor '{}' is not resolved",
                            self.lock_version,
                            state.sensor.as_deref().unwrap_or_default()
                        ));
                    }
                    if state.heartbeat_timeout_us == Some(0) {
                        return Err(format!(
                            "lockfile v{} controller '{name}' safety heartbeat timeout must be positive",
                            self.lock_version
                        ));
                    }
                }
            }
        }
        if self.lock_version >= 4 {
            let packages: std::collections::BTreeSet<&str> = self
                .packages
                .iter()
                .map(|package| package.id.as_str())
                .collect();
            for (name, controller) in &self.controllers {
                let build = controller.build.as_ref().ok_or_else(|| {
                    format!(
                        "lockfile v{} controller '{name}' has no compiled build configuration",
                        self.lock_version
                    )
                })?;
                if build.schema != CONTROLLER_BUILD_SCHEMA {
                    return Err(format!(
                        "lockfile v{} controller '{name}' build schema '{}' is not '{}'",
                        self.lock_version, build.schema, CONTROLLER_BUILD_SCHEMA
                    ));
                }
                if !build
                    .board
                    .strip_prefix(&controller.board)
                    .is_some_and(|suffix| suffix.starts_with('@'))
                {
                    return Err(format!(
                        "lockfile v{} controller '{name}' build board '{}' does not pin '{}'",
                        self.lock_version, build.board, controller.board
                    ));
                }
                for package in std::iter::once(build.board.as_str())
                    .chain(std::iter::once(build.chip.as_str()))
                    .chain(build.native_drivers.iter().map(String::as_str))
                {
                    if !packages.contains(package) {
                        return Err(format!(
                            "lockfile v{} controller '{name}' build input '{package}' is not in packages",
                            self.lock_version
                        ));
                    }
                }
                for (field, value) in [
                    ("target_triple", build.target_triple.as_str()),
                    ("toolchain", build.toolchain.as_str()),
                    ("build_profile", build.build_profile.as_str()),
                    ("protocol_version", build.protocol_version.as_str()),
                    ("abi_version", build.abi_version.as_str()),
                ] {
                    if value.is_empty() || value.trim() != value {
                        return Err(format!(
                            "lockfile v{} controller '{name}' build {field} is empty or padded",
                            self.lock_version
                        ));
                    }
                }
                for (field, value) in [
                    ("protocol_version", build.protocol_version.as_str()),
                    ("abi_version", build.abi_version.as_str()),
                ] {
                    if !versioned_interface(value) {
                        return Err(format!(
                            "lockfile v{} controller '{name}' build {field} is not a versioned interface",
                            self.lock_version
                        ));
                    }
                }
                if build.flash_bytes == 0
                    || build.ram_bytes == 0
                    || build.bootloader_offset_bytes >= build.flash_bytes
                {
                    return Err(format!(
                        "lockfile v{} controller '{name}' has invalid build memory layout",
                        self.lock_version
                    ));
                }
                for (field, values) in [
                    ("features", &build.features),
                    ("native_drivers", &build.native_drivers),
                ] {
                    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
                        return Err(format!(
                            "lockfile v{} controller '{name}' build {field} must be sorted and unique",
                            self.lock_version
                        ));
                    }
                }
                if build
                    .features
                    .iter()
                    .any(|feature| !dryer_machine_schema::valid_identifier(feature))
                {
                    return Err(format!(
                        "lockfile v{} controller '{name}' build features contain an invalid identifier",
                        self.lock_version
                    ));
                }
            }
        }
        if self.lock_version >= 5 {
            let source = self.registry_source.as_ref().ok_or_else(|| {
                format!(
                    "lockfile v{} has no registry source identity",
                    self.lock_version
                )
            })?;
            source.validate().map_err(|error| {
                format!(
                    "lockfile v{} has invalid registry source identity: {error}",
                    self.lock_version
                )
            })?;
        }
        Ok(())
    }
}

fn versioned_interface(value: &str) -> bool {
    value.split_once("/v").is_some_and(|(name, version)| {
        !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_lowercase() || character == '.')
            && matches!(version.as_bytes().first(), Some(b'1'..=b'9'))
            && version.bytes().all(|digit| digit.is_ascii_digit())
            && version.parse::<u32>().is_ok()
    })
}
