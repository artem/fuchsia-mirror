// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef FFL_FIXED_FORMAT_H_
#define FFL_FIXED_FORMAT_H_

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <type_traits>

#include <ffl/saturating_arithmetic.h>
#include <ffl/utility.h>

namespace ffl {

// Forward declaration.
template <typename Integer, size_t FractionalBits>
struct FixedFormat;

// Type representing an intermediate value of a given FixedFormat.
template <typename>
struct Value;

template <typename Integer, size_t FractionalBits>
struct Value<FixedFormat<Integer, FractionalBits>> {
  using Format = FixedFormat<Integer, FractionalBits>;

  explicit constexpr Value(Integer value) : value{value} {}
  const Integer value;
};

// Predicate to determine whether the given integer type and number of
// fractional bits is valid.
template <typename Integer, size_t FractionalBits>
static constexpr bool FormatIsValid =
    (std::is_signed_v<Integer> && FractionalBits < sizeof(Integer) * 8) ||
    (std::is_unsigned_v<Integer> && FractionalBits <= sizeof(Integer) * 8);

// Type representing the format of a fixed-point value in terms of the
// underlying integer type and fractional precision. Provides key constants and
// operations for fixed-point computation and format manipulation.
template <typename Integer_, size_t FractionalBits_>
struct FixedFormat {
  static_assert(std::is_integral_v<Integer_>,
                "The Integer template parameter must be an integral type!");
  static_assert(FormatIsValid<Integer_, FractionalBits_>,
                "The number of fractional bits must fit within the positive bits!");
  static_assert(sizeof(Integer_) * 8 <= 64,
                "The Integer template paramter must have at most 64 bits!");

  // The underlying integral type of the fixed-point values in this format.
  using Integer = Integer_;

  // Indicates whether the underlying integer is singed or unsigned.
  static constexpr bool IsSigned = std::is_signed_v<Integer>;
  static constexpr bool IsUnsigned = std::is_unsigned_v<Integer>;

  // Numeric constants for fixed-point computations.
  static constexpr size_t Bits = sizeof(Integer) * 8;
  static constexpr size_t FractionalBits = FractionalBits_;
  static constexpr size_t IntegralBits = Bits - FractionalBits - (IsSigned ? 1 : 0);
  static constexpr size_t PositiveBits = IntegralBits + FractionalBits;
  static constexpr size_t Power = FractionalBits == 64 ? 0 : size_t{1} << FractionalBits;

  static constexpr Integer One = 1;  // Typed constant used in shifts below.
  static constexpr Integer FractionalMask = Power - 1;
  static constexpr Integer IntegralMask = ~FractionalMask;
  static constexpr Integer SignBit = IsSigned ? One << (Bits - 1) : 0;
  static constexpr Integer BinaryPoint = FractionalBits > 0 ? One << (FractionalBits - 1) : 0;
  static constexpr Integer OnesPlace = One << FractionalBits;

  // Indicates whether positive one can only be represented fractionally.
  // That is, the format has zero positive integral bits.
  static constexpr bool ApproximateUnit =
      (IsSigned && FractionalBits == Bits - 1) || FractionalBits == Bits;

  // Adjusted numeric constants for conversions that need headroom when there
  // are zero positive integral bits.
  static constexpr size_t AdjustedFractionalBits = FractionalBits - (ApproximateUnit ? 1 : 0);
  static constexpr size_t AdjustedPower = size_t{1} << AdjustedFractionalBits;
  static constexpr Integer AdjustmentFactor = ApproximateUnit ? 2 : 1;
  static constexpr Integer AdjustedFractionalMask = AdjustedPower - 1;
  static constexpr Integer AdjustedIntegralMask = ~AdjustedFractionalMask;

  static constexpr Integer Min = std::numeric_limits<Integer>::min();
  static constexpr Integer Max = std::numeric_limits<Integer>::max();
  static constexpr Integer IntegralMin =
      FractionalBits == 64 ? 0 : static_cast<Integer>(Min / Power);
  static constexpr Integer IntegralMax =
      FractionalBits == 64 ? 0 : static_cast<Integer>(Max / Power);

  // Saturates an intermediate value to the valid range of the base type.
  template <typename I, typename = std::enable_if_t<std::is_integral_v<I>>>
  static constexpr Integer Saturate(I value) {
    return ClampCast<Integer>(value);
  }
  static constexpr Integer Saturate(Value<FixedFormat> value) { return Saturate(value.value); }

