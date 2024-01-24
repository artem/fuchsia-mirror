// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DRIVER_SYMBOLS_RESTRICTED_SYMBOLS_H_
#define SRC_LIB_DRIVER_SYMBOLS_RESTRICTED_SYMBOLS_H_

#include <set>

namespace driver_symbols {

//  ________  _________  ________  ________
// |\   ____\|\___   ___\\   __  \|\   __  \
// \ \  \___|\|___ \  \_\ \  \|\  \ \  \|\  \
//  \ \_____  \   \ \  \ \ \  \\\  \ \   ____\
//   \|____|\  \   \ \  \ \ \  \\\  \ \  \___|
//     ____\_\  \   \ \__\ \ \_______\ \__\
//    |\_________\   \|__|  \|_______|\|__|
//    \|_________|
//
// The relative component URLs for drivers which are allowed to spawn custom threads.
//
// Drivers should avoid spawning their own threads if possible and instead utilize
// driver runtime dispatchers.
//
// See https://fuchsia.dev/fuchsia-src/concepts/drivers/driver-dispatcher-and-threads
// or reach out to the Fuchsia Driver Framework team.
//
// To remove items from the allowlist, please send a change to one of the OWNERS of
// this file to remove an element from the list below.
const std::set<std::string> kCreateThreadSymbolsDriversAllowlist = {
    "#meta/acpi-arm64.cm",
    "#meta/ahci.cm",
    "#meta/aml-g12-pdm-dsp.cm",
    "#meta/aml-g12-pdm.cm",
    "#meta/aml-g12-tdm-dai.cm",
    "#meta/aml-g12-tdm-dsp.cm",
    "#meta/aml-g12-tdm.cm",
    "#meta/aml_mipicsi.cm",
    "#meta/amlogic-display.cm",
    "#meta/amlogic_h264_encoder_driver.cm",
    "#meta/amlogic_hevc_encoder_driver.cm",
    "#meta/amlogic_video_decoder.cm",
    "#meta/aml-ram.cm",
    "#meta/aml-securemem.cm",
    "#meta/aml-thermal-s905d2g-legacy.cm",
    "#meta/aml-thermal.cm",
    "#meta/aml-usb-crg-phy-v2.cm",
    "#meta/aml-usb-phy-v2.cm",
    "#meta/ams-light.cm",
    "#meta/arm_mali_iv009_isp.cm",
    "#meta/asix-88179.cm",
    "#meta/asix-88772b.cm",
    "#meta/astro.cm",
    "#meta/at-fake-transport.cm",
    "#meta/block.core.cm",
    "#meta/brcmfmac.cm",
    "#meta/bt-hci-atheros.cm",
    "#meta/bt-hci-broadcom.cm",
    "#meta/bt-hci-intel.cm",
    "#meta/bt-hci-marvell.cm",
    "#meta/bt-hci-virtual.cm",
    "#meta/bt-host.cm",
    "#meta/bt-transport-uart.cm",
    "#meta/bt-transport-usb.cm",
    "#meta/bus-pci.cm",
    "#meta/chromiumos-ec-core.cm",
    "#meta/chromiumos-ec-lpc.cm",
    "#meta/compat.cm",
    "#meta/cr50-spi-driver.cm",
    "#meta/crg-udc.cm",
    "#meta/cypress.cm",
    "#meta/display-coordinator.cm",
    "#meta/dwc2.cm",
    "#meta/dwc3.cm",
    "#meta/dwmac.cm",
    "#meta/e1000.cm",
    "#meta/fake-ot-radio.cm",
    "#meta/fake-usb-cdc-acm.cm",
    "#meta/fidl-async-llcpp-driver.cm",
    "#meta/focaltech-sherlock.cm",
    "#meta/focaltech.cm",
    "#meta/ftdi-fake-usb.cm",
    "#meta/ftdi.cm",
    "#meta/ftl.cm",
    "#meta/fusb302.cm",
    "#meta/fvm.cm",
    "#meta/gdc.cm",
    "#meta/ge2d.cm",
    "#meta/goldfish_control.cm",
    "#meta/goldfish-display.cm",
    "#meta/goldfish_sensor.cm",
    "#meta/goldfish.cm",
    "#meta/goldfish_sync.cm",
    "#meta/goodix.cm",
    "#meta/gt6853.cm",
    "#meta/gvnic.cm",
    "#meta/hdmi-audio-codec.cm",
    "#meta/buttons.cm",
    "#meta/hidctl.cm",
    "#meta/hid-input-report.cm",
    "#meta/hid-input-report-v2.cm",
    "#meta/hid.cm",
    "#meta/i2c-hid.cm",
    "#meta/igc.cm",
    "#meta/init-test.cm",
    "#meta/integration-test.cm",
    "#meta/intel-gspi.cm",
    "#meta/intel-hda.cm",
    "#meta/intel-i2c.cm",
    "#meta/intel-i915.cm",
    "#meta/intel-spi-flash.cm",
    "#meta/iwlwifi.cm",
    "#meta/msd_arm_dfv2.cm",
    "#meta/msd_arm.cm",
    "#meta/msd_arm_rebind.cm",
    "#meta/msd_arm_rebind_dfv2.cm",
    "#meta/msd_arm_test.cm",
    "#meta/msd_arm_test_dfv2.cm",
    "#meta/libmsd_intel.cm",
    "#meta/libmsd_intel_test.cm",
    "#meta/libmsd_vsi.cm",
    "#meta/libmsd_vsi_test.cm",
    "#meta/machina.cm",
    "#meta/max98373.cm",
    "#meta/mock-device.cm",
    "#meta/motmot.cm",
    "#meta/nand.cm",
    "#meta/nelson-brownout-protection.cm",
    "#meta/nelson-module.cm",
    "#meta/nelson-usb-phy.cm",
    "#meta/network-device.cm",
    "#meta/nvme.cm",
    "#meta/nxpfmac_sdio.cm",
    "#meta/optee.cm",
    "#meta/ot-radio-luis-mfg.cm",
    "#meta/ot-radio-luis.cm",
    "#meta/ot-radio-nelson-mfg.cm",
    "#meta/ot-radio-nelson.cm",
    "#meta/ot-radio-sherlock-mfg.cm",
    "#meta/ot-radio-sherlock.cm",
    "#meta/overnet-usb.cm",
    "#meta/pc-ps2.cm",
    "#meta/pci_protocol_test_driver.cm",
    "#meta/platform-bus-x86.cm",
    "#meta/qemu-arm64.cm",
    "#meta/qemu-audio-codec.cm",
    "#meta/qmi-fake-transport.cm",
    "#meta/qmi-transport.cm",
    "#meta/ramdisk.cm",
    "#meta/ram-nand.cm",
    "#meta/realtek-audio-codec.cm",
    "#meta/rndis-function.cm",
    "#meta/rndis-host.cm",
    "#meta/sdhci.cm",
    "#meta/sdmmc.cm",
    "#meta/selina_radar.cm",
    "#meta/serial-async.cm",
    "#meta/sherlock-camera-controller_driver_component.cm",
    "#meta/simple.amd-kaveri.cm",
    "#meta/simple.bochs.cm",
    "#meta/simple.gga.cm",
    "#meta/simple.intel.cm",
    "#meta/simple.nv.cm",
    "#meta/simple.vmware.cm",
    "#meta/ssd1306_driver.cm",
    "#meta/sysmem.cm",
    "#meta/tas27xx.cm",
    "#meta/tas5707.cm",
    "#meta/tas5720.cm",
    "#meta/tas58xx.cm",
    "#meta/test-board.cm",
    "#meta/test-codec.cm",
    "#meta/test-codec2.cm",
    "#meta/test-dai.cm",
    "#meta/ti-ina231.cm",
    "#meta/tpm-driver.cm",
    "#meta/tpm-lpc.cm",
    "#meta/uart16550.cm",
    "#meta/ufs.cm",
    "#meta/ums-function.cm",
    "#meta/usb-adb-function.cm",
    "#meta/usb-audio.cm",
    "#meta/usb-bus.cm",
    "#meta/usb-cdc-acm.cm",
    "#meta/usb-cdc-ecm.cm",
    "#meta/usb-cdc-function.cm",
    "#meta/usb-hci-test-driver.cm",
    "#meta/usb-hid.cm",
    "#meta/usb-hub.cm",
    "#meta/usb-mass-storage.cm",
    "#meta/usb-peripheral.cm",
    "#meta/usb-two-endpoint-hid-fake-usb.cm",
    "#meta/usb_video.cm",
    "#meta/usb-virtual-bus.cm",
    "#meta/vim3-usb-phy.cm",
    "#meta/virtio_block.cm",
    "#meta/virtio_console.cm",
    "#meta/virtio_gpu.cm",
    "#meta/virtio_input.cm",
    "#meta/virtio_netdevice.cm",
    "#meta/virtio_rng.cm",
    "#meta/virtio_scsi.cm",
    "#meta/virtio_socket.cm",
    "#meta/virtual_audio_driver.cm",
    "#meta/virtual-bus-tester.cm",
    "#meta/wlanif.cm",
    "#meta/wlansoftmac.cm",
    "#meta/xhci.cm",
    "#meta/zxcrypt.cm",
};

// The set of restricted symbols for creating custom threads which are only allowed for drivers in
// |kCreateThreadSymbolsDriversAllowlist|.
const std::set<std::string> kCreateThreadSymbols = {
    "thrd_create",
    "thrd_create_with_name",
    "pthread_create",
};

const std::set<std::string> kRestrictedLibcSymbols = {
    "a64l",
    "accept",
    "accept4",
    "access",
    "acct",
    "acos",
    "acosf",
    "acosh",
    "acoshf",
    "acoshl",
    "acosl",
    "adjtime",
    "alarm",
    "alphasort",
    "asctime",
    "asctime_r",
    "asin",
    "asinf",
    "asinh",
    "asinhf",
    "asinhl",
    "asinl",
    "asprintf",
    "atan",
    "atan2",
    "atan2f",
    "atan2l",
    "atanf",
    "atanh",
    "atanhf",
    "atanhl",
    "atanl",
    "atexit",
    "atof",
    "atol",
    "atoll",
    "at_quick_exit",
    "basename",
    "bcmp",
    "bcopy",
    "bind",
    "bsd_signal",
    "btowc",
    "bzero",
    "c16rtomb",
    "c32rtomb",
    "cabs",
    "cabsf",
    "cabsl",
    "cacos",
    "cacosf",
    "cacosh",
    "cacoshf",
    "cacoshl",
    "cacosl",
    "call_once",
    "carg",
    "cargf",
    "cargl",
    "casin",
    "casinf",
    "casinh",
    "casinhf",
    "casinhl",
    "casinl",
    "catan",
    "catanf",
    "catanh",
    "catanhf",
    "catanhl",
    "catanl",
    "catclose",
    "catgets",
    "catopen",
    "cbrt",
    "cbrtf",
    "cbrtl",
    "ccos",
    "ccosf",
    "ccosh",
    "ccoshf",
    "ccoshl",
    "ccosl",
    "__c_dot_utf8",
    "__c_dot_utf8_locale",
    "ceill",
    "cexp",
    "cexpf",
    "cexpl",
    "cfgetispeed",
    "cfgetospeed",
    "cfmakeraw",
    "cfsetispeed",
    "cfsetospeed",
    "cfsetspeed",
    "chdir",
    "chmod",
    "chown",
    "chroot",
    "cimag",
    "cimagf",
    "cimagl",
    "clearenv",
    "clearerr",
    "clearerr_unlocked",
    "__c_locale",
    "clock",
    "clock_getcpuclockid",
    "clock_getres",
    "clock_nanosleep",
    "__clock_settime",
    "clock_settime",
    "clog",
    "clogf",
    "clogl",
    "closelog",
    "cnd_timedwait",
    "confstr",
    "conj",
    "conjf",
    "conjl",
    "connect",
    "copysign",
    "copysignf",
    "copysignl",
    "__cos",
    "cos",
    "__cosdf",
    "cosh",
    "coshf",
    "coshl",
    "__cosl",
    "cosl",
    "cpow",
    "cpowf",
    "cpowl",
    "cproj",
    "cprojf",
    "cprojl",
    "creal",
    "crealf",
    "creall",
    "creat",
    "csin",
    "csinf",
    "csinh",
    "csinhf",
    "csinhl",
    "csinl",
    "csqrt",
    "csqrtf",
    "csqrtl",
    "ctan",
    "ctanf",
    "ctanh",
    "ctanhf",
    "ctanhl",
    "ctanl",
    "ctermid",
    "ctime",
    "ctime_r",
    "__cxa_finalize",
    "daylight",
    "difftime",
    "dirname",
    "div",
    "dl_clone_loader_service",
    "_dl_debug_addr",
    "dlinfo",
    "dlopen",
    "__dn_comp",
    "dn_comp",
    "__dn_expand",
    "dn_expand",
    "dn_skipname",
    "__dns_parse",
    "dprintf",
    "drand48",
    "drem",
    "dremf",
    "dup",
    "dup3",
    "__duplocale",
    "duplocale",
    "eaccess",
    "ecvt",
    "endgrent",
    "endhostent",
    "endnetent",
    "endprotoent",
    "endpwent",
    "endservent",
    "___environ",
    "__environ",
    "_environ",
    "environ",
    "erand48",
    "erf",
    "erfc",
    "erfcf",
    "erfcl",
    "erff",
    "erfl",
    "err",
    "errx",
    "ether_aton",
    "ether_aton_r",
    "ether_hostton",
    "ether_line",
    "ether_ntoa",
    "ether_ntoa_r",
    "ether_ntohost",
    "euidaccess",
    "eventfd",
    "eventfd_read",
    "eventfd_write",
    "execl",
    "execle",
    "execlp",
    "execv",
    "execve",
    "execvp",
    "__execvpe",
    "execvpe",
    "_Exit",
    "exp",
    "exp10",
    "exp10f",
    "exp10l",
    "exp2",
    "exp2f",
    "exp2l",
    "expf",
    "expl",
    "expm1",
    "expm1f",
    "expm1l",
    "__expo2",
    "__expo2f",
    "fabs",
    "fabsf",
    "fabsl",
    "faccessat",
    "fchmod",
    "fchmodat",
    "fchown",
    "fchownat",
    "fcvt",
    "fdatasync",
    "_fd_get_context",
    "fdim",
    "fdimf",
    "fdiml",
    "fdopendir",
    "_fd_open_max",
    "_fd_release_context",
    "feclearexcept",
    "fegetenv",
    "fegetexceptflag",
    "fegetround",
    "feholdexcept",
    "feof",
    "feof_unlocked",
    "feraiseexcept",
    "ferror",
    "ferror_unlocked",
    "fesetenv",
    "fesetexceptflag",
    "fesetround",
    "fetestexcept",
    "feupdateenv",
    "fexecve",
    "fflush_unlocked",
    "ffsl",
    "fgetc",
    "fgetc_unlocked",
    "fgetgrent",
    "fgetln",
    "fgetpos",
    "fgetpwent",
    "fgets",
    "fgets_unlocked",
    "fgetwc",
    "__fgetwc_unlocked",
    "fgetwc_unlocked",
    "fgetws",
    "fgetws_unlocked",
    "fileno_unlocked",
    "finite",
    "finitef",
    "__flock",
    "flock",
    "flockfile",
    "floor",
    "floorf",
    "floorl",
    "fma",
    "fmaf",
    "fmal",
    "fmax",
    "fmaxf",
    "fmaxl",
    "fmin",
    "fminf",
    "fminl",
    "fmod",
    "fmodf",
    "fmodl",
    "fnmatch",
    "fork",
    "fpathconf",
    "__fpclassify",
    "__fpclassifyf",
    "__fpclassifyl",
    "fputc_unlocked",
    "fputs_unlocked",
    "__fputwc_unlocked",
    "fputwc_unlocked",
    "fputws",
    "fputws_unlocked",
    "fread_unlocked",
    "freeaddrinfo",
    "freeifaddrs",
    "__freelocale",
    "freopen",
    "frexp",
    "frexpf",
    "frexpl",
    "fscanf",
    "fsetpos",
    "fstatat",
    "fstatfs",
    "fstatvfs",
    "fsync",
    "ftime",
    "ftok",
    "ftruncate",
    "ftrylockfile",
    "funlockfile",
    "futimens",
    "futimes",
    "__futimesat",
    "futimesat",
    "fwide",
    "fwprintf",
    "fwrite_unlocked",
    "fwscanf",
    "gai_strerror",
    "gcvt",
    "getaddrinfo",
    "_getaddrinfo_from_dns",
    "getauxval",
    "getchar",
    "getchar_unlocked",
    "getc_unlocked",
    "get_current_dir_name",
    "getdate",
    "getdate_err",
    "__getdelim",
    "getdelim",
    "getdomainname",
    "getdtablesize",
    "getegid",
    "geteuid",
    "getgid",
    "__getgr_a",
    "getgrent",
    "__getgrent_a",
    "getgrgid",
    "getgrgid_r",
    "getgrnam",
    "getgrnam_r",
    "getgrouplist",
    "getgroups",
    "gethostbyaddr",
    "gethostbyaddr_r",
    "gethostbyname",
    "gethostbyname2",
    "gethostbyname2_r",
    "gethostbyname_r",
    "gethostent",
    "gethostid",
    "gethostname",
    "getifaddrs",
    "getitimer",
    "getline",
    "getlogin",
    "getlogin_r",
    "getnameinfo",
    "getnetbyaddr",
    "getnetbyname",
    "getnetent",
    "getopt",
    "getopt_long_only",
    "__getopt_msg",
    "getpass",
    "getpeername",
    "getpgid",
    "getpgrp",
    "getppid",
    "getprotobyname",
    "getprotobynumber",
    "getprotoent",
    "__getpw_a",
    "getpwent",
    "__getpwent_a",
    "getpwnam",
    "getpwnam_r",
    "getpwuid",
    "getpwuid_r",
    "getresgid",
    "__get_resolv_conf",
    "getresuid",
    "gets",
    "getservbyname",
    "getservbyname_r",
    "getservbyport",
    "getservbyport_r",
    "getservent",
    "getsid",
    "getsockname",
    "getsockopt",
    "getsubopt",
    "getuid",
    "__get_unsafe_stack_ptr",
    "__get_unsafe_stack_start",
    "__get_unsafe_stack_top",
    "getw",
    "getwchar",
    "getwchar_unlocked",
    "getwc_unlocked",
    "gmtime",
    "__gmtime_r",
    "gmtime_r",
    "grantpt",
    "__h_errno_location",
    "herror",
    "hstrerror",
    "hypot",
    "hypotf",
    "hypotl",
    "if_freenameindex",
    "if_indextoname",
    "if_nameindex",
    "if_nametoindex",
    "ilogb",
    "ilogbf",
    "ilogbl",
    "imaxabs",
    "imaxdiv",
    "in6addr_any",
    "in6addr_loopback",
    "index",
    "inet_addr",
    "__inet_aton",
    "inet_aton",
    "inet_lnaof",
    "inet_makeaddr",
    "inet_netof",
    "inet_network",
    "inet_ntoa",
    "inet_ntop",
    "inet_pton",
    "initgroups",
    "initstate",
    "__inotify_add_watch",
    "inotify_add_watch",
    "__inotify_init",
    "inotify_init",
    "__inotify_init1",
    "inotify_init1",
    "__inotify_rm_watch",
    "inotify_rm_watch",
    "__invtrigl_R",
    "ioctl",
    "_IO_feof_unlocked",
    "_IO_ferror_unlocked",
    "_IO_getc",
    "_IO_getc_unlocked",
    "_IO_putc",
    "_IO_putc_unlocked",
    "isalpha",
    "isascii",
    "isastream",
    "isblank",
    "ispunct",
    "issetugid",
    "iswalnum",
    "iswalpha",
    "iswblank",
    "iswcntrl",
    "iswctype",
    "iswdigit",
    "iswgraph",
    "iswprint",
    "iswpunct",
    "iswspace",
    "iswupper",
    "iswxdigit",
    "j0",
    "j0f",
    "j1",
    "j1f",
    "jn",
    "jnf",
    "jrand48",
    "kill",
    "killpg",
    "labs",
    "lchmod",
    "lchown",
    "lcong48",
    "__ldexp_cexp",
    "__ldexp_cexpf",
    "ldexpf",
    "ldexpl",
    "ldiv",
    "lgamma",
    "lgammaf",
    "__lgammaf_r",
    "lgammaf_r",
    "lgammal",
    "__lgammal_r",
    "lgammal_r",
    "__lgamma_r",
    "lgamma_r",
    "__libc_current_sigrtmax",
    "__libc_current_sigrtmin",
    "__libc_start_main",
    "link",
    "linkat",
    "listen",
    "llabs",
    "lldiv",
    "llrint",
    "llrintf",
    "llrintl",
    "llround",
    "llroundf",
    "llroundl",
    "localeconv",
    "localtime",
    "__localtime_r",
    "lockf",
    "log",
    "log10f",
    "log10l",
    "log1p",
    "log1pf",
    "log1pl",
    "log2f",
    "log2l",
    "logb",
    "logbf",
    "logbl",
    "logf",
    "logl",
    "_longjmp",
    "longjmp",
    "__lookup_ipliteral",
    "__lookup_name",
    "__lookup_serv",
    "lrand48",
    "lrint",
    "lrintf",
    "lrintl",
    "lround",
    "lroundf",
    "lroundl",
    "lstat",
    "lutimes",
    "__madvise",
    "madvise",
    "mallinfo",
    "mallinfo2",
    "malloc_disable",
    "malloc_disable_memory_tagging",
    "malloc_enable",
    "malloc_info",
    "malloc_iterate",
    "malloc_set_add_large_allocation_slack",
    "malloc_set_pattern_fill_contents",
    "malloc_set_track_allocation_stacks",
    "malloc_set_zero_contents",
    "malloc_usable_size",
    "mallopt",
    "__map_file",
    "mblen",
    "mbrtoc16",
    "mbrtoc32",
    "mbsinit",
    "mbstowcs",
    "memalign",
    "memccpy",
    "__memfd_create",
    "memfd_create",
    "mempcpy",
    "memrchr",
    "mkdirat",
    "mkdtemp",
    "mkfifo",
    "mkfifoat",
    "mknod",
    "mknodat",
    "mkostemp",
    "__mkostemps",
    "mkostemps",
    "mkostemps64",
    "mkstemp",
    "mkstemps",
    "mktemp",
    "mktime",
    "mlock",
    "mlockall",
    "__mmap",
    "_mmap_get_vmo_from_context",
    "_mmap_on_mapped",
    "modff",
    "modfl",
    "__mprotect",
    "mprotect",
    "mrand48",
    "msgctl",
    "msgget",
    "msgrcv",
    "msgsnd",
    "msync",
    "mtx_timedlock",
    "munlock",
    "munlockall",
    "__munmap",
    "nan",
    "nanf",
    "nanl",
    "nearbyint",
    "nearbyintf",
    "nearbyintl",
    "__newlocale",
    "nextafter",
    "nextafterf",
    "nextafterl",
    "nexttoward",
    "nexttowardf",
    "nexttowardl",
    "__nl_langinfo",
    "nl_langinfo",
    "__nl_langinfo_l",
    "nl_langinfo_l",
    "nrand48",
    "_ns_flagdata",
    "ns_get16",
    "ns_get32",
    "ns_initparse",
    "ns_name_uncompress",
    "ns_parserr",
    "ns_put16",
    "ns_put32",
    "ns_skiprr",
    "ntohs",
    "openat",
    "openlog",
    "open_memstream",
    "openpty",
    "open_wmemstream",
    "opterr",
    "optopt",
    "__optpos",
    "__optreset",
    "optreset",
    "__overflow",
    "__p1evll",
    "pathconf",
    "pause",
    "pclose",
    "pipe",
    "pipe2",
    "__polevll",
    "poll",
    "popen",
    "posix_close",
    "posix_fadvise",
    "posix_fallocate",
    "__posix_getopt",
    "posix_madvise",
    "posix_openpt",
    "posix_spawn",
    "posix_spawnattr_destroy",
    "posix_spawnattr_getflags",
    "posix_spawnattr_getpgroup",
    "posix_spawnattr_getschedparam",
    "posix_spawnattr_getschedpolicy",
    "posix_spawnattr_getsigdefault",
    "posix_spawnattr_getsigmask",
    "posix_spawnattr_init",
    "posix_spawnattr_setflags",
    "posix_spawnattr_setpgroup",
    "posix_spawnattr_setschedparam",
    "posix_spawnattr_setschedpolicy",
    "posix_spawnattr_setsigdefault",
    "posix_spawnattr_setsigmask",
    "posix_spawn_file_actions_addclose",
    "posix_spawn_file_actions_adddup2",
    "posix_spawn_file_actions_addopen",
    "posix_spawn_file_actions_destroy",
    "posix_spawn_file_actions_init",
    "posix_spawnp",
    "__posix_spawnx",
    "pow10",
    "pow10f",
    "pow10l",
    "powl",
    "ppoll",
    "pread",
    "preadv",
    "program_invocation_name",
    "program_invocation_short_name",
    "pselect",
    "psiginfo",
    "psignal",
    "pthread_attr_getdetachstate",
    "pthread_attr_getguardsize",
    "pthread_attr_getschedparam",
    "pthread_attr_getstack",
    "pthread_attr_getstacksize",
    "pthread_attr_setdetachstate",
    "pthread_attr_setguardsize",
    "pthread_attr_setschedparam",
    "pthread_barrierattr_destroy",
    "pthread_barrierattr_init",
    "pthread_barrier_destroy",
    "pthread_barrier_init",
    "pthread_barrier_wait",
    "pthread_cancel",
    "pthread_condattr_getclock",
    "pthread_equal",
    "pthread_exit",
    "pthread_getaffinity_np",
    "pthread_getattr_np",
    "pthread_getconcurrency",
    "pthread_getcpuclockid",
    "pthread_getname_np",
    "pthread_kill",
    "pthread_mutexattr_getprotocol",
    "pthread_mutexattr_getrobust",
    "pthread_mutexattr_gettype",
    "pthread_mutexattr_setprotocol",
    "pthread_mutexattr_setrobust",
    "pthread_mutex_consistent",
    "pthread_mutex_getprioceiling",
    "pthread_mutex_setprioceiling",
    "pthread_mutex_timedlock",
    "__pthread_mutex_trylock_owner",
    "pthread_rwlockattr_destroy",
    "pthread_rwlockattr_init",
    "pthread_rwlock_destroy",
    "pthread_rwlock_init",
    "pthread_rwlock_timedwrlock",
    "pthread_rwlock_trywrlock",
    "pthread_setaffinity_np",
    "pthread_setcancelstate",
    "pthread_setcanceltype",
    "pthread_setconcurrency",
    "pthread_sigmask",
    "pthread_spin_destroy",
    "pthread_spin_init",
    "pthread_spin_lock",
    "pthread_spin_trylock",
    "pthread_spin_unlock",
    "pthread_testcancel",
    "ptsname",
    "__ptsname_r",
    "ptsname_r",
    "putc",
    "putchar_unlocked",
    "putc_unlocked",
    "putenv",
    "putgrent",
    "putpwent",
    "putw",
    "putwc",
    "putwchar",
    "putwchar_unlocked",
    "putwc_unlocked",
    "pvalloc",
    "pwrite",
    "pwritev",
    "quick_exit",
    "raise",
    "rand",
    "__randname",
    "random",
    "readdir_r",
    "readlink",
    "readlinkat",
    "readv",
    "recv",
    "recvfrom",
    "recvmmsg",
    "recvmsg",
    "regcomp",
    "regerror",
    "regexec",
    "regfree",
    "remainder",
    "remainderf",
    "remainderl",
    "__rem_pio2",
    "__rem_pio2f",
    "__rem_pio2l",
    "__rem_pio2_large",
    "remquo",
    "remquof",
    "remquol",
    "rename",
    "renameat",
    "res_init",
    "__res_mkquery",
    "res_mkquery",
    "__res_msend",
    "__res_msend_rc",
    "__res_query",
    "res_query",
    "res_querydomain",
    "res_search",
    "__res_send",
    "res_send",
    "__res_state",
    "rewind",
    "rewinddir",
    "rindex",
    "rint",
    "rintf",
    "rintl",
    "rmdir",
    "roundf",
    "roundl",
    "__rtnetlink_enumerate",
    "__sanitizer_before_thread_create_hook",
    "__sanitizer_change_code_protection",
    "__sanitizer_fast_backtrace",
    "__sanitizer_fill_shadow",
    "__sanitizer_memory_snapshot",
    "__sanitizer_module_loaded",
    "__sanitizer_process_exit_hook",
    "__sanitizer_shadow_bounds",
    "__sanitizer_startup_hook",
    "__sanitizer_thread_create_hook",
    "__sanitizer_thread_exit_hook",
    "__sanitizer_thread_start_hook",
    "scalb",
    "scalbf",
    "scalbln",
    "scalblnf",
    "scalblnl",
    "scalbn",
    "scalbnf",
    "scalbnl",
    "scandir",
    "scanf",
    "__sched_cpucount",
    "sched_getaffinity",
    "sched_getcpu",
    "sched_getparam",
    "sched_get_priority_min",
    "sched_getscheduler",
    "sched_rr_get_interval",
    "sched_setaffinity",
    "sched_setparam",
    "__scudo_print_stats",
    "seed48",
    "seekdir",
    "select",
    "semctl",
    "semget",
    "sem_getvalue",
    "semop",
    "semtimedop",
    "sem_trywait",
    "sem_unlink",
    "send",
    "sendmmsg",
    "sendmsg",
    "sendto",
    "setbuf",
    "setbuffer",
    "setdomainname",
    "setegid",
    "setenv",
    "seteuid",
    "setgid",
    "setgrent",
    "setgroups",
    "sethostent",
    "sethostname",
    "setitimer",
    "_setjmp",
    "setjmp",
    "setlinebuf",
    "setlocale",
    "setlogmask",
    "setnetent",
    "setpgid",
    "setpgrp",
    "setprotoent",
    "setpwent",
    "setregid",
    "setresgid",
    "setresuid",
    "setreuid",
    "setservent",
    "setsid",
    "setsockopt",
    "setstate",
    "settimeofday",
    "setuid",
    "setvbuf",
    "__shgetc",
    "__shlim",
    "shmat",
    "shmctl",
    "shmdt",
    "shmget",
    "shm_open",
    "shm_unlink",
    "shutdown",
    "sigaction",
    "sigaddset",
    "sigaltstack",
    "sigandset",
    "sigdelset",
    "sigemptyset",
    "sigfillset",
    "siginterrupt",
    "sigisemptyset",
    "sigismember",
    "siglongjmp",
    "signal",
    "__signbit",
    "__signbitf",
    "__signbitl",
    "__signgam",
    "signgam",
    "significand",
    "significandf",
    "sigorset",
    "sigpause",
    "sigpending",
    "sigprocmask",
    "sigqueue",
    "sigsetjmp",
    "sigsuspend",
    "sigtimedwait",
    "sigwait",
    "sigwaitinfo",
    "__sin",
    "sin",
    "sincos",
    "sincosl",
    "__sindf",
    "sinh",
    "sinhf",
    "sinhl",
    "__sinl",
    "sinl",
    "sockatmark",
    "socket",
    "socketpair",
    "sqrt",
    "sqrtf",
    "sqrtl",
    "srand",
    "srand48",
    "srandom",
    "__stack_chk_guard",
    "statfs",
    "statvfs",
    "stime",
    "stpcpy",
    "stpncpy",
    "strcasestr",
    "strchrnul",
    "strcoll",
    "strcspn",
    "strfmon",
    "strfmon_l",
    "__strftime_fmt_1",
    "strlcat",
    "strncasecmp",
    "strncat",
    "strndup",
    "strpbrk",
    "strptime",
    "strsignal",
    "strspn",
    "strtoimax",
    "strtok",
    "strtok_r",
    "strtoumax",
    "strverscmp",
    "strxfrm",
    "stub_pselect",
    "stub_select",
    "swab",
    "swprintf",
    "swscanf",
    "symlink",
    "symlinkat",
    "sync",
    "syncfs",
    "syslog",
    "system",
    "__sysv_signal",
    "__tan",
    "tan",
    "__tandf",
    "tanf",
    "tanh",
    "tanhf",
    "tanhl",
    "__tanl",
    "tanl",
    "tcdrain",
    "tcflow",
    "tcflush",
    "tcgetattr",
    "tcgetpgrp",
    "tcgetsid",
    "tcsendbreak",
    "tcsetattr",
    "tcsetpgrp",
    "telldir",
    "tempnam",
    "tgamma",
    "tgammaf",
    "tgammal",
    "thrd_equal",
    "thrd_set_zx_process",
    "thrd_sleep",
    "thrd_yield",
    "timegm",
    "timerfd_create",
    "timerfd_gettime",
    "timerfd_settime",
    "times",
    "timezone",
    "tmpfile",
    "tmpnam",
    "__tm_to_tzname",
    "toascii",
    "towctrans",
    "towlower",
    "towupper",
    "__tre_mem_alloc_impl",
    "__tre_mem_destroy",
    "__tre_mem_new_impl",
    "trunc",
    "truncate",
    "truncf",
    "truncl",
    "tss_create",
    "tss_delete",
    "tss_get",
    "tss_set",
    "ttyname",
    "ttyname_r",
    "tzname",
    "__tzset",
    "tzset",
    "ualarm",
    "__uflow",
    "umask",
    "uname",
    "unlink",
    "unlinkat",
    "unlockpt",
    "__unsanitized_memcpy",
    "__unsanitized_memmove",
    "__unsanitized_memset",
    "unsetenv",
    "__uselocale",
    "utime",
    "utimensat",
    "utimes",
    "valloc",
    "vdprintf",
    "verr",
    "verrx",
    "versionsort",
    "vfscanf",
    "vfwprintf",
    "vfwscanf",
    "vscanf",
    "vswprintf",
    "vswscanf",
    "__vsyslog",
    "vsyslog",
    "vwarn",
    "vwarnx",
    "vwprintf",
    "vwscanf",
    "__wait",
    "wait",
    "waitid",
    "waitpid",
    "warn",
    "warnx",
    "wcpcpy",
    "wcpncpy",
    "wcscasecmp",
    "wcscat",
    "wcschr",
    "wcscmp",
    "wcscoll",
    "wcscpy",
    "wcscspn",
    "wcsdup",
    "wcsftime",
    "__wcsftime_l",
    "wcsftime_l",
    "wcsncasecmp",
    "wcsncat",
    "wcsncmp",
    "wcsncpy",
    "wcsnlen",
    "wcspbrk",
    "wcsrchr",
    "wcsrtombs",
    "wcsspn",
    "wcsstr",
    "wcstod",
    "wcstof",
    "wcstoimax",
    "wcstok",
    "wcstol",
    "wcstold",
    "wcstoll",
    "wcstombs",
    "wcstoul",
    "wcstoull",
    "wcstoumax",
    "wcswcs",
    "wcswidth",
    "wcsxfrm",
    "wctob",
    "wctomb",
    "wctrans",
    "wctype",
    "wcwidth",
    "wmemcmp",
    "wmemcpy",
    "wmemmove",
    "wmemset",
    "wordexp",
    "wordfree",
    "wprintf",
    "wscanf",
    "__xpg_basename",
    "y0",
    "y0f",
    "y1",
    "y1f",
    "yn",
    "ynf",
    "_zx_job_default",
    "_zx_process_self",
    "zx_take_startup_handle",
    "_zx_utc_reference_get",
    "zx_utc_reference_get",
    "_zx_utc_reference_swap",
    "zx_utc_reference_swap",
};

//  ________  _________  ________  ________
// |\   ____\|\___   ___\\   __  \|\   __  \
// \ \  \___|\|___ \  \_\ \  \|\  \ \  \|\  \
//  \ \_____  \   \ \  \ \ \  \\\  \ \   ____\
//   \|____|\  \   \ \  \ \ \  \\\  \ \  \___|
//     ____\_\  \   \ \__\ \ \_______\ \__\
//    |\_________\   \|__|  \|_______|\|__|
//    \|_________|
//
// The relative component URLs for drivers which are allowed to invoke fdf_env_* APIs.
//
// To remove items from the allowlist, please send a change to one of the OWNERS of
// this file to remove an element from the list below.
const std::set<std::string> kRestrictedDriverRuntimeSymbolsDriversAllowlist = {
    "#meta/network-device.cm",
};

const std::set<std::string> kRestrictedDriverRuntimeSymbols = {
    "fdf_env_dispatcher_create_with_owner",
    "fdf_env_dispatcher_dump",
    "fdf_env_dispatcher_get_dump_deprecated",
    "fdf_env_shutdown_dispatchers_async",
    "fdf_env_destroy_all_dispatchers",
    "fdf_env_register_driver_entry",
    "fdf_env_register_driver_exit",
    "fdf_env_get_current_driver",
    "fdf_env_dispatcher_has_queued_tasks",
    "fdf_env_get_thread_limit",
    "fdf_env_set_thread_limit",
};

}  // namespace driver_symbols

#endif  // SRC_LIB_DRIVER_SYMBOLS_RESTRICTED_SYMBOLS_H_
