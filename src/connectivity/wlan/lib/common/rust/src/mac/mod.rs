// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        buffer_reader::{BufferReader, IntoBufferReader},
        ie, UnalignedView,
    },
    ieee80211::MacAddr,
    num::Unsigned,
    zerocopy::{AsBytes, ByteSlice, NoCell, Ref},
};

mod ctrl;
mod data;
mod eth;
mod fields;
// Frame classes are not yet used.
// Remove once they are being used.
#[allow(dead_code)]
mod frame_class;
mod mgmt;

pub use {ctrl::*, data::*, eth::*, fields::*, frame_class::*, mgmt::*};

#[macro_export]
macro_rules! frame_len {
    () => { 0 };
    ($only:ty) => { std::mem::size_of::<$only>() };
    ($first:ty, $($tail:ty),*) => {
        std::mem::size_of::<$first>() + frame_len!($($tail),*)
    };
}

// IEEE Std 802.11-2016, 9.4.1.8
pub type Aid = u16;

// IEEE Std 802.11-2016, 9.4.1.8: A non-DMG STA assigns the value of the AID in the range of 1 to
// 2007.
pub const MAX_AID: u16 = 2007;

pub trait AsBytesExt: AsBytes + NoCell + Sized {
    /// Gets a byte slice reference from a reference to `Self`.
    ///
    /// This is essentially the reverse of `Ref` constructors and can be used to construct `Ref`
    /// fields in `zerocopy` types from a reference to `Self` instead of bytes.
    fn as_bytes_ref(&self) -> Ref<&'_ [u8], Self> {
        Ref::new(self.as_bytes())
            .expect("Unaligned or missized byte slice from `AsBytes` implementation.")
    }
}

impl<T> AsBytesExt for T where T: AsBytes + NoCell + Sized {}

// TODO(https://fxbug.dev/42079361): Use this in the `MacFrame::Data` variant.
pub struct DataFrame<B> {
    // Data Header: fixed fields
    pub fixed_fields: Ref<B, FixedDataHdrFields>,
    // Data Header: optional fields
    pub addr4: Option<Ref<B, Addr4>>,
    pub qos_ctrl: Option<UnalignedView<B, QosControl>>,
    pub ht_ctrl: Option<UnalignedView<B, HtControl>>,
    // Body
    pub body: B,
}

impl<B> DataFrame<B>
where
    B: ByteSlice,
{
    pub fn parse(bytes: B, is_body_aligned: bool) -> Option<Self> {
        let mut reader = BufferReader::new(bytes);
        let fc = FrameControl(reader.peek_value()?);
        matches!(fc.frame_type(), FrameType::DATA)
            .then(|| {
                // Parse fixed header fields
                let fixed_fields = reader.read()?;

                // Parse optional header fields
                let addr4 = if fc.to_ds() && fc.from_ds() { Some(reader.read()?) } else { None };
                let qos_ctrl =
                    if fc.data_subtype().qos() { Some(reader.read_unaligned()?) } else { None };
                let ht_ctrl = if fc.htc_order() { Some(reader.read_unaligned()?) } else { None };

                // Skip optional padding if body alignment is used.
                if is_body_aligned {
                    let full_hdr_len = FixedDataHdrFields::len(
                        Presence::<Addr4>::from_bool(addr4.is_some()),
                        Presence::<QosControl>::from_bool(qos_ctrl.is_some()),
                        Presence::<HtControl>::from_bool(ht_ctrl.is_some()),
                    );
                    skip_body_alignment_padding(full_hdr_len, &mut reader)?
                };
                Some(DataFrame {
                    fixed_fields,
                    addr4,
                    qos_ctrl,
                    ht_ctrl,
                    body: reader.into_remaining(),
                })
            })
            .flatten()
    }
}

impl<B> IntoIterator for DataFrame<B>
where
    B: ByteSlice,
{
    type IntoIter = MsduIterator<B>;
    type Item = Msdu<B>;

    fn into_iter(self) -> Self::IntoIter {
        self.into()
    }
}

pub struct MgmtFrame<B> {
    // Management Header: fixed fields
    pub mgmt_hdr: Ref<B, MgmtHdr>,
    // Management Header: optional fields
    pub ht_ctrl: Option<UnalignedView<B, HtControl>>,
    // Body
    pub body: B,
}

