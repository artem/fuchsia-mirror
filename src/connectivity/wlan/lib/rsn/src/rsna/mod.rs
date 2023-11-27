// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::integrity::{self, integrity_algorithm};
use crate::key::exchange::Key;
use crate::keywrap::{self, keywrap_algorithm};

use crate::ProtectionInfo;
use crate::{rsn_ensure, Error};
use anyhow::{anyhow, ensure};
use eapol;
use fidl_fuchsia_wlan_mlme::SaeFrame;
use wlan_common::ie::rsn::{
    akm::Akm,
    cipher::{Cipher, CIPHER_BIP_CMAC_128, GROUP_CIPHER_SUITE, TKIP},
    rsne::{RsnCapabilities, Rsne},
};
use wlan_common::ie::wpa::WpaIe;
use zerocopy::ByteSlice;

pub mod esssa;
#[cfg(test)]
pub mod test_util;

#[derive(Debug, Clone, PartialEq)]
pub enum ProtectionType {
    LegacyWpa1,
    Rsne,
}

#[derive(Debug)]
pub enum IgtkSupport {
    Unsupported,
    Capable,
    Required,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NegotiatedProtection {
    pub group_data: Cipher,
    pub pairwise: Cipher,
    pub group_mgmt: Option<Cipher>,
    pub akm: Akm,
    pub mic_size: u16,
    pub protection_type: ProtectionType,
    // Some networks carry RSN capabilities.
    // To construct a valid RSNE, these capabilities must be tracked.
    caps: Option<RsnCapabilities>,
}

impl NegotiatedProtection {
    pub fn from_protection(protection: &ProtectionInfo) -> Result<Self, anyhow::Error> {
        match protection {
            ProtectionInfo::Rsne(rsne) => Self::from_rsne(rsne),
            ProtectionInfo::LegacyWpa(wpa) => Self::from_legacy_wpa(wpa),
        }
    }

    fn key_descriptor_version(&self) -> u16 {
        let key_descriptor_type = match self.protection_type {
            ProtectionType::LegacyWpa1 => eapol::KeyDescriptor::LEGACY_WPA1,
            ProtectionType::Rsne => eapol::KeyDescriptor::IEEE802DOT11,
        };
        derive_key_descriptor_version(key_descriptor_type, self)
    }

    pub fn integrity_algorithm(&self) -> Result<Box<dyn integrity::Algorithm>, Error> {
        integrity_algorithm(self.key_descriptor_version(), &self.akm)
            .ok_or(Error::UnknownIntegrityAlgorithm)
    }

    pub fn keywrap_algorithm(&self) -> Result<Box<dyn keywrap::Algorithm>, Error> {
        keywrap_algorithm(self.key_descriptor_version(), &self.akm)
            .ok_or(Error::UnknownKeywrapAlgorithm)
    }

    /// Validates this RSNE contains only one of each cipher type and only one AKM with
    /// a defined number of MIC bytes, and produces a corresponding negotiated protection scheme.
    pub fn from_rsne(rsne: &Rsne) -> Result<Self, anyhow::Error> {
        rsne.ensure_valid_s_rsne()
            .map_err(|e| anyhow!(e).context(Error::InvalidNegotiatedProtection))?;

        // The following assignments will all succeed because ensure_valid_s_rsne() did
        // not return a Result::Err.
        let group_data = rsne.group_data_cipher_suite.as_ref().unwrap();
        let pairwise = &rsne.pairwise_cipher_suites[0];
        let akm = &rsne.akm_suites[0];
        let mic_size = akm.mic_bytes();
        let mic_size = mic_size.unwrap();

        Ok(Self {
            group_data: group_data.clone(),
            pairwise: pairwise.clone(),
            group_mgmt: rsne.group_mgmt_cipher_suite.clone(),
            akm: akm.clone(),
            mic_size,
            protection_type: ProtectionType::Rsne,
            caps: rsne.rsn_capabilities.clone(),
        })
    }