  // Rounds |value| to the given significant bit |Place| using the convergent,
  // or round-half-to-even, method to eliminate positive/negative and
  // towards/away from zero biases. This is the default rounding mode used in
  // IEEE 754 computing functions and operators.
  //
  // References:
  //   https://en.wikipedia.org/wiki/Rounding#Round_half_to_even
  //   https://en.wikipedia.org/wiki/Nearest_integer_function
  //
  // For example, rounding an 8bit value to bit 4 produces these values in the
  // constants defined below:
  //
  // uint8_t value = vvvphmmm
  //
  // PlaceBit      = 00010000 -> 000p0000
  // PlaceMask     = 11110000 -> vvvp0000
  // HalfBit       = 00001000 -> 0000h000
  // BelowHalfMask = 00000111 -> 00000mmm
  //
  // Rounding half to even is computed as follows:
  //
  //    PlaceBit        00010000
  //    BelowHalfMask   00000111
  // |  ------------------------
  //                    00010111
  //    value           vvvvvvvv
  // &  ------------------------
  //                    000v0vvv
  //                    non-zero
  // ?  ------------------------
  //    HalfBit         00001000
  // :  zero            00000000
  //    ------------------------
  //    round_bit       0000r000
  //    value           vvvvvvvv
  // +  ------------------------
  //    rounded         rrrrxxxx
  //    PlaceMask       11110000
  // &  ------------------------
  //    result          rrrr0000
  //
  template <size_t Place, typename = std::enable_if_t<(Place < PositiveBits)>>
  static constexpr Integer Round(Integer value, Bit<Place>) {
    // Bit of the significant figure to round to and mask of the significant
    // bits after rounding.
    const Integer PlaceBit = Integer{1} << Place;
    const Integer PlaceMask = ~(PlaceBit - 1);

    // Bit representing one half of the significant figure to round to
    // and mask of the bits below it, if any.
    const Integer HalfBit = Integer{1} << (Place - 1);
    const Integer BelowHalfMask = ~PlaceMask >> 1;

    // Round half to even.
    const Integer round_bit = (value & (PlaceBit | BelowHalfMask)) ? HalfBit : 0;
    const Integer rounded = SaturateAddAs<Integer>(value, round_bit);
    return rounded & PlaceMask;
  }

  // Rounding to the 0th bit is a no-op.
  static constexpr Integer Round(Integer value, Bit<0>) { return value; }

  // Rounds |value| around the integer position.
  static constexpr Integer Round(Integer value) { return Round(value, ToPlace<FractionalBits>); }

  // Converts an intermediate value in SourceFormat to this format, rounding
  // as necessary.
  template <typename SourceFormat>
  static constexpr Value<FixedFormat> Convert(Value<SourceFormat> value) {
    using Intermediate =
        BestFitting<IsSigned, std::max(SourceFormat::IntegralBits, IntegralBits) +
                                  std::max(SourceFormat::FractionalBits, FractionalBits)>;
    using IntermediateFormat = FixedFormat<Intermediate, SourceFormat::FractionalBits>;

    // Convert to the common precision. This will only clamp when converting
    // from a negative signed value to unsigned or when converting a large
    // unsigned 64bit value to signed. All other cases optimize out.
    const Intermediate promoted_value = ClampCast<Intermediate>(value.value);

    // Increase or decrease the source resolution to match this format.
    if constexpr (SourceFormat::FractionalBits > FractionalBits) {
      const Intermediate shifted_value = promoted_value / IntermediateFormat::AdjustmentFactor;
      const size_t delta = IntermediateFormat::AdjustedFractionalBits - FractionalBits;

      const auto power = Intermediate{1} << delta;
      const auto converted_value = IntermediateFormat::Round(shifted_value, ToPlace<delta>) / power;
      return Value<FixedFormat>{ClampCast<Integer>(converted_value)};
    } else if (SourceFormat::FractionalBits < FractionalBits) {
      const auto factor = std::max(IntermediateFormat::AdjustmentFactor,
                                   static_cast<Intermediate>(AdjustmentFactor));
      const auto shifted_value = SaturateMultiplyAs<Intermediate>(promoted_value, factor);
      const size_t delta = AdjustedFractionalBits - IntermediateFormat::AdjustedFractionalBits;

      const auto power = Intermediate{1} << delta;
      const auto converted_value = SaturateMultiplyAs<Integer>(shifted_value, power);
      return Value<FixedFormat>{converted_value};
    } else {
      return Value<FixedFormat>{ClampCast<Integer>(promoted_value)};
    }
  }

  // Converting to the same format is a no-op.
  static constexpr Value<FixedFormat> Convert(Value<FixedFormat> value) { return value; }
};

}  // namespace ffl

#endif  // FFL_FIXED_FORMAT_H_
