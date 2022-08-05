// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2009 Corey Tabaka
// Copyright (c) 2015 Intel Corporation
// Copyright (c) 2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_X86_INCLUDE_ARCH_X86_H_
#define ZIRCON_KERNEL_ARCH_X86_INCLUDE_ARCH_X86_H_

#include <cpuid.h>
#include <lib/arch/intrin.h>
#include <stdbool.h>
#include <stdlib.h>
#include <sys/types.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <arch/x86/registers.h>
#include <kernel/cpu.h>

struct iframe_t;
struct syscall_regs_t;

// Currently, we assume at compile time that we are operating in a mode where we
// have 48 active virtual address bits.  If/when this changes, we need to come
// back here and rework the code just a bit to handle the multiple possible
// sizes instead of assuming 48 bits at compile time.
static constexpr uint8_t kX86VAddrBits = 48;

// The definition of an x86/64 "canonical" address depends on the number of
// total meaningful virtual address bits.  The canonical address range is
// divided into two regions; the first where all of the high address bits are 0,
// and the second where all of the high address bits are 1.  IOW - if the number
// of meaningful address bits is N, then the bits [N - 1, 63] must all be either
// 1 or 0.  The lower N - 1 bits may be whatever they want to be.
//
// Precompute the mask for the [N - 1, 63] range so we can easily test to see if
// an address might be a user-mode address ((addr & Mask) == 0), or might be a
// kernel address ((addr & Mask) == Mask).
static constexpr uint64_t kX86CanonicalAddressMask = ~((uint64_t{1} << (kX86VAddrBits - 1)) - 1);

#define X86_8BYTE_MASK 0xFFFFFFFF

// Called by assembly.
extern "C" void x86_exception_handler(struct iframe_t* frame);
extern "C" void x86_nmi_handler(struct iframe_t* frame);
void platform_irq(struct iframe_t* frame);

struct arch_exception_context {
  struct iframe_t* frame;
  uint64_t cr2;
  // The |user_synth_code| and |user_synth_data| fields have different values depending on the
  // exception type.
  //
  // 1) For ZX_EXCP_POLICY_ERROR, |user_synth_code| contains the type of the policy error (a
  // ZX_EXCP_POLICY_CODE_* value), and |user_synth_data| contains additional information relevant to
  // the policy error (e.g. the syscall number for ZX_EXCP_POLICY_CODE_BAD_SYSCALL).
  //
  // 2) For ZX_EXCP_FATAL_PAGE_FAULT, |user_synth_code| contains the |zx_status_t| error code
  // returned by the page fault handler, typecast to |uint32_t|. |user_synth_data| is 0.
  //
  // 3) For all other exception types, |user_synth_code| and |user_synth_data| are both set to 0.
  uint32_t user_synth_code;
  uint32_t user_synth_data;
  bool is_page_fault;
};

// Register state layout used by x86_64_context_switch().
struct x86_64_context_switch_frame {
  uint64_t r15, r14, r13, r12;
  uint64_t rbp;
  uint64_t rbx;
  uint64_t rip;
};

// Implemented in or called from assembly.
extern "C" {

#if __has_feature(safe_stack)
void x86_64_context_switch(vaddr_t* oldsp, vaddr_t newsp, vaddr_t* old_unsafe_sp,
                           vaddr_t new_unsafe_sp);
void x86_uspace_entry(const struct iframe_t* iframe, vaddr_t unsafe_sp) __NO_RETURN;
#else
void x86_64_context_switch(vaddr_t* oldsp, vaddr_t newsp);
void x86_uspace_entry(const struct iframe_t* iframe) __NO_RETURN;
#endif

void x86_syscall();

void x86_syscall_process_pending_signals(struct syscall_regs_t* gregs);

}  // extern C

/* @brief Register all of the CPUs in the system
 *
 * Must be called only once.
 *
 * @param apic_ids A list of all APIC IDs in the system.  The BP should be in
 *        the list.
 * @param num_cpus The number of entries in the apic_ids list.
 */
void x86_init_smp(uint32_t* apic_ids, uint32_t num_cpus);