    /// Validates that this WPA1 element contains only one of each cipher type and one AKM, and
    /// produces a corresponding negotiated protection scheme.
    pub fn from_legacy_wpa(wpa: &WpaIe) -> Result<Self, anyhow::Error> {
        ensure!(wpa.unicast_cipher_list.len() == 1, Error::InvalidNegotiatedProtection);
        ensure!(wpa.akm_list.len() == 1, Error::InvalidNegotiatedProtection);
        let akm = wpa.akm_list[0].clone();
        let mic_size = akm.mic_bytes().ok_or(Error::InvalidNegotiatedProtection)?;
        let group_data = wpa.multicast_cipher.clone();
        let pairwise = wpa.unicast_cipher_list[0].clone();
        Ok(Self {
            group_data,
            pairwise,
            group_mgmt: None,
            akm,
            mic_size,
            protection_type: ProtectionType::LegacyWpa1,
            caps: None,
        })
    }

    /// Converts this NegotiatedProtection into a ProtectionInfo that may be written into 802.11
    /// frames.
    pub fn to_full_protection(&self) -> ProtectionInfo {
        match self.protection_type {
            ProtectionType::Rsne => ProtectionInfo::Rsne(Rsne {
                group_data_cipher_suite: Some(self.group_data.clone()),
                pairwise_cipher_suites: vec![self.pairwise.clone()],
                group_mgmt_cipher_suite: self.group_mgmt.clone(),
                akm_suites: vec![self.akm.clone()],
                rsn_capabilities: self.caps.clone(),
                ..Default::default()
            }),
            ProtectionType::LegacyWpa1 => ProtectionInfo::LegacyWpa(WpaIe {
                multicast_cipher: self.group_data.clone(),
                unicast_cipher_list: vec![self.pairwise.clone()],
                akm_list: vec![self.akm.clone()],
            }),
        }
    }

    pub fn igtk_support(&self) -> IgtkSupport {
        match &self.caps {
            Some(caps) => {
                if caps.mgmt_frame_protection_req() {
                    IgtkSupport::Required
                } else if caps.mgmt_frame_protection_cap() {
                    IgtkSupport::Capable
                } else {
                    IgtkSupport::Unsupported
                }
            }
            None => IgtkSupport::Unsupported,
        }
    }

