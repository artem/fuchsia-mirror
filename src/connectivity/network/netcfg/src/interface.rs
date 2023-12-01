// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context as _,
    either::Either,
    serde::{Deserialize, Deserializer, Serialize},
    std::collections::HashSet,
    std::io::{Seek, SeekFrom},
    std::{fs, io, path},
};

use crate::DeviceClass;

const INTERFACE_PREFIX_WLAN: &str = "wlan";
const INTERFACE_PREFIX_ETHERNET: &str = "eth";

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Clone)]
pub(crate) enum PersistentIdentifier {
    MacAddress(fidl_fuchsia_net_ext::MacAddress),
    TopologicalPath(String),
}

// TODO(https://fxbug.dev/118197): Remove this once we soak for some time to ensure no
// devices using the iwlwifi driver are using the legacy config format.
#[derive(Serialize, Deserialize, Debug)]
struct LegacyConfig {
    names: Vec<(PersistentIdentifier, String)>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct InterfaceConfig {
    id: PersistentIdentifier,
    name: String,
    device_class: crate::InterfaceType,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Config {
    interfaces: Vec<InterfaceConfig>,
}

impl Config {
    fn load<R: io::Read>(reader: R) -> Result<Self, anyhow::Error> {
        serde_json::from_reader(reader).map_err(Into::into)
    }

    // We use MAC addresses to identify USB devices; USB devices are those devices whose
    // topological path contains "/usb/". We use topological paths to identify on-board
    // devices; on-board devices are those devices whose topological path does not
    // contain "/usb". Topological paths of both device types are expected to
    // contain "/PCI0"; devices whose topological path does not contain "/PCI0" are
    // identified by their MAC address.
    //
    // At the time of writing, typical topological paths appear similar to:
    //
    // PCI:
    // "/dev/sys/platform/pt/PCI0/bus/02:00.0/02:00.0/e1000/ethernet"
    //
    // USB:
    // "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/007/ifc-000/<snip>/wlan/wlan-ethernet/ethernet"
    // 00:14:0 following "/PCI0/bus/" represents BDF (Bus Device Function)
    //
    // SDIO
    // "/dev/sys/platform/05:00:6/aml-sd-emmc/sdio/broadcom-wlanphy/wlanphy"
    // 05:00:6 following "platform" represents
    // vid(vendor id):pid(product id):did(device id) and are defined in each board file
    //
    // Ethernet Jack for VIM2
    // "/dev/sys/platform/04:02:7/aml-ethernet/Designware-MAC/ethernet"
    // Though it is not a sdio device, it has the vid:pid:did info following "/platform/",
    // it's handled the same way as a sdio device.
    fn generate_identifier(
        &self,
        topological_path: &str,
        mac_address: &fidl_fuchsia_net_ext::MacAddress,
    ) -> PersistentIdentifier {
        match get_bus_type_for_topological_path(&topological_path) {
            // Use the MacAddress as an identifier for the device if the
            // BusType is currently not in the known list.
            Ok(BusType::USB) | Err(_) => PersistentIdentifier::MacAddress(*mac_address),
            Ok(BusType::PCI) | Ok(BusType::SDIO) => {
                PersistentIdentifier::TopologicalPath(topological_path.to_string())
            }
        }
    }

    fn lookup_by_identifier(&self, persistent_id: &PersistentIdentifier) -> Option<usize> {
        self.interfaces.iter().enumerate().find_map(|(i, interface)| {
            if &interface.id == persistent_id {
                Some(i)
            } else {
                None
            }
        })
    }

    fn generate_name(
        &self,
        info: &DeviceInfoRef<'_>,
        naming_rules: &[NamingRule],
    ) -> Result<String, NameGenerationError> {
        generate_name_from_naming_rules(naming_rules, &self.interfaces, &info)
    }
}

/// Checks the interface and device class to check that they match.
// TODO(https://fxbug.dev/118197): Remove this function once we save the device class.
fn name_matches_interface_type(name: &str, interface_type: &crate::InterfaceType) -> bool {
    match interface_type {
        crate::InterfaceType::Wlan => name.starts_with(INTERFACE_PREFIX_WLAN),
        crate::InterfaceType::Ethernet => name.starts_with(INTERFACE_PREFIX_ETHERNET),
    }
}

// Get the NormalizedMac using the last octet of the MAC address. The offset
// modifies the last_byte in an attempt to avoid naming conflicts.
// For example, a MAC of `[0x1, 0x1, 0x1, 0x1, 0x1, 0x9]` with offset 0
// becomes `9`.
fn get_mac_identifier_from_octets(
    octets: &[u8; 6],
    interface_type: crate::InterfaceType,
    offset: u8,
) -> Result<u8, anyhow::Error> {
    if offset == u8::MAX {
        return Err(anyhow::format_err!(
            "could not find unique identifier for mac={:?}, interface_type={:?}",
            octets,
            interface_type
        ));
    }

    let last_byte = octets[octets.len() - 1];
    let (identifier, _) = last_byte.overflowing_add(offset);
    Ok(identifier)
}

// Get the normalized bus path for a topological path.
// For example, a PCI device at `02:00.1` becomes `02001`.
fn get_normalized_bus_path_for_topo_path(topological_path: &str) -> Result<String, anyhow::Error> {
    let pattern = match get_bus_type_for_topological_path(topological_path)? {
        BusType::USB | BusType::PCI => "/PCI0/bus/",
        BusType::SDIO => "/platform/",
    };

    let index = topological_path.find(pattern).ok_or_else(|| {
        anyhow::format_err!(
            "unexpected topological path {}: {} is not found",
            topological_path,
            pattern
        )
    })?;
    let topological_path = &topological_path[index + pattern.len()..];
    let index = topological_path.find('/').ok_or_else(|| {
        anyhow::format_err!(
            "unexpected topological path suffix {}: '/' is not found after {}",
            topological_path,
            pattern
        )
    })?;

    Ok(topological_path[..index]
        .trim_end_matches(|c: char| !c.is_digit(16) || c == '0')
        .chars()
        .filter(|c| c.is_digit(16))
        .collect())
}

#[derive(Debug)]
pub struct FileBackedConfig<'a> {
    path: &'a path::Path,
    config: Config,
    temp_id: u64,
}

impl<'a> FileBackedConfig<'a> {
    /// Loads the persistent/stable interface names from the backing file.
    pub fn load<P: AsRef<path::Path>>(path: &'a P) -> Result<Self, anyhow::Error> {
        const INITIAL_TEMP_ID: u64 = 0;

        let path = path.as_ref();
        match fs::File::open(path) {
            Ok(mut file) => {
                Config::load(&file)
                    .map(|config| Self { path, config, temp_id: INITIAL_TEMP_ID })
                    .or_else(|_| {
                        // Since deserialization as Config failed, try loading the file
                        // as the legacy config format.
                        let _seek = file.seek(SeekFrom::Start(0))?;
                        let legacy_config: LegacyConfig = serde_json::from_reader(&file)?;
                        // Transfer the values from the old format to the new format.
                        let new_config = Self {
                            config: Config {
                                interfaces: legacy_config
                                    .names
                                    .into_iter()
                                    .map(|(id, name)| InterfaceConfig {
                                        id,
                                        device_class: if name.starts_with(INTERFACE_PREFIX_WLAN) {
                                            crate::InterfaceType::Wlan
                                        } else {
                                            crate::InterfaceType::Ethernet
                                        },
                                        name,
                                    })
                                    .collect::<Vec<_>>(),
                            },
                            path,
                            temp_id: INITIAL_TEMP_ID,
                        };
                        // Overwrite the legacy config file with the current format.
                        //
                        // TODO(https://fxbug.dev/118197): Remove this logic once all devices
                        // persist interface configuration in the current format.
                        new_config.store().unwrap_or_else(|e| {
                            tracing::error!(
                                "failed to overwrite legacy interface config with current \
                                format: {:?}",
                                e
                            )
                        });
                        Ok(new_config)
                    })
            }
            Err(error) => {
                if error.kind() == io::ErrorKind::NotFound {
                    Ok(Self {
                        path,
                        temp_id: INITIAL_TEMP_ID,
                        config: Config { interfaces: vec![] },
                    })
                } else {
                    Err(error)
                        .with_context(|| format!("could not open config file {}", path.display()))
                }
            }
        }
    }

