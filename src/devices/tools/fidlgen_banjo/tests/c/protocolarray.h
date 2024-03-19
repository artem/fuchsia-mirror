// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// WARNING: THIS FILE IS MACHINE GENERATED. DO NOT EDIT.
// Generated from the banjo.examples.protocolarray banjo file

#pragma once


#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Forward declarations
typedef struct array_protocol array_protocol_t;
typedef struct array_protocol_ops array_protocol_ops_t;
#define array_size UINT32_C(32)
typedef struct array2_protocol array2_protocol_t;
typedef struct array2_protocol_ops array2_protocol_ops_t;
typedef struct arrayof_arrays_protocol arrayof_arrays_protocol_t;
typedef struct arrayof_arrays_protocol_ops arrayof_arrays_protocol_ops_t;

// Declarations
struct array_protocol_ops {
    void (*bool)(void* ctx, const bool b[1], bool out_b[1]);
    void (*int8)(void* ctx, const int8_t i8[1], int8_t out_i8[1]);
    void (*int16)(void* ctx, const int16_t i16[1], int16_t out_i16[1]);
    void (*int32)(void* ctx, const int32_t i32[1], int32_t out_i32[1]);
    void (*int64)(void* ctx, const int64_t i64[1], int64_t out_i64[1]);
    void (*uint8)(void* ctx, const uint8_t u8[1], uint8_t out_u8[1]);
    void (*uint16)(void* ctx, const uint16_t u16[1], uint16_t out_u16[1]);
    void (*uint32)(void* ctx, const uint32_t u32[1], uint32_t out_u32[1]);
    void (*uint64)(void* ctx, const uint64_t u64[1], uint64_t out_u64[1]);
    void (*float32)(void* ctx, const float f32[1], float out_f32[1]);
    void (*float64)(void* ctx, const double u64[1], double out_f64[1]);
    void (*handle)(void* ctx, const zx_handle_t u64[1], zx_handle_t out_f64[1]);
};


struct array_protocol {
    const array_protocol_ops_t* ops;
    void* ctx;
};

struct array2_protocol_ops {
    void (*bool)(void* ctx, const bool b[32], bool out_b[32]);
    void (*int8)(void* ctx, const int8_t i8[32], int8_t out_i8[32]);
    void (*int16)(void* ctx, const int16_t i16[32], int16_t out_i16[32]);
    void (*int32)(void* ctx, const int32_t i32[32], int32_t out_i32[32]);
    void (*int64)(void* ctx, const int64_t i64[32], int64_t out_i64[32]);
    void (*uint8)(void* ctx, const uint8_t u8[32], uint8_t out_u8[32]);
    void (*uint16)(void* ctx, const uint16_t u16[32], uint16_t out_u16[32]);
    void (*uint32)(void* ctx, const uint32_t u32[32], uint32_t out_u32[32]);
    void (*uint64)(void* ctx, const uint64_t u64[32], uint64_t out_u64[32]);
    void (*float32)(void* ctx, const float f32[32], float out_f32[32]);
    void (*float64)(void* ctx, const double u64[32], double out_f64[32]);
    void (*handle)(void* ctx, const zx_handle_t u64[32], zx_handle_t out_f64[32]);
};


struct array2_protocol {
    const array2_protocol_ops_t* ops;
    void* ctx;
};

struct arrayof_arrays_protocol_ops {
    void (*bool)(void* ctx, const bool b[32][4], bool out_b[32][4]);
    void (*int8)(void* ctx, const int8_t i8[32][4], int8_t out_i8[32][4]);
    void (*int16)(void* ctx, const int16_t i16[32][4], int16_t out_i16[32][4]);
    void (*int32)(void* ctx, const int32_t i32[32][4], int32_t out_i32[32][4]);
    void (*int64)(void* ctx, const int64_t i64[32][4], int64_t out_i64[32][4]);
    void (*uint8)(void* ctx, const uint8_t u8[32][4], uint8_t out_u8[32][4]);
    void (*uint16)(void* ctx, const uint16_t u16[32][4], uint16_t out_u16[32][4]);
    void (*uint32)(void* ctx, const uint32_t u32[32][4], uint32_t out_u32[32][4]);
    void (*uint64)(void* ctx, const uint64_t u64[32][4], uint64_t out_u64[32][4]);
    void (*float32)(void* ctx, const float f32[32][4], float out_f32[32][4]);
    void (*float64)(void* ctx, const double u64[32][4], double out_f64[32][4]);
    void (*handle)(void* ctx, const zx_handle_t u64[32][4], zx_handle_t out_f64[32][4]);
};


