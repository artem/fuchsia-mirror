// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::Error,
    anyhow::ensure,
    core::num::NonZeroU32,
    ieee80211::Ssid,
    std::{convert::TryInto, str},
    wlan_common::security::wpa::{self, credential::Psk as CommonPsk},
};

// PBKDF2-HMAC-SHA1 is considered insecure but required for PSK computation.
#[allow(deprecated)]
use mundane::insecure::insecure_pbkdf2_hmac_sha1;

/// Conversion of WPA credentials to a PSK.
pub trait ToPsk {
    fn to_psk(&self, ssid: &Ssid) -> CommonPsk;
}

/// Conversion of WPA1 credentials to a PSK.
///
/// RSN specifies that PSKs are used as-is and passphrases are transformed into a PSK. This
/// implementation performs this transformation when necessary for WPA1 credentials.
impl ToPsk for wpa::Wpa1Credentials {
    fn to_psk(&self, ssid: &Ssid) -> CommonPsk {
        match self {
            wpa::Wpa1Credentials::Psk(ref psk) => psk.clone(),
            wpa::Wpa1Credentials::Passphrase(ref passphrase) => {
                // TODO(https://fxbug.dev/95934): Unify the representation of PSKs. There can only be
                //                        one...!
                CommonPsk(
                    compute(passphrase.as_ref(), ssid)
                        .expect("invalid WPA passphrase data")
                        .as_ref()
                        .try_into()
                        .expect("invalid derived PSK data"),
                )
            }
        }
    }
}

/// Conversion of WPA2 Personal credentials to a PSK.
///
/// RSN specifies that PSKs are used as-is and passphrases are transformed into a PSK. This
/// implementation performs this transformation when necessary for WPA2 Personal credentials.
impl ToPsk for wpa::Wpa2PersonalCredentials {
    fn to_psk(&self, ssid: &Ssid) -> CommonPsk {
        match self {
            wpa::Wpa2PersonalCredentials::Psk(ref psk) => psk.clone(),
            wpa::Wpa2PersonalCredentials::Passphrase(ref passphrase) => {
                // TODO(https://fxbug.dev/95934): Unify the representation of PSKs. There can only be
                //                        one...!
                CommonPsk(
                    compute(passphrase.as_ref(), ssid)
                        .expect("invalid WPA passphrase data")
                        .as_ref()
                        .try_into()
                        .expect("invalid derived PSK data"),
                )
            }
        }
    }
}

/// Keys derived from a passphrase provide comparably low levels of security.
/// Passphrases should have a minimum length of 20 characters since shorter passphrases
/// are unlikely to prevent attacks.
pub type Psk = Box<[u8]>;