    /// Stores the persistent/stable interface names to the backing file.
    pub fn store(&self) -> Result<(), anyhow::Error> {
        let Self { path, config, temp_id: _ } = self;
        let temp_file_path = match path.file_name() {
            None => Err(anyhow::format_err!("unexpected non-file path {}", path.display())),
            Some(file_name) => {
                let mut file_name = file_name.to_os_string();
                file_name.push(".tmp");
                Ok(path.with_file_name(file_name))
            }
        }?;
        {
            let temp_file = fs::File::create(&temp_file_path).with_context(|| {
                format!("could not create temporary file {}", temp_file_path.display())
            })?;
            serde_json::to_writer_pretty(temp_file, &config).with_context(|| {
                format!(
                    "could not serialize config into temporary file {}",
                    temp_file_path.display()
                )
            })?;
        }

        fs::rename(&temp_file_path, path).with_context(|| {
            format!(
                "could not rename temporary file {} to {}",
                temp_file_path.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    /// Returns a stable interface name for the specified interface.
    pub(crate) fn generate_stable_name(
        &mut self,
        topological_path: &str,
        mac: &fidl_fuchsia_net_ext::MacAddress,
        device_class: fidl_fuchsia_hardware_network::DeviceClass,
        naming_rules: &[NamingRule],
    ) -> Result<(&str, PersistentIdentifier), NameGenerationError> {
        let persistent_id = self.config.generate_identifier(topological_path, mac);
        let info = DeviceInfoRef { topological_path, mac, device_class };
        let interface_type: crate::InterfaceType = device_class.into();
        if let Some(index) = self.config.lookup_by_identifier(&persistent_id) {
            let InterfaceConfig { name, .. } = &self.config.interfaces[index];
            if name_matches_interface_type(&name, &interface_type) {
                // Need to take a new reference to appease the borrow checker.
                let InterfaceConfig { name, .. } = &self.config.interfaces[index];
                return Ok((&name, persistent_id));
            }
            // Identifier was found in the vector, but device class does not match interface
            // name. Remove and generate a new name.
            let _interface_config = self.config.interfaces.remove(index);
        }
        let generated_name = self.config.generate_name(&info, naming_rules)?;
        let () = self.config.interfaces.push(InterfaceConfig {
            id: persistent_id.clone(),
            name: generated_name,
            device_class: interface_type,
        });
        let interface_config = &self.config.interfaces[self.config.interfaces.len() - 1];
        let () = self.store().map_err(|err| NameGenerationError::FileUpdateError {
            name: interface_config.name.to_owned(),
            persistent_id: persistent_id.clone(),
            err,
        })?;
        Ok((&interface_config.name, persistent_id))
    }

    /// Returns a temporary name for an interface.
    pub(crate) fn generate_temporary_name(
        &mut self,
        interface_type: crate::InterfaceType,
    ) -> (String, Option<PersistentIdentifier>) {
        let id = self.temp_id;
        self.temp_id += 1;

        let prefix = match interface_type {
            crate::InterfaceType::Wlan => "wlant",
            crate::InterfaceType::Ethernet => "etht",
        };
        (format!("{}{}", prefix, id), None)
    }
}

/// An error observed when generating a new name.
#[derive(Debug)]
pub enum NameGenerationError {
    GenerationError(anyhow::Error),
    FileUpdateError { name: String, persistent_id: PersistentIdentifier, err: anyhow::Error },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum BusType {
    USB,
    PCI,
    SDIO,
}

impl BusType {
    // Retrieve the list of composition rules that comprise the default name
    // for the interface based on BusType.
    // Example names for the following default rules:
    // * USB device: "ethx5"
    // * PCI/SDIO device: "wlans5009"
    fn get_default_name_composition_rules(&self) -> Vec<NameCompositionRule> {
        match *self {
            BusType::USB => vec![
                NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass },
                NameCompositionRule::Static { value: String::from("x") },
                NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::NormalizedMac },
            ],
            BusType::PCI | BusType::SDIO => vec![
                NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass },
                NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::BusType },
                NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::BusPath },
            ],
        }
    }
}

impl std::fmt::Display for BusType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match *self {
            Self::USB => "u",
            Self::PCI => "p",
            Self::SDIO => "s",
        };
        write!(f, "{}", name)
    }
}

// Extract the `BusType` for a device given the topological path.
fn get_bus_type_for_topological_path(topological_path: &str) -> Result<BusType, anyhow::Error> {
    if topological_path.contains("/PCI0") {
        // A USB bus will require a bridge over a PCI controller, so a
        // topological path for a USB bus should contain strings to represent
        // PCI and USB.
        if topological_path.contains("/usb/") {
            Ok(BusType::USB)
        } else {
            Ok(BusType::PCI)
        }
    } else if topological_path.contains("/platform/") {
        Ok(BusType::SDIO)
    } else {
        Err(anyhow::format_err!(
            "unexpected topological path {}: did not match known bus types",
            topological_path,
        ))
    }
}

fn deserialize_glob_pattern<'de, D>(deserializer: D) -> Result<glob::Pattern, D::Error>
where
    D: Deserializer<'de>,
{
    let buf = String::deserialize(deserializer)?;
    glob::Pattern::new(&buf).map_err(serde::de::Error::custom)
}

/// The matching rules available for a `NamingRule`.
#[derive(Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum MatchingRule {
    BusTypes(Vec<BusType>),
    // TODO(fxbug.dev/135495): Use a lightweight regex crate with the basic
    // regex features to allow for more configurations than glob.
    #[serde(deserialize_with = "deserialize_glob_pattern")]
    TopologicalPath(glob::Pattern),
    DeviceClasses(Vec<DeviceClass>),
    // Signals whether this rule should match any interface.
    Any(bool),
}

/// The matching rules available for a `ProvisoningRule`.
#[derive(Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(untagged)]
pub enum ProvisioningMatchingRule {
    // TODO(github.com/serde-rs/serde/issues/912): Use `other` once it supports
    // deserializing into non-unit variants. `untagged` can only be applied
    // to the entire enum, so `interface_name` is used as a field to ensure
    // stability across configuration matching rules.
    InterfaceName {
        #[serde(rename = "interface_name", deserialize_with = "deserialize_glob_pattern")]
        pattern: glob::Pattern,
    },
    Common(MatchingRule),
}

