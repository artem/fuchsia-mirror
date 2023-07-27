// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2013 Google, Inc.
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_VERSION_INCLUDE_LIB_VERSION_H_
#define ZIRCON_KERNEL_LIB_VERSION_INCLUDE_LIB_VERSION_H_

#include <stdio.h>

#include <ktl/byte.h>
#include <ktl/span.h>
#include <ktl/string_view.h>

// This is the string returned by zx_system_get_version_string.
ktl::string_view VersionString();

// This is a string of lowercase hexadecimal digits.
const char* elf_build_id_string();

// This is the raw build ID bytes.
ktl::span<const ktl::byte> ElfBuildId();

void print_version(void);

// Prints symbolizer markup context elements for the kernel.
void PrintSymbolizerContext(FILE*);

// Prints version info and kernel mappings required to interpret backtraces.
void print_backtrace_version_info(FILE* f = stdout);

#endif  // ZIRCON_KERNEL_LIB_VERSION_INCLUDE_LIB_VERSION_H_