    pub fn group_mgmt_cipher(&self) -> Cipher {
        // IEEE Std. 802.11-2016 9.4.2.25.2: BIP_CMAC_128 is the default if not specified.
        self.group_mgmt.clone().unwrap_or(CIPHER_BIP_CMAC_128)
    }
}

/// Wraps an EAPOL key frame to enforces successful decryption before the frame can be used.
pub struct EncryptedKeyData<B: ByteSlice>(eapol::KeyFrameRx<B>);

impl<B: ByteSlice> EncryptedKeyData<B> {
    /// Yields a tuple of the captured EAPOL Key frame and its decrypted key data if encryption
    /// was successful. Otherwise, an Error is returned.
    pub fn decrypt(
        self,
        kek: &[u8],
        protection: &NegotiatedProtection,
    ) -> Result<(eapol::KeyFrameRx<B>, Vec<u8>), Error> {
        let key_data = protection.keywrap_algorithm()?.unwrap_key(
            kek,
            &self.0.key_frame_fields.key_iv,
            &self.0.key_data[..],
        )?;
        Ok((self.0, key_data))
    }
}

/// Wraps an EAPOL key frame to enforce successful MIC verification before the frame can be used.
#[derive(Debug)]
pub struct WithUnverifiedMic<B: ByteSlice>(eapol::KeyFrameRx<B>);

impl<B: ByteSlice> WithUnverifiedMic<B> {
    /// Yields the captured EAPOL Key frame if the MIC was successfully verified.
    /// The Key frame is wrapped to enforce decryption of potentially encrypted key data.
    /// Returns an Error if the MIC is invalid.
    pub fn verify_mic(
        self,
        kck: &[u8],
        protection: &NegotiatedProtection,
    ) -> Result<UnverifiedKeyData<B>, Error> {
        // IEEE Std 802.11-2016, 12.7.2 h)
        // IEEE Std 802.11-2016, 12.7.2 b.6)
        let mic_bytes = protection.akm.mic_bytes().ok_or(Error::UnsupportedAkmSuite)?;
        rsn_ensure!(self.0.key_mic.len() == mic_bytes as usize, Error::InvalidMicSize);

        // If a MIC is set but the PTK was not yet derived, the MIC cannot be verified.
        let mut buf = vec![];
        self.0.write_into(true, &mut buf)?;
        let valid_mic =
            protection.integrity_algorithm()?.verify(kck, &buf[..], &self.0.key_mic[..]);
        rsn_ensure!(valid_mic, Error::InvalidMic);

        if self.0.key_frame_fields.key_info().encrypted_key_data() {
            Ok(UnverifiedKeyData::Encrypted(EncryptedKeyData(self.0)))
        } else {
            Ok(UnverifiedKeyData::NotEncrypted(self.0))
        }
    }
}

/// Carries an EAPOL Key frame and requires MIC verification if the MIC bit of the frame's info
/// field is set.
pub enum UnverifiedKeyData<B: ByteSlice> {
    Encrypted(EncryptedKeyData<B>),
    NotEncrypted(eapol::KeyFrameRx<B>),
}

/// EAPOL Key frames carried in this struct comply with IEEE Std 802.11-2016, 12.7.2.
/// Neither the Key Frame's MIC nor its key data were verified at this point.
#[derive(Debug)]
pub enum Dot11VerifiedKeyFrame<B: ByteSlice> {
    WithUnverifiedMic(WithUnverifiedMic<B>),
    WithoutMic(eapol::KeyFrameRx<B>),
}

impl<B: ByteSlice> Dot11VerifiedKeyFrame<B> {
    // [`key_replay_counter`] is the current value of the Key Replay Counter[1] in either the
    // Supplicant or Authenticator. The Supplicant initializes its Key Replay Counter to 0 and
    // updates the counter to the counter value contained in each valid message from the
    // Authenticator. The Authenticator initializes its Key Replay Counter to 0 and updates
    // the counter to the counter value contained in each message the Authenticator sends.
    //
    // [1]: IEEE 802.11-2016 12.7.2 EAPOL-Key frames
    pub fn from_frame(
        frame: eapol::KeyFrameRx<B>,
        role: &Role,
        protection: &NegotiatedProtection,
        key_replay_counter: u64,
    ) -> Result<Dot11VerifiedKeyFrame<B>, Error> {
        let sender = match role {
            Role::Supplicant => Role::Authenticator,
            Role::Authenticator => Role::Supplicant,
        };

        // IEEE Std 802.11-2016, 12.7.2 a)
        // IEEE Std 802.1X-2010, 11.9
        let key_descriptor = match frame.key_frame_fields.descriptor_type {
            eapol::KeyDescriptor::IEEE802DOT11 => eapol::KeyDescriptor::IEEE802DOT11,
            eapol::KeyDescriptor::LEGACY_WPA1
                if protection.protection_type == ProtectionType::LegacyWpa1 =>
            {
                eapol::KeyDescriptor::LEGACY_WPA1
            }
            eapol::KeyDescriptor::RC4 => {
                return Err(Error::InvalidKeyDescriptor(
                    frame.key_frame_fields.descriptor_type,
                    eapol::KeyDescriptor::IEEE802DOT11,
                )
                .into())
            }
            // Invalid value.
            _ => {
                return Err(
                    Error::UnsupportedKeyDescriptor(frame.key_frame_fields.descriptor_type).into()
                )
            }
        };

        // IEEE Std 802.11-2016, 12.7.2 b.1)
        let frame_key_descriptor_version =
            frame.key_frame_fields.key_info().key_descriptor_version();
        let expected_version = derive_key_descriptor_version(key_descriptor, protection);
        rsn_ensure!(
            frame_key_descriptor_version == expected_version,
            Error::UnsupportedKeyDescriptorVersion(frame_key_descriptor_version)
        );

        // IEEE Std 802.11-2016, 12.7.2 b.2)
        // IEEE Std 802.11-2016, 12.7.2 b.4)
        match frame.key_frame_fields.key_info().key_type() {
            eapol::KeyType::PAIRWISE => {}
            eapol::KeyType::GROUP_SMK => {
                // IEEE Std 802.11-2016, 12.7.2 b.4 ii)
                rsn_ensure!(
                    !frame.key_frame_fields.key_info().install(),
                    Error::InvalidInstallBitGroupSmkHandshake
                );
            }
        };

        // IEEE Std 802.11-2016, 12.7.2 b.5)
        if let Role::Supplicant = sender {
            rsn_ensure!(
                !frame.key_frame_fields.key_info().key_ack(),
                Error::InvalidKeyAckBitSupplicant
            );
        }

        // IEEE Std 802.11-2016, 12.7.2 b.6)
        // IEEE Std 802.11-2016, 12.7.2 b.7)
        // MIC and Secure bit depend on specific key-exchange methods and can not be verified now.
        // More specifically, there are frames which can carry a MIC or secure bit but are required
        // to compute the PTK and/or GTK and thus cannot be verified up-front.

        // IEEE Std 802.11-2016, 12.7.2 b.8)
        if let Role::Authenticator = sender {
            rsn_ensure!(
                !frame.key_frame_fields.key_info().error(),
                Error::InvalidErrorBitAuthenticator
            );
        }

        // IEEE Std 802.11-2016, 12.7.2 b.9)
        if let Role::Authenticator = sender {
            rsn_ensure!(
                !frame.key_frame_fields.key_info().request(),
                Error::InvalidRequestBitAuthenticator
            );
        }

        // IEEE Std 802.11-2016, 12.7.2 b.10)
        // Encrypted key data is validated at the end once all other validations succeeded.

        // IEEE Std 802.11-2016, 12.7.2 b.11)
        rsn_ensure!(
            !frame.key_frame_fields.key_info().smk_message(),
            Error::SmkHandshakeNotSupported
        );

        // IEEE Std 802.11-2016, 12.7.2 c)
        match frame.key_frame_fields.key_info().key_type() {
            eapol::KeyType::PAIRWISE => match sender {
                // IEEE is somewhat vague on what is expected from the frame's key_len field.
                // IEEE Std 802.11-2016, 12.7.2 c) requires the key_len to match the PTK's
                // length, while all handshakes defined in IEEE such as
                // 4-Way Handshake (12.7.6.3) and Group Key Handshake (12.7.7.3) explicitly require
                // a value of 0 for frames sent by the Supplicant.
                // Not all vendors follow the latter requirement, such as Apple with iOS.
                // To improve interoperability, a value of 0 or the pairwise temporal key length is
                // allowed for frames sent by the Supplicant.
                Role::Supplicant if frame.key_frame_fields.key_len.to_native() != 0 => {
                    let tk_len =
                        protection.pairwise.tk_bytes().ok_or(Error::UnsupportedCipherSuite)?;
                    rsn_ensure!(
                        frame.key_frame_fields.key_len.to_native() == tk_len.into(),
                        Error::InvalidKeyLength(
                            frame.key_frame_fields.key_len.to_native().into(),
                            tk_len.into()
                        )
                    );
                }
                // Authenticator must use the pairwise cipher's key length.
                Role::Authenticator => {
                    let tk_len: usize =
                        protection.pairwise.tk_bytes().ok_or(Error::UnsupportedCipherSuite)?.into();
                    rsn_ensure!(
                        usize::from(frame.key_frame_fields.key_len.to_native()) == tk_len,
                        Error::InvalidKeyLength(
                            frame.key_frame_fields.key_len.to_native().into(),
                            tk_len
                        )
                    );
                }
                _ => {}
            },
            // IEEE Std 802.11-2016, 12.7.2 c) does not specify the expected value for frames
            // involved in exchanging the GTK. Thus, leave validation and enforcement of this
            // requirement to the selected key exchange method.
            eapol::KeyType::GROUP_SMK => {}
        };

        if key_replay_counter > 0 {
            match sender {
                // Supplicant responds to messages from the Authenticator with the same
                // key replay counter.
                Role::Supplicant => {
                    rsn_ensure!(
                        frame.key_frame_fields.key_replay_counter.to_native() >= key_replay_counter,
                        Error::InvalidKeyReplayCounter(
                            frame.key_frame_fields.key_replay_counter.to_native(),
                            key_replay_counter
                        )
                    );
                }
                // Authenticator must send messages with a strictly larger key replay counter.
                //
                // TODO(b/310698434): This logic only runs upon receipt of messages. It seems
                // that an Authenticator should actually verify the current Key Replay Counter
                // is equal to the Key Replay Counter value received.
                Role::Authenticator => {
                    rsn_ensure!(
                        frame.key_frame_fields.key_replay_counter.to_native() > key_replay_counter,
                        Error::InvalidKeyReplayCounter(
                            frame.key_frame_fields.key_replay_counter.to_native(),
                            key_replay_counter
                        )
                    );
                }
            }
        }

        // IEEE Std 802.11-2016, 12.7.2
        // Encrypted Key Data bit requires MIC bit to be set for all 802.11 handshakes.
        if frame.key_frame_fields.key_info().encrypted_key_data() {
            rsn_ensure!(
                frame.key_frame_fields.key_info().key_mic(),
                Error::InvalidMicBitForEncryptedKeyData
            );
        }

        // IEEE Std 802.11-2016, 12.7.2, e)
        // Validation is specific for the selected key exchange method.

        // IEEE Std 802.11-2016, 12.7.2, f)
        // Validation is specific for the selected key exchange method.

        // IEEE Std 802.11-2016, 12.7.2, g)
        // Validation is specific for the selected key exchange method.

        // IEEE Std 802.11-2016, 12.7.2 h)
        // IEEE Std 802.11-2016, 12.7.2 b.6)
        // See explanation for IEEE Std 802.11-2016, 12.7.2 b.7) why the MIC cannot be verified
        // here.

        // IEEE Std 802.11-2016, 12.7.2 i) & j)
        // IEEE Std 802.11-2016, 12.7.2 b.10)
        // Validation is enforced by KeyFrame parser.

        if frame.key_frame_fields.key_info().key_mic() {
            Ok(Dot11VerifiedKeyFrame::WithUnverifiedMic(WithUnverifiedMic(frame)))
        } else {
            Ok(Dot11VerifiedKeyFrame::WithoutMic(frame))
        }
    }

