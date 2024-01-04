// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/nop.h>
#include <lib/arch/x86/boot-cpuid.h>
#include <lib/arch/x86/bug.h>
#include <lib/arch/x86/descriptor.h>
#include <lib/boot-options/boot-options.h>
#include <lib/console.h>
#include <lib/unittest/unittest.h>
#include <zircon/syscalls/system.h>

#include <arch/arch_ops.h>
#include <arch/mp.h>
#include <arch/x86.h>
#include <arch/x86/apic.h>
#include <arch/x86/cpuid.h>
#include <arch/x86/cpuid_test_data.h>
#include <arch/x86/fake_msr_access.h>
#include <arch/x86/feature.h>
#include <arch/x86/hwp.h>
#include <arch/x86/interrupts.h>
#include <arch/x86/platform_access.h>
#include <hwreg/x86msr.h>
#include <kernel/cpu.h>
#include <kernel/deadline.h>
#include <ktl/array.h>
#include <ktl/unique_ptr.h>

#include "../../lib/syscalls/system_priv.h"
#include "amd.h"

#include <ktl/enforce.h>

extern char __x86_indirect_thunk_r11;
extern char interrupt_non_nmi_maybe_mds_buff_overwrite;
extern char interrupt_nmi_maybe_mds_buff_overwrite;
extern char syscall_maybe_mds_buff_overwrite;

namespace {

static void rdtscp_aux(void* context) {
  uint32_t* const aux = reinterpret_cast<uint32_t*>(context);
  uint32_t tsc_lo, tsc_hi, aux_msr;
  asm volatile("rdtscp" : "=a"(tsc_lo), "=d"(tsc_hi), "=c"(aux_msr));
  *aux = aux_msr;
}

static bool test_x64_msrs() {
  BEGIN_TEST;

  interrupt_saved_state_t int_state = arch_interrupt_save();
  // Test read_msr for an MSR that is known to always exist on x64.
  uint64_t val = read_msr(X86_MSR_IA32_LSTAR);
  EXPECT_NE(val, 0ull);

  // Test write_msr to write that value back.
  write_msr(X86_MSR_IA32_LSTAR, val);
  arch_interrupt_restore(int_state);

  // Test read_msr_safe for an MSR that is known to not exist.
  // If read_msr_safe is busted, then this will #GP (panic).
  // TODO: Enable when the QEMU TCG issue is sorted (TCG never
  // generates a #GP on MSR access).
#ifdef DISABLED
  uint64_t bad_val;
  // AMD MSRC001_2xxx are only readable via Processor Debug.
  auto bad_status = read_msr_safe(0xC0012000, &bad_val);
  EXPECT_NE(bad_status, ZX_OK);
#endif

  // Test read_msr_on_cpu.
  uint64_t initial_fmask = read_msr(X86_MSR_IA32_FMASK);
  for (cpu_num_t i = 0; i < arch_max_num_cpus(); i++) {
    if (!mp_is_cpu_online(i)) {
      continue;
    }
    uint64_t fmask = read_msr_on_cpu(/*cpu=*/i, X86_MSR_IA32_FMASK);
    EXPECT_EQ(initial_fmask, fmask);
  }

  // Test write_msr_on_cpu
  for (cpu_num_t i = 0; i < arch_max_num_cpus(); i++) {
    if (!mp_is_cpu_online(i)) {
      continue;
    }
    write_msr_on_cpu(/*cpu=*/i, X86_MSR_IA32_FMASK, /*val=*/initial_fmask);
  }

  // If RDTSCP is supported, check that the TSC_AUX MSR is correctly programmed.
  if (x86_feature_test(X86_FEATURE_RDTSCP)) {
    for (cpu_num_t i = 0; i < arch_max_num_cpus(); i++) {
      if (!mp_is_cpu_online(i)) {
        continue;
      }
      uint64_t cpuid = read_msr_on_cpu(/*cpu=*/i, X86_MSR_IA32_TSC_AUX);
      EXPECT_EQ(cpuid, i);

      uint32_t aux;
      cpu_mask_t mask = {};
      mask |= cpu_num_to_mask(i);
      mp_sync_exec(MP_IPI_TARGET_MASK, mask, rdtscp_aux, reinterpret_cast<void*>(&aux));
      EXPECT_EQ(cpuid, aux);
    }
  }

  END_TEST;
}

static bool test_x64_msrs_k_commands() {
  BEGIN_TEST;

  console_run_script_locked("cpu rdmsr 0 0x10");

  END_TEST;
}

static bool test_x64_hwp_k_commands() {
  BEGIN_TEST;

  // Don't test at all if HWP disabled on the command line.
  if (!gBootOptions->x86_hwp) {
    return true;
  }

  // If we don't support HWP, expect every command to just return "not supported".
  cpu_id::CpuId cpuid;
  if (!x86::IntelHwpSupported(&cpuid)) {
    EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, console_run_script_locked("hwp"));
    return all_ok;
  }

