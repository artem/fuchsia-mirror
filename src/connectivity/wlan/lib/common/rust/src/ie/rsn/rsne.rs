// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    akm::{self, AKM_PSK, AKM_SAE},
    cipher::{self, CIPHER_CCMP_128},
    pmkid,
    suite_filter::DEFAULT_GROUP_MGMT_CIPHER,
    suite_selector,
};

use crate::appendable::{Appendable, BufferTooSmall};
use crate::organization::Oui;
use bytes::Bytes;
use fidl_fuchsia_wlan_common as fidl_common;
use nom::combinator::{map, map_res};
use nom::number::streaming::{le_u16, le_u8};
use nom::{call, cond, count, do_parse, eof, named, named_attr, take, try_parse, IResult};
use wlan_bitfield::bitfield;

use thiserror::Error;

macro_rules! if_remaining (
  ($i:expr, $f:expr) => ( cond!($i, $i.len() !=0, call!($f)) );
);

// IEEE 802.11-2016, 9.4.2.25.1
pub const ID: u8 = 48;
pub const VERSION: u16 = 1;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum Error {
    #[error("no group data cipher suite")]
    NoGroupDataCipherSuite,
    #[error("no pairwise cipher suite")]
    NoPairwiseCipherSuite,
    #[error("too many pairwise cipher suites")]
    TooManyPairwiseCipherSuites,
    #[error("no akm suite")]
    NoAkmSuite,
    #[error("too many akm suites")]
    TooManyAkmSuites,
    #[error("AKM suite does not have mic_bytes")]
    NoAkmMicBytes,
    #[error("invalid supplicant management frame protection")]
    InvalidSupplicantMgmtFrameProtection,
    #[error("invalid authenticator management frame protection")]
    InvalidAuthenticatorMgmtFrameProtection,
    #[error("cannot derive WPA2 RSNE")]
    CannotDeriveWpa2Rsne,
    #[error("cannot derive WPA3 RSNE")]
    CannotDeriveWpa3Rsne,
}

#[macro_export]
macro_rules! rsne_ensure {
    ($cond:expr, $err:expr $(,)?) => {
        if !$cond {
            return std::result::Result::Err($err);
        }
    };
}

// IEEE 802.11-2016, 9.4.2.25.1
#[derive(Debug, PartialOrd, PartialEq, Clone)]
pub struct Rsne {
    pub version: u16,
    pub group_data_cipher_suite: Option<cipher::Cipher>,
    pub pairwise_cipher_suites: Vec<cipher::Cipher>,
    pub akm_suites: Vec<akm::Akm>,
    pub rsn_capabilities: Option<RsnCapabilities>,
    pub pmkids: Vec<pmkid::Pmkid>,
    pub group_mgmt_cipher_suite: Option<cipher::Cipher>,
}

impl Default for Rsne {
    fn default() -> Self {
        Rsne {
            version: VERSION,
            group_data_cipher_suite: None,
            pairwise_cipher_suites: vec![],
            akm_suites: vec![],
            rsn_capabilities: None,
            pmkids: vec![],
            group_mgmt_cipher_suite: None,
        }
    }
}

#[bitfield(
    0         preauth,
    1         no_pairwise,
    2..=3     ptksa_replay_counter,
    4..=5     gtksa_replay_counter,
    6         mgmt_frame_protection_req,
    7         mgmt_frame_protection_cap,
    8         joint_multiband,
    9         peerkey_enabled,
    10        ssp_amsdu_cap,
    11        ssp_amsdu_req,
    12        pbac,
    13        extended_key_id,
    14..=15   _, // reserved
)]
#[derive(PartialOrd, PartialEq, Clone)]
pub struct RsnCapabilities(pub u16);

impl RsnCapabilities {
    pub fn is_wpa2_compatible(&self) -> bool {
        !self.contains_unsupported_capability()
    }

    pub fn is_wpa3_compatible(&self, wpa2_compatibility_mode: bool) -> bool {
        self.mgmt_frame_protection_cap()
            && (self.mgmt_frame_protection_req() || wpa2_compatibility_mode)
            && !self.contains_unsupported_capability()
    }

    pub fn is_compatible_with_features(
        &self,
        security_support: &fidl_common::SecuritySupport,
    ) -> bool {
        !self.mgmt_frame_protection_req() || security_support.mfp.supported
    }

    /// Returns true if RsnCapabilities contains a capability
    /// which wlanstack cannot currently agree to handle.
    fn contains_unsupported_capability(&self) -> bool {
        self.joint_multiband()
            || self.peerkey_enabled()
            || self.ssp_amsdu_req()
            || self.pbac()
            || self.extended_key_id()
    }
}

/// Used to identify the last field we will write into our RSNE buffer.
#[derive(PartialEq)]
enum FinalField {
    Version,
    GroupData,
    Pairwise,
    Akm,
    Caps,
    Pmkid,
    GroupMgmt,
}