impl MatchingRule {
    fn does_interface_match(&self, info: &DeviceInfoRef<'_>) -> Result<bool, anyhow::Error> {
        match &self {
            MatchingRule::BusTypes(type_list) => {
                // Match the interface if the interface under comparison
                // matches any of the types included in the list.
                let bus_type = get_bus_type_for_topological_path(info.topological_path)?;
                Ok(type_list.contains(&bus_type))
            }
            MatchingRule::TopologicalPath(pattern) => {
                // Match the interface if the provided pattern finds any
                // matches in the interface under comparison's
                // topological path.
                Ok(pattern.matches(info.topological_path))
            }
            MatchingRule::DeviceClasses(class_list) => {
                // Match the interface if the interface under comparison
                // matches any of the types included in the list.
                Ok(class_list.contains(&info.device_class.into()))
            }
            MatchingRule::Any(matches_any_interface) => Ok(*matches_any_interface),
        }
    }
}

impl ProvisioningMatchingRule {
    fn does_interface_match(
        &self,
        info: &DeviceInfoRef<'_>,
        interface_name: &str,
    ) -> Result<bool, anyhow::Error> {
        match &self {
            ProvisioningMatchingRule::InterfaceName { pattern } => {
                // Match the interface if the provided pattern finds any
                // matches in the interface under comparison's name.
                Ok(pattern.matches(interface_name))
            }
            ProvisioningMatchingRule::Common(matching_rule) => {
                // Handle the other `MatchingRule`s the same as the naming
                // policy matchers.
                matching_rule.does_interface_match(info)
            }
        }
    }
}

// TODO(fxbug.dev/135106): Create dynamic naming rules
// A naming rule that uses device information to produce a component of
// the interface's name.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum DynamicNameCompositionRule {
    BusPath,
    BusType,
    DeviceClass,
    // A unique value seeded by the final octet of the interface's MAC address.
    NormalizedMac,
}

impl DynamicNameCompositionRule {
    // `true` when a rule can be re-tried to produce a different name.
    fn supports_retry(&self) -> bool {
        match *self {
            DynamicNameCompositionRule::BusPath
            | DynamicNameCompositionRule::BusType
            | DynamicNameCompositionRule::DeviceClass => false,
            DynamicNameCompositionRule::NormalizedMac => true,
        }
    }
}

impl DynamicNameCompositionRule {
    fn get_name(&self, info: &DeviceInfoRef<'_>, attempt_num: u8) -> Result<String, anyhow::Error> {
        match *self {
            DynamicNameCompositionRule::BusPath => {
                get_normalized_bus_path_for_topo_path(info.topological_path)
            }
            DynamicNameCompositionRule::BusType => {
                get_bus_type_for_topological_path(info.topological_path).map(|t| t.to_string())
            }
            DynamicNameCompositionRule::DeviceClass => Ok(match info.device_class.into() {
                crate::InterfaceType::Wlan => INTERFACE_PREFIX_WLAN,
                crate::InterfaceType::Ethernet => INTERFACE_PREFIX_ETHERNET,
            }
            .to_string()),
            DynamicNameCompositionRule::NormalizedMac => {
                let fidl_fuchsia_net_ext::MacAddress { octets } = info.mac;
                let mac_identifier =
                    get_mac_identifier_from_octets(octets, info.device_class.into(), attempt_num)?;
                Ok(format!("{mac_identifier:x}"))
            }
        }
    }
}

// A rule that dictates a component of an interface's name. An interface's name
// is determined by extracting the name of each rule, in order, and
// concatenating the results.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase", tag = "type")]
pub enum NameCompositionRule {
    Static { value: String },
    Dynamic { rule: DynamicNameCompositionRule },
    // The default name composition rules based on the device's BusType.
    // Defined in `BusType::get_default_name_composition_rules`.
    Default,
}

/// A rule that dictates how interfaces that align with the property matching
/// rules should be named.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub struct NamingRule {
    /// A set of rules to check against an interface's properties. All rules
    /// must apply for the naming scheme to take effect.
    pub matchers: HashSet<MatchingRule>,
    /// The rules to apply to the interface to produce the interface's name.
    pub naming_scheme: Vec<NameCompositionRule>,
}

impl NamingRule {
    // An interface's name is determined by extracting the name of each rule,
    // in order, and concatenating the results. Returns an error if the
    // interface name cannot be generated.
    fn generate_name(
        &self,
        interfaces: &[InterfaceConfig],
        info: &DeviceInfoRef<'_>,
    ) -> Result<String, NameGenerationError> {
        // When a bus type cannot be found for a path, use the USB
        // default naming policy which uses a MAC address.
        let bus_type =
            get_bus_type_for_topological_path(&info.topological_path).unwrap_or(BusType::USB);

        // Expand any `Default` rules into the `Static` and `Dynamic` rules in a single vector.
        // If this was being consumed once, we could avoid the call to `collect`. However, since we
        // want to use it twice, we need to convert it to a form where the items can be itererated
        // over without consuming them.
        let expanded_rules = self
            .naming_scheme
            .iter()
            .map(|rule| {
                if let NameCompositionRule::Default = rule {
                    Either::Right(bus_type.get_default_name_composition_rules().into_iter())
                } else {
                    Either::Left(std::iter::once(rule.clone()))
                }
            })
            .flatten()
            .collect::<Vec<_>>();

        // Determine whether any rules present support retrying for a unique name.
        let should_reattempt_on_conflict = expanded_rules.iter().any(|rule| {
            if let NameCompositionRule::Dynamic { rule } = rule {
                rule.supports_retry()
            } else {
                false
            }
        });

        let mut attempt_num = 0u8;
        loop {
            let name = expanded_rules
                .iter()
                .map(|rule| match rule {
                    NameCompositionRule::Static { value } => Ok(value.clone()),
                    // Dynamic rules require the knowledge of `DeviceInfo` properties.
                    NameCompositionRule::Dynamic { rule } => rule
                        .get_name(info, attempt_num)
                        .map_err(NameGenerationError::GenerationError),
                    NameCompositionRule::Default => {
                        unreachable!(
                            "Default naming rules should have been pre-expanded. \
                             Nested default rules are not supported."
                        );
                    }
                })
                .collect::<Result<String, NameGenerationError>>()?;

            if interfaces.iter().any(|interface| interface.name == name) {
                if should_reattempt_on_conflict {
                    attempt_num += 1;
                    // Try to generate another name with the modified attempt number.
                    continue;
                }

                tracing::warn!(
                    "name ({name}) already used for an interface installed by netcfg. \
                 using name since it is possible that the interface using this name is no \
                 longer active"
                );
            }
            return Ok(name);
        }
    }

    // An interface must align with all specified `MatchingRule`s.
    fn does_interface_match(&self, info: &DeviceInfoRef<'_>) -> bool {
        self.matchers.iter().all(|rule| rule.does_interface_match(info).unwrap_or_default())
    }
}