struct arrayof_arrays_protocol {
    const arrayof_arrays_protocol_ops_t* ops;
    void* ctx;
};


// Helpers
static inline void array_bool(const array_protocol_t* proto, const bool b[1], bool out_b[1]) {
    proto->ops->bool(proto->ctx, b, out_b);
}

static inline void array_int8(const array_protocol_t* proto, const int8_t i8[1], int8_t out_i8[1]) {
    proto->ops->int8(proto->ctx, i8, out_i8);
}

static inline void array_int16(const array_protocol_t* proto, const int16_t i16[1], int16_t out_i16[1]) {
    proto->ops->int16(proto->ctx, i16, out_i16);
}

static inline void array_int32(const array_protocol_t* proto, const int32_t i32[1], int32_t out_i32[1]) {
    proto->ops->int32(proto->ctx, i32, out_i32);
}

static inline void array_int64(const array_protocol_t* proto, const int64_t i64[1], int64_t out_i64[1]) {
    proto->ops->int64(proto->ctx, i64, out_i64);
}

static inline void array_uint8(const array_protocol_t* proto, const uint8_t u8[1], uint8_t out_u8[1]) {
    proto->ops->uint8(proto->ctx, u8, out_u8);
}

static inline void array_uint16(const array_protocol_t* proto, const uint16_t u16[1], uint16_t out_u16[1]) {
    proto->ops->uint16(proto->ctx, u16, out_u16);
}

static inline void array_uint32(const array_protocol_t* proto, const uint32_t u32[1], uint32_t out_u32[1]) {
    proto->ops->uint32(proto->ctx, u32, out_u32);
}

static inline void array_uint64(const array_protocol_t* proto, const uint64_t u64[1], uint64_t out_u64[1]) {
    proto->ops->uint64(proto->ctx, u64, out_u64);
}

static inline void array_float32(const array_protocol_t* proto, const float f32[1], float out_f32[1]) {
    proto->ops->float32(proto->ctx, f32, out_f32);
}

static inline void array_float64(const array_protocol_t* proto, const double u64[1], double out_f64[1]) {
    proto->ops->float64(proto->ctx, u64, out_f64);
}

static inline void array_handle(const array_protocol_t* proto, const zx_handle_t u64[1], zx_handle_t out_f64[1]) {
    proto->ops->handle(proto->ctx, u64, out_f64);
}

static inline void array2_bool(const array2_protocol_t* proto, const bool b[32], bool out_b[32]) {
    proto->ops->bool(proto->ctx, b, out_b);
}

static inline void array2_int8(const array2_protocol_t* proto, const int8_t i8[32], int8_t out_i8[32]) {
    proto->ops->int8(proto->ctx, i8, out_i8);
}

static inline void array2_int16(const array2_protocol_t* proto, const int16_t i16[32], int16_t out_i16[32]) {
    proto->ops->int16(proto->ctx, i16, out_i16);
}

static inline void array2_int32(const array2_protocol_t* proto, const int32_t i32[32], int32_t out_i32[32]) {
    proto->ops->int32(proto->ctx, i32, out_i32);
}

static inline void array2_int64(const array2_protocol_t* proto, const int64_t i64[32], int64_t out_i64[32]) {
    proto->ops->int64(proto->ctx, i64, out_i64);
}

static inline void array2_uint8(const array2_protocol_t* proto, const uint8_t u8[32], uint8_t out_u8[32]) {
    proto->ops->uint8(proto->ctx, u8, out_u8);
}

