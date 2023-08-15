// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_TEST_LOAD_TESTS_H_
#define SRC_LIB_ELFLDLTL_TEST_LOAD_TESTS_H_

#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/phdr.h>

constexpr size_t kPageSize = 0x1000;

template <uint64_t Flags, uint64_t FileSz = kPageSize, uint64_t MemSz = kPageSize>
struct CreatePhdr {
  template <typename Elf>
  struct type {
    constexpr auto operator()(typename Elf::size_type& offset) {
      using Phdr = typename Elf::Phdr;
      Phdr phdr{.type = elfldltl::ElfPhdrType::kLoad,
                .offset = offset,
                .vaddr = offset,
                .filesz = FileSz,
                .memsz = MemSz};
      phdr.flags = Flags;
      offset += kPageSize;
      return phdr;
    }
  };
};

template <typename Elf>
using ConstantPhdr = CreatePhdr<elfldltl::PhdrBase::kRead>::type<Elf>;

template <typename Elf>
using ZeroFillPhdr =
    CreatePhdr<elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kWrite, 0>::type<Elf>;

template <typename Elf>
using DataWithZeroFillPhdr = CreatePhdr<elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kWrite,
                                        kPageSize, kPageSize * 2>::type<Elf>;

template <typename Elf>
using DataPhdr = CreatePhdr<elfldltl::PhdrBase::kRead | elfldltl::PhdrBase::kWrite>::type<Elf>;

#endif  // SRC_LIB_ELFLDLTL_TEST_LOAD_TESTS_H_