    /// CAUTION: Returns the underlying frame without verifying its MIC or encrypted key data if
    /// either one is present.
    /// Only use this if you know what you are doing.
    pub fn unsafe_get_raw(&self) -> &eapol::KeyFrameRx<B> {
        match self {
            Dot11VerifiedKeyFrame::WithUnverifiedMic(WithUnverifiedMic(frame)) => frame,
            Dot11VerifiedKeyFrame::WithoutMic(frame) => frame,
        }
    }
}

/// IEEE Std 802.11-2016, 12.7.2 b.1)
/// Key Descriptor Version is based on the negotiated AKM, Pairwise- and Group Cipher suite.
pub fn derive_key_descriptor_version(
    key_descriptor_type: eapol::KeyDescriptor,
    protection: &NegotiatedProtection,
) -> u16 {
    let akm = &protection.akm;
    let pairwise = &protection.pairwise;

    if !akm.has_known_algorithm() || !pairwise.has_known_usage() {
        return 0;
    }

    match akm.suite_type {
        1 | 2 => match key_descriptor_type {
            eapol::KeyDescriptor::RC4 => match pairwise.suite_type {
                TKIP | GROUP_CIPHER_SUITE => 1,
                _ => 0,
            },
            eapol::KeyDescriptor::IEEE802DOT11 | eapol::KeyDescriptor::LEGACY_WPA1 => {
                if pairwise.suite_type == TKIP || pairwise.suite_type == GROUP_CIPHER_SUITE {
                    1
                } else if pairwise.is_enhanced() || protection.group_data.is_enhanced() {
                    2
                } else {
                    0
                }
            }
            _ => 0,
        },
        // Interestingly, IEEE 802.11 does not specify any pairwise- or group cipher
        // requirements for these AKMs.
        3..=6 => 3,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role {
    Authenticator,
    Supplicant,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum SecAssocStatus {
    WrongPassword,
    PmkSaEstablished,
    EssSaEstablished,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum AuthRejectedReason {
    /// Unable to generate a PMK with the peer.
    AuthFailed,
    /// The peer never responded or sent too many invalid responses.
    TooManyRetries,
    /// Association took too long, and the PMKSA has expired.
    PmksaExpired,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum AuthStatus {
    Success,
    Rejected(AuthRejectedReason),
    InternalError,
}

#[derive(Debug, PartialEq, Clone)]
pub enum SecAssocUpdate {
    TxEapolKeyFrame {
        frame: eapol::KeyFrameBuf,
        // Indicates whether we expect that our peer in the EAPOL exchange will send us a
        // response to this frame. If so, we must also schedule a timeout for the response.
        expect_response: bool,
    },
    Key(Key),
    Status(SecAssocStatus),
    // These values are used to handle SAE exchanges.
    TxSaeFrame(SaeFrame),
    SaeAuthStatus(AuthStatus),
    ScheduleSaeTimeout(u64),
}

pub type UpdateSink = Vec<SecAssocUpdate>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsna::{test_util, NegotiatedProtection, Role};
    use wlan_common::{
        assert_variant,
        ie::rsn::{
            akm::{self, AKM_PSK},
            cipher::{self, CIPHER_CCMP_128, CIPHER_GCMP_256},
            fake_wpa2_s_rsne,
            rsne::Rsne,
        },
    };

    #[test]
    fn test_negotiated_protection_from_rsne() {
        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_GCMP_256),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        NegotiatedProtection::from_rsne(&rsne).expect("error, could not create negotiated RSNE");

        let rsne = Rsne::wpa3_rsne();
        NegotiatedProtection::from_rsne(&rsne).expect("error, could not create negotiated RSNE");

        let rsne = Rsne {
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        NegotiatedProtection::from_rsne(&rsne).expect_err("error, created negotiated RSNE");

        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            akm_suites: vec![AKM_PSK],
            ..Default::default()
        };
        NegotiatedProtection::from_rsne(&rsne).expect_err("error, created negotiated RSNE");

        let rsne = Rsne {
            group_data_cipher_suite: Some(CIPHER_CCMP_128),
            pairwise_cipher_suites: vec![CIPHER_CCMP_128],
            ..Default::default()
        };
        NegotiatedProtection::from_rsne(&rsne).expect_err("error, created negotiated RSNE");
    }

    // IEEE requires the key length to be zeroed in the 4-Way Handshake but some vendors send the
    // pairwise cipher's key length instead. The requirement was relaxed to improve
    // interoperability,
    #[test]
    fn test_supplicant_sends_zeroed_and_non_zeroed_key_length() {
        let protection = NegotiatedProtection::from_rsne(&fake_wpa2_s_rsne())
            .expect("could not derive negotiated RSNE");
        let mut env = test_util::FourwayTestEnv::new(test_util::HandshakeKind::Wpa2, 1, 3);

        // Use arbitrarily chosen key_replay_counter.
        let msg1 = env.initiate(11.into());
        let (msg2_base, ptk) = env.send_msg1_to_supplicant(msg1.keyframe(), 11.into());

        // IEEE 802.11 compliant key length.
        let mut buf = vec![];
        let mut msg2 = msg2_base.copy_keyframe_mut(&mut buf);
        msg2.key_frame_fields.key_len.set_from_native(0);
        env.finalize_key_frame(&mut msg2, Some(ptk.kck()));
        let result = Dot11VerifiedKeyFrame::from_frame(msg2, &Role::Authenticator, &protection, 12);
        assert!(result.is_ok(), "failed verifying message: {}", result.unwrap_err());

        // Use CCMP-128 key length. Not officially IEEE 802.11 compliant but relaxed for
        // interoperability.
        let mut buf = vec![];
        let mut msg2 = msg2_base.copy_keyframe_mut(&mut buf);
        msg2.key_frame_fields.key_len.set_from_native(16);
        env.finalize_key_frame(&mut msg2, Some(ptk.kck()));
        let result = Dot11VerifiedKeyFrame::from_frame(msg2, &Role::Authenticator, &protection, 12);
        assert!(result.is_ok(), "failed verifying message: {}", result.unwrap_err());
    }

    // Fuchsia requires EAPOL frames sent from the Supplicant to contain a key length of either 0 or
    // the PTK's length.
    #[test]
    fn test_supplicant_sends_random_key_length() {
        let mut env = test_util::FourwayTestEnv::new(test_util::HandshakeKind::Wpa2, 1, 3);

        // Use arbitrarily chosen key_replay_counter.
        let msg1 = env.initiate(12.into());
        let (msg2, ptk) = env.send_msg1_to_supplicant(msg1.keyframe(), 12.into());
        let mut buf = vec![];
        let mut msg2 = msg2.copy_keyframe_mut(&mut buf);

        msg2.key_frame_fields.key_len.set_from_native(29);
        env.finalize_key_frame(&mut msg2, Some(ptk.kck()));

        let protection = NegotiatedProtection::from_rsne(&fake_wpa2_s_rsne())
            .expect("could not derive negotiated RSNE");
        let result = Dot11VerifiedKeyFrame::from_frame(msg2, &Role::Authenticator, &protection, 12);
        assert!(result.is_err(), "successfully verified illegal message");
    }

    #[test]
    fn test_to_rsne() {
        let rsne = Rsne::wpa2_rsne();
        let negotiated_protection = NegotiatedProtection::from_rsne(&rsne)
            .expect("error, could not create negotiated RSNE")
            .to_full_protection();
        assert_variant!(negotiated_protection, ProtectionInfo::Rsne(actual_protection) => {
            assert_eq!(actual_protection, rsne);
        });
    }

    #[test]
    fn test_to_legacy_wpa() {
        let wpa_ie = make_wpa(Some(cipher::TKIP), vec![cipher::TKIP], vec![akm::PSK]);
        let negotiated_protection = NegotiatedProtection::from_legacy_wpa(&wpa_ie)
            .expect("error, could not create negotiated WPA")
            .to_full_protection();
        assert_variant!(negotiated_protection, ProtectionInfo::LegacyWpa(actual_protection) => {
            assert_eq!(actual_protection, wpa_ie);
        });
    }

    #[test]
    fn test_igtk_support() {
        // Standard WPA3 RSNE requires MFP.
        let rsne = Rsne::wpa3_rsne();
        let negotiated_protection =
            NegotiatedProtection::from_rsne(&rsne).expect("Could not create negotiated RSNE");
        assert_variant!(negotiated_protection.igtk_support(), IgtkSupport::Required);
        assert_eq!(negotiated_protection.group_mgmt_cipher(), CIPHER_BIP_CMAC_128);

        // Mixed mode RSNE is compatible with MFP.
        let mut rsne = Rsne::wpa3_rsne();
        rsne.rsn_capabilities.replace(RsnCapabilities(0).with_mgmt_frame_protection_cap(true));
        let negotiated_protection =
            NegotiatedProtection::from_rsne(&rsne).expect("Could not create negotiated RSNE");
        assert_variant!(negotiated_protection.igtk_support(), IgtkSupport::Capable);

        // WPA2 RSNE doesn't support MFP.
        let rsne = Rsne::wpa2_rsne();
        let negotiated_protection =
            NegotiatedProtection::from_rsne(&rsne).expect("Could not create negotiated RSNE");
        assert_variant!(negotiated_protection.igtk_support(), IgtkSupport::Unsupported);
    }

    #[test]
    fn test_default_igtk_cipher() {
        let mut rsne = Rsne::wpa3_rsne();
        rsne.group_mgmt_cipher_suite.take(); // Default to BIP_CMAC_128.
        let negotiated_protection =
            NegotiatedProtection::from_rsne(&rsne).expect("Could not create negotiated RSNE");
        assert_variant!(negotiated_protection.igtk_support(), IgtkSupport::Required);
        assert_eq!(negotiated_protection.group_mgmt_cipher(), CIPHER_BIP_CMAC_128);
    }

    fn make_wpa(unicast: Option<u8>, multicast: Vec<u8>, akms: Vec<u8>) -> WpaIe {
        WpaIe {
            multicast_cipher: unicast
                .map(cipher::Cipher::new_dot11)
                .expect("failed to make wpa ie!"),
            unicast_cipher_list: multicast.into_iter().map(cipher::Cipher::new_dot11).collect(),
            akm_list: akms.into_iter().map(akm::Akm::new_dot11).collect(),
        }
    }
}