impl Rsne {
    pub fn wpa2_rsne() -> Self {
        Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        }
    }

    pub fn wpa2_rsne_with_caps(rsn_capabilities: RsnCapabilities) -> Self {
        Self::wpa2_rsne().with_caps(rsn_capabilities)
    }

    pub fn wpa2_wpa3_rsne() -> Self {
        Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_SAE, AKM_PSK],
            rsn_capabilities: Some(RsnCapabilities(0).with_mgmt_frame_protection_cap(true)),
            // Always explicitly include a group management cipher suite. There
            // is no reason to rely on the default group management cipher
            // suite selection defined in IEEE 802.11-2016 9.4.2.25.2 if we are making
            // the Rsne ourselves.
            group_mgmt_cipher_suite: Some(DEFAULT_GROUP_MGMT_CIPHER),
            ..Default::default()
        }
    }

    pub fn wpa2_wpa3_rsne_with_extra_caps(rsn_capabilities: RsnCapabilities) -> Self {
        let rsne = Self::wpa2_wpa3_rsne();
        let wpa2_wpa3_minimum_rsn_capabilities = rsne.rsn_capabilities.as_ref().unwrap().clone();
        rsne.with_caps(RsnCapabilities(
            wpa2_wpa3_minimum_rsn_capabilities.raw() | rsn_capabilities.raw(),
        ))
    }

    pub fn wpa3_rsne() -> Self {
        Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_SAE],
            rsn_capabilities: Some(
                RsnCapabilities(0)
                    .with_mgmt_frame_protection_cap(true)
                    .with_mgmt_frame_protection_req(true),
            ),
            // Always explicitly include a group management cipher suite. There
            // is no reason to rely on the default group management cipher
            // suite selection defined in IEEE 802.11-2016 9.4.2.25.2 if we are making
            // the Rsne ourselves.
            group_mgmt_cipher_suite: Some(DEFAULT_GROUP_MGMT_CIPHER),
            ..Default::default()
        }
    }

    pub fn wpa3_rsne_with_extra_caps(rsn_capabilities: RsnCapabilities) -> Self {
        let rsne = Self::wpa3_rsne();
        let wpa3_minimum_rsn_capabilities = rsne.rsn_capabilities.as_ref().unwrap().clone();
        rsne.with_caps(RsnCapabilities(
            wpa3_minimum_rsn_capabilities.raw() | rsn_capabilities.raw(),
        ))
    }

    /// Constructs Supplicant's RSNE with:
    /// Group Data Cipher: same as A-RSNE (CCMP-128 or TKIP)
    /// Pairwise Cipher: best from A-RSNE (prefer CCMP-128 over TKIP)
    /// AKM: PSK
    pub fn derive_wpa2_s_rsne(
        &self,
        security_support: &fidl_common::SecuritySupport,
    ) -> Result<Self, Error> {
        if !self.is_wpa2_rsn_compatible(&security_support) {
            return Err(Error::CannotDeriveWpa2Rsne);
        }

        // Enable management frame protection if supported.
        let rsn_capabilities = match self.rsn_capabilities.clone() {
            Some(cap) => {
                if cap.mgmt_frame_protection_cap() && security_support.mfp.supported {
                    Some(cap.with_mgmt_frame_protection_req(true))
                } else {
                    Some(cap)
                }
            }
            None => None,
        };

        // CCMP-128 is better than TKIP
        let pairwise_cipher_suites =
            vec![match self.pairwise_cipher_suites.iter().max_by_key(|cipher_suite| {
                match **cipher_suite {
                    CIPHER_CCMP_128 => 1,
                    _ => 0,
                }
            }) {
                Some(cipher_suite) => cipher_suite.clone(),
                None => return Err(Error::NoPairwiseCipherSuite),
            }];

        Ok(Rsne {
            group_data_cipher_suite: self.group_data_cipher_suite.clone(),
            pairwise_cipher_suites,
            akm_suites: vec![AKM_PSK],
            rsn_capabilities,
            ..Default::default()
        })
    }

    /// Constructs Supplicant's RSNE with:
    /// Group Data Cipher: CCMP-128
    /// Pairwise Cipher: CCMP-128
    /// AKM: SAE
    pub fn derive_wpa3_s_rsne(
        &self,
        security_support: &fidl_common::SecuritySupport,
    ) -> Result<Rsne, Error> {
        if !self.is_wpa3_rsn_compatible(&security_support) {
            return Err(Error::CannotDeriveWpa3Rsne);
        }

        let rsn_capabilities = match self.rsn_capabilities.clone() {
            Some(cap) => Some(cap.with_mgmt_frame_protection_req(true)),
            None => None,
        };

        Ok(Rsne {
            group_data_cipher_suite: self.group_data_cipher_suite.clone(),
            pairwise_cipher_suites: vec![cipher::Cipher {
                oui: suite_selector::OUI,
                suite_type: cipher::CCMP_128,
            }],
            akm_suites: vec![akm::Akm { oui: suite_selector::OUI, suite_type: akm::SAE }],
            rsn_capabilities,
            ..Default::default()
        })
    }

    /// Validates this RSNE contains only one of each cipher type and only one AKM with
    /// a defined number of MIC bytes.
    pub fn ensure_valid_s_rsne(&self) -> Result<(), Error> {
        let s_rsne = self;
        s_rsne.group_data_cipher_suite.as_ref().ok_or(Error::NoGroupDataCipherSuite)?;

        rsne_ensure!(s_rsne.pairwise_cipher_suites.len() >= 1, Error::NoPairwiseCipherSuite);
        rsne_ensure!(s_rsne.pairwise_cipher_suites.len() <= 1, Error::TooManyPairwiseCipherSuites);

        rsne_ensure!(s_rsne.akm_suites.len() >= 1, Error::NoAkmSuite);
        rsne_ensure!(s_rsne.akm_suites.len() <= 1, Error::TooManyAkmSuites);

        let akm = &s_rsne.akm_suites[0];
        rsne_ensure!(akm.mic_bytes().is_some(), Error::NoAkmMicBytes);

        Ok(())
    }

    /// Verify that Supplicant RSNE is a subset of Authenticator RSNE
    pub fn is_valid_subset_of(&self, a_rsne: &Rsne) -> Result<bool, Error> {
        let s_rsne = self;
        s_rsne.ensure_valid_s_rsne()?;

        let s_caps = s_rsne.rsn_capabilities.as_ref().unwrap_or(&RsnCapabilities(0));
        let s_mgmt_req = s_caps.mgmt_frame_protection_req();
        let s_mgmt_cap = s_caps.mgmt_frame_protection_cap();
        let a_caps = a_rsne.rsn_capabilities.as_ref().unwrap_or(&RsnCapabilities(0));
        let a_mgmt_req = a_caps.mgmt_frame_protection_req();
        let a_mgmt_cap = a_caps.mgmt_frame_protection_cap();

        // IEEE Std 802.11-2016, 12.6.3, Table 12-2
        match (a_mgmt_cap, a_mgmt_req, s_mgmt_cap, s_mgmt_req) {
            (true, _, false, true) => return Err(Error::InvalidSupplicantMgmtFrameProtection),
            (false, true, true, _) => return Err(Error::InvalidAuthenticatorMgmtFrameProtection),
            (true, true, false, false) => return Ok(false),
            (false, false, true, true) => return Ok(false),
            // the remaining cases fall into either of these buckets:
            // 1 - spec mentions that "The AP may associate with the STA"
            // 2 - it's not covered in the spec, which means presumably we can ignore it. For example,
            //     if AP/client is not management frame protection capable, then it probably doesn't
            //     matter whether the opposite party advertises an invalid setting
            _ => (),
        }

        Ok(a_rsne
            .group_data_cipher_suite
            .iter()
            // .unwrap() will succeed because .ensure_valid_s_rsne() was run at the beginning of this function.
            .any(|c| c == s_rsne.group_data_cipher_suite.as_ref().unwrap())
            && a_rsne.pairwise_cipher_suites.iter().any(|c| *c == s_rsne.pairwise_cipher_suites[0])
            && a_rsne.akm_suites.iter().any(|c| *c == s_rsne.akm_suites[0]))
    }

    /// IEEE Std. 802.11-2016 9.4.2.25.1
    ///    "All fields after the Version field are optional. If any
    ///     nonzero-length field is absent, then none of the subsequent
    ///     fields is included."
    /// Determine the last field we will write to produce the smallest RSNE
    /// that matches this specification.
    fn final_field(&self) -> FinalField {
        if self.group_data_cipher_suite.is_none() {
            FinalField::Version
        } else if self.rsn_capabilities.is_none() {
            if self.akm_suites.is_empty() {
                if self.pairwise_cipher_suites.is_empty() {
                    FinalField::GroupData
                } else {
                    FinalField::Pairwise
                }
            } else {
                FinalField::Akm
            }
        } else {
            if self.group_mgmt_cipher_suite.is_none() {
                if self.pmkids.is_empty() {
                    FinalField::Caps
                } else {
                    FinalField::Pmkid
                }
            } else {
                FinalField::GroupMgmt
            }
        }
    }

    /// IEEE Std. 802.11-2016 9.4.2.25.1 specifies lengths for all fields.
    pub fn len(&self) -> usize {
        let final_field = self.final_field();
        let mut length: usize = 4; // Element Id (1) + Length (1) + Version (2)
        if final_field == FinalField::Version {
            return length;
        }
        length += 4; // Group data cipher (4)
        if final_field == FinalField::GroupData {
            return length;
        }
        // Pairwise cipher count (2) + pairwise ciphers (4 * count)
        length += 2 + 4 * self.pairwise_cipher_suites.len();
        if final_field == FinalField::Pairwise {
            return length;
        }
        // AKM count (2) + AKMs (4 * count)
        length += 2 + 4 * self.akm_suites.len();
        if final_field == FinalField::Akm {
            return length;
        }
        length += 2; // RSN capabilities (2)
        if final_field == FinalField::Caps {
            return length;
        }
        // PMKID count (2) + PMKIDs (16 * count)
        length += 2 + 16 * self.pmkids.len();
        if final_field == FinalField::Pmkid {
            return length;
        }
        length + 4 // Group management cipher (4)
    }

    pub fn into_bytes(self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.len());
        self.write_into(&mut buf).expect("error writing RSNE into buffer");
        buf
    }

    pub fn write_into<A: Appendable>(&self, buf: &mut A) -> Result<(), BufferTooSmall> {
        if !buf.can_append(self.len()) {
            return Err(BufferTooSmall);
        }
        let final_field = self.final_field();

        buf.append_byte(ID)?;
        buf.append_byte((self.len() - 2) as u8)?;
        buf.append_value(&self.version)?;
        if final_field == FinalField::Version {
            return Ok(());
        }

        match self.group_data_cipher_suite.as_ref() {
            None => return Ok(()),
            Some(cipher) => {
                buf.append_bytes(&cipher.oui[..])?;
                buf.append_byte(cipher.suite_type)?;
            }
        };
        if final_field == FinalField::GroupData {
            return Ok(());
        }

        buf.append_value(&(self.pairwise_cipher_suites.len() as u16))?;
        for cipher in &self.pairwise_cipher_suites {
            buf.append_bytes(&cipher.oui[..])?;
            buf.append_byte(cipher.suite_type)?;
        }
        if final_field == FinalField::Pairwise {
            return Ok(());
        }

        buf.append_value(&(self.akm_suites.len() as u16))?;
        for akm in &self.akm_suites {
            buf.append_bytes(&akm.oui[..])?;
            buf.append_byte(akm.suite_type)?;
        }
        if final_field == FinalField::Akm {
            return Ok(());
        }

        match self.rsn_capabilities.as_ref() {
            None => return Ok(()),
            Some(caps) => buf.append_value(&caps.0)?,
        };
        if final_field == FinalField::Caps {
            return Ok(());
        }

        buf.append_value(&(self.pmkids.len() as u16))?;
        for pmkid in &self.pmkids {
            buf.append_bytes(&pmkid[..])?;
        }
        if final_field == FinalField::Pmkid {
            return Ok(());
        }

        if let Some(cipher) = self.group_mgmt_cipher_suite.as_ref() {
            buf.append_bytes(&cipher.oui[..])?;
            buf.append_byte(cipher.suite_type)?;
        }
        Ok(())
    }

    /// Supported Ciphers and AKMs:
    /// Group Data Ciphers: CCMP-128, TKIP
    /// Pairwise Cipher: CCMP-128, TKIP
    /// AKM: PSK, SAE
    pub fn is_wpa2_rsn_compatible(&self, security_support: &fidl_common::SecuritySupport) -> bool {
        let group_data_supported = self.group_data_cipher_suite.as_ref().is_some_and(|c| {
            // IEEE allows TKIP usage only for compatibility reasons.
            c.has_known_usage()
                && (c.suite_type == cipher::CCMP_128 || c.suite_type == cipher::TKIP)
        });

        let pairwise_supported = self.pairwise_cipher_suites.iter().any(|c| {
            c.has_known_usage()
                && (c.suite_type == cipher::CCMP_128 || c.suite_type == cipher::TKIP)
        });
        let akm_supported =
            self.akm_suites.iter().any(|a| a.has_known_algorithm() && a.suite_type == akm::PSK);
        let caps_supported =
            self.rsn_capabilities.as_ref().map_or(true, RsnCapabilities::is_wpa2_compatible);
        let features_supported = self
            .rsn_capabilities
            .as_ref()
            .map_or(true, |caps| caps.is_compatible_with_features(security_support));

        group_data_supported
            && pairwise_supported
            && akm_supported
            && caps_supported
            && features_supported
    }

    /// Check if this is a supported WPA3-Personal or WPA3-Personal transition AP per the WFA WPA3 specification.
    /// Supported Ciphers and AKMs:
    /// Group Data Ciphers: CCMP-128, TKIP
    /// Pairwise Cipher: CCMP-128
    /// AKM: SAE (also PSK in transition mode)
    /// The MFPR bit is required, except for transition mode.
    pub fn is_wpa3_rsn_compatible(&self, security_support: &fidl_common::SecuritySupport) -> bool {
        let group_data_supported = self.group_data_cipher_suite.as_ref().is_some_and(|c| {
            c.has_known_usage()
                && (c.suite_type == cipher::CCMP_128 || c.suite_type == cipher::TKIP)
        });
        let pairwise_supported = self
            .pairwise_cipher_suites
            .iter()
            .any(|c| c.has_known_usage() && c.suite_type == cipher::CCMP_128);
        let sae_supported =
            self.akm_suites.iter().any(|a| a.has_known_algorithm() && a.suite_type == akm::SAE);
        let wpa2_compatibility_mode =
            self.akm_suites.iter().any(|a| a.has_known_algorithm() && a.suite_type == akm::PSK);
        let caps_supported = self
            .rsn_capabilities
            .as_ref()
            .is_some_and(|caps| caps.is_wpa3_compatible(wpa2_compatibility_mode));
        let mut features_supported = self
            .rsn_capabilities
            .as_ref()
            .map_or(true, |caps| caps.is_compatible_with_features(security_support));
        // WFA WPA3 specification v3.0: 2.3 rule 7: Verify that we actually support MFP, regardless of whether
        // the features bits indicate we need that support. SAE without MFP is not a valid configuration.
        features_supported &= security_support.mfp.supported;
        group_data_supported
            && pairwise_supported
            && sae_supported
            && caps_supported
            && features_supported
    }

    fn with_caps(mut self, rsn_capabilities: RsnCapabilities) -> Self {
        self.rsn_capabilities = Some(rsn_capabilities);
        self
    }
}

