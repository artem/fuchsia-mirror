// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_ABI_H_
#define LIB_LD_ABI_H_

// This defines a common "passive" ABI that runtime code like a C library can
// use to interrogate basic dynamic linking details.  It's called a "passive"
// ABI because it exports almost no entry points, but only some immutable data
// structures and the ELF symbol names by which to find them.
//
// The traditional PT_INTERP dynamic linker sets up this data in memory while
// doing the initial-exec dynamic linking, and then makes it all read-only so
// it's guaranteed never to change again.  The runtime dynamic linking support
// (-ldl) can ingest this data into its own data structures and manage those
// to provide a richer runtime ABI.  Basic fallback implementations of simple
// support calls like dl_iterate_phdr and dlsym can be provided by the C
// library when libdl.so is not linked in.
//
// For out-of-process dynamic linking, a simple stub implementation of this
// same ABI can be loaded in lieu of the traditional dynamic linker, giving
// the same simple runtime ABI for data that is populated out of process.

#include <lib/elfldltl/abi-ptr.h>
#include <lib/elfldltl/abi-span.h>
#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/svr4-abi.h>
#include <lib/elfldltl/symbol.h>
#include <lib/elfldltl/tls-layout.h>

#include <string_view>

namespace ld::abi {

template <class Elf = elfldltl::Elf<>, class AbiTraits = elfldltl::LocalAbiTraits>
struct Abi {
  // Aliases to avoid using `typename` all over the place.
  using Word = typename Elf::Word;
  using Addr = typename Elf::Addr;
  using Phdr = typename Elf::Phdr;
  using LinkMap = typename Elf::template LinkMap<AbiTraits>;
  using RDebug = typename Elf::template RDebug<AbiTraits>;

  // All pointer members in Abi and inner types must use Ptr<T> in place of T*.
  template <typename T>
  using Ptr = elfldltl::AbiPtr<T, Elf, AbiTraits>;

  // Members in Abi and inner types use Span<T> in place of std::span<T>.
  template <typename T, size_t N = cpp20::dynamic_extent>
  using Span = elfldltl::AbiSpan<T, N, Elf, AbiTraits>;

  // This is a shorthand for instantiating the elfldltl types that take Elf and
  // AbiTraits template parameters, such as elfldltl::SymbolInfo.
  template <template <class, class> class Class>
  using Type = Class<Elf, AbiTraits>;

  // Forward declarations for types declared in module.h and tls.h.
  struct Module;
  struct TlsModule;

  // This lists all the initial-exec modules.  Embedded `link_map::l_prev` and
  // `link_map::l_next` form a doubly-linked list in load order, which is a
  // breadth-first pre-order of the DT_NEEDED dependencies where the main
  // executable is always first and dependents always precede dependencies
  // (except for any redundancies).
  Ptr<const Module> loaded_modules;

  // TLS details for initial-exec modules that have PT_TLS segments.  The entry
  // at index `.tls_mod_id - 1` describes that module's PT_TLS.  A module with
  // `.tls_mod_id == 0` has no PT_TLS segment.  TLS module ID numbers above
  // static_tls_modules.size() are not used at startup but may be assigned to
  // dynamically-loaded modules later.
  Span<const TlsModule> static_tls_modules;

  // This is a parallel array to `static_tls_modules` indexed the same way.  It
  // holds the offset from the thread pointer to each module's segment in the
  // static TLS block.  The entry at index `.tls_mod_id - 1` is the offset of
  // that module's PT_TLS segment.
  //
  // This is kept in a simple flat array rather than in a member in each
  // TlsModule so that very simple and efficient code can look up the offsets,
  // perhaps implemented in assembly (such as in TLSDESC hook code or an
  // implementation of __tls_get_addr).  TlsModule describes strictly the
  // information extracted from the module's PT_TLS segment itself and is only
  // used when initializing new thread's static TLS area.  The offsets are
  // dynamically assigned by the dynamic linker at startup, and are referenced
  // by each GD/LD access callback into the runtime.
  //
  // This offset is actually a negative number on some machines like x86, but
  // it's always calculated using address-sized unsigned arithmetic.  On some
  // machines where it's non-negative, there is a nonempty psABI-specified
  // reserved region right after the thread pointer, so a real offset is never
  // zero; other machines like RISC-V do start the first module at offset zero.
  // The ordering of offsets after the first is theoretically arbitrary but is
  // in fact ascending.  When the main executable has a PT_TLS it must have
  // `.tls_mod_id` of 1 and it must have the smallest offset since this is
  // statically calculated by the linker for Local-Exec model accesses based
  // only on the psABI's fixed offset and the PT_TLS alignment requirement.
  Span<const Addr> static_tls_offsets;

  // This gives the required size and alignment of the overall static TLS area.
  // The alignment matches the max of static_tls_modules[...].tls_alignment and
  // any psABI-specified minimum alignment.  The runtime is responsible for
  // adding in space and adjusting alignment as needed for any private data
  // structures or ABI-mandated fixed-offset slots (e.g. stack canary value,
  // secondary stack pointer pseudo-register).  When the thread pointer points
  // to (or after, e.g. on x86) memory of at least this size and alignment,
  // then `$tp + .static_tls_offsets[tls_modid - 1]` will yield a pointer with
  // the space and alignment expected by that module's PT_TLS.
  elfldltl::TlsLayout<Elf> static_tls_layout;
};

// This is the DT_SONAME value representing the ABI declared in this file.
inline constexpr elfldltl::Soname<> kSoname{"ld.so.1"};

// This is the standard PT_INTERP value for using a compatible dynamic linker
// as the startup dynamic linker.  The actual PT_INTERP value in an executable
// ET_DYN file might have a prefix to select a particular implementation.
inline constexpr std::string_view kInterp = kSoname.str();

// These are the sole exported symbols, with hash values cached statically.
inline constexpr elfldltl::SymbolName kAbiSymbol{"_ld_abi"};
inline constexpr elfldltl::SymbolName kRDebugSymbol{"_r_debug"};

// These are the sole exported symbols in the ld.so ABI.  They should be used
// in C++ via their scoped names such as ld::_ld_abi normally.  But the ld.so
// symbolic ABI does not include any C++ name mangling, so they use simple C
// linkage names in the name space reserved for the implementation.

extern "C" {

// This is the single hook to find the ld.so passive ABI described above.
extern const Abi<> _ld_abi;

// This is not meant to be used as part of the passive ABI, really.  However,
// traditionally debuggers expect to find it as a symbol in the dynamic linker,
// and exporting it makes sure they can even without debugging symbols.
extern const Abi<>::RDebug _r_debug;

}  // extern "C"

}  // namespace ld::abi

#endif  // LIB_LD_ABI_H_
