// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <stddef.h>

#include <hwreg/asm.h>

#include "linuxboot.h"

int main(int argc, char** argv) {
  return hwreg::AsmHeader()
      .Macro("SETUP_HEADER_SETUP_SECTS", offsetof(linuxboot::setup_header, setup_sects))
      .Macro("SETUP_HEADER_SYSSIZE", offsetof(linuxboot::setup_header, syssize))
      .Macro("SETUP_HEADER_BOOT_FLAG", offsetof(linuxboot::setup_header, boot_flag))
      .Macro("LINUXBOOT_BOOT_FLAG", linuxboot::setup_header::kBootFlag)
      .Macro("SETUP_HEADER_JUMP", offsetof(linuxboot::setup_header, jump))
      .Macro("SETUP_HEADER_HEADER", offsetof(linuxboot::setup_header, header))
      .Macro("SETUP_HEADER_VERSION", offsetof(linuxboot::setup_header, version))
      .Macro("SETUP_HEADER_LOADFLAGS", offsetof(linuxboot::setup_header, loadflags))
      .Macro("LOADFLAGS_LOADED_HIGH", linuxboot::setup_header::LoadFlags::kLoadedHigh)
      .Macro("SETUP_HEADER_INITRD_ADDR_MAX", offsetof(linuxboot::setup_header, initrd_addr_max))
      .Macro("SETUP_HEADER_KERNEL_ALIGNMENT", offsetof(linuxboot::setup_header, kernel_alignment))
      .Macro("SETUP_HEADER_RELOCATABLE_KERNEL",
             offsetof(linuxboot::setup_header, relocatable_kernel))
      .Macro("SETUP_HEADER_MIN_ALIGNMENT", offsetof(linuxboot::setup_header, min_alignment))
      .Macro("SETUP_HEADER_XLOADFLAGS", offsetof(linuxboot::setup_header, xloadflags))
      .Macro("XLF_KERNEL_64", linuxboot::setup_header::kKernel64)
      .Macro("XLF_CAN_BE_LOADED_ABOVE_4G", linuxboot::setup_header::kCanBeLoadedAbove4g)
      .Macro("SETUP_HEADER_CMDLINE_SIZE", offsetof(linuxboot::setup_header, cmdline_size))
      .Macro("SETUP_HEADER_INIT_SIZE", offsetof(linuxboot::setup_header, init_size))
      .Macro("SIZEOF_SETUP_HEADER", sizeof(linuxboot::setup_header))
      .Macro("BOOT_PARAMS_HDR", offsetof(linuxboot::boot_params, hdr))
      .Macro("BOOT_PARAMS_E820_ENTRIES", offsetof(linuxboot::boot_params, e820_entries))
      .Macro("BOOT_PARAMS_E820_TABLE", offsetof(linuxboot::boot_params, e820_table))
      .Macro("SIZEOF_BOOT_PARAMS", sizeof(linuxboot::boot_params))
      .Macro("SIZEOF_E820ENTRY", sizeof(E820Entry))
      .Macro("E820_MAGIC", linuxboot::kE820Magic)
      .Macro("MAX_E820_TABLE_ENTRIES", linuxboot::kMaxE820TableEntries)
      .Macro("ENTRY64_OFFSET", linuxboot::kEntry64Offset)

      .Main(argc, argv);
}
