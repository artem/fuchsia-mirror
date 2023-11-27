// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

const DEFAULT_CONFIG: &str = include_str!("../default_ssh_config");

pub const IGNORED_KEYS: [&str; 29] = [
    "Host",
    "Match",
    "BindAddress",
    "ChallengeResponseAuthentication",
    "Cipher",
    "Ciphers",
    "DynamicForward",
    "ExitOnForwardFailure",
    "GSSAPIKeyExchange",
    "GSSAPIClientIdentity",
    "GSSAPIDelegateCredentials",
    "GSSAPIRenewalForcesRekey",
    "GSSAPITrustDns",
    "HashKnownHosts",
    "HostKeyAlgorithms",
    "HostKeyAlias",
    "HostName",
    "LocalCommand",
    "MACs",
    "NoHostAuthenticationForLocalhost",
    "PreferredAuthentications",
    "Protocol",
    "RhostsRSAAuthentication",
    "RSAAuthentication",
    "SmartcardDevice",
    "Tunnel",
    "TunnelDevice",
    "UsePrivilegedPort",
    "VisualHostKey",
];

pub const CONFIG_KEYS: [&str; 31] = [
    "AddressFamily",
    "BatchMode",
    "CheckHostIP",
    "Compression",
    "ConnectionAttempts",
    "ConnectTimeout",
    "ControlMaster",
    "ControlPath",
    "EscapeChar",
    "ForwardAgent",
    "ForwardX11",
    "ForwardX11Trusted",
    "GatewayPorts",
    "GlobalKnownHostsFile",
    "GSSAPIAuthentication",
    "HostbasedAuthentication",
    "IdentitiesOnly",
    "IdentityFile",
    "KbdInteractiveAuthentication",
    "LocalForward",
    "LogLevel",
    "ProxyCommand",
    "PubkeyAuthentication",
    "RemoteForward",
    "SendEnv",
    "ServerAliveCountMax",
    "ServerAliveInterval",
    "StrictHostKeyChecking",
    "TCPKeepAlive",
    "UserKnownHostsFile",
    "VerifyHostKeyDNS",
];

#[derive(Debug, Default)]
pub struct SshConfig {
    data: BTreeMap<&'static str, String>,
}

impl SshConfig {
    pub fn empty() -> Self {
        SshConfig::default()
    }

    /// New config instance with defaults loaded.
    pub fn new() -> Result<Self> {
        let mut me = Self::empty();
        me.read_default_config(DEFAULT_CONFIG)?;
        Ok(me)
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        if CONFIG_KEYS.contains(&key) {
            Ok(self.data.get(key).cloned())
        } else {
            bail!("Unknown or unsupported key {key}")
        }
    }

    pub fn set<S: Into<String>>(&mut self, key: &str, value: S) -> Result<Option<String>> {
        if let Some(key_str) = CONFIG_KEYS.iter().find(|k| k == &&key) {
            Ok(self.data.insert(key_str, value.into()))
        } else {
            bail!("Unknown or unsupported key {key}")
        }
    }

    pub fn to_args(&self) -> Vec<String> {
        let args: Vec<String> = self
            .data
            .iter()
            .map(|(k, v)| vec!["-o".to_string(), format!("{k}={v}")])
            .flatten()
            .map(|s| s.to_string())
            .collect();
        args
    }

    fn read_default_config(&mut self, config_contents: &str) -> Result<()> {
        for mut l in config_contents.lines() {
            l = l.trim();
            match l {
                "" => (),
                s if s.starts_with("#") => (),
                s => {
                    let mut parts = s.split(&['=', ' ']);
                    if let Some(key) = parts.next() {
                        let vs = parts.collect::<Vec<_>>().join(" ");
                        let value = vs.trim().to_string();
                        if let Some(key_str) = CONFIG_KEYS.iter().find(|k| k == &&key) {
                            self.data.insert(key_str, value);
                        } else if IGNORED_KEYS.contains(&key) {
                            tracing::info!("ignoring ssh config key {key}");
                        } else {
                            bail!("Unknown configuration key \"{key}\" in {l}");
                        }
                    } else {
                        bail!("Invalid configuration line [{l}");
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        let empty = SshConfig::empty();
        assert_eq!(empty.data.iter().count(), 0)
    }

    #[test]
    fn test_new() {
        let new = SshConfig::new().expect("new config with defaults");
        assert!(new.data.iter().count() != 0)
    }

    #[test]
    fn test_comments_and_blank_lines() {
        let comments_and_blanks = r#"#comment line
#then some blanks


        # and some leading spaces"
# comment1
# comment2
        # indented comment.
"#;
        let mut cfg = SshConfig::empty();
        cfg.read_default_config(comments_and_blanks).expect("no error reading comments and blanks");
        assert_eq!(cfg.data.iter().count(), 0)
    }

    #[test]
    fn test_setting_values() {
        let values = r#"
# simple
AddressFamily=any
# Then with spaces
BatchMode no
CheckHostIP  yes
# then with both
VerifyHostKeyDNS =  no
# TCPKeepAlive=yes
SendEnv= two words
RemoteForward = "quoted two words"
"#;
        let mut cfg = SshConfig::empty();
        cfg.read_default_config(values).expect("no error reading values data");
        assert_eq!(cfg.data.iter().count(), 6);
        assert_eq!(cfg.get("AddressFamily").unwrap(), Some("any".into()));
        assert_eq!(cfg.get("BatchMode").unwrap(), Some("no".into()));
        assert_eq!(cfg.get("CheckHostIP").unwrap(), Some("yes".into()));
        assert_eq!(cfg.get("VerifyHostKeyDNS").unwrap(), Some("no".into()));
        assert_eq!(cfg.get("TCPKeepAlive").unwrap(), None);
        assert_eq!(cfg.get("SendEnv").unwrap(), Some("two words".into()));
        assert_eq!(cfg.get("RemoteForward").unwrap(), Some("\"quoted two words\"".into()));

        let args = cfg.to_args();
        assert_eq!(
            args,
            vec![
                "-o",
                "AddressFamily=any",
                "-o",
                "BatchMode=no",
                "-o",
                "CheckHostIP=yes",
                "-o",
                "RemoteForward=\"quoted two words\"",
                "-o",
                "SendEnv=two words",
                "-o",
                "VerifyHostKeyDNS=no",
            ]
        )
    }
}