  // Test top-level parsing.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp invalid"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp 3"));

  // Set policy.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-policy"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-policy invalid-policy"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-policy 3"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-policy performance 42"));
  EXPECT_EQ(ZX_OK, console_run_script_locked("hwp set-policy performance"));
  EXPECT_EQ(ZX_OK, console_run_script_locked("hwp set-policy power-save"));

  // Set Freq
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-freq"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-freq 0"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-freq 256"));
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, console_run_script_locked("hwp set-freq 10 10"));
  EXPECT_EQ(ZX_OK, console_run_script_locked("hwp set-freq 100"));
  EXPECT_EQ(ZX_OK, console_run_script_locked("hwp set-freq 255"));

  // Restore the policy to default.
  MsrAccess msr;
  x86::IntelHwpInit(&cpuid, &msr, gBootOptions->x86_hwp_policy);

  END_TEST;
}

static bool test_x64_cpu_uarch_config_selection() {
  BEGIN_TEST;

  EXPECT_EQ(get_microarch_config(&cpu_id::kCpuIdCorei5_6260U)->x86_microarch,
            X86_MICROARCH_INTEL_SKYLAKE);

  // Intel Xeon E5-2690 V4 is Broadwell
  EXPECT_EQ(get_microarch_config(&cpu_id::kCpuIdXeon2690v4)->x86_microarch,
            X86_MICROARCH_INTEL_BROADWELL);

  // Intel Celeron J3455 is Goldmont
  EXPECT_EQ(get_microarch_config(&cpu_id::kCpuIdCeleronJ3455)->x86_microarch,
            X86_MICROARCH_INTEL_GOLDMONT);

  // AMD A4-9120C is Bulldozer
  EXPECT_EQ(get_microarch_config(&cpu_id::kCpuIdAmdA49120C)->x86_microarch,
            X86_MICROARCH_AMD_BULLDOZER);

  // AMD Ryzen Threadripper 2970WX is Zen
  EXPECT_EQ(get_microarch_config(&cpu_id::kCpuIdThreadRipper2970wx)->x86_microarch,
            X86_MICROARCH_AMD_ZEN);

  END_TEST;
}

static uint32_t intel_make_microcode_checksum(uint32_t* patch, size_t bytes) {
  size_t dwords = bytes / sizeof(uint32_t);
  uint32_t sum = 0;
  for (size_t i = 0; i < dwords; i++) {
    sum += patch[i];
  }
  return -sum;
}

