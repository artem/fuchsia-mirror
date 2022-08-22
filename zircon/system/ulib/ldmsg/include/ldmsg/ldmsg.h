// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LDMSG_LDMSG_H_
#define LDMSG_LDMSG_H_

#include <stdint.h>
#include <zircon/compiler.h>
#include <zircon/fidl.h>
#include <zircon/types.h>

// In principle, this header should be generated by the FIDL compiler, but this
// protocol is used at such a low level (e.g., in userboot and in libc) that we
// have a hand-written implementation available.

__BEGIN_CDECLS

// See system/fidl/fuchsia-ldsvc/ldsvc.fidl for the definition of these message ordinals.
#define LDMSG_OP_DONE ((uint64_t)0x63BA6B76D3671001)
#define LDMSG_OP_LOAD_OBJECT ((uint64_t)0x48C5A151D6DF2853)
#define LDMSG_OP_CONFIG ((uint64_t)0x6A8A1A1464632841)
#define LDMSG_OP_CLONE ((uint64_t)0x57E643A9AB6E4C29)

// The payload format used for all the requests other than LDMSG_OP_CLONE.
typedef struct ldmsg_common ldmsg_common_t;
struct ldmsg_common {
  alignas(FIDL_ALIGNMENT) fidl_string_t string;
  alignas(FIDL_ALIGNMENT) zx_handle_t object;
};

// The payload format used for LDMSG_OP_CLONE.
typedef struct ldmsg_clone ldmsg_clone_t;
struct ldmsg_clone {
  alignas(FIDL_ALIGNMENT) zx_handle_t object;
};

// The maximum size of a ldmsg_req_t payload.
#define LDMSG_MAX_PAYLOAD (1024 - sizeof(fidl_message_header_t))

// The message format used for requests.
//
// This struct contains 1024 bytes. After the message header, the data varies
// according to the ordinal. The space after the fix-size portion of the message
// is used to contain string data.
//
// The |ldmsg_req_encode| function will encode a message of at most 1023 bytes
// so that a server that uses this struct will always have one byte remaining to
// null-terminate the string data. The client does not necessarily include a
// null terminator.
typedef struct ldmsg_req ldmsg_req_t;
struct ldmsg_req {
  fidl_message_header_t header;
  union {
    ldmsg_common_t common;
    ldmsg_clone_t clone;
    char data[LDMSG_MAX_PAYLOAD];
  };
};

// The message format used for responses.
//
// Depending on the ordinal in the message header, the |object| field might or
// might not be part of the message.
//
// Consider using |ldmsg_rsp_get_size| to determine how much of this structure
// is used for a given ordinal.
typedef struct ldmsg_rsp ldmsg_rsp_t;
struct ldmsg_rsp {
  fidl_message_header_t header;
  zx_status_t rv;
  zx_handle_t object;
};

// Initialize the FIDL transaction header stored within ldmsg_req.
//
// TODO(38643) replace with fidl_init_txn_header once it is inline
void ldmsg_req_init_txn_header(fidl_message_header_t* header, uint64_t ordinal);

// Encode the message in |req|.
//
// The format of the message will be determined by the ordinal in the message's
// header. If the ordinal is invalid, this function will return
// ZX_ERR_INVALID_ARGS.
//
// The given |data| will be copied into |*req| at the appropriate location if
// the message format contains a string. If |len| is too large, this function
// will return ZX_ERR_OUT_OF_RANGE.
//
// Otherwise, this function will return ZX_OK.
zx_status_t ldmsg_req_encode(ldmsg_req_t* req, size_t* req_len_out, const char* data, size_t len);

// Decode the message in |req|.
//
// The format of the message will be determined by the ordinal in the message's
// header. If the ordinal is invalid, this function will return
// ZX_ERR_INVALID_ARGS.
//
// Returns whether the message could be correctly decoded. Upon success, if the
// ordinal specifies a message that includes a string, |*data_out| will point to
// the string within the |req|, which is of length |*len_out|. The byte after
// the string is guaranteed to be zero.
//
// If the message is not of the correct size or the content of the message fails
// validation, this function will return ZX_ERR_INVALID_ARGS.
//
// Otherwise, this function will return ZX_OK.
zx_status_t ldmsg_req_decode(ldmsg_req_t* req, size_t req_len, const char** data_out,
                             size_t* len_out);

// The appropriate size message to send for the given |rsp|.
//
// The size of the message depends on the ordinal in the message's
// header. If the ordinal is invalid, this function will return 0.
size_t ldmsg_rsp_get_size(ldmsg_rsp_t* rsp);

__END_CDECLS

#endif  // LDMSG_LDMSG_H_