static inline void array2_uint16(const array2_protocol_t* proto, const uint16_t u16[32], uint16_t out_u16[32]) {
    proto->ops->uint16(proto->ctx, u16, out_u16);
}

static inline void array2_uint32(const array2_protocol_t* proto, const uint32_t u32[32], uint32_t out_u32[32]) {
    proto->ops->uint32(proto->ctx, u32, out_u32);
}

static inline void array2_uint64(const array2_protocol_t* proto, const uint64_t u64[32], uint64_t out_u64[32]) {
    proto->ops->uint64(proto->ctx, u64, out_u64);
}

static inline void array2_float32(const array2_protocol_t* proto, const float f32[32], float out_f32[32]) {
    proto->ops->float32(proto->ctx, f32, out_f32);
}

static inline void array2_float64(const array2_protocol_t* proto, const double u64[32], double out_f64[32]) {
    proto->ops->float64(proto->ctx, u64, out_f64);
}

static inline void array2_handle(const array2_protocol_t* proto, const zx_handle_t u64[32], zx_handle_t out_f64[32]) {
    proto->ops->handle(proto->ctx, u64, out_f64);
}

static inline void arrayof_arrays_bool(const arrayof_arrays_protocol_t* proto, const bool b[32][4], bool out_b[32][4]) {
    proto->ops->bool(proto->ctx, b, out_b);
}

static inline void arrayof_arrays_int8(const arrayof_arrays_protocol_t* proto, const int8_t i8[32][4], int8_t out_i8[32][4]) {
    proto->ops->int8(proto->ctx, i8, out_i8);
}

static inline void arrayof_arrays_int16(const arrayof_arrays_protocol_t* proto, const int16_t i16[32][4], int16_t out_i16[32][4]) {
    proto->ops->int16(proto->ctx, i16, out_i16);
}

static inline void arrayof_arrays_int32(const arrayof_arrays_protocol_t* proto, const int32_t i32[32][4], int32_t out_i32[32][4]) {
    proto->ops->int32(proto->ctx, i32, out_i32);
}

static inline void arrayof_arrays_int64(const arrayof_arrays_protocol_t* proto, const int64_t i64[32][4], int64_t out_i64[32][4]) {
    proto->ops->int64(proto->ctx, i64, out_i64);
}

static inline void arrayof_arrays_uint8(const arrayof_arrays_protocol_t* proto, const uint8_t u8[32][4], uint8_t out_u8[32][4]) {
    proto->ops->uint8(proto->ctx, u8, out_u8);
}

static inline void arrayof_arrays_uint16(const arrayof_arrays_protocol_t* proto, const uint16_t u16[32][4], uint16_t out_u16[32][4]) {
    proto->ops->uint16(proto->ctx, u16, out_u16);
}

static inline void arrayof_arrays_uint32(const arrayof_arrays_protocol_t* proto, const uint32_t u32[32][4], uint32_t out_u32[32][4]) {
    proto->ops->uint32(proto->ctx, u32, out_u32);
}

static inline void arrayof_arrays_uint64(const arrayof_arrays_protocol_t* proto, const uint64_t u64[32][4], uint64_t out_u64[32][4]) {
    proto->ops->uint64(proto->ctx, u64, out_u64);
}

static inline void arrayof_arrays_float32(const arrayof_arrays_protocol_t* proto, const float f32[32][4], float out_f32[32][4]) {
    proto->ops->float32(proto->ctx, f32, out_f32);
}

static inline void arrayof_arrays_float64(const arrayof_arrays_protocol_t* proto, const double u64[32][4], double out_f64[32][4]) {
    proto->ops->float64(proto->ctx, u64, out_f64);
}

static inline void arrayof_arrays_handle(const arrayof_arrays_protocol_t* proto, const zx_handle_t u64[32][4], zx_handle_t out_f64[32][4]) {
    proto->ops->handle(proto->ctx, u64, out_f64);
}


__END_CDECLS