static bool test_x64_intel_ucode_loader() {
  BEGIN_TEST;

  // x86_intel_check_microcode_patch checks if a microcode patch is suitable for a particular
  // CPU. Test that its match logic works for various CPUs and conditions we commonly use.

  {
    uint32_t fake_patch[512] = {};
    // Intel(R) Celeron(R) CPU J3455 (Goldmont), NUC6CAYH
    cpu_id::TestDataSet data = {};
    data.leaf0 = {.reg = {0x15, 0x756e6547, 0x6c65746e, 0x49656e69}};
    data.leaf1 = {.reg = {0x506c9, 0x2200800, 0x4ff8ebbf, 0xbfebfbff}};
    data.leaf4 = {.reg = {0x3c000121, 0x140003f, 0x3f, 0x1}};
    data.leaf7 = {.reg = {0x0, 0x2294e283, 0x0, 0x2c000000}};
    cpu_id::FakeCpuId cpu(data);
    FakeMsrAccess fake_msrs = {};
    fake_msrs.msrs_[0] = {X86_MSR_IA32_PLATFORM_ID, 0x1ull << 50};  // Apollo Lake

    // Reject an all-zero patch.
    EXPECT_FALSE(
        x86_intel_check_microcode_patch(&cpu, &fake_msrs, {fake_patch, sizeof(fake_patch)}));

    // Reject patch with non-matching processor signature.
    fake_patch[0] = 0x1;
    fake_patch[4] = intel_make_microcode_checksum(fake_patch, sizeof(fake_patch));
    EXPECT_FALSE(
        x86_intel_check_microcode_patch(&cpu, &fake_msrs, {fake_patch, sizeof(fake_patch)}));

    // Expect matching patch to pass
    fake_patch[0] = 0x1;
    fake_patch[3] = data.leaf1.reg[0];  // Signature match
    fake_patch[6] = 0x3;                // Processor flags match PLATFORM_ID
    fake_patch[4] = 0;
    fake_patch[4] = intel_make_microcode_checksum(fake_patch, sizeof(fake_patch));
    EXPECT_TRUE(
        x86_intel_check_microcode_patch(&cpu, &fake_msrs, {fake_patch, sizeof(fake_patch)}));
    // Real header from 2019-01-15, rev 38
    fake_patch[0] = 0x1;
    fake_patch[1] = 0x38;
    fake_patch[2] = 0x01152019;
    fake_patch[3] = 0x506c9;
    fake_patch[6] = 0x3;  // Processor flags match PLATFORM_ID
    fake_patch[4] = 0;
    fake_patch[4] = intel_make_microcode_checksum(fake_patch, sizeof(fake_patch));
    EXPECT_TRUE(
        x86_intel_check_microcode_patch(&cpu, &fake_msrs, {fake_patch, sizeof(fake_patch)}));
  }

  END_TEST;
}

class FakeWriteMsr : public MsrAccess {
 public:
  void write_msr(uint32_t msr_index, uint64_t value) override {
    DEBUG_ASSERT(written_ == false);
    written_ = true;
    msr_index_ = msr_index;
  }

  bool written_ = false;
  uint32_t msr_index_;
};

static bool test_x64_intel_ucode_patch_loader() {
  BEGIN_TEST;

  cpu_id::TestDataSet data = {};
  cpu_id::FakeCpuId cpu(data);
  FakeWriteMsr msrs;
  uint32_t fake_patch[512] = {};

  // This test can only run on physical Intel x86-64 hosts; x86_intel_get_patch_level
  // does not use an interface to access patch_level registers and those registers are
  // only present/writable on h/w.
  if (x86_vendor == X86_VENDOR_INTEL && !x86_feature_test(X86_FEATURE_HYPERVISOR)) {
    // Expect that a patch == current patch is not loaded.
    uint32_t current_patch_level = x86_intel_get_patch_level();
    fake_patch[1] = current_patch_level;
    x86_intel_load_microcode_patch(&cpu, &msrs, {fake_patch, sizeof(fake_patch)});
    EXPECT_FALSE(msrs.written_);

    // Expect that a newer patch is loaded.
    fake_patch[1] = current_patch_level + 1;
    x86_intel_load_microcode_patch(&cpu, &msrs, {fake_patch, sizeof(fake_patch)});
    EXPECT_TRUE(msrs.written_);
    EXPECT_EQ(msrs.msr_index_, X86_MSR_IA32_BIOS_UPDT_TRIG);
  }

  END_TEST;
}

