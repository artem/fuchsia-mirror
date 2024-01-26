// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_LIB_LINUX_UAPI_STUB_MISSING_INCLUDES_H_
#define SRC_STARNIX_LIB_LINUX_UAPI_STUB_MISSING_INCLUDES_H_

// Adding includes that are not detected by rust-bindings because they are
// defined using functions

#include <asm/ioctls.h>
#include <linux/seccomp.h>

// generate.py will remove __bindgen_missing_ from the start of constant names.
#define C(x) const __u32 __bindgen_missing_##x = x

C(SECCOMP_IOCTL_NOTIF_RECV);
C(SECCOMP_IOCTL_NOTIF_SEND);
C(SECCOMP_IOCTL_NOTIF_ID_VALID);
C(SECCOMP_IOCTL_NOTIF_ADDFD);

C(TIOCSPTLCK);
C(TIOCGPTLCK);
C(TIOCGPKT);
C(TIOCSIG);
C(TIOCGPTN);

C(BLKGETSIZE);
C(BLKFLSBUF);
C(BLKGETSIZE64);

C(BINDER_WRITE_READ);
C(BINDER_SET_IDLE_TIMEOUT);
C(BINDER_SET_MAX_THREADS);
C(BINDER_SET_IDLE_PRIORITY);
C(BINDER_SET_CONTEXT_MGR);
C(BINDER_THREAD_EXIT);
C(BINDER_VERSION);
C(BINDER_GET_NODE_DEBUG_INFO);
C(BINDER_GET_NODE_INFO_FOR_REF);
C(BINDER_SET_CONTEXT_MGR_EXT);
C(BINDER_FREEZE);
C(BINDER_GET_FROZEN_INFO);
C(BINDER_ENABLE_ONEWAY_SPAM_DETECTION);

C(EVIOCGVERSION);
C(EVIOCGID);

C(RWF_HIPRI);
C(RWF_DSYNC);
C(RWF_SYNC);
C(RWF_NOWAIT);
C(RWF_APPEND);
C(RWF_SUPPORTED);

// `EVIOCGBIT`, `EVIOCGPROP`, and `EVIOCGABS` are invoked with various paraemters to query
// metadata about an input device. Create Rust symbols for the commonly used invocations.
//
// The `EVIOCGBIT` invocations specify a `size` _just_ large enough to report all off the
// feature bits for that attribute.
//
// TODO(quiche): Eventually, it will probably be better to provide a way to parse the fields
// within an `ioctl()`'s `request` parameter. That would allow, e.g., the input code to
// respond to any request for `EV_KEY` feature bits, even if the caller provided a buffer
// larger than that needed for the available bits.
#define N_BYTES(BITS) (((BITS) + CHAR_BIT - 1) / CHAR_BIT)
const __u32 EVIOCGBIT_EV_KEY = EVIOCGBIT(EV_KEY, N_BYTES(KEY_MAX));
const __u32 EVIOCGBIT_EV_ABS = EVIOCGBIT(EV_ABS, N_BYTES(ABS_MAX));
const __u32 EVIOCGBIT_EV_REL = EVIOCGBIT(EV_REL, N_BYTES(REL_MAX));
const __u32 EVIOCGBIT_EV_SW = EVIOCGBIT(EV_SW, N_BYTES(SW_MAX));
const __u32 EVIOCGBIT_EV_LED = EVIOCGBIT(EV_LED, N_BYTES(LED_MAX));
const __u32 EVIOCGBIT_EV_FF = EVIOCGBIT(EV_FF, N_BYTES(FF_MAX));
const __u32 EVIOCGBIT_EV_MSC = EVIOCGBIT(EV_MSC, N_BYTES(MSC_MAX));
const __u32 EVIOCGPROP = EVIOCGPROP(N_BYTES(INPUT_PROP_MAX));
const __u32 EVIOCGABS_X = EVIOCGABS(ABS_X);
const __u32 EVIOCGABS_Y = EVIOCGABS(ABS_Y);
#undef N_BYTES

// Symbols for remote binder device driver

struct remote_binder_start_command {
  const char* incoming_service;
};

struct remote_binder_wait_command {
  char spawn_thread;
};

const __u32 _REMOTE_BINDER_START = _IOR('R', 1, struct remote_binder_start_command);
const __u32 REMOTE_BINDER_START = _REMOTE_BINDER_START;

const __u32 _REMOTE_BINDER_WAIT = _IOW('R', 2, struct remote_binder_wait_command);
const __u32 REMOTE_BINDER_WAIT = _REMOTE_BINDER_WAIT;

C(FS_IOC_FSGETXATTR);
C(FS_IOC_FSSETXATTR);

C(FS_IOC_GETFLAGS);
C(FS_IOC_SETFLAGS);

// Symbols for fsverity

C(FS_IOC_ENABLE_VERITY);
C(FS_IOC_MEASURE_VERITY);
C(FS_IOC_READ_VERITY_METADATA);

// Symbols for uinput

C(UI_DEV_CREATE);
C(UI_DEV_DESTROY);
C(UI_DEV_SETUP);
C(UI_ABS_SETUP);
C(UI_SET_EVBIT);
C(UI_SET_KEYBIT);
C(UI_SET_RELBIT);
C(UI_SET_ABSBIT);
C(UI_SET_MSCBIT);
C(UI_SET_LEDBIT);
C(UI_SET_SNDBIT);
C(UI_SET_FFBIT);
C(UI_SET_PHYS);
C(UI_SET_SWBIT);
C(UI_SET_PROPBIT);
C(UI_BEGIN_FF_UPLOAD);
C(UI_END_FF_UPLOAD);
C(UI_BEGIN_FF_ERASE);
C(UI_END_FF_ERASE);
C(UI_GET_VERSION);

C(ASHMEM_SET_NAME);
C(ASHMEM_GET_NAME);
C(ASHMEM_SET_SIZE);
C(ASHMEM_GET_SIZE);
C(ASHMEM_SET_PROT_MASK);
C(ASHMEM_GET_PROT_MASK);
C(ASHMEM_PIN);
C(ASHMEM_UNPIN);
C(ASHMEM_GET_PIN_STATUS);
C(ASHMEM_PURGE_ALL_CACHES);
C(ASHMEM_GET_FILE_ID);

C(RNDGETENTCNT);
C(RNDADDTOENTCNT);
C(RNDGETPOOL);
C(RNDADDENTROPY);
C(RNDZAPENTCNT);
C(RNDCLEARPOOL);
C(RNDRESEEDCRNG);

typedef struct new_utsname utsname;
typedef __kernel_gid_t gid_t;
typedef __kernel_ino_t ino_t;
typedef __kernel_mode_t mode_t;
typedef __kernel_off_t off_t;

#endif  // SRC_STARNIX_LIB_LINUX_UAPI_STUB_MISSING_INCLUDES_H_
