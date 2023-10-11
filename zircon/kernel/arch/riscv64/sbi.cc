// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "arch/riscv64/sbi.h"

#include <debug.h>
#include <lib/arch/riscv64/sbi-call.h>
#include <lib/arch/riscv64/sbi.h>
#include <trace.h>

#include <arch/riscv64/mp.h>
#include <pdev/power.h>

#define LOCAL_TRACE 0

// Basic SBI wrapper routines and extension detection.
//
// NOTE: Assumes a few extensions are present.
// Timer, Ipi, Rfence, Hart management, and System Reset are all
// assumed to be present. This may need to be revisited in the future
// if it turns out some of these are not always there.
//
// SBI API documentation lives at
// https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/riscv-sbi.adoc

namespace {
enum class sbi_extension : uint8_t {
  Base,
  Timer,
  Ipi,
  Rfence,
  Hart,
  SystemReset,
  Pmu,
  Dbcn,
  Susp,
  Cppc,
};

// A bitmap of all the detected SBI extensions.
uint32_t supported_extension_bitmap;

// For every extension we track, call a callable with all the information we
// may want.
template <typename Callable>
void for_every_extension(Callable callable) {
  callable(sbi_extension::Base, "BASE", arch::RiscvSbiEid::kBase);
  callable(sbi_extension::Timer, "TIMER", arch::RiscvSbiEid::kTimer);
  callable(sbi_extension::Ipi, "IPI", arch::RiscvSbiEid::kIpi);
  callable(sbi_extension::Rfence, "RFENCE", arch::RiscvSbiEid::kRfence);
  callable(sbi_extension::Hart, "HSM", arch::RiscvSbiEid::kHart);
  callable(sbi_extension::SystemReset, "SRST", arch::RiscvSbiEid::kSystemReset);
  callable(sbi_extension::Pmu, "PMU", arch::RiscvSbiEid::kPmu);
  callable(sbi_extension::Dbcn, "DBCN", arch::RiscvSbiEid::kDbcn);
  callable(sbi_extension::Susp, "SUSP", arch::RiscvSbiEid::kSusp);
  callable(sbi_extension::Cppc, "CPPC", arch::RiscvSbiEid::kCppc);
}

bool sbi_extension_present(sbi_extension ext) {
  return (1U << static_cast<uint8_t>(ext)) & supported_extension_bitmap;
}

}  // anonymous namespace

zx_status_t riscv_status_to_zx_status(arch::RiscvSbiError error) {
  switch (error) {
    case arch::RiscvSbiError::kSuccess:
      return ZX_OK;
    case arch::RiscvSbiError::kFailed:
      return ZX_ERR_INTERNAL;
    case arch::RiscvSbiError::kNotSupported:
      return ZX_ERR_NOT_SUPPORTED;
    case arch::RiscvSbiError::kInvalidParam:
    case arch::RiscvSbiError::kInvalidAddress:
      return ZX_ERR_INVALID_ARGS;
    case arch::RiscvSbiError::kDenied:
      return ZX_ERR_ACCESS_DENIED;
    case arch::RiscvSbiError::kNoShmem:
      return ZX_ERR_NO_RESOURCES;
    case arch::RiscvSbiError::kAlreadyAvailable:
    case arch::RiscvSbiError::kAlreadyStarted:
    case arch::RiscvSbiError::kAlreadyStopped:
    default:
      return ZX_ERR_BAD_STATE;
  }
}

zx::result<power_cpu_state> sbi_get_cpu_state(uint64_t hart_id) {
  arch::RiscvSbiRet ret = arch::RiscvSbi::HartGetStatus(static_cast<arch::HartId>(hart_id));
  if (ret.error != arch::RiscvSbiError::kSuccess) {
    return zx::error(riscv_status_to_zx_status(ret.error));
  }
  switch (static_cast<arch::RiscvSbiHartState>(ret.value)) {
    case arch::RiscvSbiHartState::kStarted:
      return zx::success(power_cpu_state::STARTED);
    case arch::RiscvSbiHartState::kStopped:
      return zx::success(power_cpu_state::STOPPED);
    case arch::RiscvSbiHartState::kStartPending:
      return zx::success(power_cpu_state::START_PENDING);
    case arch::RiscvSbiHartState::kStopPending:
      return zx::success(power_cpu_state::STOP_PENDING);
    case arch::RiscvSbiHartState::kSuspended:
      return zx::success(power_cpu_state::SUSPENDED);
    case arch::RiscvSbiHartState::kSuspendPending:
      return zx::success(power_cpu_state::SUSPEND_PENDING);
    case arch::RiscvSbiHartState::kResumePending:
      return zx::success(power_cpu_state::RESUME_PENDING);
    default:
      // We should never reach here.
      return zx::error(ZX_ERR_INTERNAL);
  }
}