static bool test_x64_power_limits() {
  BEGIN_TEST;
  FakeMsrAccess fake_msrs = {};
  // defaults on Ava/Eve. They both use the same Intel chipset
  // only diff is the WiFi. Ava uses Broadcom vs Eve uses Intel
  fake_msrs.msrs_[0] = {X86_MSR_PKG_POWER_LIMIT, 0x1807800dd8038};
  fake_msrs.msrs_[1] = {X86_MSR_RAPL_POWER_UNIT, 0xA0E03};
  // This default value does not look right, but this is a RO MSR
  fake_msrs.msrs_[2] = {X86_MSR_PKG_POWER_INFO, 0x24};
  // Read the defaults from pkg power msr.
  uint64_t default_val = fake_msrs.read_msr(X86_MSR_PKG_POWER_LIMIT);
  uint32_t new_power_limit = 4500;
  uint32_t new_time_window = 24000000;
  // expected value in MSR with the new power limit and time window
  uint64_t new_msr = 0x18078009d8024;
  zx_system_powerctl_arg_t arg;
  arg.x86_power_limit.clamp = static_cast<uint8_t>(default_val >> 16 & 0x01);
  arg.x86_power_limit.enable = static_cast<uint8_t>(default_val >> 15 & 0x01);
  // changing the value to 4.5W from 7W = 0x24 in the MSR
  // X86_MSR_PKG_POWER_LIMIT & 0x7FFF = 0x24 * power_units should give 4.5W
  arg.x86_power_limit.power_limit = new_power_limit;
  // changing the value to 24s from 28s = 0x4E in the MSR
  arg.x86_power_limit.time_window = new_time_window;
  // write it back again to see if the new function does it right
  auto status = arch_system_powerctl(ZX_SYSTEM_POWERCTL_X86_SET_PKG_PL1, &arg, &fake_msrs);
  if (status != ZX_ERR_NOT_SUPPORTED) {
    uint64_t new_val = fake_msrs.read_msr(X86_MSR_PKG_POWER_LIMIT);
    EXPECT_EQ(new_val, new_msr, "Set power limit failed");
  }
  END_TEST;
}

static bool test_amd_platform_init() {
  BEGIN_TEST;
  FakeMsrAccess fake_msrs = {};

  // Test that set_lfence_serializing sets the LFENCE bit when its not already set.
  fake_msrs.msrs_[0] = {X86_MSR_AMD_F10_DE_CFG, 0};
  x86_amd_set_lfence_serializing(&cpu_id::kCpuIdThreadRipper2970wx, &fake_msrs);
  EXPECT_EQ(fake_msrs.msrs_[0].value, 0x2ull, "");

  // Test that set_lfence_serializing doesn't change the LFENCE bit when its set.
  fake_msrs.msrs_[0] = {X86_MSR_AMD_F10_DE_CFG, 0x2ull};
  fake_msrs.no_writes_ = true;
  x86_amd_set_lfence_serializing(&cpu_id::kCpuIdThreadRipper2970wx, &fake_msrs);
  EXPECT_EQ(fake_msrs.msrs_[0].value, 0x2ull, "");

  END_TEST;
}

static bool test_spectre_v2_mitigations() {
  BEGIN_TEST;
  bool sp_match;

  // Execute x86_ras_fill and make sure %rsp is unchanged.
  __asm__ __volatile__(
      "mov %%rsp, %%r11\n"
      "call x86_ras_fill\n"
      "cmp %%rsp, %%r11\n"
      "setz %0"
      : "=r"(sp_match)::"memory", "%r11");
  EXPECT_EQ(sp_match, true);

  // Test that retpoline thunks are correctly patched.
  unsigned char check_buffer[16] = {};
  memcpy(check_buffer, &__x86_indirect_thunk_r11, sizeof(check_buffer));

  if (gBootOptions->x86_disable_spec_mitigations) {
    // If speculative execution mitigations are disabled or Enhanced IBRS is enabled, we expect the
    // retpoline thunk to be:
    // __x86_indirect_thunk:
    //   41 ff e3        jmp *%r11
    EXPECT_EQ(check_buffer[0], 0x41);
    EXPECT_EQ(check_buffer[1], 0xff);
    EXPECT_EQ(check_buffer[2], 0xe3);
  } else {
    // We expect the generic thunk to be:
    // __x86_indirect_thunk:
    //  e8 ?? ?? ?? ?? call ...
    //
    // We cannot test the exact contents of the thunk as the call target depends on the internal
    // alignment. Instead check that the first byte is the call instruction we expect.
    EXPECT_EQ(check_buffer[0], 0xe8);
  }

  END_TEST;
}

