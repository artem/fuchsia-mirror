// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TLSDESC_H_
#define LIB_LD_TLSDESC_H_

#include <lib/arch/asm.h>

// These declarations relate to the TLSDESC runtime ABI.  While the particular
// ABI details are specific to each machine, they all fit a common pattern.
//
// The R_*_TLSDESC relocation type directs dynamic linking to fill in a special
// pair of adjacent GOT slots.  The first slot is unfilled at link time and
// gets the PC of a special function provided by the dynamic linking runtime.
// For each TLS reference, the compiler generates an indirect call via this GOT
// slot.  The compiler also passes the address of its GOT slot to the function.
//
// This is a normal indirect call at the machine level.  However, it uses its
// own bespoke calling convention specified in the psABI for each machine
// rather than the standard C/C++ calling convention.  The convention for each
// machine is similar: the use of the return address register and/or stack is
// normal; one or two registers are designated for the argument (GOT address),
// return value, and scratch; all other registers are preserved by the call,
// except the condition codes.  The return value is a signed offset from the
// psABI-specified thread-pointer register.  Notably, it's expected to be added
// to the thread pointer to yield a valid pointer or nullptr for undefined weak
// symbol references, so it may be a difference of unrelated pointers to reach
// a heap address not near the thread pointer or to reach zero for nullptr.
//
// This makes the impact of the runtime call on code generation very minimal.
// The runtime implementation both can refer to the value stored in the GOT
// slot by dynamic linking and can in theory dynamically update both slots to
// lazily redirect to a different runtime entry point and argument data.
//
// The relocation's symbol and addend are meant to apply to the second GOT slot
// of the pair.  (For DT_REL format, the addend is stored in place there.)
// When dynamic linking chooses an entry point to store into the first GOT slot
// it also chooses the value to store in the second slot, which is some kind of
// offset or address that includes the addend and symbol value calculations.

// This enumerates the canonical entry points provided by a startup dynamic
// linker for the different kinds of TLSDESC resolution needed in the simple
// static TLS cases.  These entry points and their semantics are not part of
// the TLSDESC ABI, they are just implementation details provided by library
// code that can be used in a dynamic linker implementation.  Each of these
// canonical implementations has a corresponding definition of what must be
// stored in the second GOT slot, below called the "argument" (though each
// psABI calling convention actually passes the address of the first GOT slot
// in a register, from which the adjacent slot must be loaded).
//
//  * kStatic handles a fixed offset from the thread pointer.  The argument is
//    that final offset, which already includes any relocation addend as well
//    as the symbol's offset into the PT_TLS and the definer's TLS block's
//    offset from the thread pointer.  The hook returns that offset to be added
//    to the thread pointer: the same offset in every thread so the TLS block
//    offset must be statically assigned before new threads can be created.
//    The dynamic linker must perform the static TLS layout to assign
//    correctly-aligned and sized regions relative to the thread pointer
//    (<lib/elfldltl/tls-layout.h> handles the details of that arithmetic), and
//    then install this entry point alongside the relocation's symbol's
//    definer's TLS block offset plus the symbol value and relocation addend.
//
//  * kUndefinedWeak handles an undefined weak STT_TLS symbol where there was
//    no relocation addend.  The argument is ignored.  The hook returns the
//    thread pointer negated, so the address resolves as zero (a null pointer)
//    when the thread pointer is added to the return value.  The dynamic linker
//    need only store the entry point in the GOT.
//
//  * kUndefinedWeakAddend handles an undefined weak STT_TLS symbol where there
//    was a nonzero relocation addend.  The argument is the addend.  The hook
//    returns the thread pointer negated plus the addend, to yield the same
//    result as applying the addend to a byte pointer that was null.  The
//    dynamic linker must store the entry point and addend in the GOT.
//
#define LD_TLSDESC_STATIC_HOOKS(Macro) \
  Macro(kStatic) Macro(kUndefinedWeak) Macro(kUndefinedWeakAddend)

#define LD_TLSDESC_STATIC_HOOKS_FIRST_MAGIC 0x00534c54  // LE 'TLS\0'

#ifdef __ASSEMBLER__  // clang-format off

// In assembly, `.tlsdesc.lsda kName` with one of the LD_TLSDESC_STATIC_HOOKS
// names described above yields a `.cfi_lsda DW_EH_PE_absptr, <magic number>`
// that will give the current FDE an LSDA value magic corresponding to the
// ld::TlsdescStaticHook C++ enum defined below.  The library's implementation
// code uses these to aid in locating entry points in the stub dynamic linker
// with no ELF symbols.
.macro .tlsdesc.lsda name
  .cfi_lsda 0, .Ltlsdesc.hook.\name // 0 is DW_EH_PE_absptr encoding: no reloc.
.endm
  // This bit just gets all the .Ltlsdesc.hook.\name values defined.
  // These local symbols will usually be elided from the symbol table.
  .macro _.tlsdesc.hook.const name
    .Ltlsdesc.hook.\name = LD_TLSDESC_STATIC_HOOKS_FIRST_MAGIC + \@ - 1
  .endm
#define _LD_TLSDESC_HOOK_CONST(name) _.tlsdesc.hook.const name;
  LD_TLSDESC_STATIC_HOOKS(_LD_TLSDESC_HOOK_CONST)
#undef _LD_TLSDESC_HOOK_CONST
  .purgem _.tlsdesc.hook.const

// The pseudo-op `.tlsdesc.cfi`, given `.cfi_startproc` initial state,
// resets CFI to indicate the special ABI for the R_*_TLSDESC callback
// function on this machine.
//
// Other conveniences are defined specific to each machine.