fn read_suite_selector<T>(input: &[u8]) -> IResult<&[u8], T>
where
    T: suite_selector::Factory<Suite = T>,
{
    let (i1, bytes) = try_parse!(input, take!(4));
    let oui = Oui::new([bytes[0], bytes[1], bytes[2]]);
    return Ok((i1, T::new(oui, bytes[3])));
}

fn read_pmkid(input: &[u8]) -> IResult<&[u8], pmkid::Pmkid> {
    let f = |bytes| {
        let pmkid_data = Bytes::copy_from_slice(bytes);
        return pmkid::new(pmkid_data);
    };

    map_res(nom::bytes::streaming::take(16usize), f)(input)
}

named!(akm<&[u8], akm::Akm>, call!(read_suite_selector::<akm::Akm>));
named!(cipher<&[u8], cipher::Cipher>, call!(read_suite_selector::<cipher::Cipher>));

named_attr!(
    /// convert bytes of an RSNE information element into an RSNE representation. This method
    /// does not depend on the information element length field (second byte) and thus does not
    /// validate that it's correct
    , // comma ends the attribute list to named_attr
    pub from_bytes<&[u8], Rsne>,
       do_parse!(
           _element_id: le_u8 >>
           _length: le_u8 >>
           version: le_u16 >>
           group_cipher: if_remaining!(cipher) >>
           pairwise_count: if_remaining!(le_u16) >>
           pairwise_list: count!(cipher, pairwise_count.unwrap_or(0) as usize)  >>
           akm_count: if_remaining!(le_u16) >>
           akm_list: count!(akm, akm_count.unwrap_or(0) as usize)  >>
           rsn_capabilities: if_remaining!(map(le_u16, RsnCapabilities)) >>
           pmkid_count: if_remaining!(le_u16) >>
           pmkid_list: count!(read_pmkid, pmkid_count.unwrap_or(0) as usize)  >>
           group_mgmt_cipher_suite: if_remaining!(cipher) >>
           eof!() >>
           (Rsne{
                version: version,
                group_data_cipher_suite: group_cipher,
                pairwise_cipher_suites: pairwise_list,
                akm_suites: akm_list,
                rsn_capabilities: rsn_capabilities,
                pmkids: pmkid_list,
                group_mgmt_cipher_suite: group_mgmt_cipher_suite
           })
    )
);

