/* origin: FreeBSD /usr/src/lib/msun/src/math_private.h */
/*
 * ====================================================
 * Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
 *
 * Developed at SunPro, a Sun Microsystems, Inc. business.
 * Permission to use, copy, modify, and distribute this
 * software is freely granted, provided that this notice
 * is preserved.
 * ====================================================
 */

#pragma once

#include <complex.h>
#include <endian.h>
#include <float.h>
#include <math.h>
#include <stdint.h>

// Clang does not support the C99 rounding mode pragma. Support seems
// unlikely to be coming soon, but for reference the clang/llvm bug
// tracking this fact may be found at:
// https://llvm.org/bugs/show_bug.cgi?id=8100
#define PRAGMA_STDC_FENV_ACCESS_ON

// GCC complains about the STDC CX_LIMITED_RANGE pragma.
#define PRAGMA_STDC_CX_LIMITED_RANGE_ON

#if LDBL_MANT_DIG == 53 && LDBL_MAX_EXP == 1024
#elif LDBL_MANT_DIG == 64 && LDBL_MAX_EXP == 16384 && __BYTE_ORDER == __LITTLE_ENDIAN
union ldshape {
  long double f;
  struct {
    uint64_t m;
    uint16_t se;
  } i;
};
#elif LDBL_MANT_DIG == 113 && LDBL_MAX_EXP == 16384 && __BYTE_ORDER == __LITTLE_ENDIAN
union ldshape {
  long double f;
  struct {
    uint64_t lo;
    uint32_t mid;
    uint16_t top;
    uint16_t se;
  } i;
  struct {
    uint64_t lo;
    uint64_t hi;
  } i2;
};
#elif LDBL_MANT_DIG == 113 && LDBL_MAX_EXP == 16384 && __BYTE_ORDER == __BIG_ENDIAN
union ldshape {
  long double f;
  struct {
    uint16_t se;
    uint16_t top;
    uint32_t mid;
    uint64_t lo;
  } i;
  struct {
    uint64_t hi;
    uint64_t lo;
  } i2;
};
#else
#error Unsupported long double representation
#endif

#define FORCE_EVAL(x)                         \
  do {                                        \
    if (sizeof(x) == sizeof(float)) {         \
      volatile float __x;                     \
      __x = (x);                              \
      (void)__x;                              \
    } else if (sizeof(x) == sizeof(double)) { \
      volatile double __x;                    \
      __x = (x);                              \
      (void)__x;                              \
    } else {                                  \
      volatile long double __x;               \
      __x = (x);                              \
      (void)__x;                              \
    }                                         \
  } while (0)

/* Get two 32 bit ints from a double.  */
#define EXTRACT_WORDS(hi, lo, d) \
  do {                           \
    union {                      \
      double f;                  \
      uint64_t i;                \
    } __u;                       \
    __u.f = (d);                 \
    (hi) = __u.i >> 32;          \
    (lo) = (uint32_t)__u.i;      \
  } while (0)

/* Get the more significant 32 bit int from a double.  */
#define GET_HIGH_WORD(hi, d) \
  do {                       \
    union {                  \
      double f;              \
      uint64_t i;            \
    } __u;                   \
    __u.f = (d);             \
    (hi) = __u.i >> 32;      \
  } while (0)

/* Get the less significant 32 bit int from a double.  */
#define GET_LOW_WORD(lo, d) \
  do {                      \
    union {                 \
      double f;             \
      uint64_t i;           \
    } __u;                  \
    __u.f = (d);            \
    (lo) = (uint32_t)__u.i; \
  } while (0)

/* Set a double from two 32 bit ints.  */
#define INSERT_WORDS(d, hi, lo)                      \
  do {                                               \
    union {                                          \
      double f;                                      \
      uint64_t i;                                    \
    } __u;                                           \
    __u.i = ((uint64_t)(hi) << 32) | (uint32_t)(lo); \
    (d) = __u.f;                                     \
  } while (0)

/* Set the more significant 32 bits of a double from an int.  */
#define SET_HIGH_WORD(d, hi)       \
  do {                             \
    union {                        \
      double f;                    \
      uint64_t i;                  \
    } __u;                         \
    __u.f = (d);                   \
    __u.i &= 0xffffffff;           \
    __u.i |= (uint64_t)(hi) << 32; \
    (d) = __u.f;                   \
  } while (0)

/* Set the less significant 32 bits of a double from an int.  */
#define SET_LOW_WORD(d, lo)         \
  do {                              \
    union {                         \
      double f;                     \
      uint64_t i;                   \
    } __u;                          \
    __u.f = (d);                    \
    __u.i &= 0xffffffff00000000ull; \
    __u.i |= (uint32_t)(lo);        \
    (d) = __u.f;                    \
  } while (0)

/* Get a 32 bit int from a float.  */
#define GET_FLOAT_WORD(w, d) \
  do {                       \
    union {                  \
      float f;               \
      uint32_t i;            \
    } __u;                   \
    __u.f = (d);             \
    (w) = __u.i;             \
  } while (0)

/* Set a float from a 32 bit int.  */
#define SET_FLOAT_WORD(d, w) \
  do {                       \
    union {                  \
      float f;               \
      uint32_t i;            \
    } __u;                   \
    __u.i = (w);             \
    (d) = __u.f;             \
  } while (0)

#undef __CMPLX
#undef CMPLX
#undef CMPLXF
#undef CMPLXL

#define __CMPLX(x, y, t)  \
  ((union {               \
     _Complex t __z;      \
     t __xy[2];           \
   }){.__xy = {(x), (y)}} \
       .__z)

#define CMPLX(x, y) __CMPLX(x, y, double)
#define CMPLXF(x, y) __CMPLX(x, y, float)
#define CMPLXL(x, y) __CMPLX(x, y, long double)

// This is a macro for performing a cast from a uint32_t to an int32_t as a
// signed 32-bit two's complement representation. This is a platform-independent
// way of performing two's complement without assuming the underlying
// representation of a signed int is two's complement. With optimizations, Clang
// treats this as a no-op.
#define TWOS_COMPLEMENT_UINT32_TO_INT32(x) \
    (((x) <= INT32_MAX) ? (int32_t)(x) : ((int32_t)((x) - (uint32_t)(INT32_MIN)) + INT32_MIN))

/* fdlibm kernel functions */

int __rem_pio2_large(double*, double*, int, int, int);

int __rem_pio2(double, double*);
double __sin(double, double, int);
double __cos(double, double);
double __tan(double, double, int);
double __expo2(double);
double complex __ldexp_cexp(double complex, int);

int __rem_pio2f(float, double*);
float __sindf(double);
float __cosdf(double);
float __tandf(double, int);
float __expo2f(float);
float complex __ldexp_cexpf(float complex, int);

int __rem_pio2l(long double, long double*);
long double __sinl(long double, long double, int);
long double __cosl(long double, long double);
long double __tanl(long double, long double, int);

/* polynomial evaluation */
long double __polevll(long double, const long double*, int);
long double __p1evll(long double, const long double*, int);