#if defined(__aarch64__)

.macro .tlsdesc.cfi
  // Almost all registers are preserved from the caller.  The integer set does
  // not include x30 (LR) or SP, which .cfi_startproc covered.
  .cfi.all_integer .cfi_same_value
  .cfi.all_vectorfp .cfi_same_value
.endm

// On AArch64 ILP32, GOT entries are 4 bytes, not 8.
# ifdef _LP64
tlsdesc.r0 .req x0
tlsdesc.r1 .req x1
tlsdesc.value_offset = 8
# else
tlsdesc.r0 .req w0
tlsdesc.r1 .req w1
tlsdesc.value_offset = 4
# endif

#elif defined(__riscv)

.macro .tlsdesc.cfi
  // Almost all registers are preserved from the caller.  The integer set does
  // not include sp, which .cfi_startproc covered.
  .cfi.all_integer .cfi_same_value
  .cfi.all_vectorfp .cfi_same_value

  // The return address is in t0 rather than the usual ra, and preserved there.
  .cfi_return_column t0
.endm

# ifdef _LP64
.macro tlsdesc.load reg, mem
  ld \reg, \mem
.endm
.macro tlsdesc.sub rd, r1, r2
  sub \rd, \r1, \r2
.endm
tlsdesc.value_offset = 8
# else
.macro tlsdesc.load reg, mem
  lw \reg, \mem
.endm
.macro tlsdesc.sub rd, r1, r2
  subw \rd, \r1, \r2
.endm
tlsdesc.value_offset = 4
# endif

#elif defined(__x86_64__)

.macro .tlsdesc.cfi
  // Almost all registers are preserved from the caller.  The integer set does
  // not include %rsp, which .cfi_startproc covered.
  .cfi.all_integer .cfi_same_value
  .cfi.all_vectorfp .cfi_same_value
.endm

# ifdef _LP64
#define tlsdesc_ax rax
tlsdesc.value_offset = 4
# else
#define tlsdesc_ax eax
tlsdesc.value_offset = 8
# endif

#else

// Not all machines have TLSDESC support specified in the psABI.

#endif

#else  // clang-format on

#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/symbol.h>

#include <cstddef>
#include <optional>

namespace [[gnu::visibility("hidden")]] ld {

// These are callback functions to be used in the TlsDescGot::function slot
// at runtime.  Though they're declared here as C++ functions with an
// argument, they're actually implemented in assembly code with a bespoke
// calling convention for the argument, return value, and register usage
// that's different from normal functions, so these cannot actually be
// called from C++.  These symbol names are not visible anywhere outside
// the dynamic linking implementation itself and these functions are only
// ever called by compiler-generated TLSDESC references.

extern "C" {

// These are used for undefined weak definitions.  The value slot contains just
// the addend; the first entry-point ignores the addend and is cheaper for a
// zero addend (the most common case), while the second supports an addend.
// The implementation returns the addend minus the thread pointer, such that
// adding the thread pointer back to this offset produces zero with a zero
// addend, and thus nullptr.
ptrdiff_t _ld_tlsdesc_runtime_undefined_weak(const elfldltl::Elf<>::TlsDescGot& got);
ptrdiff_t _ld_tlsdesc_runtime_undefined_weak_addend(const elfldltl::Elf<>::TlsDescGot& got);

// In this minimal implementation used for PT_TLS segments in the static TLS
// set, desc.valueu is always simply a fixed offset from the thread pointer.
// Note this offset might be negative, but it's always handled as uintptr_t to
// ensure well-defined overflow arithmetic.
ptrdiff_t _ld_tlsdesc_runtime_static(const elfldltl::Elf<>::TlsDescGot& got);

}  // extern "C"

// ld::TlsdescRuntime::k* can be used as indices into an array of
// kTlsdescRuntimeCount to cover all the entry points.

enum class TlsdescRuntime {
#define LD_TLSDESC_RUNTIME_CONST(name) name,
  LD_TLSDESC_STATIC_HOOKS(LD_TLSDESC_RUNTIME_CONST)
#undef LD_TLSDESC_RUNTIME_CONST
};

inline constexpr size_t kTlsdescRuntimeCount = []() {
  size_t n = 0;
#define LD_TLSDESC_RUNTIME_MAX(name) ++n;
  LD_TLSDESC_STATIC_HOOKS(LD_TLSDESC_RUNTIME_MAX)
#undef LD_TLSDESC_RUNTIME_MAX
  return n;
}();

// For each hook there is a different uint32_t magic number that will be found
// in its FDE's LSDA.
template <TlsdescRuntime Hook>
inline constexpr uint32_t kTlsdescRuntimeMagic =
    LD_TLSDESC_STATIC_HOOKS_FIRST_MAGIC + static_cast<uint32_t>(Hook);

// In the stub dynamic linker, the only FDEs should be for these entry points,
// so each one's LSDA should correspond to one of the TlsdescRuntime indices.
constexpr std::optional<TlsdescRuntime> TlsdescRuntimeFromMagic(uintptr_t magic) {
  if (magic >= LD_TLSDESC_STATIC_HOOKS_FIRST_MAGIC &&
      magic < LD_TLSDESC_STATIC_HOOKS_FIRST_MAGIC + kTlsdescRuntimeCount) {
    magic -= LD_TLSDESC_STATIC_HOOKS_FIRST_MAGIC;
    return static_cast<TlsdescRuntime>(magic);
  }
  return std::nullopt;
}

}  // namespace ld

#endif  // __ASSEMBLER__

#endif  // LIB_LD_TLSDESC_H_