impl<B> MgmtFrame<B>
where
    B: ByteSlice,
{
    pub fn parse(reader: impl IntoBufferReader<B>, is_body_aligned: bool) -> Option<Self> {
        let reader = reader.into_buffer_reader();
        let fc = FrameControl(reader.peek_value()?);
        matches!(fc.frame_type(), FrameType::MGMT)
            .then(|| MgmtFrame::parse_frame_type_unchecked(reader, is_body_aligned))
            .flatten()
    }

    fn parse_frame_type_unchecked(
        reader: impl IntoBufferReader<B>,
        is_body_aligned: bool,
    ) -> Option<Self> {
        let mut reader = reader.into_buffer_reader();
        let fc = FrameControl(reader.peek_value()?);
        // Parse fixed header fields
        let mgmt_hdr = reader.read()?;

        // Parse optional header fields
        let ht_ctrl = if fc.htc_order() { Some(reader.read_unaligned()?) } else { None };
        // Skip optional padding if body alignment is used.
        if is_body_aligned {
            let full_hdr_len = MgmtHdr::len(Presence::<HtControl>::from_bool(ht_ctrl.is_some()));
            skip_body_alignment_padding(full_hdr_len, &mut reader)?
        }
        Some(MgmtFrame { mgmt_hdr, ht_ctrl, body: reader.into_remaining() })
    }

    pub fn try_into_mgmt_body(self) -> (Ref<B, MgmtHdr>, Option<MgmtBody<B>>) {
        let MgmtFrame { mgmt_hdr, body, .. } = self;
        let mgmt_subtype = { mgmt_hdr.frame_ctrl }.mgmt_subtype();
        (mgmt_hdr, MgmtBody::parse(mgmt_subtype, body))
    }

    pub fn into_ies(self) -> (Ref<B, MgmtHdr>, impl Iterator<Item = (ie::Id, B)>) {
        let MgmtFrame { mgmt_hdr, body, .. } = self;
        (mgmt_hdr, ie::Reader::new(body))
    }

    pub fn frame_ctrl(&self) -> FrameControl {
        self.mgmt_hdr.frame_ctrl
    }

    pub fn mgmt_subtype(&self) -> MgmtSubtype {
        self.frame_ctrl().mgmt_subtype()
    }
}

pub enum MacFrame<B> {
    Mgmt(MgmtFrame<B>),
    // TODO(https://fxbug.dev/42079361): Implement a dedicated type for control frames.
    Data {
        // Data Header: fixed fields
        fixed_fields: Ref<B, FixedDataHdrFields>,
        // Data Header: optional fields
        addr4: Option<Ref<B, Addr4>>,
        qos_ctrl: Option<UnalignedView<B, QosControl>>,
        ht_ctrl: Option<UnalignedView<B, HtControl>>,
        // Body
        body: B,
    },
    // TODO(https://fxbug.dev/42079361): Implement a dedicated type for control frames.
    Ctrl {
        // Control Header: frame control
        frame_ctrl: FrameControl,
        // Body
        body: B,
    },
    // TODO(https://fxbug.dev/42079361): Implement a dedicated type for control frames.
    Unsupported {
        frame_ctrl: FrameControl,
    },
}

impl<B: ByteSlice> MacFrame<B> {
    /// If `body_aligned` is |true| the frame's body is expected to be 4 byte aligned.
    pub fn parse(bytes: B, body_aligned: bool) -> Option<MacFrame<B>> {
        let mut reader = BufferReader::new(bytes);
        let fc = FrameControl(reader.peek_value()?);
        match fc.frame_type() {
            FrameType::MGMT => {
                MgmtFrame::parse_frame_type_unchecked(reader, body_aligned).map(From::from)
            }
            FrameType::DATA => {
                // Parse fixed header fields
                let fixed_fields = reader.read()?;

                // Parse optional header fields
                let addr4 = if fc.to_ds() && fc.from_ds() { Some(reader.read()?) } else { None };

                let qos_ctrl =
                    if fc.data_subtype().qos() { Some(reader.read_unaligned()?) } else { None };

                let ht_ctrl = if fc.htc_order() { Some(reader.read_unaligned()?) } else { None };

                // Skip optional padding if body alignment is used.
                if body_aligned {
                    let full_hdr_len = FixedDataHdrFields::len(
                        Presence::<Addr4>::from_bool(addr4.is_some()),
                        Presence::<QosControl>::from_bool(qos_ctrl.is_some()),
                        Presence::<HtControl>::from_bool(ht_ctrl.is_some()),
                    );
                    skip_body_alignment_padding(full_hdr_len, &mut reader)?
                };
                Some(MacFrame::Data {
                    fixed_fields,
                    addr4,
                    qos_ctrl,
                    ht_ctrl,
                    body: reader.into_remaining(),
                })
            }
            FrameType::CTRL => {
                // Parse frame control.
                let frame_ctrl = reader.read_value()?;
                Some(MacFrame::Ctrl { frame_ctrl, body: reader.into_remaining() })
            }
            _type => Some(MacFrame::Unsupported { frame_ctrl: fc }),
        }
    }
}

impl<B> From<MgmtFrame<B>> for MacFrame<B> {
    fn from(mgmt: MgmtFrame<B>) -> Self {
        MacFrame::Mgmt(mgmt)
    }
}

