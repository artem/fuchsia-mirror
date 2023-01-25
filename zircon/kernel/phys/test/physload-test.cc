// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "../physload.h"

#include <zircon/assert.h>

#include <ktl/string_view.h>
#include <phys/elf-image.h>
#include <phys/symbolize.h>

#include <ktl/enforce.h>

extern "C" void PhysLoadModuleMain(UartDriver& uart, PhysBootTimes boot_times,
                                   KernelStorage kernel_storage) {
  constexpr const char* kTestName = "physload-test";

  gSymbolize->set_name(kTestName);

  ktl::string_view me = gSymbolize->modules().back()->name();
  ZX_ASSERT_MSG(me == kTestName, "symbolize->name() \"%s\", last module name \"%.*s\"",
                gSymbolize->name(), static_cast<int>(me.size()), me.data());

  Allocation::GetPool().PrintMemoryRanges(gSymbolize->name());

  printf("\n*** Test succeeded ***\n%s\n\n", BOOT_TEST_SUCCESS_STRING);

  abort();
}