// Find the first `NamingRule` that matches the device and attempt to
// construct a name from the provided `NameCompositionRule`s.
fn generate_name_from_naming_rules(
    naming_rules: &[NamingRule],
    interfaces: &[InterfaceConfig],
    info: &DeviceInfoRef<'_>,
) -> Result<String, NameGenerationError> {
    // TODO(fxbug.dev/136397): Consider adding an option to the rules to allow
    // fallback rules when name generation fails.
    // Use the first naming rule that matches the interface to enforce consistent
    // interface names, even if there are other matching rules.
    let fallback_rule = fallback_naming_rule();
    let first_matching_rule =
        naming_rules.iter().find(|rule| rule.does_interface_match(&info)).unwrap_or(
            // When there are no `NamingRule`s that match the device,
            // use a fallback rule that has the Default naming scheme.
            &fallback_rule,
        );

    first_matching_rule.generate_name(interfaces, &info)
}

// Matches any device and uses the default naming rule.
fn fallback_naming_rule() -> NamingRule {
    NamingRule {
        matchers: HashSet::from([MatchingRule::Any(true)]),
        naming_scheme: vec![NameCompositionRule::Default],
    }
}

/// Whether the interface should be provisioned locally by netcfg, or
/// delegated. Provisioning is the set of events that occurs after
/// interface enumeration, such as starting a DHCP client and assigning
/// an IP to the interface. Provisioning actions work to support
/// Internet connectivity.
#[derive(Copy, Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum ProvisioningAction {
    /// Netcfg will provision the interface
    Local,
    /// Netcfg will not provision the interface. The provisioning
    /// of the interface will occur elsewhere
    Delegated,
}

/// A rule that dictates how interfaces that align with the property matching
/// rules should be provisioned.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub struct ProvisioningRule {
    /// A set of rules to check against an interface's properties. All rules
    /// must apply for the provisioning action to take effect.
    #[allow(unused)]
    pub matchers: HashSet<ProvisioningMatchingRule>,
    /// The provisioning policy that netcfg applies to a matching
    /// interface.
    #[allow(unused)]
    pub provisioning: ProvisioningAction,
}

// A ref version of `devices::DeviceInfo` to avoid the need to clone data
// unnecessarily. Devices without MAC are not supported yet, see
// `add_new_device` in `lib.rs`. This makes mac into a required field for
// ease of use.
struct DeviceInfoRef<'a> {
    device_class: fidl_fuchsia_hardware_network::DeviceClass,
    mac: &'a fidl_fuchsia_net_ext::MacAddress,
    topological_path: &'a str,
}

impl ProvisioningRule {
    // An interface must align with all specified `MatchingRule`s.
    fn does_interface_match(&self, info: &DeviceInfoRef<'_>, interface_name: &str) -> bool {
        self.matchers
            .iter()
            .all(|rule| rule.does_interface_match(info, interface_name).unwrap_or_default())
    }
}