static bool test_mds_taa_mitigation() {
  BEGIN_TEST;
  for (char* src : {&interrupt_non_nmi_maybe_mds_buff_overwrite,
                    &interrupt_nmi_maybe_mds_buff_overwrite, &syscall_maybe_mds_buff_overwrite}) {
    unsigned char check_buffer[5];
    memcpy(check_buffer, src, sizeof(check_buffer));
    if (x86_cpu_should_md_clear_on_user_return()) {
      EXPECT_EQ(check_buffer[0], 0xe8);  // Expect a call to mds_buff_overwrite
    } else {
      // If speculative execution mitigations are disabled or we're not affected by MDS or don't
      // have MD_CLEAR, expect NOPs.
      for (size_t i = 0; i < sizeof(check_buffer); i++) {
        EXPECT_EQ(check_buffer[i], arch::X86NopTraits::kNop5[i]);
      }
    }
  }

  END_TEST;
}

static bool test_gdt_mapping() {
  BEGIN_TEST;

  arch::GdtRegister64 gdtr;
  __asm__ volatile("sgdt %0" : "=m"(gdtr));
  ASSERT_EQ(gdtr.limit, 0xffff);

  uint64_t* gdt = reinterpret_cast<uint64_t*>(gdtr.base);

  // Force a read of each page of the GDT.
  for (size_t i = 0; i < (gdtr.limit + 1) / sizeof(uint64_t); i += PAGE_SIZE / sizeof(uint64_t)) {
    uint64_t output;
    __asm__ volatile("mov %[entry], %0" : "=r"(output) : [entry] "m"(gdt[i]));
  }

  END_TEST;
}

// TODO(https://fxbug.dev/88368): Unconditionally re-enable this test when https://fxbug.dev/88368 has been
// resolved.
//
// Spam all the CPUs with NMIs, see that nothing bad happens.
bool test_nmi_spam() {
  BEGIN_TEST;

  if (x86_hypervisor == X86_HYPERVISOR_KVM) {
    printf("skipping test, see https://fxbug.dev/88368\n");
    END_TEST;
  }

  constexpr zx_duration_t kDuration = ZX_MSEC(100);
  const Deadline deadline = Deadline::after(kDuration);
  do {
    apic_send_mask_ipi(X86_INT_NMI, mp_get_active_mask(), DELIVERY_MODE_NMI);
  } while (current_time() < deadline.when());

  END_TEST;
}

}  // anonymous namespace

UNITTEST_START_TESTCASE(x64_platform_tests)
UNITTEST("basic test of read/write MSR variants", test_x64_msrs)
UNITTEST("test k cpu rdmsr commands", test_x64_msrs_k_commands)
UNITTEST("test k hwp commands", test_x64_hwp_k_commands)
UNITTEST("test uarch_config is correctly selected", test_x64_cpu_uarch_config_selection)
UNITTEST("test Intel x86 microcode patch loader match and load logic", test_x64_intel_ucode_loader)
UNITTEST("test Intel x86 microcode patch loader mechanism", test_x64_intel_ucode_patch_loader)
UNITTEST("test pkg power limit change", test_x64_power_limits)
UNITTEST("test amd_platform_init", test_amd_platform_init)
UNITTEST("test spectre v2 mitigation building blocks", test_spectre_v2_mitigations)
UNITTEST("test mds mitigation building blocks", test_mds_taa_mitigation)
UNITTEST("test gdt is mapped", test_gdt_mapping)
UNITTEST("test nmi spam", test_nmi_spam)
UNITTEST_END_TESTCASE(x64_platform_tests, "x64_platform_tests", "")
