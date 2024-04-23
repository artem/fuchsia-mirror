// SPDX-License-Identifier: MIT

use netlink_packet_utils::{Emitable, Parseable};

use crate::link::link_flag::LinkFlags;
use crate::link::{
    BondMode, BondPortState, InfoBond, InfoBondPort, InfoData, InfoKind,
    InfoPortData, InfoPortKind, LinkAttribute, LinkHeader, LinkInfo,
    LinkLayerType, LinkMessage, LinkMessageBuffer, MiiStatus,
};
use crate::AddressFamily;

#[test]
fn test_bond_link_info() {
    let raw: Vec<u8> = vec![
        0x00, 0x00, 0x01, 0x00, 0x18, 0x00, 0x00, 0x00, 0x43, 0x14, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xcc, 0x00, // length 204
        0x12, 0x00, // IFLA_LINKINFO 18
        0x09, 0x00, // length 9
        0x01, 0x00, // IFLA_INFO_KIND 1
        0x62, 0x6f, 0x6e, 0x64, 0x00, // 'bond\0'
        0x00, 0x00, 0x00, // padding
        0xbc, 0x00, // length 188
        0x02, 0x00, // IFLA_INFO_DATA
        0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x08, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x1c, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x06, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x08, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x09, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x05, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x0d, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x08, 0x00, 0x0f, 0x00, 0x01, 0x00, 0x00, 0x00, 0x05, 0x00, 0x10, 0x00,
        0x01, 0x00, 0x00, 0x00, 0x05, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x08, 0x00, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x13, 0x00,
        0x01, 0x00, 0x00, 0x00, 0x08, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x05, 0x00, 0x1d, 0x00, 0x01, 0x00, 0x00, 0x00, 0x05, 0x00, 0x15, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x05, 0x00, 0x1b, 0x00, 0x01, 0x00, 0x00, 0x00, 0x05, 0x00, 0x1e, 0x00,
        0x02, 0x00, 0x00, 0x00,
    ];

    let expected = LinkMessage {
        header: LinkHeader {
            interface_family: AddressFamily::Unspec,
            index: 24,
            link_layer_type: LinkLayerType::Ether,
            flags: LinkFlags::Broadcast
                | LinkFlags::Controller
                | LinkFlags::LowerUp
                | LinkFlags::Multicast
                | LinkFlags::Running
                | LinkFlags::Up,
            change_mask: LinkFlags::empty(),
        },
        attributes: vec![LinkAttribute::LinkInfo(vec![
            LinkInfo::Kind(InfoKind::Bond),
            LinkInfo::Data(InfoData::Bond(vec![
                InfoBond::Mode(BondMode::BalanceRr),
                InfoBond::MiiMon(0),
                InfoBond::UpDelay(0),
                InfoBond::DownDelay(0),
                InfoBond::PeerNotifDelay(0),
                InfoBond::UseCarrier(1),
                InfoBond::ArpInterval(0),
                InfoBond::ArpValidate(0),
                InfoBond::ArpAllTargets(0),
                InfoBond::PrimaryReselect(0),
                InfoBond::FailOverMac(0),
                InfoBond::XmitHashPolicy(0),
                InfoBond::ResendIgmp(1),
                InfoBond::NumPeerNotif(1),
                InfoBond::AllPortsActive(0),
                InfoBond::MinLinks(0),
                InfoBond::LpInterval(1),
                InfoBond::PacketsPerPort(1),
                InfoBond::AdLacpActive(1),
                InfoBond::AdLacpRate(0),
                InfoBond::AdSelect(0),
                InfoBond::TlbDynamicLb(1),
                InfoBond::MissedMax(2),
            ])),
        ])],
    };

    assert_eq!(
        expected,
        LinkMessage::parse(&LinkMessageBuffer::new(&raw)).unwrap()
    );

    let mut buf = vec![0; expected.buffer_len()];

    expected.emit(&mut buf);

    assert_eq!(buf, raw);
}

#[test]
fn test_bond_port_link_info() {
    let raw: Vec<u8> = vec![
        0x00, 0x00, 0x01, 0x00, 0x15, 0x00, 0x00, 0x00, 0x43, 0x18, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x54, 0x00, 0x12, 0x00, 0x09, 0x00, 0x01, 0x00,
        0x76, 0x65, 0x74, 0x68, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00, 0x04, 0x00,
        0x62, 0x6f, 0x6e, 0x64, 0x00, 0x00, 0x00, 0x00, 0x38, 0x00, 0x05, 0x00,
        0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x02, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0a, 0x00, 0x04, 0x00, 0x00, 0x23, 0x45, 0x67, 0x89, 0x1a, 0x00, 0x00,
        0x06, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x09, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    let expected = LinkMessage {
        header: LinkHeader {
            interface_family: AddressFamily::Unspec,
            index: 21,
            link_layer_type: LinkLayerType::Ether,
            flags: LinkFlags::Broadcast
                | LinkFlags::LowerUp
                | LinkFlags::Multicast
                | LinkFlags::Port
                | LinkFlags::Running
                | LinkFlags::Up,
            change_mask: LinkFlags::empty(),
        },
        attributes: vec![LinkAttribute::LinkInfo(vec![
            LinkInfo::Kind(InfoKind::Veth),
            LinkInfo::PortKind(InfoPortKind::Bond),
            LinkInfo::PortData(InfoPortData::BondPort(vec![
                InfoBondPort::BondPortState(BondPortState::Active),
                InfoBondPort::MiiStatus(MiiStatus::Up),
                InfoBondPort::LinkFailureCount(0),
                InfoBondPort::PermHwaddr(vec![
                    0x00, 0x23, 0x45, 0x67, 0x89, 0x1a,
                ]),
                InfoBondPort::QueueId(0),
                InfoBondPort::Prio(0),
            ])),
        ])],
    };

    assert_eq!(
        expected,
        LinkMessage::parse(&LinkMessageBuffer::new(&raw)).unwrap()
    );

    let mut buf = vec![0; expected.buffer_len()];

    expected.emit(&mut buf);

    assert_eq!(buf, raw);
}