/// Skips optional padding required for body alignment.
fn skip_body_alignment_padding<B: ByteSlice>(
    hdr_len: usize,
    reader: &mut BufferReader<B>,
) -> Option<()> {
    const OPTIONAL_BODY_ALIGNMENT_BYTES: usize = 4;

    let padded_len = round_up(hdr_len, OPTIONAL_BODY_ALIGNMENT_BYTES);
    let padding = padded_len - hdr_len;
    reader.read_bytes(padding).map(|_| ())
}

fn round_up<T: Unsigned + Copy>(value: T, multiple: T) -> T {
    let overshoot = value + multiple - T::one();
    overshoot - overshoot % multiple
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{assert_variant, test_utils::fake_frames::*},
    };

    #[test]
    fn parse_mgmt_frame() {
        let bytes = make_mgmt_frame(false);
        assert_variant!(
            MacFrame::parse(&bytes[..], false),
            Some(MacFrame::Mgmt(MgmtFrame { mgmt_hdr, ht_ctrl, body })) => {
                assert_eq!(0x0101, { mgmt_hdr.frame_ctrl.0 });
                assert_eq!(0x0202, { mgmt_hdr.duration });
                assert_eq!(MacAddr::from([3, 3, 3, 3, 3, 3]), mgmt_hdr.addr1);
                assert_eq!(MacAddr::from([4, 4, 4, 4, 4, 4]), mgmt_hdr.addr2);
                assert_eq!(MacAddr::from([5, 5, 5, 5, 5, 5]), mgmt_hdr.addr3);
                assert_eq!(0x0606, { mgmt_hdr.seq_ctrl.0 });
                assert!(ht_ctrl.is_none());
                assert_eq!(&body[..], &[9, 9, 9]);
            },
            "expected management frame"
        );
    }

    #[test]
    fn parse_mgmt_frame_too_short_unsupported() {
        // Valid MGMT header must have a minium length of 24 bytes.
        assert!(MacFrame::parse(&[0; 22][..], false).is_none());

        // Unsupported frame type.
        assert_variant!(
            MacFrame::parse(&[0xFF; 24][..], false),
            Some(MacFrame::Unsupported { frame_ctrl }) => {
                assert_eq!(frame_ctrl, FrameControl(0xFFFF))
            },
            "expected unsupported frame type"
        );
    }

    #[test]
    fn parse_data_frame() {
        let bytes = make_data_frame_single_llc(None, None);
        assert_variant!(
            MacFrame::parse(&bytes[..], false),
            Some(MacFrame::Data { fixed_fields, addr4, qos_ctrl, ht_ctrl, body }) => {
                assert_eq!(0b00000000_10001000, { fixed_fields.frame_ctrl.0 });
                assert_eq!(0x0202, { fixed_fields.duration });
                assert_eq!(MacAddr::from([3, 3, 3, 3, 3, 3]), fixed_fields.addr1);
                assert_eq!(MacAddr::from([4, 4, 4, 4, 4, 4]), fixed_fields.addr2);
                assert_eq!(MacAddr::from([5, 5, 5, 5, 5, 5]), fixed_fields.addr3);
                assert_eq!(0x0606, { fixed_fields.seq_ctrl.0 });
                assert!(addr4.is_none());
                assert_eq!(0x0101, qos_ctrl.expect("qos_ctrl not present").get().0);
                assert!(ht_ctrl.is_none());
                assert_eq!(&body[..], &[7, 7, 7, 8, 8, 8, 9, 10, 11, 11, 11]);
            },
            "expected management frame"
        );
    }

    #[test]
    fn parse_ctrl_frame() {
        assert_variant!(
            MacFrame::parse(&[
                0b10100100, 0b00000000, // Frame Control
                0b00000001, 0b11000000, // Masked AID
                2, 2, 2, 2, 2, 2, // addr1
                4, 4, 4, 4, 4, 4, // addr2
            ][..], false),
            Some(MacFrame::Ctrl { frame_ctrl, body }) => {
                assert_eq!(0b00000000_10100100, frame_ctrl.0);
                assert_eq!(&body[..], &[
                    0b00000001, 0b11000000, // Masked AID
                    2, 2, 2, 2, 2, 2, // addr1
                    4, 4, 4, 4, 4, 4, // addr2
                ]);
            },
            "expected control frame"
        );
    }

    #[test]
    fn round_up_to_4() {
        assert_eq!(0, round_up(0u32, 4));
        assert_eq!(4, round_up(1u32, 4));
        assert_eq!(4, round_up(2u32, 4));
        assert_eq!(4, round_up(3u32, 4));
        assert_eq!(4, round_up(4u32, 4));
        assert_eq!(8, round_up(5u32, 4));
    }
}