/* @brief Bring all of the specified APs up and hand them over to the kernel
 *
 * This function must not be called before x86_init_smp.
 *
 * May be called by any running CPU.  Due to requiring use of the very limited
 * low 1MB of memory, this function is not re-entrant.  Itshould not be executed
 * more than once concurrently.
 *
 * @param apic_ids A list of all APIC IDs to launch.
 * @param count The number of entries in the apic_ids list.
 *
 * @return ZX_ERR_INVALID_ARGS if an unknown APIC ID was provided.
 * @return ZX_ERR_BAD_STATE if one of the targets is currently online
 * @return ZX_ERR_TIMED_OUT if one of the targets failed to launch
 */
zx_status_t x86_bringup_aps(uint32_t* apic_ids, uint32_t count);

#define IO_BITMAP_BITS 65536
#define IO_BITMAP_BYTES (IO_BITMAP_BITS / 8)
#define IO_BITMAP_LONGS (IO_BITMAP_BITS / sizeof(long))

/*
 * Assignment of Interrupt Stack Table entries
 */
#define NUM_ASSIGNED_IST_ENTRIES 3
#define NMI_IST_INDEX 1
#define MCE_IST_INDEX 2
#define DBF_IST_INDEX 3

/*
 * x86-64 TSS structure
 */
typedef struct {
  uint32_t rsvd0;
  uint64_t rsp0;
  uint64_t rsp1;
  uint64_t rsp2;
  uint32_t rsvd1;
  uint32_t rsvd2;
  uint64_t ist1;
  uint64_t ist2;
  uint64_t ist3;
  uint64_t ist4;
  uint64_t ist5;
  uint64_t ist6;
  uint64_t ist7;
  uint32_t rsvd3;
  uint32_t rsvd4;
  uint16_t rsvd5;
  uint16_t iomap_base;

  uint8_t tss_bitmap[IO_BITMAP_BYTES + 1];
} __PACKED tss_64_t;

typedef tss_64_t tss_t;

static inline void x86_clts() { __asm__ __volatile__("clts"); }
static inline void x86_hlt() { __asm__ __volatile__("hlt"); }
static inline void x86_sti() { __asm__ __volatile__("sti"); }
static inline void x86_cli() { __asm__ __volatile__("cli"); }
static inline void x86_ltr(uint16_t sel) { __asm__ __volatile__("ltr %%ax" ::"a"(sel)); }
static inline void x86_lidt(uintptr_t base) { __asm volatile("lidt (%0)" ::"r"(base) : "memory"); }
static inline void x86_lgdt(uintptr_t base) { __asm volatile("lgdt (%0)" ::"r"(base) : "memory"); }

static inline uint8_t inp(uint16_t _port) {
  uint8_t rv;
  __asm__ __volatile__("inb %1, %0" : "=a"(rv) : "dN"(_port));
  return (rv);
}

static inline uint16_t inpw(uint16_t _port) {
  uint16_t rv;
  __asm__ __volatile__("inw %1, %0" : "=a"(rv) : "dN"(_port));
  return (rv);
}

static inline uint32_t inpd(uint16_t _port) {
  uint32_t rv;
  __asm__ __volatile__("inl %1, %0" : "=a"(rv) : "dN"(_port));
  return (rv);
}

static inline void outp(uint16_t _port, uint8_t _data) {
  __asm__ __volatile__("outb %1, %0" : : "dN"(_port), "a"(_data));
}

static inline void outpw(uint16_t _port, uint16_t _data) {
  __asm__ __volatile__("outw %1, %0" : : "dN"(_port), "a"(_data));
}

static inline void outpd(uint16_t _port, uint32_t _data) {
  __asm__ __volatile__("outl %1, %0" : : "dN"(_port), "a"(_data));
}

static inline void cpuid(uint32_t sel, uint32_t* a, uint32_t* b, uint32_t* c, uint32_t* d) {
  __cpuid(sel, *a, *b, *c, *d);
}

/* cpuid wrapper with ecx set to a second argument */
static inline void cpuid_c(uint32_t sel, uint32_t sel_c, uint32_t* a, uint32_t* b, uint32_t* c,
                           uint32_t* d) {
  __cpuid_count(sel, sel_c, *a, *b, *c, *d);
}

static inline ulong x86_get_cr2() {
  ulong rv;

  __asm__ __volatile__("mov %%cr2, %0" : "=r"(rv));

  return rv;
}

