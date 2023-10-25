// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context as _,
    serde::{Deserialize, Deserializer, Serialize},
    std::collections::HashSet,
    std::io::{Seek, SeekFrom},
    std::{fs, io, path},
};

use crate::{devices, DeviceClass};

const INTERFACE_PREFIX_WLAN: &str = "wlan";
const INTERFACE_PREFIX_ETHERNET: &str = "eth";

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Clone)]
enum PersistentIdentifier {
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

    fn generate_identifier(
        &self,
        topological_path: std::borrow::Cow<'_, str>,
        mac_address: fidl_fuchsia_net_ext::MacAddress,
    ) -> PersistentIdentifier {
        match get_bus_type_for_topological_path(&topological_path) {
            Ok(BusType::USB) => PersistentIdentifier::MacAddress(mac_address),
            Ok(BusType::PCI) => {
                PersistentIdentifier::TopologicalPath(topological_path.into_owned())
            }
            Ok(BusType::SDIO) => {
                PersistentIdentifier::TopologicalPath(topological_path.into_owned())
            }
            // Use the MacAddress as an identifier for the device if the
            // BusType is currently not in the known list.
            Err(_) => PersistentIdentifier::MacAddress(mac_address),
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

    // We use MAC addresses to identify USB devices; USB devices are those devices whose
    // topological path contains "/usb/". We use topological paths to identify on-board
    // devices; on-board devices are those devices whose topological path does not
    // contain "/usb". Topological paths of
    // both device types are expected to
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
    fn generate_name_from_mac(
        &self,
        octets: [u8; 6],
        interface_type: crate::InterfaceType,
    ) -> Result<String, anyhow::Error> {
        let prefix = format!(
            "{}{}",
            match interface_type {
                crate::InterfaceType::Wlan => INTERFACE_PREFIX_WLAN,
                crate::InterfaceType::Ethernet => INTERFACE_PREFIX_ETHERNET,
            },
            "x"
        );

        let last_byte = octets[octets.len() - 1];
        for i in 0u8..255u8 {
            let candidate = ((last_byte as u16 + i as u16) % 256 as u16) as u8;
            if self.interfaces.iter().any(|interface| {
                interface.name.starts_with(&prefix)
                    && u8::from_str_radix(&interface.name[prefix.len()..], 16) == Ok(candidate)
            }) {
                continue; // if the candidate is used, try next one
            } else {
                return Ok(format!("{}{:x}", prefix, candidate));
            }
        }
        Err(anyhow::format_err!(
            "could not find unique name for mac={}, interface_type={:?}",
            fidl_fuchsia_net_ext::MacAddress { octets: octets },
            interface_type
        ))
    }

    fn generate_name_from_topological_path(
        &self,
        topological_path: &str,
        interface_type: crate::InterfaceType,
    ) -> Result<String, anyhow::Error> {
        let prefix = match interface_type {
            crate::InterfaceType::Wlan => INTERFACE_PREFIX_WLAN,
            crate::InterfaceType::Ethernet => INTERFACE_PREFIX_ETHERNET,
        };
        let (suffix, pat) = if topological_path.contains("/PCI0/bus/") {
            ("p", "/PCI0/bus/")
        } else {
            ("s", "/platform/")
        };

        let index = topological_path.find(pat).ok_or_else(|| {
            anyhow::format_err!(
                "unexpected topological path {}: {} is not found",
                topological_path,
                pat
            )
        })?;
        let topological_path = &topological_path[index + pat.len()..];
        let index = topological_path.find('/').ok_or_else(|| {
            anyhow::format_err!(
                "unexpected topological path suffix {}: '/' is not found after {}",
                topological_path,
                pat
            )
        })?;

        let mut name = format!("{}{}", prefix, suffix);
        for digit in topological_path[..index]
            .trim_end_matches(|c: char| !c.is_digit(16) || c == '0')
            .chars()
            .filter(|c| c.is_digit(16))
        {
            name.push(digit);
        }
        Ok(name)
    }

    fn generate_name(
        &self,
        persistent_id: &PersistentIdentifier,
        interface_type: crate::InterfaceType,
    ) -> Result<String, anyhow::Error> {
        match persistent_id {
            PersistentIdentifier::MacAddress(fidl_fuchsia_net_ext::MacAddress { octets }) => {
                self.generate_name_from_mac(*octets, interface_type)
            }
            PersistentIdentifier::TopologicalPath(ref topological_path) => {
                self.generate_name_from_topological_path(&topological_path, interface_type)
            }
        }
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
        topological_path: std::borrow::Cow<'_, str>,
        mac_address: fidl_fuchsia_net_ext::MacAddress,
        interface_type: crate::InterfaceType,
    ) -> Result<&str, NameGenerationError<'_>> {
        let persistent_id = self.config.generate_identifier(topological_path, mac_address);

        if let Some(index) = self.config.lookup_by_identifier(&persistent_id) {
            let InterfaceConfig { name, .. } = &self.config.interfaces[index];
            if name_matches_interface_type(&name, &interface_type) {
                // Need to take a new reference to appease the borrow checker.
                let InterfaceConfig { name, .. } = &self.config.interfaces[index];
                return Ok(&name);
            }
            // Identifier was found in the vector, but device class does not match interface
            // name. Remove and generate a new name.
            let _interface_config = self.config.interfaces.remove(index);
        }
        let generated_name = self
            .config
            .generate_name(&persistent_id, interface_type)
            .map_err(NameGenerationError::GenerationError)?;
        let () = self.config.interfaces.push(InterfaceConfig {
            id: persistent_id,
            name: generated_name,
            device_class: interface_type,
        });
        let interface_config = &self.config.interfaces[self.config.interfaces.len() - 1];
        let () = self.store().map_err(|err| NameGenerationError::FileUpdateError {
            name: &interface_config.name,
            err,
        })?;
        Ok(&interface_config.name)
    }

    /// Returns a temporary name for an interface.
    pub(crate) fn generate_temporary_name(
        &mut self,
        interface_type: crate::InterfaceType,
    ) -> String {
        let id = self.temp_id;
        self.temp_id += 1;

        let prefix = match interface_type {
            crate::InterfaceType::Wlan => "wlant",
            crate::InterfaceType::Ethernet => "etht",
        };
        format!("{}{}", prefix, id)
    }
}

/// An error observed when generating a new name.
#[derive(Debug)]
pub enum NameGenerationError<'a> {
    GenerationError(anyhow::Error),
    FileUpdateError { name: &'a str, err: anyhow::Error },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum BusType {
    USB,
    PCI,
    SDIO,
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

// TODO(fxbug.dev/135094): Add interface matchers for naming policy
#[derive(Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum MatchingRule {
    BusTypes(Vec<BusType>),
    // TODO(fxbug.dev/135495): Use a lightweight regex crate with the basic
    // regex features to allow for more configurations than glob.
    #[serde(deserialize_with = "deserialize_glob_pattern")]
    TopologicalPath(glob::Pattern),
    DeviceClasses(Vec<DeviceClass>),
}

impl MatchingRule {
    // TODO(fxbug.dev/135098): Use interface matching fn when there is naming
    // policy to apply
    #[allow(unused)]
    fn does_interface_match(&self, info: &devices::DeviceInfo) -> Result<bool, anyhow::Error> {
        match &self {
            MatchingRule::BusTypes(type_list) => {
                // Match the interface if the interface under comparison
                // matches any of the types included in the list.
                let bus_type = get_bus_type_for_topological_path(&info.topological_path)?;
                Ok(type_list.contains(&bus_type))
            }
            MatchingRule::TopologicalPath(pattern) => {
                // Match the interface if the provided pattern finds any
                // matches in the interface under comparison's
                // topological path.
                Ok(pattern.matches(&info.topological_path))
            }
            MatchingRule::DeviceClasses(class_list) => {
                // Match the interface if the interface under comparison
                // matches any of the types included in the list.
                Ok(class_list.contains(&info.device_class.into()))
            }
        }
    }
}

// TODO(fxbug.dev/135097): Create framework for meta/static naming rules
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum NameCompositionRule {}

/// A rule that dictates how interfaces that align with the property matching
/// rules should be named.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub struct NamingRule {
    // TODO(fxbug.dev/135094): Add and use interface matching rules
    /// A set of rules to check against an interface's properties. All rules
    /// must apply for the naming scheme to take effect.
    #[allow(unused)]
    pub matchers: HashSet<MatchingRule>,
    // TODO(fxbug.dev/135098): Add static naming rules
    // TODO(fxbug.dev/135106): Add meta naming rules
    /// The rules to apply to the interface to produce the interface's name.
    #[allow(unused)]
    pub naming_scheme: Vec<NameCompositionRule>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use fidl_fuchsia_hardware_network as fhwnet;

