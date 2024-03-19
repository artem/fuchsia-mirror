// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_FIDL_CPP_INCLUDE_LIB_FIDL_CPP_ANY_ERROR_IN_H_
#define SRC_LIB_FIDL_CPP_INCLUDE_LIB_FIDL_CPP_ANY_ERROR_IN_H_

#include <lib/fidl/cpp/unified_messaging.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/string_view.h>
#include <lib/stdcompat/variant.h>

#include <array>
#include <string>

namespace fidl {
namespace internal {

// A helper to erase the |DomainError| template type and share the common
// printing logic.
class ErrorsInBase {
 protected:
  using FormattingBuffer = std::array<char, 512>;

  // |prelude| will be printed first.
  // |display_error| invokes the |DisplayError<T>| specialization.
  // Returns how many bytes are written, excluding the NUL terminator.
  static size_t FormatImpl(const char* prelude, FormattingBuffer& buffer,
                           fit::inline_callback<size_t(char*, size_t)> display_error);

  template <typename DomainError>
  static size_t Format(FormattingBuffer& buffer,
                       const cpp17::variant<fidl::Error, DomainError>& error) {
    return FormatImpl(Prelude(error), buffer, [&](char* destination, size_t capacity) {
      return std::visit(
          [&](auto&& error) { return FormatDisplayError(error, destination, capacity); }, error);
    });
  }

 private:
  template <typename DomainError>
  static const char* Prelude(const cpp17::variant<fidl::Error, DomainError>& error) {
    switch (error.index()) {
      case 0:
        return kFrameworkErrorPrelude;
      case 1:
        return kDomainErrorPrelude;
      default:
        __builtin_trap();
    }
  }

  static const char* kFrameworkErrorPrelude;
  static const char* kDomainErrorPrelude;
};

// |ErrorsInImpl| is used to implement |ErrorsIn|.
//
// |DomainError| must be a domain object generated by the FIDL toolchain.
// |DomainError| must be int32, uint32, or an enum of one of those types.
template <typename DomainError>
class ErrorsInImpl : private ErrorsInBase {
 public:
  explicit ErrorsInImpl(fidl::Error framework_error) : error_(framework_error) {}
  explicit ErrorsInImpl(DomainError domain_error) : error_(domain_error) {}

  // Check if the error is a framework error: an error from the FIDL framework.
  bool is_framework_error() const { return cpp17::holds_alternative<fidl::Error>(error_); }

  // Accesses the framework error: an error from the FIDL framework.
  const fidl::Error& framework_error() const { return cpp17::get<fidl::Error>(error_); }

  // Accesses the framework error: an error from the FIDL framework.
  fidl::Error& framework_error() { return cpp17::get<fidl::Error>(error_); }

  // Check if the error is a domain error: an error defined in the method from the FIDL schema.
  bool is_domain_error() const { return cpp17::holds_alternative<DomainError>(error_); }

  // Accesses the domain error: an error defined in the method from the FIDL schema.
  const DomainError& domain_error() const { return cpp17::get<DomainError>(error_); }

  // Accesses the domain error: an error defined in the method from the FIDL schema.
  DomainError& domain_error() { return cpp17::get<DomainError>(error_); }

  // Prints a description of the error.
  //
  // If a logging API supports output streams (`<<` operators), piping the
  // error to the log via `<<` is more efficient than calling this function.
  std::string FormatDescription() const {
    FormattingBuffer buffer;
    size_t length = Format(buffer, error_);
    return std::string(&(*buffer.begin()), length);
  }

 private:
  friend std::ostream& operator<<(std::ostream& ostream, const ErrorsInImpl& any_error) {
    FormattingBuffer buffer;
    size_t length = Format(buffer, any_error.error_);
    ostream << cpp17::string_view(&(*buffer.begin()), length);
    return ostream;
  }

  cpp17::variant<fidl::Error, DomainError> error_;
};

}  // namespace internal

// |ErrorsIn<Method>| represents the set of all possible errors during a
// fallible two-way |Method|:
//
//   - Framework errors: errors from the FIDL framework.
//   - Domain errors: errors defined in the |Method| from the FIDL schema.
//
// Users should first inspect if an instance contains a framework or domain
// error, before diving into the individual variant. Alternatively, they may
// print a description either by piping it to an |std::ostream| or calling
// |FormatDescription|.
template <typename FidlMethod>
class ErrorsIn : public internal::ErrorsInImpl<internal::NaturalDomainError<FidlMethod>> {
 public:
  using internal::ErrorsInImpl<internal::NaturalDomainError<FidlMethod>>::ErrorsInImpl;
};

}  // namespace fidl

#endif  // SRC_LIB_FIDL_CPP_INCLUDE_LIB_FIDL_CPP_ANY_ERROR_IN_H_