#[cfg(test)]
mod tests {
    use super::{
        akm::{AKM_EAP, AKM_FT_PSK},
        cipher::{CIPHER_BIP_CMAC_256, CIPHER_GCMP_256, CIPHER_TKIP},
        *,
    };
    use crate::test_utils::fake_features::fake_security_support_empty;
    use crate::test_utils::FixedSizedTestBuffer;
    use test_case::test_case;

    #[cfg(feature = "benchmark")]
    mod bench {
        use self::test::Bencher;
        use super::*;
        #[cfg()]
        #[bench]
        fn bench_parse_with_nom(b: &mut Bencher) {
            let frame: Vec<u8> = vec![
                0x30, 0x2A, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04,
                0x01, 0x00, 0x00, 0x0f, 0xac, 0x02, 0xa8, 0x04, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
                0x05, 0x06, 0x07, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x00, 0x0f,
                0xac, 0x04,
            ];
            b.iter(|| from_bytes(&frame));
        }
    }

    #[test]
    fn test_write_into() {
        let frame: Vec<u8> = vec![
            0x30, 0x2A, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04,
            0x01, 0x00, 0x00, 0x0f, 0xac, 0x02, 0xa8, 0x04, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x05, 0x06, 0x07, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x00, 0x0f,
            0xac, 0x04,
        ];
        let mut buf = Vec::with_capacity(128);
        let result = from_bytes(&frame);
        assert!(result.is_ok());
        let rsne = result.unwrap().1;
        rsne.write_into(&mut buf).expect("failed writing RSNE");
        let rsne_len = buf.len();
        let left_over = buf.split_off(rsne_len);
        assert_eq!(&buf[..], &frame[..]);
        assert!(left_over.iter().all(|b| *b == 0));
    }

    #[test]
    fn test_short_buffer() {
        let frame: Vec<u8> = vec![
            0x30, 0x2A, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04,
            0x01, 0x00, 0x00, 0x0f, 0xac, 0x02, 0xa8, 0x04, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x05, 0x06, 0x07, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x00, 0x0f,
            0xac, 0x04,
        ];
        let mut buf = FixedSizedTestBuffer::new(32);
        let result = from_bytes(&frame);
        assert!(result.is_ok());
        let rsne = result.unwrap().1;
        rsne.write_into(&mut buf).expect_err("expected writing RSNE to fail");
        assert_eq!(buf.bytes_written(), 0);
    }