void riscv64_sbi_early_init() {
  // Probe to see what extensions are present
  auto probe_and_set_extension = [](sbi_extension extension_bit, const char *,
                                    arch::RiscvSbiEid eid) {
    // Base extension is always present
    if (extension_bit == sbi_extension::Base) {
      supported_extension_bitmap = 1U << static_cast<uint8_t>(sbi_extension::Base);
      return;
    }

    // Probe the extension
    arch::RiscvSbiRet ret = arch::RiscvSbi::ProbeExtension(eid);

    // It shouldn't be legal for the base probe extension call to return anything but success,
    // but check here anyway.
    if (ret.error != arch::RiscvSbiError::kSuccess) {
      return;
    }

    supported_extension_bitmap |=
        (ret.value != 0) ? (1U << static_cast<uint8_t>(extension_bit)) : 0;
  };

  for_every_extension(probe_and_set_extension);

  // Register with the pdev power driver.
  static const pdev_power_ops sbi_ops = {
      .reboot = [](power_reboot_flags flags) -> zx_status_t { return sbi_reset(); },
      .shutdown = sbi_shutdown,
      .cpu_off = sbi_hart_stop,
      .cpu_on = sbi_hart_start,
      .get_cpu_state = sbi_get_cpu_state,
  };

  pdev_register_power(&sbi_ops);
}

void riscv64_sbi_init() {
  // Dump SBI version info and extensions found in early probing
  if (DPRINTF_ENABLED_FOR_LEVEL(INFO)) {
    dprintf(INFO, "RISCV: mvendorid %#lx marchid %#lx mimpid %#lx\n",
            arch::RiscvSbi::GetMvendorid().value, arch::RiscvSbi::GetMarchid().value,
            arch::RiscvSbi::GetMimpid().value);

    uint64_t spec_version = arch::RiscvSbi::GetSpecVersion().value;
    dprintf(INFO, "RISCV: SBI spec version %lu.%lu impl id %#lx version %#lx\n",
            (spec_version >> 24) & 0x7f, spec_version & ((1 << 24) - 1),
            arch::RiscvSbi::GetImplId().value, arch::RiscvSbi::GetImplVersion().value);

    dprintf(INFO, "RISCV: extensions: ");
    auto print_extension = [](sbi_extension extension, const char *name, arch::RiscvSbiEid) {
      if (sbi_extension_present(extension)) {
        dprintf(INFO, "%s ", name);
      }
    };
    for_every_extension(print_extension);
    dprintf(INFO, "\n");
  }
}

arch::RiscvSbiRet sbi_send_ipi(arch::HartMask mask, arch::HartMaskBase mask_base) {
  LTRACEF("hart_mask %#lx, mask_base %lu\n", mask, mask_base);

  return arch::RiscvSbi::SendIpi(mask, mask_base);
}

zx_status_t sbi_hart_start(uint64_t id, paddr_t start_addr, uint64_t priv) {
  arch::HartId hart_id = static_cast<arch::HartId>(id);
  LTRACEF("hart_id %lu, start_addr %#lx, priv %#lx\n", hart_id, start_addr, priv);
  arch::RiscvSbiRet ret = arch::RiscvSbi::HartStart(hart_id, start_addr, priv);
  return riscv_status_to_zx_status(ret.error);
}

zx_status_t sbi_hart_stop() {
  LTRACEF("local hart %u\n", riscv64_curr_hart_id());
  arch::RiscvSbiRet ret = arch::RiscvSbi::HartStop();
  return riscv_status_to_zx_status(ret.error);
}

arch::RiscvSbiRet sbi_remote_sfence_vma(cpu_mask_t cpu_mask, uintptr_t start, uintptr_t size) {
  arch::HartMask hart_mask = riscv64_cpu_mask_to_hart_mask(cpu_mask);
  arch::HartMaskBase hart_mask_base = 0;

  LTRACEF("start %#lx, size %#lx, cpu_mask %#x, hart_mask %#lx\n", start, size, cpu_mask,
          hart_mask);

  return arch::RiscvSbi::RemoteSfenceVma(hart_mask, hart_mask_base, start, size);
}

arch::RiscvSbiRet sbi_remote_sfence_vma_asid(cpu_mask_t cpu_mask, uintptr_t start, uintptr_t size,
                                             uint64_t asid) {
  arch::HartMask hart_mask = riscv64_cpu_mask_to_hart_mask(cpu_mask);
  arch::HartMaskBase hart_mask_base = 0;

  LTRACEF("start %#lx, size %#lx, asid %lu, cpu_mask %#x, hart_mask %#lx\n", start, size, asid,
          cpu_mask, hart_mask);

  return arch::RiscvSbi::RemoteSfenceVmaAsid(hart_mask, hart_mask_base, start, size, asid);
}

zx_status_t sbi_shutdown() {
  arch::RiscvSbiRet ret = arch::RiscvSbi::SystemReset(arch::RiscvSbiResetType::kShutdown,
                                                      arch::RiscvSbiResetReason::kNone);
  return riscv_status_to_zx_status(ret.error);
}

zx_status_t sbi_reset() {
  arch::RiscvSbiRet ret = arch::RiscvSbi::SystemReset(arch::RiscvSbiResetType::kWarmReboot,
                                                      arch::RiscvSbiResetReason::kNone);
  return riscv_status_to_zx_status(ret.error);
}
