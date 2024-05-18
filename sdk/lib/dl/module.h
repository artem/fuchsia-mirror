// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_MODULE_H_
#define LIB_DL_MODULE_H_

#include <lib/elfldltl/alloc-checker-container.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/resolve.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/fit/result.h>
#include <lib/ld/decoded-module-in-memory.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>
#include <lib/ld/memory.h>
#include <lib/ld/module.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/vector.h>

#include "diagnostics.h"

namespace dl {

using Elf = elfldltl::Elf<>;
using Soname = elfldltl::Soname<>;

// TODO(https://fxbug.dev/324136435): These KMax* variables are copied from
// //sdk/lib/ld/startup-load.h, we should share these symbols instead.
// Usually there are fewer than five segments, so this seems like a reasonable
// upper bound to support.
inline constexpr size_t kMaxSegments = 8;

// There can be quite a few metadata phdrs in addition to a PT_LOAD for each
// segment, so allow a fair few more.
inline constexpr size_t kMaxPhdrs = 32;
static_assert(kMaxPhdrs > kMaxSegments);

class ModuleHandle;
// A list of unique "permanent" ModuleHandle data structures used to represent
// a loaded file in the system image.
// TODO(caslyn): talk about relationship with LoadModuleList.
using ModuleHandleList = fbl::DoublyLinkedList<std::unique_ptr<ModuleHandle>>;

template <class Loader>
class LoadModule;
// A list of unique "temporary" LoadModule data structures used for loading a
// file.
template <class Loader>
using LoadModuleList = fbl::DoublyLinkedList<std::unique_ptr<LoadModule<Loader>>>;

// TODO(https://fxbug.dev/324136831): comment on how ModuleHandle relates to
// startup modules when the latter is supported.
// TODO(https://fxbug.dev/328135195): comment on the reference counting when
// that gets implemented.

// A ModuleHandle is created for every unique ELF file object loaded either
// directly or indirectly as a dependency of another module. It holds the
// ld::abi::Abi<...>::Module data structure that describes the module in the
// passive ABI (see //sdk/lib/ld/module.h).

// A ModuleHandle has a corresponding LoadModule (see below) to represent the
// ELF file when it is first loaded by `dlopen`. Whereas a LoadModule is
// ephemeral and lives only as long as it takes to load a module and its
// dependencies in `dlopen`, the ModuleHandle is a "permanent" data structure
// that is kept alive in the RuntimeDynamicLinker's `modules_` list until the
// module is unloaded.

// While this is an internal API, a ModuleHandle* is the void* handle returned
// by the public <dlfcn.h> API.
class ModuleHandle : public fbl::DoublyLinkedListable<std::unique_ptr<ModuleHandle>> {
 public:
  using Addr = Elf::Addr;
  using SymbolInfo = elfldltl::SymbolInfo<Elf>;
  using AbiModule = ld::AbiModule<>;

  // Not copyable, but movable.
  ModuleHandle(const ModuleHandle&) = delete;
  ModuleHandle(ModuleHandle&&) = default;

  // See unmap-[posix|zircon].cc for the dtor. On destruction, the module's load
  // image is unmapped per the semantics of the OS implementation.
  ~ModuleHandle();

  // The name of the module handle is set to the filename passed to dlopen() to
  // create the module handle. This is usually the same as the DT_SONAME of the
  // AbiModule, but that is not guaranteed. When performing an equality check,
  // match against both possible name values.
  constexpr bool operator==(const Soname& name) const {
    return name == name_ || name == abi_module_.soname;
  }

  constexpr const Soname& name() const { return name_; }

  // TODO(https://fxbug.dev/333920495): pass in the symbolizer_modid.
  [[nodiscard]] static std::unique_ptr<ModuleHandle> Create(fbl::AllocChecker& ac, Soname name) {
    std::unique_ptr<ModuleHandle> module{new (ac) ModuleHandle};
    if (module) [[likely]] {
      module->name_ = name;
    }
    return module;
  }

  constexpr AbiModule& module() { return abi_module_; }

  constexpr const AbiModule& module() const { return abi_module_; }

  constexpr Addr load_bias() const { return abi_module_.link_map.addr; }

  const SymbolInfo& symbol_info() const { return abi_module_.symbols; }

  size_t vaddr_size() const { return abi_module_.vaddr_end - abi_module_.vaddr_start; }

 private:
  // A ModuleHandle can only be created with Module::Create...).
  ModuleHandle() = default;