static inline ulong x86_get_cr3() {
  ulong rv;

  __asm__ __volatile__("mov %%cr3, %0" : "=r"(rv));
  return rv;
}

static inline void x86_set_cr3(ulong in_val) {
  __asm__ __volatile__("mov %0,%%cr3 \n\t" : : "r"(in_val));
}

static inline ulong x86_get_cr0() {
  ulong rv;

  __asm__ __volatile__("mov %%cr0, %0 \n\t" : "=r"(rv));
  return rv;
}

static inline ulong x86_get_cr4() {
  ulong rv;

  __asm__ __volatile__("mov %%cr4, %0 \n\t" : "=r"(rv));
  return rv;
}

static inline void x86_set_cr0(ulong in_val) {
  __asm__ __volatile__("mov %0,%%cr0 \n\t" : : "r"(in_val));
}

static inline void x86_set_cr4(ulong in_val) {
  __asm__ __volatile__("mov %0,%%cr4 \n\t" : : "r"(in_val));
}

#define DEFINE_REGISTER_ACCESSOR(REG)                     \
  static inline void set_##REG(uint16_t value) {          \
    __asm__ volatile("mov %0, %%" #REG : : "r"(value));   \
  }                                                       \
  static inline uint16_t get_##REG() {                    \
    uint16_t value;                                       \
    __asm__ volatile("mov %%" #REG ", %0" : "=r"(value)); \
    return value;                                         \
  }

DEFINE_REGISTER_ACCESSOR(ds)
DEFINE_REGISTER_ACCESSOR(es)
DEFINE_REGISTER_ACCESSOR(fs)
DEFINE_REGISTER_ACCESSOR(gs)

#undef DEFINE_REGISTER_ACCESSOR

static inline uint64_t read_msr(uint32_t msr_id) {
  uint32_t msr_read_val_lo;
  uint32_t msr_read_val_hi;

  __asm__ __volatile__("rdmsr \n\t" : "=a"(msr_read_val_lo), "=d"(msr_read_val_hi) : "c"(msr_id));

  return ((uint64_t)msr_read_val_hi << 32) | msr_read_val_lo;
}

static inline uint32_t read_msr32(uint32_t msr_id) {
  uint32_t msr_read_val;

  __asm__ __volatile__("rdmsr \n\t" : "=a"(msr_read_val) : "c"(msr_id) : "rdx");

  return msr_read_val;
}

// Implemented in assembly.
extern "C" zx_status_t read_msr_safe(uint32_t msr_id, uint64_t* val);

// Read msr |msr_id| on CPU |cpu| and return the 64-bit value.
uint64_t read_msr_on_cpu(cpu_num_t cpu, uint32_t msr_id);

static inline void write_msr(uint32_t msr_id, uint64_t msr_write_val) {
  __asm__ __volatile__("wrmsr \n\t"
                       :
                       : "c"(msr_id), "a"(msr_write_val & 0xffffffff), "d"(msr_write_val >> 32));
}

// Write value |val| into MSR |msr_id|. Returns ZX_OK if the write was successful or
// a non-Ok status if the write failed because we received a #GP fault.
zx_status_t write_msr_safe(uint32_t msr_id, uint64_t val);

// Write value |val| into MSR |msr_id| on cpu |cpu|.
void write_msr_on_cpu(cpu_num_t cpu, uint32_t msr_id, uint64_t val);

static inline bool x86_is_paging_enabled() {
  if (x86_get_cr0() & X86_CR0_PG)
    return true;

  return false;
}

static inline bool x86_is_PAE_enabled() {
  if (x86_is_paging_enabled() == false)
    return false;

  if (!(x86_get_cr4() & X86_CR4_PAE))
    return false;

  return true;
}

static inline uint64_t x86_read_gs_offset64(uintptr_t offset) {
  uint64_t ret;
  __asm__("movq  %%gs:%1, %0" : "=r"(ret) : "m"(*(uint64_t*)(offset)));
  return ret;
}

static inline void x86_write_gs_offset64(uintptr_t offset, uint64_t val) {
  __asm__("movq  %0, %%gs:%1" : : "ir"(val), "m"(*(uint64_t*)(offset)) : "memory");
}

