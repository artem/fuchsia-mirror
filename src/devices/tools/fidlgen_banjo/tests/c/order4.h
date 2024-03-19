// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// WARNING: THIS FILE IS MACHINE GENERATED. DO NOT EDIT.
// Generated from the banjo.examples.order4 banjo file

#pragma once


#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Forward declarations
typedef struct one one_t;
typedef struct two two_t;

// Declarations
struct one {
    const zx_handle_t* one_handle_list;
    size_t one_handle_count;
};

struct two {
    zx_handle_t two_handle[1];
};


// Helpers


__END_CDECLS