  static void Unmap(uintptr_t vaddr, size_t len);
  Soname name_;
  AbiModule abi_module_;
};

// Use a AllocCheckerContainer that supports fallible allocations; methods
// return a boolean value to signify allocation success or failure.
template <typename T>
using Vector = elfldltl::AllocCheckerContainer<fbl::Vector>::Container<T>;

// LoadModule is the temporary data structure created to load a file; a
// LoadModule is created when a file needs to be loaded, and is destroyed after
// the file module and all its dependencies have been loaded, decoded, symbols
// resolved, and relro protected.
template <class Loader>
class LoadModule : public ld::LoadModule<ld::DecodedModuleInMemory<>>,
                   public fbl::DoublyLinkedListable<std::unique_ptr<LoadModule<Loader>>> {
 public:
  using Relro = typename Loader::Relro;
  using Phdr = Elf::Phdr;
  using Dyn = Elf::Dyn;
  using LoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StaticVector<kMaxSegments>::Container>;

  // TODO(https://fxbug.dev/331421403): Implement TLS.
  struct NoTlsDesc {
    using TlsDescGot = typename Elf::TlsDescGot;
    constexpr TlsDescGot operator()() const {
      assert(false && "TLS is not supported");
      return {};
    }
    template <class Diagnostics, class Definition>
    constexpr fit::result<bool, TlsDescGot> operator()(Diagnostics& diag,
                                                       const Definition& defn) const {
      assert(false && "TLS is not supported");
      return fit::error{false};
    }
  };

  // This is the observer used to collect DT_NEEDED offsets from the dynamic phdr.
  static const constexpr std::string_view kNeededError{"DT_NEEDED offsets"};
  using NeededObserver = elfldltl::DynamicValueCollectionObserver<  //
      Elf, elfldltl::ElfDynTag::kNeeded, Vector<size_type>, kNeededError>;

  // The LoadModule::Create(...) takes a reference to the ModuleHandle for the
  // file, setting information on it during the loading, decoding, and
  // relocation process.
  [[nodiscard]] static std::unique_ptr<LoadModule> Create(fbl::AllocChecker& ac,
                                                          ModuleHandle& module) {
    std::unique_ptr<LoadModule> load_module{new (ac) LoadModule(module)};
    if (load_module) [[likely]] {
      load_module->set_name(module.name());
      // Have the underlying DecodedModule (see <lib/ld/decoded-module.h>) point to
      // the ABIModule embedded in the ModuleHandle, so that its information will
      // be filled out during decoding operations.
      load_module->decoded().set_module(module.module());
    }
    return load_module;
  }

  // Load `file` into the system image, decode phdrs and save the metadata in
  // the the ABI module. Decode the module's dependencies (if any), and
  // return a vector their so names.
  template <class File>
  std::optional<Vector<Soname>> Load(Diagnostics& diag, File&& file) {
    // Read the file header and program headers into stack buffers and map in
    // the image.  This fills in load_info() as well as the module vaddr bounds
    // and phdrs fields.
    Loader loader;
    auto headers = decoded().LoadFromFile(diag, loader, std::move(file));
    if (!headers) [[unlikely]] {
      return std::nullopt;
    }

    Vector<size_type> needed_offsets;
    // TODO(https://fxbug.dev/331421403): TLS is not supported yet.
    size_type max_tls_modid = 0;
    if (!decoded().DecodeFromMemory(  //
            diag, loader.memory(), loader.page_size(), *headers, max_tls_modid,
            elfldltl::DynamicRelocationInfoObserver(decoded().reloc_info()),
            NeededObserver(needed_offsets))) [[unlikely]] {
      return std::nullopt;
    }

    // After successfully loading the file, finalize the module's mapping by
    // calling `Commit` on the loader. Save the returned relro capability
    // that will be used to apply relro protections later.
    loader_relro_ = std::move(loader).Commit(decoded().relro_bounds());

    // TODO(https://fxbug.dev/324136435): The code that parses the names from
    // the symbol table be shared with <lib/ld/remote-decoded-module.h>.
    if (Vector<Soname> needed_names;
        needed_names.reserve(diag, kNeededError, needed_offsets.size())) [[likely]] {
      for (size_type offset : needed_offsets) {
        std::string_view name = this->symbol_info().string(offset);
        if (name.empty()) [[unlikely]] {
          diag.FormatError("DT_NEEDED has DT_STRTAB offset ", offset, " with DT_STRSZ ",
                           this->symbol_info().strtab().size());
          return std::nullopt;
        }
        if (!needed_names.push_back(diag, kNeededError, Soname{name})) [[unlikely]] {
          return std::nullopt;
        }
      }
      return std::move(needed_names);
    }

    return std::nullopt;
  }

  // Perform relative and symbolic relocations, resolving symbols from the
  // list of modules as needed.
  bool Relocate(Diagnostics& diag, LoadModuleList<Loader>& modules) {
    constexpr NoTlsDesc kNoTlsDesc{};
    auto memory = ld::ModuleMemory{module()};
    auto resolver = elfldltl::MakeSymbolResolver(*this, modules, diag, kNoTlsDesc);
    return elfldltl::RelocateRelative(diag, memory, reloc_info(), load_bias()) &&
           elfldltl::RelocateSymbolic(memory, diag, reloc_info(), symbol_info(), load_bias(),
                                      resolver);
  }

 private:
  // A LoadModule can only be created with LoadModule::Create...).
  explicit LoadModule(ModuleHandle& module) : module_(module) {}

  // This is a reference to the "permanent" module data structure that this
  // LoadModule is responsible for: runtime information is set on the `module_`
  // during the course of the loading process. Whereas this LoadModule instance
  // will get destroyed at the end of `dlopen`, its `module_` will live as long
  // as the file is loaded in the RuntimeDynamicLinker's `modules_` list.
  ModuleHandle& module_;
  Relro loader_relro_;
};

}  // namespace dl

#endif  // LIB_DL_MODULE_H_