static inline uint32_t x86_read_gs_offset32(uintptr_t offset) {
  uint32_t ret;
  __asm__("movl  %%gs:%1, %0" : "=r"(ret) : "m"(*(uint32_t*)(offset)));
  return ret;
}

static inline void x86_write_gs_offset32(uintptr_t offset, uint32_t val) {
  __asm__("movl   %0, %%gs:%1" : : "ir"(val), "m"(*(uint32_t*)(offset)) : "memory");
}

typedef uint64_t x86_flags_t;

static inline uint64_t x86_save_flags() {
  // This is marked uninitialized since although it is declared as an output only (i.e. not an
  // input+output) operand the compiler may still decide to pattern fill it. Specifically if this
  // method got inlined such that the flags are going to get saved directly to memory that target
  // memory will get pattern filled, then immediately overwritten by the popq.
  // Alternatively if the output operand were marked as reg only, instead of reg+mem the compiler
  // would realize it never needs to pattern fill, however it produces slightly worse code gen as,
  // in scenarios where the final flags do want to get stored to memory, it forces an intermediate
  // register to be used.
  uint64_t __UNINITIALIZED state;

  __asm__ volatile(
      "pushfq;"
      "popq %0"
      : "=rm"(state)::"memory");

  return state;
}

static inline void x86_restore_flags(uint64_t flags) {
  __asm__ volatile(
      "pushq %0;"
      "popfq" ::"g"(flags)
      : "memory", "cc");
}

static inline void inprep(uint16_t _port, uint8_t* _buffer, uint32_t _reads) {
  __asm__ __volatile__(
      "pushfq \n\t"
      "cli \n\t"
      "cld \n\t"
      "rep insb \n\t"
      "popfq \n\t"
      :
      : "d"(_port), "D"(_buffer), "c"(_reads));
}

static inline void outprep(uint16_t _port, uint8_t* _buffer, uint32_t _writes) {
  __asm__ __volatile__(
      "pushfq \n\t"
      "cli \n\t"
      "cld \n\t"
      "rep outsb \n\t"
      "popfq \n\t"
      :
      : "d"(_port), "S"(_buffer), "c"(_writes));
}

static inline void inpwrep(uint16_t _port, uint16_t* _buffer, uint32_t _reads) {
  __asm__ __volatile__(
      "pushfq \n\t"
      "cli \n\t"
      "cld \n\t"
      "rep insw \n\t"
      "popfq \n\t"
      :
      : "d"(_port), "D"(_buffer), "c"(_reads));
}

static inline void outpwrep(uint16_t _port, uint16_t* _buffer, uint32_t _writes) {
  __asm__ __volatile__(
      "pushfq \n\t"
      "cli \n\t"
      "cld \n\t"
      "rep outsw \n\t"
      "popfq \n\t"
      :
      : "d"(_port), "S"(_buffer), "c"(_writes));
}

static inline void inpdrep(uint16_t _port, uint32_t* _buffer, uint32_t _reads) {
  __asm__ __volatile__(
      "pushfq \n\t"
      "cli \n\t"
      "cld \n\t"
      "rep insl \n\t"
      "popfq \n\t"
      :
      : "d"(_port), "D"(_buffer), "c"(_reads));
}

static inline void outpdrep(uint16_t _port, uint32_t* _buffer, uint32_t _writes) {
  __asm__ __volatile__(
      "pushfq \n\t"
      "cli \n\t"
      "cld \n\t"
      "rep outsl \n\t"
      "popfq \n\t"
      :
      : "d"(_port), "S"(_buffer), "c"(_writes));
}

// Implemented in assembly.
extern "C" {

void x86_monitor(volatile void* addr);
// |hints| is used to specify which C-state the processor should enter when
// MWAIT is called.
// See Table 4-11 in the "Intel 64 and IA-32 Architectures Software Developer's
// Manual, Vol 2B" for encoding.
void x86_mwait(uint32_t hints);

// Enable interrupts and halt this processor. Requires interrupts are disabled on entry.
void x86_enable_ints_and_hlt();

void mds_buff_overwrite();
void x86_ras_fill();

}  // extern C

#endif  // ZIRCON_KERNEL_ARCH_X86_INCLUDE_ARCH_X86_H_