pub fn compute(passphrase: &[u8], ssid: &Ssid) -> Result<Psk, anyhow::Error> {
    // IEEE Std 802.11-2016, J.4.1 provides a reference implementation that describes the
    // passphrase as:
    //
    //   ... sequence of between 8 and 63 ASCII-encoded characters ... Each character in the
    //   pass-phrase has an encoding in the range 32 to 126 (decimal).
    //
    // However, the standard does not seem to specify this encoding or otherwise state that it is a
    // requirement. In practice, devices accept UTF-8 encoded passphrases, which is far less
    // restrictive than a subset of ASCII. This code attempts to parse the passphrase as UTF-8 and
    // emits an error if this is not possible. Note that this also accepts the ASCII encoding
    // suggested by J.4.1.
    let _utf8 = str::from_utf8(passphrase)
        .map_err(|error| Error::InvalidPassphraseEncoding(error.valid_up_to()))?;

    // IEEE Std 802.11-2016, J.4.1 suggests a passphrase length of [8, 64). However, J.4.1 also
    // suggests ASCII encoding and this code expects UTF-8 encoded passphrases. This implicitly
    // supports the ASCII encodings described in J.4.1. However, the length of the byte sequence no
    // longer represents the number of encoded characters, so non-ASCII passphrases may appear to
    // have arbitrary character limits. Note that the character count can be obtained via
    // `_utf8.chars().count()`.
    ensure!(
        passphrase.len() >= 8 && passphrase.len() <= 63,
        Error::InvalidPassphraseLen(passphrase.len())
    );

    // Compute PSK: IEEE Std 802.11-2016, J.4.1
    let size = 256 / 8;
    let mut psk = vec![0_u8; size];
    const ITERS: NonZeroU32 = const_unwrap::const_unwrap_option(NonZeroU32::new(4096));
    insecure_pbkdf2_hmac_sha1(&passphrase[..], &ssid[..], ITERS, &mut psk[..]);
    Ok(psk.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use {
        super::*, hex::FromHex, std::convert::TryFrom,
        wlan_common::security::wpa::credential::Passphrase,
    };

    fn assert_psk(password: &str, ssid: &str, expected: &str) {
        let psk = compute(password.as_bytes(), &Ssid::try_from(ssid).unwrap())
            .expect("computing PSK failed");
        let expected = Vec::from_hex(expected).unwrap();
        assert_eq!(&psk[..], &expected[..]);
    }

    // IEEE Std 802.11-2016, J.4.2, Test case 1
    #[test]
    fn test_psk_test_case_1() {
        assert_psk(
            "password",
            "IEEE",
            "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e",
        );
    }

    // IEEE Std 802.11-2016, J.4.2, Test case 2
    #[test]
    fn test_psk_test_case_2() {
        assert_psk(
            "ThisIsAPassword",
            "ThisIsASSID",
            "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af",
        );
    }

    // IEEE Std 802.11-2016, J.4.2, Test case 3
    #[test]
    fn test_psk_test_case_3() {
        assert_psk(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
            "becb93866bb8c3832cb777c2f559807c8c59afcb6eae734885001300a981cc62",
        );
    }

    #[test]
    fn test_psk_too_short_password() {
        let result = compute("short".as_bytes(), &Ssid::try_from("Some SSID").unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn test_psk_too_long_password() {
        let result = compute(
            "1234567890123456789012345678901234567890123456789012345678901234".as_bytes(),
            &Ssid::try_from("Some SSID").unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_psk_ascii_bounds_password() {
        let result =
            compute("\x20ASCII Bound Test \x7E".as_bytes(), &Ssid::try_from("Some SSID").unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_psk_non_ascii_password() {
        assert!(compute("パスワード".as_bytes(), &Ssid::try_from("Some SSID").unwrap()).is_ok());
    }

    #[test]
    fn test_psk_invalid_encoding_password() {
        assert!(compute(&[0xFFu8; 32], &Ssid::try_from("Some SSID").unwrap()).is_err());
    }

    #[test]
    fn wpa1_credentials_to_psk() {
        let ssid = Ssid::try_from("IEEE").unwrap();

        let credentials = wpa::Wpa1Credentials::Psk(CommonPsk::from([0u8; 32]));
        assert_eq!(CommonPsk::from([0u8; 32]), credentials.to_psk(&ssid));

        let credentials =
            wpa::Wpa1Credentials::Passphrase(Passphrase::try_from("password").unwrap());
        assert_eq!(
            CommonPsk::parse("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
                .unwrap(),
            credentials.to_psk(&ssid),
        );
    }

    #[test]
    fn wpa2_personal_credentials_to_psk() {
        let ssid = Ssid::try_from("IEEE").unwrap();

        let credentials = wpa::Wpa2PersonalCredentials::Psk(CommonPsk::from([0u8; 32]));
        assert_eq!(CommonPsk::from([0u8; 32]), credentials.to_psk(&ssid));

        let credentials =
            wpa::Wpa2PersonalCredentials::Passphrase(Passphrase::try_from("password").unwrap());
        assert_eq!(
            CommonPsk::parse("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
                .unwrap(),
            credentials.to_psk(&ssid),
        );
    }
}