    #[test]
    fn test_rsn_fields_representation() {
        let frame: Vec<u8> = vec![
            0x30, // element id
            0x2A, // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac, 0x04, // group data cipher suite
            0x01, 0x00, // pairwise cipher suite count
            0x00, 0x0f, 0xac, 0x04, // pairwise cipher suite list
            0x01, 0x00, // akm suite count
            0x00, 0x0f, 0xac, 0x02, // akm suite list
            0xa8, 0x04, // rsn capabilities
            0x01, 0x00, // pmk id count
            // pmk id list
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
            0x10, 0x11, 0x00, 0x0f, 0xac, 0x04, // group management cipher suite
        ];
        let result = from_bytes(&frame);
        assert!(result.is_ok());
        let rsne = result.unwrap().1;

        assert_eq!(rsne.version, VERSION);
        assert_eq!(rsne.len(), 0x2a + 2);

        assert!(rsne.group_data_cipher_suite.is_some());
        assert_eq!(rsne.group_data_cipher_suite, Some(CIPHER_CCMP_128));
        assert_eq!(rsne.pairwise_cipher_suites.len(), 1);
        assert_eq!(rsne.pairwise_cipher_suites[0].oui, Oui::DOT11);
        assert_eq!(rsne.pairwise_cipher_suites[0].suite_type, cipher::CCMP_128);
        assert_eq!(rsne.akm_suites.len(), 1);
        assert_eq!(rsne.akm_suites[0].suite_type, akm::PSK);

        let rsn_capabilities = rsne.rsn_capabilities.expect("should have RSN capabilities");
        assert_eq!(rsn_capabilities.preauth(), false);
        assert_eq!(rsn_capabilities.no_pairwise(), false);
        assert_eq!(rsn_capabilities.ptksa_replay_counter(), 2);
        assert_eq!(rsn_capabilities.gtksa_replay_counter(), 2);
        assert!(!rsn_capabilities.mgmt_frame_protection_req());
        assert!(rsn_capabilities.mgmt_frame_protection_cap());
        assert!(!rsn_capabilities.joint_multiband());
        assert!(!rsn_capabilities.peerkey_enabled());
        assert!(rsn_capabilities.ssp_amsdu_cap());
        assert!(!rsn_capabilities.ssp_amsdu_req());
        assert!(!rsn_capabilities.pbac());
        assert!(!rsn_capabilities.extended_key_id());

        assert_eq!(rsn_capabilities.0, 0xa8 + (0x04 << 8));

        let pmkids: &[u8] = &[
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
            0x10, 0x11,
        ];
        assert_eq!(rsne.pmkids.len(), 1);
        assert_eq!(rsne.pmkids[0], Bytes::from(pmkids));

        assert_eq!(rsne.group_mgmt_cipher_suite, Some(CIPHER_CCMP_128));
    }

    #[test]
    fn test_rsn_capabilities_setters() {
        let mut rsn_caps = RsnCapabilities(0u16);
        rsn_caps.set_ptksa_replay_counter(2);
        rsn_caps.set_gtksa_replay_counter(2);
        rsn_caps.set_mgmt_frame_protection_cap(true);
        rsn_caps.set_ssp_amsdu_cap(true);

        assert_eq!(rsn_caps.0, 0xa8 + (0x04 << 8));
    }

    #[test]
    fn test_invalid_wpa2_caps() {
        assert!(RsnCapabilities(0).is_wpa2_compatible());

        let caps = RsnCapabilities(0).with_joint_multiband(true);
        assert!(!caps.is_wpa2_compatible());

        let caps = RsnCapabilities(0).with_peerkey_enabled(true);
        assert!(!caps.is_wpa2_compatible());

        let caps = RsnCapabilities(0).with_ssp_amsdu_req(true);
        assert!(!caps.is_wpa2_compatible());

        let caps = RsnCapabilities(0).with_pbac(true);
        assert!(!caps.is_wpa2_compatible());

        let caps = RsnCapabilities(0).with_extended_key_id(true);
        assert!(!caps.is_wpa2_compatible());
    }

    static MFP_SUPPORT_ONLY: fidl_common::SecuritySupport = fidl_common::SecuritySupport {
        mfp: fidl_common::MfpFeature { supported: true },
        sae: fidl_common::SaeFeature {
            driver_handler_supported: false,
            sme_handler_supported: false,
        },
    };