    #[derive(Clone)]
    struct TestCase {
        topological_path: &'static str,
        mac: [u8; 6],
        interface_type: crate::InterfaceType,
        want_name: &'static str,
    }

    #[test]
    fn test_generate_name() {
        let test_cases = vec![
            // usb interfaces
            TestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
                mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlanx1",
            },
            TestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:15.0/00:15.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
                mac: [0x02, 0x02, 0x02, 0x02, 0x02, 0x02],
                interface_type: crate::InterfaceType::Ethernet,
                want_name: "ethx2",
            },
            // pci intefaces
            TestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/ethernet",
                mac: [0x03, 0x03, 0x03, 0x03, 0x03, 0x03],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlanp0014",
            },
            TestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:15.0/00:14.0/ethernet",
                mac: [0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
                interface_type: crate::InterfaceType::Ethernet,
                want_name: "ethp0015",
            },
            // platform interfaces (ethernet jack and sdio devices)
            TestCase {
                topological_path:
                    "/dev/sys/platform/05:00:6/aml-sd-emmc/sdio/broadcom-wlanphy/wlanphy",
                mac: [0x05, 0x05, 0x05, 0x05, 0x05, 0x05],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlans05006",
            },
            TestCase {
                topological_path: "/dev/sys/platform/04:02:7/aml-ethernet/Designware-MAC/ethernet",
                mac: [0x07, 0x07, 0x07, 0x07, 0x07, 0x07],
                interface_type: crate::InterfaceType::Ethernet,
                want_name: "eths04027",
            },
            // unknown interfaces
            TestCase {
                topological_path: "/dev/sys/unknown",
                mac: [0x08, 0x08, 0x08, 0x08, 0x08, 0x08],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlanx8",
            },
            TestCase {
                topological_path: "unknown",
                mac: [0x09, 0x09, 0x09, 0x09, 0x09, 0x09],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlanx9",
            },
        ];
        let config = Config { interfaces: vec![] };
        for TestCase { topological_path, mac, interface_type, want_name } in test_cases.into_iter()
        {
            let persistent_id = config.generate_identifier(
                topological_path.into(),
                fidl_fuchsia_net_ext::MacAddress { octets: mac },
            );
            let name = config
                .generate_name(&persistent_id, interface_type)
                .expect("failed to generate the name");
            assert_eq!(name, want_name);
        }
    }

    #[test]
    fn test_generate_stable_name() {
        #[derive(Clone)]
        struct FileBackedConfigTestCase {
            topological_path: &'static str,
            mac: [u8; 6],
            interface_type: crate::InterfaceType,
            want_name: &'static str,
            expected_size: usize,
        }

        let test_cases = vec![
            // Base case.
            FileBackedConfigTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
                mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlanp0014",
                expected_size: 1,
            },
            // Same topological path as the base case, different MAC address.
            FileBackedConfigTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
                mac: [0xFE, 0x01, 0x01, 0x01, 0x01, 0x01],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlanp0014",
                expected_size: 1,
            },
            // Test case that labels iwilwifi as ethernet.
            FileBackedConfigTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/01:00.0/01:00.0/iwlwifi-wlan-softmac/wlan-ethernet/ethernet",
                mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
                interface_type: crate::InterfaceType::Ethernet,
                want_name: "ethp01",
                expected_size: 2,
            },
            // Test case that changes the previous test case's device class to wlan.
            // The test should detect that the device class doesn't match the interface
            // name, and overwrite with the new interface name that does match.
            FileBackedConfigTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/01:00.0/01:00.0/iwlwifi-wlan-softmac/wlan-ethernet/ethernet",
                mac: [0x01, 0x01, 0x01, 0x01, 0x01, 0x01],
                interface_type: crate::InterfaceType::Wlan,
                want_name: "wlanp01",
                expected_size: 2,
            },
        ];

        let temp_dir = tempfile::tempdir_in("/tmp").expect("failed to create the temp dir");
        let path = temp_dir.path().join("net.config.json");

        // query an existing interface with the same topo path and a different mac address
        for (
            _i,
            FileBackedConfigTestCase {
                topological_path,
                mac,
                interface_type,
                want_name,
                expected_size,
            },
        ) in test_cases.into_iter().enumerate()
        {
            let mut interface_config =
                FileBackedConfig::load(&path).expect("failed to load the interface config");

            let name = interface_config
                .generate_stable_name(
                    topological_path.into(),
                    fidl_fuchsia_net_ext::MacAddress { octets: mac },
                    interface_type,
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
        assert_eq!(
            &interface_config.generate_temporary_name(crate::InterfaceType::Ethernet),
            "etht0"
        );
        assert_eq!(&interface_config.generate_temporary_name(crate::InterfaceType::Wlan), "wlant1");
    }

    #[test]
    fn test_get_usb_255() {
        let topo_usb = "/dev/pci-00:14.0-fidl/xhci/usb/004/004/ifc-000/ax88179/ethernet";

        // test cases for 256 usb interfaces
        let mut config = Config { interfaces: vec![] };
        for n in 0u8..255u8 {
            let octets = [n, 0x01, 0x01, 0x01, 0x01, 00];

            let persistent_id = config
                .generate_identifier(topo_usb.into(), fidl_fuchsia_net_ext::MacAddress { octets });

            if let Some(index) = config.lookup_by_identifier(&persistent_id) {
                assert_eq!(config.interfaces[index].name, format!("{}{:x}", "wlanx", n));
            } else {
                let name = config
                    .generate_name(&persistent_id, crate::InterfaceType::Wlan)
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
        let persistent_id = config
            .generate_identifier(topo_usb.into(), fidl_fuchsia_net_ext::MacAddress { octets });
        assert!(config.generate_name(&persistent_id, crate::InterfaceType::Wlan).is_err());
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

    #[test]
    fn test_interface_matching_by_bus_type() {
        #[derive(Clone)]
        struct MatchBusTypeTestCase {
            topological_path: &'static str,
            bus_types: Vec<BusType>,
            expected_bus_type: BusType,
            want_match: bool,
        }

        let test_cases = vec![
            // Base case for PCI.
            MatchBusTypeTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
                bus_types: vec![BusType::PCI],
                expected_bus_type: BusType::PCI,
                want_match: true,
            },
            // Same topological path as the base case for PCI, but with
            // non-matching bus types.
            MatchBusTypeTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
                bus_types: vec![BusType::USB, BusType::SDIO],
                expected_bus_type: BusType::PCI,
                want_match: false,
            },
            // Base case for USB.
            MatchBusTypeTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
                bus_types: vec![BusType::USB],
                expected_bus_type: BusType::USB,
                want_match: true,
            },
            // Same topological path as the base case for USB, but with
            // non-matching bus types. Ensure that even though PCI is
            // present in the topological path, it does not match a PCI
            // controller.
            MatchBusTypeTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0/00:14.0/xhci/usb/004/004/ifc-000/ax88179/ethernet",
                bus_types: vec![BusType::PCI, BusType::SDIO],
                expected_bus_type: BusType::USB,
                want_match: false,
            },
            MatchBusTypeTestCase {
                topological_path: "/dev/sys/platform/05:00:6/aml-sd-emmc/sdio/broadcom-wlanphy/wlanphy",
                bus_types: vec![BusType::SDIO],
                expected_bus_type: BusType::SDIO,
                want_match: true,
            },
        ];

        for (
            _i,
            MatchBusTypeTestCase { topological_path, bus_types, expected_bus_type, want_match },
        ) in test_cases.into_iter().enumerate()
        {
            let device_info = devices::DeviceInfo {
                topological_path: topological_path.to_owned(),
                // `device_class` and `mac` have no effect on `BusType`
                // matching, so we use arbitrary values.
                device_class: fhwnet::DeviceClass::Virtual,
                mac: Default::default(),
            };

            // Verify the `BusType` determined from the device's
            // topological path.
            let bus_type =
                get_bus_type_for_topological_path(&device_info.topological_path).unwrap();
            assert_eq!(bus_type, expected_bus_type);

            // Create a matching rule for the provided `BusType` list.
            let matching_rule = MatchingRule::BusTypes(bus_types);
            let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
            assert_eq!(does_interface_match, want_match);
        }
    }

    #[test]
    fn test_interface_matching_by_bus_type_unsupported() {
        let device_info = devices::DeviceInfo {
            // `device_class` and `mac` have no effect on `BusType`
            // matching, so we use arbitrary values.
            device_class: fhwnet::DeviceClass::Virtual,
            mac: Default::default(),
            topological_path: String::from("/dev/sys/unsupported-bus/ethernet"),
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

    #[test]
    fn test_interface_matching_by_topological_path() {
        #[derive(Clone)]
        struct MatchPathTestCase {
            topological_path: &'static str,
            glob_str: &'static str,
            want_match: bool,
        }

        let test_cases = vec![
            // Glob matches the number pattern of XX:XX in the path.
            MatchPathTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
                glob_str: r"*[0-9][0-9]:[0-9][0-9]*",
                want_match: true,
            },
            // Glob is a matcher for any character, so all paths will match.
            MatchPathTestCase {
                topological_path: "pattern/will/match/anything",
                glob_str: r"*",
                want_match: true,
            },
            // Glob checks for '00' after the colon but it will not find it.
            MatchPathTestCase {
                topological_path: "/dev/sys/platform/pt/PCI0/bus/00:14.0_/00:14.0/ethernet",
                glob_str: r"*[0-9][0-9]:00*",
                want_match: false,
            },
        ];

        for (_i, MatchPathTestCase { topological_path, glob_str, want_match }) in
            test_cases.into_iter().enumerate()
        {
            let device_info = devices::DeviceInfo {
                topological_path: topological_path.to_owned(),
                // `device_class` and `mac` have no effect on `TopologicalPath`
                // matching, so we use arbitrary values.
                device_class: fhwnet::DeviceClass::Virtual,
                mac: Default::default(),
            };

            // Create a matching rule for the provided glob expression.
            let matching_rule =
                MatchingRule::TopologicalPath(glob::Pattern::new(glob_str).unwrap());
            let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
            assert_eq!(does_interface_match, want_match);
        }
    }

    #[test]
    fn test_interface_matching_by_device_class() {
        #[derive(Clone)]
        struct MatchDeviceClassTestCase {
            device_class_input: fhwnet::DeviceClass,
            device_classes: Vec<DeviceClass>,
            expected_device_class: DeviceClass,
            want_match: bool,
        }

        let test_cases = vec![
            // Base case for Ethernet.
            MatchDeviceClassTestCase {
                device_class_input: fhwnet::DeviceClass::Ethernet,
                device_classes: vec![DeviceClass::Ethernet],
                expected_device_class: DeviceClass::Ethernet,
                want_match: true,
            },
            // Same fhwnet::DeviceClass as the base case for Ethernet, but with
            // non-matching device classes.
            MatchDeviceClassTestCase {
                device_class_input: fhwnet::DeviceClass::Ethernet,
                device_classes: vec![DeviceClass::Wlan, DeviceClass::WlanAp],
                expected_device_class: DeviceClass::Ethernet,
                want_match: false,
            },
            // Base case for Wlan.
            MatchDeviceClassTestCase {
                device_class_input: fhwnet::DeviceClass::Wlan,
                device_classes: vec![DeviceClass::Wlan],
                expected_device_class: DeviceClass::Wlan,
                want_match: true,
            },
            // Same fhwnet::DeviceClass as the base case for Wlan, but with
            // non-matching device classes.
            MatchDeviceClassTestCase {
                device_class_input: fhwnet::DeviceClass::Wlan,
                device_classes: vec![DeviceClass::Ethernet, DeviceClass::WlanAp],
                expected_device_class: DeviceClass::Wlan,
                want_match: false,
            },
            // Base case for Ap.
            MatchDeviceClassTestCase {
                device_class_input: fhwnet::DeviceClass::WlanAp,
                device_classes: vec![DeviceClass::WlanAp],
                expected_device_class: DeviceClass::WlanAp,
                want_match: true,
            },
            // Same fhwnet::DeviceClass as the base case for Wlan, but with
            // non-matching device classes. Ensure that Wlan is differentiated
            // from Ap for interface matching.
            MatchDeviceClassTestCase {
                device_class_input: fhwnet::DeviceClass::WlanAp,
                device_classes: vec![DeviceClass::Ethernet, DeviceClass::Wlan],
                expected_device_class: DeviceClass::WlanAp,
                want_match: false,
            },
        ];

        for (
            _i,
            MatchDeviceClassTestCase {
                device_class_input,
                device_classes,
                expected_device_class,
                want_match,
            },
        ) in test_cases.into_iter().enumerate()
        {
            let device_info = devices::DeviceInfo {
                device_class: device_class_input,
                // `mac` and `topological_path` have no effect on `DeviceClass`
                // matching for the provided test cases, so we use
                // arbitrary values.
                mac: Default::default(),
                topological_path: Default::default(),
            };

            // Verify the `DeviceClass` determined from the device info.
            let device_class: DeviceClass = device_info.device_class.into();
            assert_eq!(device_class, expected_device_class);

            // Create a matching rule for the provided `DeviceClass` list.
            let matching_rule = MatchingRule::DeviceClasses(device_classes);
            let does_interface_match = matching_rule.does_interface_match(&device_info).unwrap();
            assert_eq!(does_interface_match, want_match);
        }
    }
}