// Find the first `ProvisioningRule` that matches the device and get
// the associated `ProvisioningAction`. By default, use Local provisioning
// so that Netcfg will provision interfaces unless configuration
// indicates otherwise.
pub(crate) fn find_provisioning_action_from_provisioning_rules(
    provisioning_rules: &[ProvisioningRule],
    topological_path: &str,
    mac: &fidl_fuchsia_net_ext::MacAddress,
    device_class: fidl_fuchsia_hardware_network::DeviceClass,
    interface_name: &str,
) -> ProvisioningAction {
    let info = DeviceInfoRef { topological_path, mac, device_class };
    provisioning_rules
        .iter()
        .find_map(|rule| {
            if rule.does_interface_match(&info, &interface_name) {
                Some(rule.provisioning)
            } else {
                None
            }
        })
        .unwrap_or(
            // When there are no `ProvisioningRule`s that match the device,
            // use Local provisioning.
            ProvisioningAction::Local,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use std::io::Write;
    use test_case::test_case;

    use fidl_fuchsia_hardware_network as fhwnet;

    // This is a lossy conversion between `InterfaceType` and `DeviceClass`
    // that allows tests to use a `devices::DeviceInfo` struct instead of
    // handling the fields individually.
    fn device_class_from_interface_type(ty: crate::InterfaceType) -> fhwnet::DeviceClass {
        match ty {
            crate::InterfaceType::Ethernet => fhwnet::DeviceClass::Ethernet,
            crate::InterfaceType::Wlan => fhwnet::DeviceClass::Wlan,
        }
    }

    // usb interfaces
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
        [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
        crate::InterfaceType::Wlan,
        "wlanx1";
        "usb_wlan"
    )]
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:15.0/00:15.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
        [0x02, 0x02, 0x02, 0x02, 0x02, 0x02],
        crate::InterfaceType::Ethernet,
        "ethx2";
        "usb_eth"
    )]
    // pci interfaces
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/ethernet",
        [0x03, 0x03, 0x03, 0x03, 0x03, 0x03],
        crate::InterfaceType::Wlan,
        "wlanp0014";
        "pci_wlan"
    )]
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:15.0/00:14.0/ethernet",
        [0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        crate::InterfaceType::Ethernet,
        "ethp0015";
        "pci_eth"
    )]
    // platform interfaces (ethernet jack and sdio devices)
    #[test_case(
        "/dev/sys/platform/05:00:6/aml-sd-emmc/sdio/broadcom-wlanphy/wlanphy",
        [0x05, 0x05, 0x05, 0x05, 0x05, 0x05],
        crate::InterfaceType::Wlan,
        "wlans05006";
        "platform_wlan"
    )]
    #[test_case(
        "/dev/sys/platform/04:02:7/aml-ethernet/Designware-MAC/ethernet",
        [0x07, 0x07, 0x07, 0x07, 0x07, 0x07],
        crate::InterfaceType::Ethernet,
        "eths04027";
        "platform_eth"
    )]
    // unknown interfaces
    #[test_case(
        "/dev/sys/unknown",
        [0x08, 0x08, 0x08, 0x08, 0x08, 0x08],
        crate::InterfaceType::Wlan,
        "wlanx8";
        "unknown_wlan1"
    )]
    #[test_case(
        "unknown",
        [0x09, 0x09, 0x09, 0x09, 0x09, 0x09],
        crate::InterfaceType::Wlan,
        "wlanx9";
        "unknown_wlan2"
    )]
    fn test_generate_name(
        topological_path: &'static str,
        mac: [u8; 6],
        interface_type: crate::InterfaceType,
        want_name: &'static str,
    ) {
        let config = Config { interfaces: vec![] };
        let name = config
            .generate_name(
                &DeviceInfoRef {
                    device_class: device_class_from_interface_type(interface_type),
                    mac: &fidl_fuchsia_net_ext::MacAddress { octets: mac },
                    topological_path,
                },
                &[],
            )
            .expect("failed to generate the name");
        assert_eq!(name, want_name);
    }

    struct StableNameTestCase {
        topological_path: &'static str,
        mac: [u8; 6],
        interface_type: crate::InterfaceType,
        want_name: &'static str,
        expected_size: usize,
    }

    // Base case. Interface should be added to config.
    #[test_case([StableNameTestCase {
        topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
        interface_type: crate::InterfaceType::Wlan,
        want_name: "wlanp0014",
        expected_size: 1 }];
        "single_interface"
    )]
    // Test case that shares the same topo path and interface name, with
    // different MAC address. New interface should not be added.
    #[test_case([StableNameTestCase {
        topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
        interface_type: crate::InterfaceType::Wlan,
        want_name: "wlanp0014",
        expected_size: 1}, StableNameTestCase {
        topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        mac: [0xFE, 0x01, 0x01, 0x01, 0x01, 0x01],
        interface_type: crate::InterfaceType::Wlan,
        want_name: "wlanp0014",
        expected_size: 1 }];
        "two_interfaces_different_mac"
    )]
    #[test_case([StableNameTestCase {
        topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
        interface_type: crate::InterfaceType::Wlan,
        want_name: "wlanp0014",
        expected_size: 1}, StableNameTestCase {
        topological_path: "/dev/sys/platform/pt/PCI0/bus/01:00.0/01:00.0/iwlwifi-wlan-softmac/wlan-ethernet/ethernet",
        mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
        interface_type: crate::InterfaceType::Ethernet,
        want_name: "ethp01",
        expected_size: 2 }];
        "two_distinct_interfaces"
    )]
    // Test case that labels iwilwifi as ethernet, then changes the device
    // class to wlan. The test should detect that the device class doesn't
    // match the interface name, and overwrite with the new interface name
    // that does match.
    #[test_case([StableNameTestCase {
        topological_path: "/dev/sys/platform/pt/PCI0/bus/01:00.0/01:00.0/iwlwifi-wlan-softmac/wlan-ethernet/ethernet",
        mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
        interface_type: crate::InterfaceType::Ethernet,
        want_name: "ethp01",
        expected_size: 1 }, StableNameTestCase {
        topological_path: "/dev/sys/platform/pt/PCI0/bus/01:00.0/01:00.0/iwlwifi-wlan-softmac/wlan-ethernet/ethernet",
        mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
        interface_type: crate::InterfaceType::Wlan,
        want_name: "wlanp01",
        expected_size: 1 }];
        "two_interfaces_different_device_class"
    )]
    fn test_generate_stable_name(test_cases: impl IntoIterator<Item = StableNameTestCase>) {
        let temp_dir = tempfile::tempdir_in("/tmp").expect("failed to create the temp dir");
        let path = temp_dir.path().join("net.config.json");

        // query an existing interface with the same topo path and a different mac address
        for (
            _i,
            StableNameTestCase { topological_path, mac, interface_type, want_name, expected_size },
        ) in test_cases.into_iter().enumerate()
        {
            let mut interface_config =
                FileBackedConfig::load(&path).expect("failed to load the interface config");

            let (name, _identifier) = interface_config
                .generate_stable_name(
                    topological_path,
                    &fidl_fuchsia_net_ext::MacAddress { octets: mac },
                    device_class_from_interface_type(interface_type),
                    &[],
                )
                .expect("failed to get the interface name");
            assert_eq!(name, want_name);
            // Load the file again to ensure that generate_stable_name saved correctly.
            interface_config =
                FileBackedConfig::load(&path).expect("failed to load the interface config");
            assert_eq!(interface_config.config.interfaces.len(), expected_size);
        }
    }

    #[test]
    fn test_generate_temporary_name() {
        let temp_dir = tempfile::tempdir_in("/tmp").expect("failed to create the temp dir");
        let path = temp_dir.path().join("net.config.json");
        let mut interface_config =
            FileBackedConfig::load(&path).expect("failed to load the interface config");
        let (name_0, id_0) =
            interface_config.generate_temporary_name(crate::InterfaceType::Ethernet);
        assert_eq!(&name_0, "etht0");
        assert_matches!(id_0, None);

        let (name_1, id_1) = interface_config.generate_temporary_name(crate::InterfaceType::Wlan);
        assert_eq!(&name_1, "wlant1");
        assert_matches!(id_1, None);
    }

    #[test]
    fn test_get_usb_255() {
        let topo_usb = "/dev/pci-00:14.0-fidl/xhci/usb/004/004/ifc-000/ax88179/ethernet";

        // test cases for 256 usb interfaces
        let mut config = Config { interfaces: vec![] };
        for n in 0u8..255u8 {
            let octets = [n, 0x01, 0x01, 0x01, 0x01, 00];

            let persistent_id =
                config.generate_identifier(topo_usb, &fidl_fuchsia_net_ext::MacAddress { octets });

            if let Some(index) = config.lookup_by_identifier(&persistent_id) {
                assert_eq!(config.interfaces[index].name, format!("{}{:x}", "wlanx", n));
            } else {
                let name = config
                    .generate_name(
                        &DeviceInfoRef {
                            device_class: device_class_from_interface_type(
                                crate::InterfaceType::Wlan,
                            ),
                            mac: &fidl_fuchsia_net_ext::MacAddress { octets },
                            topological_path: topo_usb,
                        },
                        &[],
                    )
                    .expect("failed to generate the name");
                assert_eq!(name, format!("{}{:x}", "wlanx", n));
                config.interfaces.push(InterfaceConfig {
                    id: persistent_id,
                    name: name,
                    device_class: crate::InterfaceType::Ethernet,
                });
            }
        }
        let octets = [0x00, 0x00, 0x01, 0x01, 0x01, 00];
        assert!(config
            .generate_name(
                &DeviceInfoRef {
                    device_class: device_class_from_interface_type(crate::InterfaceType::Wlan),
                    mac: &fidl_fuchsia_net_ext::MacAddress { octets },
                    topological_path: topo_usb
                },
                &[]
            )
            .is_err());
    }

    #[test]
    fn test_get_usb_255_with_naming_rule() {
        let topo_usb = "/dev/pci-00:14.0-fidl/xhci/usb/004/004/ifc-000/ax88179/ethernet";

        let naming_rule = NamingRule {
            matchers: HashSet::new(),
            naming_scheme: vec![
                NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::NormalizedMac },
                NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::NormalizedMac },
            ],
        };

        // test cases for 256 usb interfaces
        let mut config = Config { interfaces: vec![] };
        for n in 0u8..255u8 {
            let octets = [n, 0x01, 0x01, 0x01, 0x01, 00];
            let persistent_id =
                config.generate_identifier(topo_usb, &fidl_fuchsia_net_ext::MacAddress { octets });

            let info = DeviceInfoRef {
                device_class: fhwnet::DeviceClass::Ethernet,
                mac: &fidl_fuchsia_net_ext::MacAddress { octets },
                topological_path: topo_usb,
            };

            let name = naming_rule
                .generate_name(&config.interfaces, &info)
                .expect("failed to generate the name");
            // With only NormalizedMac as a NameCompositionRule, the name
            // should simply be the NormalizedMac itself.
            assert_eq!(name, format!("{n:x}{n:x}"));

            config.interfaces.push(InterfaceConfig {
                id: persistent_id,
                name: name,
                device_class: crate::InterfaceType::Ethernet,
            });
        }

        let octets = [0x00, 0x00, 0x01, 0x01, 0x01, 00];
        assert!(naming_rule
            .generate_name(
                &config.interfaces,
                &DeviceInfoRef {
                    device_class: fhwnet::DeviceClass::Ethernet,
                    mac: &fidl_fuchsia_net_ext::MacAddress { octets },
                    topological_path: topo_usb
                }
            )
            .is_err());
    }

    #[test]
    fn test_load_malformed_file() {
        let temp_dir = tempfile::tempdir_in("/tmp").expect("failed to create the temp dir");
        let path = temp_dir.path().join("net.config.json");
        {
            let mut file = fs::File::create(&path).expect("failed to open file for writing");
            // Write invalid JSON and close the file
            let () = file.write_all(b"{").expect("failed to write broken json into file");
        }
        assert_eq!(
            FileBackedConfig::load(&path)
                .unwrap_err()
                .downcast_ref::<serde_json::error::Error>()
                .unwrap()
                .classify(),
            serde_json::error::Category::Eof
        );
    }

    #[test]
    fn test_store_nonexistent_path() {
        let interface_config = FileBackedConfig::load(&"not/a/real/path")
            .expect("failed to load the interface config");
        assert_eq!(
            interface_config.store().unwrap_err().downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::NotFound
        );
    }

    const ETHERNET_TOPO_PATH: &str =
        "/dev/pci-00:15.0-fidl/xhci/usb/004/004/ifc-000/ax88179/ethernet";
    const ETHERNET_NAME: &str = "ethx2";
    const WLAN_TOPO_PATH: &str = "/dev/pci-00:14.0/ethernet";
    const WLAN_NAME: &str = "wlanp0014";

    #[test]
    fn test_load_legacy_config_file() {
        let test_config = LegacyConfig {
            names: vec![
                (
                    PersistentIdentifier::TopologicalPath(ETHERNET_TOPO_PATH.to_string()),
                    ETHERNET_NAME.to_string(),
                ),
                (
                    PersistentIdentifier::TopologicalPath(WLAN_TOPO_PATH.to_string()),
                    WLAN_NAME.to_string(),
                ),
            ],
        };

        let temp_dir = tempfile::tempdir_in("/tmp").expect("failed to create the temp dir");
        let path = temp_dir.path().join("net.config.json");
        {
            let file = fs::File::create(&path).expect("failed to open file for writing");

            serde_json::to_writer_pretty(file, &test_config)
                .expect("could not serialize config into temporary file");

            let new_config =
                FileBackedConfig::load(&path).expect("failed to load the interface config");
            assert_eq!(
                new_config.config,
                Config {
                    interfaces: vec![
                        InterfaceConfig {
                            id: PersistentIdentifier::TopologicalPath(
                                ETHERNET_TOPO_PATH.to_string()
                            ),
                            name: ETHERNET_NAME.to_string(),
                            device_class: crate::InterfaceType::Ethernet
                        },
                        InterfaceConfig {
                            id: PersistentIdentifier::TopologicalPath(WLAN_TOPO_PATH.to_string()),
                            name: WLAN_NAME.to_string(),
                            device_class: crate::InterfaceType::Wlan
                        },
                    ]
                }
            )
        }
    }

    #[test]
    fn overwrites_legacy_config_file_on_load() {
        let legacy_config = LegacyConfig {
            names: vec![
                (
                    PersistentIdentifier::TopologicalPath(ETHERNET_TOPO_PATH.to_string()),
                    ETHERNET_NAME.to_string(),
                ),
                (
                    PersistentIdentifier::TopologicalPath(WLAN_TOPO_PATH.to_string()),
                    WLAN_NAME.to_string(),
                ),
            ],
        };

        let temp_dir = tempfile::tempdir_in("/tmp").expect("failed to create the temp dir");
        let path = temp_dir.path().join("net.config.json");

        let file = fs::File::create(&path).expect("create config file");
        serde_json::to_writer_pretty(file, &legacy_config).expect("serialize legacy config");

        let new_config = FileBackedConfig::load(&path).expect("load interface config");

        let persisted = std::fs::read_to_string(&path).expect("read persisted config");
        let expected_new_format =
            serde_json::to_string_pretty(&new_config.config).expect("serialize config");
        assert_eq!(persisted, expected_new_format);
    }

    // Arbitrary values for devices::DeviceInfo for cases where DeviceInfo has
    // no impact on the test.
    fn default_device_info() -> DeviceInfoRef<'static> {
        DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Ethernet,
            mac: &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x1] },
            topological_path: "",
        }
    }

    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        vec![BusType::PCI],
        BusType::PCI,
        true;
        "pci_match"
    )]
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        vec![BusType::USB, BusType::SDIO],
        BusType::PCI,
        false;
        "pci_no_match"
    )]
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
        vec![BusType::USB],
        BusType::USB,
        true;
        "usb_match"
    )]
    // Same topological path as the case for USB, but with
    // non-matching bus types. Ensure that even though PCI is
    // present in the topological path, it does not match a PCI
    // controller.
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
        vec![BusType::PCI, BusType::SDIO],
        BusType::USB,
        false;
        "usb_no_match"
    )]
    #[test_case(
        "/dev/sys/platform/05:00:6/aml-sd-emmc/sdio/broadcom-wlanphy/wlanphy",
        vec![BusType::SDIO],
        BusType::SDIO,
        true;
        "sdio_match"
    )]
    fn test_interface_matching_by_bus_type(
        topological_path: &'static str,
        bus_types: Vec<BusType>,
        expected_bus_type: BusType,
        want_match: bool,
    ) {
        let device_info = DeviceInfoRef {
            topological_path: topological_path,
            // `device_class` and `mac` have no effect on `BusType`
            // matching, so we use arbitrary values.
            ..default_device_info()
        };

        // Verify the `BusType` determined from the device's
        // topological path.
        let bus_type = get_bus_type_for_topological_path(&device_info.topological_path).unwrap();
        assert_eq!(bus_type, expected_bus_type);

        // Create a matching rule for the provided `BusType` list.
        let matching_rule = MatchingRule::BusTypes(bus_types);
        let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
        assert_eq!(does_interface_match, want_match);
    }

    #[test]
    fn test_interface_matching_by_bus_type_unsupported() {
        let device_info = DeviceInfoRef {
            topological_path: "/dev/sys/unsupported-bus/ethernet",
            // `device_class` and `mac` have no effect on `BusType`
            // matching, so we use arbitrary values.
            ..default_device_info()
        };

        // Verify the `BusType` determined from the device's topological
        // path raises an error for being unsupported.
        let bus_type_res = get_bus_type_for_topological_path(&device_info.topological_path);
        assert!(bus_type_res.is_err());

        // Create a matching rule for the provided `BusType` list. A device
        // with an unsupported path should raise an error to signify the need
        // to support a new bus type.
        let matching_rule = MatchingRule::BusTypes(vec![BusType::USB, BusType::PCI, BusType::SDIO]);
        let interface_match_res = matching_rule.does_interface_match(&device_info);
        assert!(interface_match_res.is_err());
    }

    // Glob matches the number pattern of XX:XX in the path.
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        r"*[0-9][0-9]:[0-9][0-9]*",
        true;
        "pattern_matches"
    )]
    #[test_case("pattern/will/match/anything", r"*", true; "pattern_matches_any")]
    // Glob checks for '00' after the colon but it will not find it.
    #[test_case(
        "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
        r"*[0-9][0-9]:00*",
        false;
        "no_matches"
    )]
    fn test_interface_matching_by_topological_path(
        topological_path: &'static str,
        glob_str: &'static str,
        want_match: bool,
    ) {
        let device_info = DeviceInfoRef {
            topological_path,
            // `device_class` and `mac` have no effect on `TopologicalPath`
            // matching, so we use arbitrary values.
            ..default_device_info()
        };

        // Create a matching rule for the provided glob expression.
        let matching_rule = MatchingRule::TopologicalPath(glob::Pattern::new(glob_str).unwrap());
        let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
        assert_eq!(does_interface_match, want_match);
    }

    // Glob matches the default naming by MAC address.
    #[test_case(
        "ethx5",
        r"ethx[0-9]*",
        true;
        "pattern_matches"
    )]
    #[test_case("arbitraryname", r"*", true; "pattern_matches_any")]
    // Glob matches default naming by SDIO + bus path.
    #[test_case(
        "wlans1002",
        r"eths[0-9][0-9][0-9][0-9]*",
        false;
        "no_matches"
    )]
    fn test_interface_matching_by_interface_name(
        interface_name: &'static str,
        glob_str: &'static str,
        want_match: bool,
    ) {
        // Create a matching rule for the provided glob expression.
        let provisioning_matching_rule = ProvisioningMatchingRule::InterfaceName {
            pattern: glob::Pattern::new(glob_str).unwrap(),
        };
        let does_interface_match = provisioning_matching_rule
            .does_interface_match(&default_device_info(), interface_name)
            .unwrap();
        assert_eq!(does_interface_match, want_match);
    }

    #[test_case(
        fhwnet::DeviceClass::Ethernet,
        vec![DeviceClass::Ethernet],
        DeviceClass::Ethernet,
        true;
        "eth_match"
    )]
    #[test_case(
        fhwnet::DeviceClass::Ethernet,
        vec![DeviceClass::Wlan, DeviceClass::WlanAp],
        DeviceClass::Ethernet,
        false;
        "eth_no_match"
    )]
    #[test_case(
        fhwnet::DeviceClass::Wlan,
        vec![DeviceClass::Wlan],
        DeviceClass::Wlan,
        true;
        "wlan_match"
    )]
    #[test_case(
        fhwnet::DeviceClass::Wlan,
        vec![DeviceClass::Ethernet, DeviceClass::WlanAp],
        DeviceClass::Wlan,
        false;
        "wlan_no_match"
    )]
    #[test_case(
        fhwnet::DeviceClass::WlanAp,
        vec![DeviceClass::WlanAp],
        DeviceClass::WlanAp,
        true;
        "ap_match"
    )]
    #[test_case(
        fhwnet::DeviceClass::WlanAp,
        vec![DeviceClass::Ethernet, DeviceClass::Wlan],
        DeviceClass::WlanAp,
        false;
        "ap_no_match"
    )]
    fn test_interface_matching_by_device_class(
        device_class: fhwnet::DeviceClass,
        device_classes: Vec<DeviceClass>,
        expected_device_class: DeviceClass,
        want_match: bool,
    ) {
        let device_info = DeviceInfoRef { device_class, ..default_device_info() };

        // Verify the `DeviceClass` determined from the device info.
        let device_class: DeviceClass = device_info.device_class.into();
        assert_eq!(device_class, expected_device_class);

        // Create a matching rule for the provided `DeviceClass` list.
        let matching_rule = MatchingRule::DeviceClasses(device_classes);
        let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
        assert_eq!(does_interface_match, want_match);
    }

    // The device information should not have any impact on whether the
    // interface matches, but we use Ethernet and Wlan as base cases
    // to ensure that all interfaces are accepted or all interfaces
    // are rejected.
    #[test_case(fhwnet::DeviceClass::Ethernet, ETHERNET_TOPO_PATH)]
    #[test_case(fhwnet::DeviceClass::Wlan, WLAN_TOPO_PATH)]
    fn test_interface_matching_by_any_matching_rule(
        device_class: fhwnet::DeviceClass,
        topological_path: &'static str,
    ) {
        let device_info = DeviceInfoRef {
            device_class,
            mac: &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x1] },
            topological_path,
        };

        // Create a matching rule that should match any interface.
        let matching_rule = MatchingRule::Any(true);
        let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
        assert!(does_interface_match);

        // Create a matching rule that should reject any interface.
        let matching_rule = MatchingRule::Any(false);
        let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
        assert!(!does_interface_match);
    }

    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        vec![MatchingRule::DeviceClasses(vec![DeviceClass::Wlan])],
        false;
        "false_single_rule"
    )]
    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        vec![MatchingRule::DeviceClasses(vec![DeviceClass::Wlan]), MatchingRule::Any(true)],
        false;
        "false_one_rule_of_multiple"
    )]
    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        vec![MatchingRule::Any(true)],
        true;
        "true_single_rule"
    )]
    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        vec![MatchingRule::DeviceClasses(vec![DeviceClass::Ethernet]), MatchingRule::Any(true)],
        true;
        "true_multiple_rules"
    )]
    fn test_does_interface_match(
        info: DeviceInfoRef<'_>,
        matching_rules: Vec<MatchingRule>,
        want_match: bool,
    ) {
        let naming_rule =
            NamingRule { matchers: HashSet::from_iter(matching_rules), naming_scheme: Vec::new() };
        assert_eq!(naming_rule.does_interface_match(&info), want_match);
    }

    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        "",
        vec![
            ProvisioningMatchingRule::Common(
                MatchingRule::DeviceClasses(vec![DeviceClass::Wlan])
            )
        ],
        false;
        "false_single_rule"
    )]
    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Wlan, ..default_device_info() },
        "wlanx5009",
        vec![
            ProvisioningMatchingRule::InterfaceName {
                pattern: glob::Pattern::new("ethx*").unwrap()
            },
            ProvisioningMatchingRule::Common(MatchingRule::Any(true))
        ],
        false;
        "false_one_rule_of_multiple"
    )]
    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        "",
        vec![ProvisioningMatchingRule::Common(MatchingRule::Any(true))],
        true;
        "true_single_rule"
    )]
    #[test_case(
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        "wlanx5009",
        vec![
            ProvisioningMatchingRule::Common(
                MatchingRule::DeviceClasses(vec![DeviceClass::Ethernet])
            ),
            ProvisioningMatchingRule::InterfaceName {
                pattern: glob::Pattern::new("wlanx*").unwrap()
            }
        ],
        true;
        "true_multiple_rules"
    )]
    fn test_does_interface_match_provisioning_rule(
        info: DeviceInfoRef<'_>,
        interface_name: &str,
        matching_rules: Vec<ProvisioningMatchingRule>,
        want_match: bool,
    ) {
        let provisioning_rule = ProvisioningRule {
            matchers: HashSet::from_iter(matching_rules),
            provisioning: ProvisioningAction::Local,
        };
        assert_eq!(provisioning_rule.does_interface_match(&info, interface_name), want_match);
    }

    #[test_case(
        vec![NameCompositionRule::Static { value: String::from("x") }],
        default_device_info(),
        "x";
        "single_static"
    )]
    #[test_case(
        vec![
            NameCompositionRule::Static { value: String::from("eth") },
            NameCompositionRule::Static { value: String::from("x") },
            NameCompositionRule::Static { value: String::from("100") },
        ],
        default_device_info(),
        "ethx100";
        "multiple_static"
    )]
    #[test_case(
        vec![NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::NormalizedMac }],
        DeviceInfoRef {
            mac: &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x1] },
            ..default_device_info()
        },
        "1";
        "normalized_mac"
    )]
    #[test_case(
        vec![
            NameCompositionRule::Static { value: String::from("eth") },
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::NormalizedMac },
        ],
        DeviceInfoRef {
            mac: &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x9] },
            ..default_device_info()
        },
        "eth9";
        "normalized_mac_with_static"
    )]
    #[test_case(
        vec![NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass }],
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        "eth";
        "eth_device_class"
    )]
    #[test_case(
        vec![NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass }],
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Wlan, ..default_device_info() },
        "wlan";
        "wlan_device_class"
    )]
    #[test_case(
        vec![
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass },
            NameCompositionRule::Static { value: String::from("x") },
        ],
        DeviceInfoRef { device_class: fhwnet::DeviceClass::Ethernet, ..default_device_info() },
        "ethx";
        "device_class_with_static"
    )]
    #[test_case(
        vec![
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass },
            NameCompositionRule::Static { value: String::from("x") },
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::NormalizedMac },
        ],
        DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Wlan,
            mac: &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x8] },
            ..default_device_info()
        },
        "wlanx8";
        "device_class_with_static_with_normalized_mac"
    )]
    #[test_case(
        vec![
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass },
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::BusType },
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::BusPath },
        ],
        DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Ethernet,
            topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
            ..default_device_info()
        },
        "ethp0014";
        "device_class_with_pci_bus_type_with_bus_path"
    )]
    #[test_case(
        vec![
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::DeviceClass },
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::BusType },
            NameCompositionRule::Dynamic { rule: DynamicNameCompositionRule::BusPath },
        ],
        DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Ethernet,
            topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
            ..default_device_info()
        },
        "ethu0014";
        "device_class_with_usb_bus_type_with_bus_path"
    )]
    #[test_case(
        vec![NameCompositionRule::Default],
        DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Ethernet,
            topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
            ..default_device_info()
        },
        "ethx1";
        "default_usb"
    )]
    #[test_case(
        vec![NameCompositionRule::Default],
        DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Ethernet,
            topological_path: "/dev/sys/platform/05:00:6/aml-sd-emmc/sdio/broadcom-wlanphy/wlanphy",
            ..default_device_info()
        },
        "eths05006";
        "default_sdio"
    )]
    fn test_naming_rules(
        composition_rules: Vec<NameCompositionRule>,
        info: DeviceInfoRef<'_>,
        expected_name: &'static str,
    ) {
        let naming_rule = NamingRule { matchers: HashSet::new(), naming_scheme: composition_rules };

        let name = naming_rule.generate_name(&[], &info);
        assert_eq!(name.unwrap(), expected_name.to_owned());
    }

    #[test]
    fn test_generate_name_from_naming_rule_interface_name_exists_no_reattempt() {
        let shared_interface_name = "x".to_owned();
        let interfaces = vec![InterfaceConfig {
            id: PersistentIdentifier::TopologicalPath("".to_owned()),
            name: shared_interface_name.clone(),
            device_class: crate::InterfaceType::Ethernet,
        }];

        let naming_rule = NamingRule {
            matchers: HashSet::new(),
            naming_scheme: vec![NameCompositionRule::Static {
                value: shared_interface_name.clone(),
            }],
        };

        let name = naming_rule.generate_name(&interfaces, &default_device_info()).unwrap();
        assert_eq!(name, shared_interface_name);
    }

    // This test is different from `test_get_usb_255_with_naming_rule` as this
    // test increments the last byte, ensuring that the offset is reset prior
    // to each name being generated.
    #[test]
    fn test_generate_name_from_naming_rule_many_unique_macs() {
        let topo_usb = "/dev/pci-00:14.0-fidl/xhci/usb/004/004/ifc-000/ax88179/ethernet";

        let naming_rule = NamingRule {
            matchers: HashSet::new(),
            naming_scheme: vec![NameCompositionRule::Dynamic {
                rule: DynamicNameCompositionRule::NormalizedMac,
            }],
        };

        // test cases for 256 usb interfaces
        let mut config = Config { interfaces: vec![] };

        for n in 0u8..255u8 {
            let octets = [0x01, 0x01, 0x01, 0x01, 0x01, n];
            let persistent_id =
                config.generate_identifier(topo_usb, &fidl_fuchsia_net_ext::MacAddress { octets });
            let info = DeviceInfoRef {
                device_class: fhwnet::DeviceClass::Ethernet,
                mac: &fidl_fuchsia_net_ext::MacAddress { octets },
                topological_path: topo_usb,
            };

            let name = naming_rule
                .generate_name(&config.interfaces, &info)
                .expect("failed to generate the name");
            assert_eq!(name, format!("{n:x}"));

            config.interfaces.push(InterfaceConfig {
                id: persistent_id,
                name: name,
                device_class: crate::InterfaceType::Ethernet,
            });
        }
    }

    #[test_case(true, "x"; "matches_first_rule")]
    #[test_case(false, "ethx1"; "fallback_default")]
    fn test_generate_name_from_naming_rules(match_first_rule: bool, expected_name: &'static str) {
        // Use an Ethernet device that is determined to have a USB bus type
        // from the topological path.
        let info = DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Ethernet,
            mac: &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x1] },
            topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet"
        };
        let name = generate_name_from_naming_rules(
            &[
                NamingRule {
                    matchers: HashSet::from([MatchingRule::Any(match_first_rule)]),
                    naming_scheme: vec![NameCompositionRule::Static { value: String::from("x") }],
                },
                // Include an arbitrary rule that matches no interface
                // to ensure that it has no impact on the test.
                NamingRule {
                    matchers: HashSet::from([MatchingRule::Any(false)]),
                    naming_scheme: vec![NameCompositionRule::Static { value: String::from("y") }],
                },
            ],
            &[],
            &info,
        )
        .unwrap();
        assert_eq!(name, expected_name.to_owned());
    }

    #[test]
    fn test_generate_name_from_naming_rules_all_errors() {
        // Create a `DeviceInfo` that has a device class, but has an empty
        // topological path. This should prohibit the `BusType`
        // dynamic `NameCompositionRule`s.
        let info = DeviceInfoRef {
            device_class: fhwnet::DeviceClass::Ethernet,
            topological_path: "",
            ..default_device_info()
        };

        // Both names should fail to be generated since the `BusType` relies on
        // the topological path. The GenerationError should be surfaced.
        let name = generate_name_from_naming_rules(
            &[NamingRule {
                matchers: HashSet::from([MatchingRule::Any(true)]),
                naming_scheme: vec![NameCompositionRule::Dynamic {
                    rule: DynamicNameCompositionRule::BusType,
                }],
            }],
            &[],
            &info,
        );
        assert_matches!(
            name,
            Err(NameGenerationError::GenerationError(e)) =>
                e.to_string().contains("bus type")
        );
    }

    #[test_case(true, ProvisioningAction::Delegated; "matches_first_rule")]
    #[test_case(false, ProvisioningAction::Local; "fallback_default")]
    fn test_find_provisioning_action_from_provisioning_rules(
        match_first_rule: bool,
        expected: ProvisioningAction,
    ) {
        let provisioning_action = find_provisioning_action_from_provisioning_rules(
            &[ProvisioningRule {
                matchers: HashSet::from([ProvisioningMatchingRule::Common(MatchingRule::Any(
                    match_first_rule,
                ))]),
                provisioning: ProvisioningAction::Delegated,
            }],
            "",
            &fidl_fuchsia_net_ext::MacAddress { octets: [0x1, 0x1, 0x1, 0x1, 0x1, 0x1] },
            fhwnet::DeviceClass::Wlan,
            "wlans5009",
        );
        assert_eq!(provisioning_action, expected);
    }
}