    #[test_case(MFP_SUPPORT_ONLY, true)]
    #[test_case(fake_security_support_empty(), false)]
    #[fuchsia::test]
    fn test_wpa2_enables_pmf_if_supported(
        security_support: fidl_common::SecuritySupport,
        expect_mfp: bool,
    ) {
        let a_rsne =
            Rsne::wpa2_rsne_with_caps(RsnCapabilities(0).with_mgmt_frame_protection_cap(true));
        assert!(a_rsne.is_wpa2_rsn_compatible(&security_support));

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&security_support)
            .expect("Should be able to derive s_rsne with PMF");
        assert!(s_rsne.is_wpa2_rsn_compatible(&security_support));
        assert_eq!(
            expect_mfp,
            s_rsne
                .rsn_capabilities
                .expect("PMF RSNE should have RSN capabilities")
                .mgmt_frame_protection_req()
        );
    }

    #[test]
    fn test_invalid_wpa3_caps() {
        assert!(!RsnCapabilities(0).is_wpa3_compatible(false));

        let wpa3_caps = RsnCapabilities(0)
            .with_mgmt_frame_protection_cap(true)
            .with_mgmt_frame_protection_req(true);
        assert!(wpa3_caps.is_wpa3_compatible(false));

        let caps = wpa3_caps.clone().with_joint_multiband(true);
        assert!(!caps.is_wpa3_compatible(false));

        let caps = wpa3_caps.clone().with_peerkey_enabled(true);
        assert!(!caps.is_wpa3_compatible(false));

        let caps = wpa3_caps.clone().with_ssp_amsdu_req(true);
        assert!(!caps.is_wpa3_compatible(false));

        let caps = wpa3_caps.clone().with_pbac(true);
        assert!(!caps.is_wpa3_compatible(false));

        let caps = wpa3_caps.clone().with_extended_key_id(true);
        assert!(!caps.is_wpa3_compatible(false));

        let wpa2_wpa3_caps = RsnCapabilities(0).with_mgmt_frame_protection_cap(true);
        assert!(wpa2_wpa3_caps.is_wpa3_compatible(true));

        let caps = wpa2_wpa3_caps.clone().with_extended_key_id(true);
        assert!(!caps.is_wpa3_compatible(true));
    }

    #[test]
    fn test_with_caps() {
        assert!(Rsne::wpa2_rsne().rsn_capabilities.is_none());
        let rsne_with_caps =
            Rsne::wpa2_rsne_with_caps(RsnCapabilities(0).with_peerkey_enabled(true));
        assert!(rsne_with_caps.rsn_capabilities.as_ref().unwrap().peerkey_enabled());

        assert!(!Rsne::wpa2_wpa3_rsne().rsn_capabilities.unwrap().peerkey_enabled());
        let rsne_with_caps =
            Rsne::wpa2_wpa3_rsne_with_extra_caps(RsnCapabilities(0).with_peerkey_enabled(true));
        assert!(rsne_with_caps.rsn_capabilities.as_ref().unwrap().peerkey_enabled());
        assert!(rsne_with_caps.rsn_capabilities.as_ref().unwrap().mgmt_frame_protection_cap());

        assert!(!Rsne::wpa3_rsne().rsn_capabilities.unwrap().peerkey_enabled());
        let rsne_with_caps =
            Rsne::wpa3_rsne_with_extra_caps(RsnCapabilities(0).with_peerkey_enabled(true));
        assert!(rsne_with_caps.rsn_capabilities.as_ref().unwrap().peerkey_enabled());
        assert!(
            rsne_with_caps.rsn_capabilities.as_ref().unwrap().mgmt_frame_protection_cap()
                && rsne_with_caps.rsn_capabilities.as_ref().unwrap().mgmt_frame_protection_req()
        );
    }

    #[test]
    fn test_incompatible_group_data_cipher() {
        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_GCMP_256),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), false);
    }

    #[test]
    fn test_no_group_data_cipher() {
        let rsne = Rsne {
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), false);

        let rsne = Rsne {
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_SAE],
            ..Default::default()
        };
        let mut security_support = fake_security_support_empty();
        security_support.mfp.supported = true;
        assert_eq!(rsne.is_wpa3_rsn_compatible(&security_support), false);
    }

    #[test]
    fn test_rsne_unsupported_group_data_cipher() {
        let s_rsne = Rsne::wpa2_rsne();
        let mut a_rsne = Rsne::wpa2_rsne();
        a_rsne.group_data_cipher_suite = Some(CIPHER_GCMP_256);
        assert!(!s_rsne.is_valid_subset_of(&a_rsne).expect("expect Ok result"));
    }

    #[test]
    fn test_ccmp_128_group_data_cipher_ccmp_128_pairwise_cipher() {
        let a_rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&fake_security_support_empty())
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes = vec![
            0x30, // element id, 48 expressed as hexadecimal value
            0x12, // length in octets, 18 expressed as hexadecimal value
            0x01, 0x00, // Version 1
            0x00, 0x0F, 0xac, 0x04, // CCMP-128 as group data cipher suite
            0x01, 0x00, // pairwise cipher suite count
            0x00, 0x0F, 0xAC, 0x04, // CCMP-128 as pairwise cipher suite
            0x01, 0x00, // authentication count
            0x00, 0x0F, 0xAC, 0x02, // PSK authentication
        ];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_tkip_group_data_cipher_ccmp_128_pairwise_cipher() {
        let a_rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_TKIP),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&fake_security_support_empty())
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes = vec![
            0x30, // element id, 48 expressed as hexadecimal value
            0x12, // length in octets, 18 expressed as hexadecimal value
            0x01, 0x00, // Version 1
            0x00, 0x0F, 0xac, 0x02, // TKIP as group data cipher suite
            0x01, 0x00, // pairwise cipher suite count
            0x00, 0x0F, 0xAC, 0x04, // CCMP-128 as pairwise cipher suite
            0x01, 0x00, // authentication count
            0x00, 0x0F, 0xAC, 0x02, // PSK authentication
        ];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_tkip_group_data_cipher_tkip_pairwise_cipher() {
        let a_rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_TKIP),
            pairwise_cipher_suites: vec![CIPHER_TKIP],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&fake_security_support_empty())
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes = vec![
            0x30, // element id, 48 expressed as hexadecimal value
            0x12, // length in octets, 18 expressed as hexadecimal value
            0x01, 0x00, // Version 1
            0x00, 0x0F, 0xac, 0x02, // TKIP as group data cipher suite
            0x01, 0x00, // pairwise cipher suite count
            0x00, 0x0F, 0xAC, 0x02, // TKIP as pairwise cipher suite
            0x01, 0x00, // authentication count
            0x00, 0x0F, 0xAC, 0x02, // PSK authentication
        ];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_tkip_group_data_cipher_prefer_ccmp_128_pairwise_cipher() {
        let a_rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_TKIP),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128, CIPHER_TKIP],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&fake_security_support_empty())
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes = vec![
            0x30, // element id, 48 expressed as hexadecimal value
            0x12, // length in octets, 18 expressed as hexadecimal value
            0x01, 0x00, // Version 1
            0x00, 0x0F, 0xac, 0x02, // TKIP as group data cipher suite
            0x01, 0x00, // pairwise cipher suite count
            0x00, 0x0F, 0xAC, 0x04, // CCMP-128 as pairwise cipher suite
            0x01, 0x00, // authentication count
            0x00, 0x0F, 0xAC, 0x02, // PSK authentication
        ];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_ccmp_128_group_data_cipher_prefer_ccmp_128_pairwise_cipher() {
        let a_rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128, CIPHER_TKIP],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&fake_security_support_empty())
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes = vec![
            0x30, // element id, 48 expressed as hexadecimal value
            0x12, // length in octets, 18 expressed as hexadecimal value
            0x01, 0x00, // Version 1
            0x00, 0x0F, 0xac, 0x04, // CCMP-128 as group data cipher suite
            0x01, 0x00, // pairwise cipher suite count
            0x00, 0x0F, 0xAC, 0x04, // CCMP-128 as pairwise cipher suite
            0x01, 0x00, // authentication count
            0x00, 0x0F, 0xAC, 0x02, // PSK authentication
        ];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_compatible_pairwise_cipher() {
        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()));

        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_TKIP],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()));
    }

    #[test]
    fn test_incompatible_pairwise_cipher() {
        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_BIP_CMAC_256],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), false);
    }

    #[test]
    fn test_no_pairwise_cipher() {
        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), false);

        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            akm_suites: vec![AKM_SAE],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY), false);
    }

    #[test]
    fn test_rsne_unsupported_pairwise_cipher() {
        let s_rsne = Rsne::wpa2_rsne();
        let mut a_rsne = Rsne::wpa2_rsne();
        a_rsne.pairwise_cipher_suites = vec![CIPHER_BIP_CMAC_256];
        assert!(!s_rsne.is_valid_subset_of(&a_rsne).expect("expect Ok result"));
    }

    #[test]
    fn test_incompatible_akm() {
        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_EAP],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), false);
        assert_eq!(rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY), false);

        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY), false);

        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_SAE],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), false);
    }

    #[test]
    fn test_no_akm() {
        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            ..Default::default()
        };
        assert_eq!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), false);
        assert_eq!(rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY), false);
    }

    #[test]
    fn test_rsne_unsupported_akm() {
        let s_rsne = Rsne::wpa2_rsne();
        let mut a_rsne = Rsne::wpa2_rsne();
        a_rsne.akm_suites = vec![AKM_EAP];
        assert!(!s_rsne.is_valid_subset_of(&a_rsne).expect("expect Ok result"));
    }

    #[test]
    fn test_ensure_valid_s_rsne() {
        let s_rsne = Rsne::wpa2_rsne();
        let result = s_rsne.ensure_valid_s_rsne();
        assert!(result.is_ok());

        let mut s_rsne = Rsne::wpa2_rsne();
        s_rsne.group_data_cipher_suite = None;
        let result = s_rsne.ensure_valid_s_rsne();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::NoGroupDataCipherSuite);

        let mut s_rsne = Rsne::wpa2_rsne();
        s_rsne.pairwise_cipher_suites = vec![];
        let result = s_rsne.ensure_valid_s_rsne();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::NoPairwiseCipherSuite);

        let mut s_rsne = Rsne::wpa2_rsne();
        s_rsne.pairwise_cipher_suites.push(CIPHER_GCMP_256);
        let result = s_rsne.ensure_valid_s_rsne();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::TooManyPairwiseCipherSuites);

        let mut s_rsne = Rsne::wpa2_rsne();
        s_rsne.akm_suites = vec![];
        let result = s_rsne.ensure_valid_s_rsne();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::NoAkmSuite);

        let mut s_rsne = Rsne::wpa2_rsne();
        s_rsne.akm_suites.push(AKM_EAP);
        let result = s_rsne.ensure_valid_s_rsne();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::TooManyAkmSuites);

        let mut s_rsne = Rsne::wpa2_rsne();
        s_rsne.akm_suites = vec![akm::Akm::new_dot11(200)];
        let result = s_rsne.ensure_valid_s_rsne();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::NoAkmMicBytes);
    }

    #[test]
    fn test_compatible_wpa2_rsne() {
        let rsne = Rsne::wpa2_rsne();
        assert!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()));
    }

    #[test]
    fn test_compatible_wpa2_wpa3_rsne() {
        let rsne = Rsne::wpa2_wpa3_rsne();
        assert!(rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()));
        assert!(rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY));
    }

    #[test]
    fn test_compatible_wpa3_rsne() {
        let rsne = Rsne::wpa3_rsne();
        assert!(rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY));
    }

    #[test]
    fn test_incompatible_wpa3_rsne_no_mfp() {
        let rsne = Rsne::wpa3_rsne();
        assert!(!rsne.is_wpa3_rsn_compatible(&fake_security_support_empty()));
    }

    #[test]
    fn test_ccmp128_group_data_pairwise_cipher_psk() {
        let a_rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&fake_security_support_empty())
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes =
            vec![48, 18, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 2];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_valid_rsne() {
        let s_rsne = Rsne::wpa2_rsne();
        let a_rsne = Rsne::wpa2_rsne();
        assert!(s_rsne.is_valid_subset_of(&a_rsne).expect("expect Ok result"));
    }

    #[test]
    fn test_ccmp_tkip_mode() {
        let a_rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128, CIPHER_TKIP],
            akm_suites: vec![AKM_PSK, AKM_FT_PSK],
            ..Default::default()
        };
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&fake_security_support_empty())
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes =
            vec![48, 18, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 2];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_ccmp128_group_data_pairwise_cipher_sae() {
        let a_rsne = Rsne::wpa3_rsne();
        assert_eq!(a_rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY), true);

        let s_rsne = a_rsne
            .derive_wpa3_s_rsne(&MFP_SUPPORT_ONLY)
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes =
            vec![48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 8, 192, 0];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_wpa3_transition_mode() {
        let a_rsne = Rsne::wpa2_wpa3_rsne();
        assert_eq!(a_rsne.is_wpa2_rsn_compatible(&fake_security_support_empty()), true);
        assert_eq!(a_rsne.is_wpa3_rsn_compatible(&fake_security_support_empty()), false);
        assert_eq!(a_rsne.is_wpa3_rsn_compatible(&MFP_SUPPORT_ONLY), true);

        let s_rsne = a_rsne
            .derive_wpa2_s_rsne(&MFP_SUPPORT_ONLY)
            .expect("could not derive WPA2 Supplicant RSNE");
        let expected_rsne_bytes =
            vec![48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 2, 192, 0];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);

        let s_rsne = a_rsne
            .derive_wpa3_s_rsne(&MFP_SUPPORT_ONLY)
            .expect("could not derive WPA3 Supplicant RSNE");
        let expected_rsne_bytes =
            vec![48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 8, 192, 0];
        assert_eq!(s_rsne.into_bytes(), expected_rsne_bytes);
    }

    #[test]
    fn test_wpa2_psk_rsne_bytes() {
        // Compliant with IEEE Std 802.11-2016, 9.4.2.25.
        let expected: Vec<u8> = vec![
            0x30, 0x14, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04,
            0x01, 0x00, 0x00, 0x0f, 0xac, 0x02, 0x00, 0x00,
        ];
        let rsne = Rsne::wpa2_rsne_with_caps(RsnCapabilities(0));
        let mut actual = Vec::with_capacity(rsne.len());
        rsne.write_into(&mut actual).expect("error writing RSNE");

        assert_eq!(&expected[..], &actual[..]);
    }

    #[test]
    fn test_supplicant_missing_required_mpfc() {
        let s_rsne = Rsne::wpa2_rsne();
        let a_rsne = Rsne::wpa2_rsne_with_caps(
            RsnCapabilities(0)
                .with_mgmt_frame_protection_req(true)
                .with_mgmt_frame_protection_cap(true),
        );
        assert!(!s_rsne.is_valid_subset_of(&a_rsne).expect("expect Ok result"));
    }

    #[test]
    fn test_authenticator_missing_required_mpfc() {
        let s_rsne = Rsne::wpa2_rsne_with_caps(
            RsnCapabilities(0)
                .with_mgmt_frame_protection_req(true)
                .with_mgmt_frame_protection_cap(true),
        );
        let a_rsne = Rsne::wpa2_rsne();
        assert!(!s_rsne.is_valid_subset_of(&a_rsne).expect("expect Ok result"));
    }

    #[test]
    fn test_supplicant_has_invalid_mgmt_frame_protection_fields() {
        let s_rsne = Rsne::wpa2_rsne_with_caps(
            RsnCapabilities(0)
                .with_mgmt_frame_protection_req(true)
                .with_mgmt_frame_protection_cap(false),
        );
        // AP only cares about client's invalid setting if AP is mgmt frame protection capable
        let a_rsne =
            Rsne::wpa2_rsne_with_caps(RsnCapabilities(0).with_mgmt_frame_protection_cap(true));

        let result = s_rsne.is_valid_subset_of(&a_rsne);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::InvalidSupplicantMgmtFrameProtection);
    }

    #[test]
    fn test_authenticator_has_invalid_mgmt_frame_protection_fields() {
        // client only cares about AP's invalid setting if client is mgmt frame protection capable
        let s_rsne =
            Rsne::wpa2_rsne_with_caps(RsnCapabilities(0).with_mgmt_frame_protection_cap(true));
        let a_rsne = Rsne::wpa2_rsne_with_caps(
            RsnCapabilities(0)
                .with_mgmt_frame_protection_req(true)
                .with_mgmt_frame_protection_cap(false),
        );

        let result = s_rsne.is_valid_subset_of(&a_rsne);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::InvalidAuthenticatorMgmtFrameProtection);
    }

    #[test]
    fn test_write_until_version() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            0x02, // length
            0x01, 0x00, // version
        ];
        let rsne = Rsne { version: VERSION, ..Default::default() };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }

    #[test]
    fn test_write_until_group_data() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            0x06, // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac, 0x04, // group data cipher suite
        ];
        let rsne = Rsne {
            version: VERSION,
            group_data_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::CCMP_128)),
            ..Default::default()
        };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }

    #[test]
    fn test_write_until_pairwise() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            12,   // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac, 0x04, // group data cipher suite
            0x01, 0x00, // pairwise suite count
            0x00, 0x0f, 0xac, 0x04, // pairwise cipher suite
        ];
        let rsne = Rsne {
            version: VERSION,
            group_data_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::CCMP_128)),
            pairwise_cipher_suites: vec![cipher::Cipher::new_dot11(cipher::CCMP_128)],
            ..Default::default()
        };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }

    #[test]
    fn test_write_until_akm() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            14,   // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac, 0x04, // group data cipher suite
            0x00, 0x00, // pairwise suite count
            0x01, 0x00, // pairwise suite count
            0x00, 0x0f, 0xac, 0x02, // pairwise cipher suite
        ];
        let rsne = Rsne {
            version: VERSION,
            group_data_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::CCMP_128)),
            akm_suites: vec![akm::Akm::new_dot11(akm::PSK)],
            ..Default::default()
        };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }

    #[test]
    fn test_write_until_rsn_capabilities() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            12,   // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac, 0x04, // group data cipher suite
            0x00, 0x00, // pairwise suite count
            0x00, 0x00, // akm suite count
            0xcd, 0xab, // rsn capabilities
        ];
        let rsne = Rsne {
            version: VERSION,
            group_data_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::CCMP_128)),
            rsn_capabilities: Some(RsnCapabilities(0xabcd)),
            ..Default::default()
        };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }

    static PMKID_VAL: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

    #[test]
    fn test_write_until_pmkids() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            30,   // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac, 0x04, // group data cipher suite
            0x00, 0x00, // pairwise suite count
            0x00, 0x00, // akm suite count
            0xcd, 0xab, // rsn capabilities
            0x01, 0x00, // pmkid count
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, // pmkid
        ];
        let rsne = Rsne {
            version: VERSION,
            group_data_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::CCMP_128)),
            rsn_capabilities: Some(RsnCapabilities(0xabcd)),
            pmkids: vec![Bytes::from_static(&PMKID_VAL[..])],
            ..Default::default()
        };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }

    #[test]
    fn test_write_until_group_mgmt() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            18,   // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac, 0x04, // group data cipher suite
            0x00, 0x00, // pairwise suite count
            0x00, 0x00, // akm suite count
            0xcd, 0xab, // rsn capabilities
            0x00, 0x00, // pmkids count
            0x00, 0x0f, 0xac, 0x06, // group management cipher suite
        ];
        let rsne = Rsne {
            version: VERSION,
            group_data_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::CCMP_128)),
            rsn_capabilities: Some(RsnCapabilities(0xabcd)),
            group_mgmt_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::BIP_CMAC_128)),
            ..Default::default()
        };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }

    #[test]
    fn test_end_write_on_missing_caps() {
        let expected_frame: Vec<u8> = vec![
            0x30, // element id
            0x06, // length
            0x01, 0x00, // version
            0x00, 0x0f, 0xac,
            0x04, // group data cipher suite
                  // We don't write group management suite because caps are missing.
        ];
        let rsne = Rsne {
            version: VERSION,
            group_data_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::CCMP_128)),
            rsn_capabilities: None,
            group_mgmt_cipher_suite: Some(cipher::Cipher::new_dot11(cipher::BIP_CMAC_128)),
            ..Default::default()
        };
        let mut buf = vec![];
        rsne.write_into(&mut buf).expect("Failed to write rsne");
        assert_eq!(&buf[..], &expected_frame[..]);
    }
}
