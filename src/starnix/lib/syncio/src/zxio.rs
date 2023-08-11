/* automatically generated by rust-bindgen 0.66.1 */

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use zerocopy::{AsBytes, FromBytes, FromZeroes};

pub const EPERM: u32 = 1;
pub const ENOENT: u32 = 2;
pub const ESRCH: u32 = 3;
pub const EINTR: u32 = 4;
pub const EIO: u32 = 5;
pub const ENXIO: u32 = 6;
pub const ENOEXEC: u32 = 8;
pub const EBADF: u32 = 9;
pub const ECHILD: u32 = 10;
pub const EAGAIN: u32 = 11;
pub const ENOMEM: u32 = 12;
pub const EACCES: u32 = 13;
pub const EFAULT: u32 = 14;
pub const ENOTBLK: u32 = 15;
pub const EBUSY: u32 = 16;
pub const EEXIST: u32 = 17;
pub const EXDEV: u32 = 18;
pub const ENODEV: u32 = 19;
pub const ENOTDIR: u32 = 20;
pub const EISDIR: u32 = 21;
pub const EINVAL: u32 = 22;
pub const ENFILE: u32 = 23;
pub const EMFILE: u32 = 24;
pub const ENOTTY: u32 = 25;
pub const ETXTBSY: u32 = 26;
pub const EFBIG: u32 = 27;
pub const ENOSPC: u32 = 28;
pub const ESPIPE: u32 = 29;
pub const EROFS: u32 = 30;
pub const EMLINK: u32 = 31;
pub const EPIPE: u32 = 32;
pub const EDOM: u32 = 33;
pub const ERANGE: u32 = 34;
pub const EDEADLK: u32 = 35;
pub const ENAMETOOLONG: u32 = 36;
pub const ENOLCK: u32 = 37;
pub const ENOSYS: u32 = 38;
pub const ENOTEMPTY: u32 = 39;
pub const ELOOP: u32 = 40;
pub const EWOULDBLOCK: u32 = 11;
pub const ENOMSG: u32 = 42;
pub const EIDRM: u32 = 43;
pub const ECHRNG: u32 = 44;
pub const ELNRNG: u32 = 48;
pub const EUNATCH: u32 = 49;
pub const ENOCSI: u32 = 50;
pub const EBADE: u32 = 52;
pub const EBADR: u32 = 53;
pub const EXFULL: u32 = 54;
pub const ENOANO: u32 = 55;
pub const EBADRQC: u32 = 56;
pub const EBADSLT: u32 = 57;
pub const EDEADLOCK: u32 = 35;
pub const EBFONT: u32 = 59;
pub const ENOSTR: u32 = 60;
pub const ENODATA: u32 = 61;
pub const ETIME: u32 = 62;
pub const ENOSR: u32 = 63;
pub const ENONET: u32 = 64;
pub const ENOPKG: u32 = 65;
pub const EREMOTE: u32 = 66;
pub const ENOLINK: u32 = 67;
pub const EADV: u32 = 68;
pub const ESRMNT: u32 = 69;
pub const ECOMM: u32 = 70;
pub const EPROTO: u32 = 71;
pub const EMULTIHOP: u32 = 72;
pub const EDOTDOT: u32 = 73;
pub const EBADMSG: u32 = 74;
pub const EOVERFLOW: u32 = 75;
pub const ENOTUNIQ: u32 = 76;
pub const EBADFD: u32 = 77;
pub const EREMCHG: u32 = 78;
pub const ELIBACC: u32 = 79;
pub const ELIBBAD: u32 = 80;
pub const ELIBSCN: u32 = 81;
pub const ELIBMAX: u32 = 82;
pub const ELIBEXEC: u32 = 83;
pub const EILSEQ: u32 = 84;
pub const ERESTART: u32 = 85;
pub const ESTRPIPE: u32 = 86;
pub const EUSERS: u32 = 87;
pub const ENOTSOCK: u32 = 88;
pub const EDESTADDRREQ: u32 = 89;
pub const EMSGSIZE: u32 = 90;
pub const EPROTOTYPE: u32 = 91;
pub const ENOPROTOOPT: u32 = 92;
pub const EPROTONOSUPPORT: u32 = 93;
pub const ESOCKTNOSUPPORT: u32 = 94;
pub const EOPNOTSUPP: u32 = 95;
pub const ENOTSUP: u32 = 95;
pub const EPFNOSUPPORT: u32 = 96;
pub const EAFNOSUPPORT: u32 = 97;
pub const EADDRINUSE: u32 = 98;
pub const EADDRNOTAVAIL: u32 = 99;
pub const ENETDOWN: u32 = 100;
pub const ENETUNREACH: u32 = 101;
pub const ENETRESET: u32 = 102;
pub const ECONNABORTED: u32 = 103;
pub const ECONNRESET: u32 = 104;
pub const ENOBUFS: u32 = 105;
pub const EISCONN: u32 = 106;
pub const ENOTCONN: u32 = 107;
pub const ESHUTDOWN: u32 = 108;
pub const ETOOMANYREFS: u32 = 109;
pub const ETIMEDOUT: u32 = 110;
pub const ECONNREFUSED: u32 = 111;
pub const EHOSTDOWN: u32 = 112;
pub const EHOSTUNREACH: u32 = 113;
pub const EALREADY: u32 = 114;
pub const EINPROGRESS: u32 = 115;
pub const ESTALE: u32 = 116;
pub const EUCLEAN: u32 = 117;
pub const ENOTNAM: u32 = 118;
pub const ENAVAIL: u32 = 119;
pub const EISNAM: u32 = 120;
pub const EREMOTEIO: u32 = 121;
pub const EDQUOT: u32 = 122;
pub const ENOMEDIUM: u32 = 123;
pub const EMEDIUMTYPE: u32 = 124;
pub const ECANCELED: u32 = 125;
pub const ENOKEY: u32 = 126;
pub const EKEYEXPIRED: u32 = 127;
pub const EKEYREVOKED: u32 = 128;
pub const EKEYREJECTED: u32 = 129;
pub const EOWNERDEAD: u32 = 130;
pub const ENOTRECOVERABLE: u32 = 131;
pub const ERFKILL: u32 = 132;
pub const EHWPOISON: u32 = 133;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_RAW: u32 = 3;
pub const SOCK_RDM: u32 = 4;
pub const SOCK_SEQPACKET: u32 = 5;
pub const SOCK_DCCP: u32 = 6;
pub const SOCK_PACKET: u32 = 10;
pub const SOCK_CLOEXEC: u32 = 524288;
pub const SOCK_NONBLOCK: u32 = 2048;
pub const AF_UNSPEC: u32 = 0;
pub const AF_LOCAL: u32 = 1;
pub const AF_UNIX: u32 = 1;
pub const AF_FILE: u32 = 1;
pub const AF_INET: u32 = 2;
pub const AF_AX25: u32 = 3;
pub const AF_IPX: u32 = 4;
pub const AF_APPLETALK: u32 = 5;
pub const AF_NETROM: u32 = 6;
pub const AF_BRIDGE: u32 = 7;
pub const AF_ATMPVC: u32 = 8;
pub const AF_X25: u32 = 9;
pub const AF_INET6: u32 = 10;
pub const AF_ROSE: u32 = 11;
pub const AF_DECnet: u32 = 12;
pub const AF_NETBEUI: u32 = 13;
pub const AF_SECURITY: u32 = 14;
pub const AF_KEY: u32 = 15;
pub const AF_NETLINK: u32 = 16;
pub const AF_ROUTE: u32 = 16;
pub const AF_PACKET: u32 = 17;
pub const AF_ASH: u32 = 18;
pub const AF_ECONET: u32 = 19;
pub const AF_ATMSVC: u32 = 20;
pub const AF_RDS: u32 = 21;
pub const AF_SNA: u32 = 22;
pub const AF_IRDA: u32 = 23;
pub const AF_PPPOX: u32 = 24;
pub const AF_WANPIPE: u32 = 25;
pub const AF_LLC: u32 = 26;
pub const AF_IB: u32 = 27;
pub const AF_MPLS: u32 = 28;
pub const AF_CAN: u32 = 29;
pub const AF_TIPC: u32 = 30;
pub const AF_BLUETOOTH: u32 = 31;
pub const AF_IUCV: u32 = 32;
pub const AF_RXRPC: u32 = 33;
pub const AF_ISDN: u32 = 34;
pub const AF_PHONET: u32 = 35;
pub const AF_IEEE802154: u32 = 36;
pub const AF_CAIF: u32 = 37;
pub const AF_ALG: u32 = 38;
pub const AF_NFC: u32 = 39;
pub const AF_VSOCK: u32 = 40;
pub const AF_KCM: u32 = 41;
pub const AF_QIPCRTR: u32 = 42;
pub const AF_SMC: u32 = 43;
pub const AF_XDP: u32 = 44;
pub const AF_MAX: u32 = 45;
pub const SO_DEBUG: u32 = 1;
pub const SO_REUSEADDR: u32 = 2;
pub const SO_TYPE: u32 = 3;
pub const SO_ERROR: u32 = 4;
pub const SO_DONTROUTE: u32 = 5;
pub const SO_BROADCAST: u32 = 6;
pub const SO_SNDBUF: u32 = 7;
pub const SO_RCVBUF: u32 = 8;
pub const SO_KEEPALIVE: u32 = 9;
pub const SO_OOBINLINE: u32 = 10;
pub const SO_NO_CHECK: u32 = 11;
pub const SO_PRIORITY: u32 = 12;
pub const SO_LINGER: u32 = 13;
pub const SO_BSDCOMPAT: u32 = 14;
pub const SO_REUSEPORT: u32 = 15;
pub const SO_PASSCRED: u32 = 16;
pub const SO_PEERCRED: u32 = 17;
pub const SO_RCVLOWAT: u32 = 18;
pub const SO_SNDLOWAT: u32 = 19;
pub const SO_ACCEPTCONN: u32 = 30;
pub const SO_PEERSEC: u32 = 31;
pub const SO_SNDBUFFORCE: u32 = 32;
pub const SO_RCVBUFFORCE: u32 = 33;
pub const SO_PROTOCOL: u32 = 38;
pub const SO_DOMAIN: u32 = 39;
pub const SO_RCVTIMEO: u32 = 20;
pub const SO_SNDTIMEO: u32 = 21;
pub const SO_TIMESTAMP: u32 = 29;
pub const SO_TIMESTAMPNS: u32 = 35;
pub const SO_TIMESTAMPING: u32 = 37;
pub const SO_SECURITY_AUTHENTICATION: u32 = 22;
pub const SO_SECURITY_ENCRYPTION_TRANSPORT: u32 = 23;
pub const SO_SECURITY_ENCRYPTION_NETWORK: u32 = 24;
pub const SO_BINDTODEVICE: u32 = 25;
pub const SO_ATTACH_FILTER: u32 = 26;
pub const SO_DETACH_FILTER: u32 = 27;
pub const SO_GET_FILTER: u32 = 26;
pub const SO_PEERNAME: u32 = 28;
pub const SO_PASSSEC: u32 = 34;
pub const SO_MARK: u32 = 36;
pub const SO_RXQ_OVFL: u32 = 40;
pub const SO_WIFI_STATUS: u32 = 41;
pub const SO_PEEK_OFF: u32 = 42;
pub const SO_NOFCS: u32 = 43;
pub const SO_LOCK_FILTER: u32 = 44;
pub const SO_SELECT_ERR_QUEUE: u32 = 45;
pub const SO_BUSY_POLL: u32 = 46;
pub const SO_MAX_PACING_RATE: u32 = 47;
pub const SO_BPF_EXTENSIONS: u32 = 48;
pub const SO_INCOMING_CPU: u32 = 49;
pub const SO_ATTACH_BPF: u32 = 50;
pub const SO_DETACH_BPF: u32 = 27;
pub const SO_ATTACH_REUSEPORT_CBPF: u32 = 51;
pub const SO_ATTACH_REUSEPORT_EBPF: u32 = 52;
pub const SO_CNX_ADVICE: u32 = 53;
pub const SO_MEMINFO: u32 = 55;
pub const SO_INCOMING_NAPI_ID: u32 = 56;
pub const SO_COOKIE: u32 = 57;
pub const SO_PEERGROUPS: u32 = 59;
pub const SO_ZEROCOPY: u32 = 60;
pub const SO_TXTIME: u32 = 61;
pub const SO_BINDTOIFINDEX: u32 = 62;
pub const SO_DETACH_REUSEPORT_BPF: u32 = 68;
pub const SOL_SOCKET: u32 = 1;
pub const SOL_IP: u32 = 0;
pub const SOL_IPV6: u32 = 41;
pub const SOL_ICMPV6: u32 = 58;
pub const SOL_RAW: u32 = 255;
pub const SOL_DECNET: u32 = 261;
pub const SOL_X25: u32 = 262;
pub const SOL_PACKET: u32 = 263;
pub const SOL_ATM: u32 = 264;
pub const SOL_AAL: u32 = 265;
pub const SOL_IRDA: u32 = 266;
pub const SOL_NETBEUI: u32 = 267;
pub const SOL_LLC: u32 = 268;
pub const SOL_DCCP: u32 = 269;
pub const SOL_NETLINK: u32 = 270;
pub const SOL_TIPC: u32 = 271;
pub const SOL_RXRPC: u32 = 272;
pub const SOL_PPPOL2TP: u32 = 273;
pub const SOL_BLUETOOTH: u32 = 274;
pub const SOL_PNPIPE: u32 = 275;
pub const SOL_RDS: u32 = 276;
pub const SOL_IUCV: u32 = 277;
pub const SOL_CAIF: u32 = 278;
pub const SOL_ALG: u32 = 279;
pub const SOL_NFC: u32 = 280;
pub const SOL_KCM: u32 = 281;
pub const SOL_TLS: u32 = 282;
pub const SOL_XDP: u32 = 283;
pub const SOMAXCONN: u32 = 128;
pub const MSG_OOB: u32 = 1;
pub const MSG_PEEK: u32 = 2;
pub const MSG_DONTROUTE: u32 = 4;
pub const MSG_CTRUNC: u32 = 8;
pub const MSG_PROXY: u32 = 16;
pub const MSG_TRUNC: u32 = 32;
pub const MSG_DONTWAIT: u32 = 64;
pub const MSG_EOR: u32 = 128;
pub const MSG_WAITALL: u32 = 256;
pub const MSG_FIN: u32 = 512;
pub const MSG_SYN: u32 = 1024;
pub const MSG_CONFIRM: u32 = 2048;
pub const MSG_RST: u32 = 4096;
pub const MSG_ERRQUEUE: u32 = 8192;
pub const MSG_NOSIGNAL: u32 = 16384;
pub const MSG_MORE: u32 = 32768;
pub const MSG_WAITFORONE: u32 = 65536;
pub const MSG_BATCH: u32 = 262144;
pub const MSG_ZEROCOPY: u32 = 67108864;
pub const MSG_FASTOPEN: u32 = 536870912;
pub const MSG_CMSG_CLOEXEC: u32 = 1073741824;
pub const IPPROTO_IP: u32 = 0;
pub const IPPROTO_HOPOPTS: u32 = 0;
pub const IPPROTO_ICMP: u32 = 1;
pub const IPPROTO_IGMP: u32 = 2;
pub const IPPROTO_IPIP: u32 = 4;
pub const IPPROTO_TCP: u32 = 6;
pub const IPPROTO_EGP: u32 = 8;
pub const IPPROTO_PUP: u32 = 12;
pub const IPPROTO_UDP: u32 = 17;
pub const IPPROTO_IDP: u32 = 22;
pub const IPPROTO_TP: u32 = 29;
pub const IPPROTO_DCCP: u32 = 33;
pub const IPPROTO_IPV6: u32 = 41;
pub const IPPROTO_ROUTING: u32 = 43;
pub const IPPROTO_FRAGMENT: u32 = 44;
pub const IPPROTO_RSVP: u32 = 46;
pub const IPPROTO_GRE: u32 = 47;
pub const IPPROTO_ESP: u32 = 50;
pub const IPPROTO_AH: u32 = 51;
pub const IPPROTO_ICMPV6: u32 = 58;
pub const IPPROTO_NONE: u32 = 59;
pub const IPPROTO_DSTOPTS: u32 = 60;
pub const IPPROTO_MTP: u32 = 92;
pub const IPPROTO_BEETPH: u32 = 94;
pub const IPPROTO_ENCAP: u32 = 98;
pub const IPPROTO_PIM: u32 = 103;
pub const IPPROTO_COMP: u32 = 108;
pub const IPPROTO_SCTP: u32 = 132;
pub const IPPROTO_MH: u32 = 135;
pub const IPPROTO_UDPLITE: u32 = 136;
pub const IPPROTO_MPLS: u32 = 137;
pub const IPPROTO_RAW: u32 = 255;
pub const IPPROTO_MAX: u32 = 256;
pub const IP_TOS: u32 = 1;
pub const IP_TTL: u32 = 2;
pub const IP_HDRINCL: u32 = 3;
pub const IP_OPTIONS: u32 = 4;
pub const IP_ROUTER_ALERT: u32 = 5;
pub const IP_RECVOPTS: u32 = 6;
pub const IP_RETOPTS: u32 = 7;
pub const IP_PKTINFO: u32 = 8;
pub const IP_PKTOPTIONS: u32 = 9;
pub const IP_PMTUDISC: u32 = 10;
pub const IP_MTU_DISCOVER: u32 = 10;
pub const IP_RECVERR: u32 = 11;
pub const IP_RECVTTL: u32 = 12;
pub const IP_RECVTOS: u32 = 13;
pub const IP_MTU: u32 = 14;
pub const IP_FREEBIND: u32 = 15;
pub const IP_IPSEC_POLICY: u32 = 16;
pub const IP_XFRM_POLICY: u32 = 17;
pub const IP_PASSSEC: u32 = 18;
pub const IP_TRANSPARENT: u32 = 19;
pub const IP_ORIGDSTADDR: u32 = 20;
pub const IP_RECVORIGDSTADDR: u32 = 20;
pub const IP_MINTTL: u32 = 21;
pub const IP_NODEFRAG: u32 = 22;
pub const IP_CHECKSUM: u32 = 23;
pub const IP_BIND_ADDRESS_NO_PORT: u32 = 24;
pub const IP_MULTICAST_IF: u32 = 32;
pub const IP_MULTICAST_TTL: u32 = 33;
pub const IP_MULTICAST_LOOP: u32 = 34;
pub const IP_ADD_MEMBERSHIP: u32 = 35;
pub const IP_DROP_MEMBERSHIP: u32 = 36;
pub const IP_UNBLOCK_SOURCE: u32 = 37;
pub const IP_BLOCK_SOURCE: u32 = 38;
pub const IP_ADD_SOURCE_MEMBERSHIP: u32 = 39;
pub const IP_DROP_SOURCE_MEMBERSHIP: u32 = 40;
pub const IP_MSFILTER: u32 = 41;
pub const IP_MULTICAST_ALL: u32 = 49;
pub const IP_UNICAST_IF: u32 = 50;
pub const IP_RECVRETOPTS: u32 = 7;
pub const IP_PMTUDISC_DONT: u32 = 0;
pub const IP_PMTUDISC_WANT: u32 = 1;
pub const IP_PMTUDISC_DO: u32 = 2;
pub const IP_PMTUDISC_PROBE: u32 = 3;
pub const IP_PMTUDISC_INTERFACE: u32 = 4;
pub const IP_PMTUDISC_OMIT: u32 = 5;
pub const IP_DEFAULT_MULTICAST_TTL: u32 = 1;
pub const IP_DEFAULT_MULTICAST_LOOP: u32 = 1;
pub const IP_MAX_MEMBERSHIPS: u32 = 20;
pub const IPV6_ADDRFORM: u32 = 1;
pub const IPV6_2292PKTINFO: u32 = 2;
pub const IPV6_2292HOPOPTS: u32 = 3;
pub const IPV6_2292DSTOPTS: u32 = 4;
pub const IPV6_2292RTHDR: u32 = 5;
pub const IPV6_2292PKTOPTIONS: u32 = 6;
pub const IPV6_CHECKSUM: u32 = 7;
pub const IPV6_2292HOPLIMIT: u32 = 8;
pub const IPV6_NEXTHOP: u32 = 9;
pub const IPV6_AUTHHDR: u32 = 10;
pub const IPV6_UNICAST_HOPS: u32 = 16;
pub const IPV6_MULTICAST_IF: u32 = 17;
pub const IPV6_MULTICAST_HOPS: u32 = 18;
pub const IPV6_MULTICAST_LOOP: u32 = 19;
pub const IPV6_JOIN_GROUP: u32 = 20;
pub const IPV6_LEAVE_GROUP: u32 = 21;
pub const IPV6_ROUTER_ALERT: u32 = 22;
pub const IPV6_MTU_DISCOVER: u32 = 23;
pub const IPV6_MTU: u32 = 24;
pub const IPV6_RECVERR: u32 = 25;
pub const IPV6_V6ONLY: u32 = 26;
pub const IPV6_JOIN_ANYCAST: u32 = 27;
pub const IPV6_LEAVE_ANYCAST: u32 = 28;
pub const IPV6_IPSEC_POLICY: u32 = 34;
pub const IPV6_XFRM_POLICY: u32 = 35;
pub const IPV6_HDRINCL: u32 = 36;
pub const IPV6_RECVPKTINFO: u32 = 49;
pub const IPV6_PKTINFO: u32 = 50;
pub const IPV6_RECVHOPLIMIT: u32 = 51;
pub const IPV6_HOPLIMIT: u32 = 52;
pub const IPV6_RECVHOPOPTS: u32 = 53;
pub const IPV6_HOPOPTS: u32 = 54;
pub const IPV6_RTHDRDSTOPTS: u32 = 55;
pub const IPV6_RECVRTHDR: u32 = 56;
pub const IPV6_RTHDR: u32 = 57;
pub const IPV6_RECVDSTOPTS: u32 = 58;
pub const IPV6_DSTOPTS: u32 = 59;
pub const IPV6_RECVPATHMTU: u32 = 60;
pub const IPV6_PATHMTU: u32 = 61;
pub const IPV6_DONTFRAG: u32 = 62;
pub const IPV6_RECVTCLASS: u32 = 66;
pub const IPV6_TCLASS: u32 = 67;
pub const IPV6_AUTOFLOWLABEL: u32 = 70;
pub const IPV6_ADDR_PREFERENCES: u32 = 72;
pub const IPV6_MINHOPCOUNT: u32 = 73;
pub const IPV6_ORIGDSTADDR: u32 = 74;
pub const IPV6_RECVORIGDSTADDR: u32 = 74;
pub const IPV6_TRANSPARENT: u32 = 75;
pub const IPV6_UNICAST_IF: u32 = 76;
pub const IPV6_ADD_MEMBERSHIP: u32 = 20;
pub const IPV6_DROP_MEMBERSHIP: u32 = 21;
pub const IPV6_RXHOPOPTS: u32 = 54;
pub const IPV6_RXDSTOPTS: u32 = 59;
pub const IPV6_PMTUDISC_DONT: u32 = 0;
pub const IPV6_PMTUDISC_WANT: u32 = 1;
pub const IPV6_PMTUDISC_DO: u32 = 2;
pub const IPV6_PMTUDISC_PROBE: u32 = 3;
pub const IPV6_PMTUDISC_INTERFACE: u32 = 4;
pub const IPV6_PMTUDISC_OMIT: u32 = 5;
pub const IPV6_PREFER_SRC_TMP: u32 = 1;
pub const IPV6_PREFER_SRC_PUBLIC: u32 = 2;
pub const IPV6_PREFER_SRC_PUBTMP_DEFAULT: u32 = 256;
pub const IPV6_PREFER_SRC_COA: u32 = 4;
pub const IPV6_PREFER_SRC_HOME: u32 = 1024;
pub const IPV6_PREFER_SRC_CGA: u32 = 8;
pub const IPV6_PREFER_SRC_NONCGA: u32 = 2048;
pub const IPV6_RTHDR_LOOSE: u32 = 0;
pub const IPV6_RTHDR_STRICT: u32 = 1;
pub const IPV6_RTHDR_TYPE_0: u32 = 0;
pub type zx_rights_t = u32;
pub type time_t = ::std::os::raw::c_long;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct timespec {
    pub tv_sec: time_t,
    pub tv_nsec: ::std::os::raw::c_long,
}
pub type zx_time_t = i64;
pub type zx_handle_t = u32;
pub type zx_status_t = i32;
pub type zx_signals_t = u32;
pub type zx_koid_t = u64;
pub type zx_off_t = u64;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct zx_iovec {
    pub buffer: *mut ::std::os::raw::c_void,
    pub capacity: usize,
}
impl Default for zx_iovec {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type zx_iovec_t = zx_iovec;
pub type zx_obj_type_t = u32;
pub type zxio_flags_t = u32;
pub type zxio_vmo_flags_t = u32;
pub type zxio_signals_t = u32;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct zxio_tag {
    pub reserved: [u64; 4usize],
}
pub type zxio_t = zxio_tag;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct zxio_private {
    pub reserved: [u64; 29usize],
}
pub type zxio_private_t = zxio_private;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct zxio_storage {
    pub io: zxio_t,
    pub reserved: zxio_private_t,
}
pub type zxio_storage_t = zxio_storage;
pub type zxio_object_type_t = u32;
pub type zxio_storage_alloc = ::std::option::Option<
    unsafe extern "C" fn(
        type_: zxio_object_type_t,
        out_storage: *mut *mut zxio_storage_t,
        out_context: *mut *mut ::std::os::raw::c_void,
    ) -> zx_status_t,
>;
pub type zxio_node_protocols_t = u64;
pub type zxio_id_t = u64;
pub type zxio_operations_t = u64;
pub type zxio_abilities_t = zxio_operations_t;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct zxio_node_attr {
    pub protocols: zxio_node_protocols_t,
    pub abilities: zxio_abilities_t,
    pub id: zxio_id_t,
    pub content_size: u64,
    pub storage_size: u64,
    pub link_count: u64,
    pub creation_time: u64,
    pub modification_time: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub __bindgen_padding_0: [u8; 4usize],
    pub rdev: u64,
    pub has: zxio_node_attr_zxio_node_attr_has_t,
    pub __bindgen_padding_1: [u8; 4usize],
}
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct zxio_node_attr_zxio_node_attr_has_t {
    pub protocols: bool,
    pub abilities: bool,
    pub id: bool,
    pub content_size: bool,
    pub storage_size: bool,
    pub link_count: bool,
    pub creation_time: bool,
    pub modification_time: bool,
    pub mode: bool,
    pub uid: bool,
    pub gid: bool,
    pub rdev: bool,
}
pub type zxio_node_attributes_t = zxio_node_attr;
pub type zxio_seek_origin_t = u32;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct zxio_dirent_iterator {
    pub io: *mut zxio_t,
    pub opaque: [u8; 65584usize],
}
impl Default for zxio_dirent_iterator {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type zxio_dirent_iterator_t = zxio_dirent_iterator;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct zxio_dirent {
    pub protocols: zxio_node_protocols_t,
    pub abilities: zxio_abilities_t,
    pub id: zxio_id_t,
    pub has: zxio_dirent_zxio_dirent_has_t,
    pub name_length: u8,
    pub __bindgen_padding_0: [u8; 4usize],
    pub name: *mut ::std::os::raw::c_char,
}
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct zxio_dirent_zxio_dirent_has_t {
    pub protocols: bool,
    pub abilities: bool,
    pub id: bool,
}
impl Default for zxio_dirent {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type zxio_dirent_t = zxio_dirent;
pub type zxio_shutdown_options_t = u32;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct zxio_open_options {
    pub protocols: zxio_node_protocols_t,
    pub optional_rights: u64,
    pub file_flags: u64,
    pub node_flags: u64,
    pub mode: u32,
    pub __bindgen_padding_0: [u8; 4usize],
    pub rights: u64,
    pub create_attr: *const zxio_node_attributes_t,
}
impl Default for zxio_open_options {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type zxio_open_options_t = zxio_open_options;
pub type zxio_xattr_set_mode_t = u32;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct zxio_xattr_data {
    pub data: *mut ::std::os::raw::c_void,
    pub vmo: zx_handle_t,
    pub __bindgen_padding_0: [u8; 4usize],
    pub len: usize,
}
impl Default for zxio_xattr_data {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type zxio_xattr_data_t = zxio_xattr_data;
pub type va_list = __builtin_va_list;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct zx_info_handle_basic {
    pub koid: zx_koid_t,
    pub rights: zx_rights_t,
    pub type_: zx_obj_type_t,
    pub related_koid: zx_koid_t,
    pub reserved: u32,
    pub padding1: [u8; 4usize],
}
pub type zx_info_handle_basic_t = zx_info_handle_basic;
extern "C" {
    pub fn zxio_create(handle: zx_handle_t, storage: *mut zxio_storage_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_create_with_on_open(
        handle: zx_handle_t,
        storage: *mut zxio_storage_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_create_with_on_representation(
        handle: zx_handle_t,
        inout_attr: *mut zxio_node_attributes_t,
        storage: *mut zxio_storage_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_create_with_info(
        handle: zx_handle_t,
        handle_info: *const zx_info_handle_basic_t,
        storage: *mut zxio_storage_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_create_with_type(
        storage: *mut zxio_storage_t,
        type_: zxio_object_type_t,
        ...
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_close(io: *mut zxio_t, should_wait: bool) -> zx_status_t;
}
extern "C" {
    pub fn zxio_release(io: *mut zxio_t, out_handle: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_borrow(io: *mut zxio_t, out_handle: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_clone(io: *mut zxio_t, out_handle: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_wait_one(
        io: *mut zxio_t,
        signals: zxio_signals_t,
        deadline: zx_time_t,
        out_observed: *mut zxio_signals_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_wait_begin(
        io: *mut zxio_t,
        zxio_signals: zxio_signals_t,
        out_handle: *mut zx_handle_t,
        out_zx_signals: *mut zx_signals_t,
    );
}
extern "C" {
    pub fn zxio_wait_end(
        io: *mut zxio_t,
        zx_signals: zx_signals_t,
        out_zxio_signals: *mut zxio_signals_t,
    );
}
extern "C" {
    pub fn zxio_sync(io: *mut zxio_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_attr_get(io: *mut zxio_t, out_attr: *mut zxio_node_attributes_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_attr_set(io: *mut zxio_t, attr: *const zxio_node_attributes_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_read(
        io: *mut zxio_t,
        buffer: *mut ::std::os::raw::c_void,
        capacity: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_read_at(
        io: *mut zxio_t,
        offset: zx_off_t,
        buffer: *mut ::std::os::raw::c_void,
        capacity: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_write(
        io: *mut zxio_t,
        buffer: *const ::std::os::raw::c_void,
        capacity: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_write_at(
        io: *mut zxio_t,
        offset: zx_off_t,
        buffer: *const ::std::os::raw::c_void,
        capacity: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_readv(
        io: *mut zxio_t,
        vector: *const zx_iovec_t,
        vector_count: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_readv_at(
        io: *mut zxio_t,
        offset: zx_off_t,
        vector: *const zx_iovec_t,
        vector_count: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_writev(
        io: *mut zxio_t,
        vector: *const zx_iovec_t,
        vector_count: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_writev_at(
        io: *mut zxio_t,
        offset: zx_off_t,
        vector: *const zx_iovec_t,
        vector_count: usize,
        flags: zxio_flags_t,
        out_actual: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_seek(
        io: *mut zxio_t,
        start: zxio_seek_origin_t,
        offset: i64,
        out_offset: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_truncate(io: *mut zxio_t, length: u64) -> zx_status_t;
}
extern "C" {
    pub fn zxio_flags_get(io: *mut zxio_t, out_flags: *mut u32) -> zx_status_t;
}
extern "C" {
    pub fn zxio_flags_set(io: *mut zxio_t, flags: u32) -> zx_status_t;
}
extern "C" {
    pub fn zxio_token_get(io: *mut zxio_t, out_token: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_vmo_get(
        io: *mut zxio_t,
        flags: zxio_vmo_flags_t,
        out_vmo: *mut zx_handle_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_on_mapped(io: *mut zxio_t, ptr: *mut ::std::os::raw::c_void) -> zx_status_t;
}
extern "C" {
    pub fn zxio_vmo_get_copy(io: *mut zxio_t, out_vmo: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_vmo_get_clone(io: *mut zxio_t, out_vmo: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_vmo_get_exact(io: *mut zxio_t, out_vmo: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_vmo_get_exec(io: *mut zxio_t, out_vmo: *mut zx_handle_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_get_read_buffer_available(
        io: *mut zxio_t,
        out_available: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_shutdown(
        io: *mut zxio_t,
        options: zxio_shutdown_options_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_open(
        directory: *mut zxio_t,
        flags: u32,
        path: *const ::std::os::raw::c_char,
        path_len: usize,
        storage: *mut zxio_storage_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_open2(
        directory: *mut zxio_t,
        path: *const ::std::os::raw::c_char,
        path_len: usize,
        options: *const zxio_open_options_t,
        inout_attr: *mut zxio_node_attributes_t,
        storage: *mut zxio_storage_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_open_async(
        directory: *mut zxio_t,
        flags: u32,
        path: *const ::std::os::raw::c_char,
        path_len: usize,
        request: zx_handle_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_unlink(
        directory: *mut zxio_t,
        name: *const ::std::os::raw::c_char,
        name_len: usize,
        flags: ::std::os::raw::c_int,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_rename(
        old_directory: *mut zxio_t,
        old_path: *const ::std::os::raw::c_char,
        old_path_len: usize,
        new_directory_token: zx_handle_t,
        new_path: *const ::std::os::raw::c_char,
        new_path_len: usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_link(
        src_directory: *mut zxio_t,
        src_path: *const ::std::os::raw::c_char,
        src_path_len: usize,
        dst_directory_token: zx_handle_t,
        dst_path: *const ::std::os::raw::c_char,
        dst_path_len: usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_link_into(
        object: *mut zxio_t,
        dst_directory_token: zx_handle_t,
        dst_path: *const ::std::os::raw::c_char,
        dst_path_len: usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_dirent_iterator_init(
        iterator: *mut zxio_dirent_iterator_t,
        directory: *mut zxio_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_dirent_iterator_next(
        iterator: *mut zxio_dirent_iterator_t,
        inout_entry: *mut zxio_dirent_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_dirent_iterator_rewind(iterator: *mut zxio_dirent_iterator_t) -> zx_status_t;
}
extern "C" {
    pub fn zxio_dirent_iterator_destroy(iterator: *mut zxio_dirent_iterator_t);
}
extern "C" {
    pub fn zxio_isatty(io: *mut zxio_t, tty: *mut bool) -> zx_status_t;
}
extern "C" {
    pub fn zxio_get_window_size(io: *mut zxio_t, width: *mut u32, height: *mut u32) -> zx_status_t;
}
extern "C" {
    pub fn zxio_set_window_size(io: *mut zxio_t, width: u32, height: u32) -> zx_status_t;
}
extern "C" {
    pub fn zxio_ioctl(
        io: *mut zxio_t,
        request: ::std::os::raw::c_int,
        out_code: *mut i16,
        va: *mut __va_list_tag,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_read_link(
        io: *mut zxio_t,
        out_target: *mut *const u8,
        out_target_len: *mut usize,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_create_symlink(
        io: *mut zxio_t,
        name: *const ::std::os::raw::c_char,
        name_len: usize,
        target: *const u8,
        target_len: usize,
        storage: *mut zxio_storage_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_xattr_list(
        io: *mut zxio_t,
        callback: ::std::option::Option<
            unsafe extern "C" fn(
                context: *mut ::std::os::raw::c_void,
                name: *const u8,
                name_len: usize,
            ),
        >,
        context: *mut ::std::os::raw::c_void,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_xattr_get(
        io: *mut zxio_t,
        name: *const u8,
        name_len: usize,
        callback: ::std::option::Option<
            unsafe extern "C" fn(
                context: *mut ::std::os::raw::c_void,
                data: zxio_xattr_data_t,
            ) -> zx_status_t,
        >,
        context: *mut ::std::os::raw::c_void,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_xattr_set(
        io: *mut zxio_t,
        name: *const u8,
        name_len: usize,
        value: *const u8,
        value_len: usize,
        mode: zxio_xattr_set_mode_t,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_xattr_remove(io: *mut zxio_t, name: *const u8, name_len: usize) -> zx_status_t;
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct iovec {
    pub iov_base: *mut ::std::os::raw::c_void,
    pub iov_len: usize,
}
impl Default for iovec {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type socklen_t = ::std::os::raw::c_uint;
pub type sa_family_t = ::std::os::raw::c_ushort;
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct msghdr {
    pub msg_name: *mut ::std::os::raw::c_void,
    pub msg_namelen: socklen_t,
    pub __bindgen_padding_0: [u8; 4usize],
    pub msg_iov: *mut iovec,
    pub msg_iovlen: ::std::os::raw::c_int,
    pub __bindgen_padding_1: [u8; 4usize],
    pub msg_control: *mut ::std::os::raw::c_void,
    pub msg_controllen: socklen_t,
    pub msg_flags: ::std::os::raw::c_int,
}
impl Default for msghdr {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct cmsghdr {
    pub cmsg_len: socklen_t,
    pub cmsg_level: ::std::os::raw::c_int,
    pub cmsg_type: ::std::os::raw::c_int,
}
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct sockaddr {
    pub sa_family: sa_family_t,
    pub sa_data: [::std::os::raw::c_char; 14usize],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sockaddr_storage {
    pub ss_family: sa_family_t,
    pub __ss_padding: [::std::os::raw::c_char; 118usize],
    pub __ss_align: ::std::os::raw::c_ulong,
}
impl Default for sockaddr_storage {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type zxio_service_connector = ::std::option::Option<
    unsafe extern "C" fn(
        service_name: *const ::std::os::raw::c_char,
        provider_handle: *mut zx_handle_t,
    ) -> zx_status_t,
>;
extern "C" {
    pub fn zxio_socket(
        service_connector: zxio_service_connector,
        domain: ::std::os::raw::c_int,
        type_: ::std::os::raw::c_int,
        protocol: ::std::os::raw::c_int,
        allocator: zxio_storage_alloc,
        out_context: *mut *mut ::std::os::raw::c_void,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_bind(
        io: *mut zxio_t,
        addr: *const sockaddr,
        addrlen: socklen_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_connect(
        io: *mut zxio_t,
        addr: *const sockaddr,
        addrlen: socklen_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_listen(
        io: *mut zxio_t,
        backlog: ::std::os::raw::c_int,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_accept(
        io: *mut zxio_t,
        addr: *mut sockaddr,
        addrlen: *mut socklen_t,
        out_storage: *mut zxio_storage_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_getsockname(
        io: *mut zxio_t,
        addr: *mut sockaddr,
        addrlen: *mut socklen_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_getpeername(
        io: *mut zxio_t,
        addr: *mut sockaddr,
        addrlen: *mut socklen_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_getsockopt(
        io: *mut zxio_t,
        level: ::std::os::raw::c_int,
        optname: ::std::os::raw::c_int,
        optval: *mut ::std::os::raw::c_void,
        optlen: *mut socklen_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_setsockopt(
        io: *mut zxio_t,
        level: ::std::os::raw::c_int,
        optname: ::std::os::raw::c_int,
        optval: *const ::std::os::raw::c_void,
        optlen: socklen_t,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_recvmsg(
        io: *mut zxio_t,
        msg: *mut msghdr,
        flags: ::std::os::raw::c_int,
        out_actual: *mut usize,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_sendmsg(
        io: *mut zxio_t,
        msg: *const msghdr,
        flags: ::std::os::raw::c_int,
        out_actual: *mut usize,
        out_code: *mut i16,
    ) -> zx_status_t;
}
extern "C" {
    pub fn zxio_get_posix_mode(
        protocols: zxio_node_protocols_t,
        abilities: zxio_abilities_t,
    ) -> u32;
}
pub type in_port_t = u16;
pub type in_addr_t = u32;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct in_addr {
    pub s_addr: in_addr_t,
}
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct sockaddr_in {
    pub sin_family: sa_family_t,
    pub sin_port: in_port_t,
    pub sin_addr: in_addr,
    pub sin_zero: [u8; 8usize],
}
#[repr(C)]
#[derive(Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct in6_addr {
    pub __in6_union: in6_addr__bindgen_ty_1,
}
#[repr(C)]
#[derive(Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub union in6_addr__bindgen_ty_1 {
    pub __s6_addr: [u8; 16usize],
    pub __s6_addr16: [u16; 8usize],
    pub __s6_addr32: [u32; 4usize],
}
impl Default for in6_addr__bindgen_ty_1 {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
impl Default for in6_addr {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
#[repr(C)]
#[derive(Copy, Clone)]
pub struct sockaddr_in6 {
    pub sin6_family: sa_family_t,
    pub sin6_port: in_port_t,
    pub sin6_flowinfo: u32,
    pub sin6_addr: in6_addr,
    pub sin6_scope_id: u32,
}
impl Default for sockaddr_in6 {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
#[repr(C)]
#[derive(Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct in6_pktinfo {
    pub ipi6_addr: in6_addr,
    pub ipi6_ifindex: ::std::os::raw::c_uint,
}
impl Default for in6_pktinfo {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
pub type suseconds_t = ::std::os::raw::c_long;
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct timeval {
    pub tv_sec: time_t,
    pub tv_usec: suseconds_t,
}
pub const ZXIO_SHUTDOWN_OPTIONS_READ: zxio_shutdown_options_t = 2;
pub const ZXIO_SHUTDOWN_OPTIONS_WRITE: zxio_shutdown_options_t = 1;
pub const ZXIO_NODE_PROTOCOL_NONE: zxio_node_protocols_t = 0;
pub const ZXIO_NODE_PROTOCOL_CONNECTOR: zxio_node_protocols_t = 1;
pub const ZXIO_NODE_PROTOCOL_DIRECTORY: zxio_node_protocols_t = 2;
pub const ZXIO_NODE_PROTOCOL_FILE: zxio_node_protocols_t = 4;
pub const ZXIO_NODE_PROTOCOL_SYMLINK: zxio_node_protocols_t = 8;
pub const ZXIO_SEEK_ORIGIN_START: zxio_seek_origin_t = 0;
pub const ZXIO_SEEK_ORIGIN_CURRENT: zxio_seek_origin_t = 1;
pub const ZXIO_SEEK_ORIGIN_END: zxio_seek_origin_t = 2;
pub type __builtin_va_list = [__va_list_tag; 1usize];
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct __va_list_tag {
    pub gp_offset: ::std::os::raw::c_uint,
    pub fp_offset: ::std::os::raw::c_uint,
    pub overflow_arg_area: *mut ::std::os::raw::c_void,
    pub reg_save_area: *mut ::std::os::raw::c_void,
}
impl Default for __va_list_tag {
    fn default() -> Self {
        let mut s = ::std::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            ::std::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}
