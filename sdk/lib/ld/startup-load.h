// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_STARTUP_LOAD_H_
#define LIB_LD_STARTUP_LOAD_H_

#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/relro.h>
#include <lib/elfldltl/resolve.h>
#include <lib/elfldltl/self.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>
#include <lib/ld/tls.h>
#include <lib/ld/tlsdesc.h>

#include <algorithm>

#include <fbl/intrusive_double_list.h>

#include "allocator.h"
#include "bootstrap.h"
#include "mutable-abi.h"
#include "startup-diagnostics.h"

namespace ld {

// The startup dynamic linker always uses the default ELF layout.
using Elf = elfldltl::Elf<>;
using size_type = Elf::size_type;
using Addr = Elf::Addr;
using Addend = Elf::Addend;
using Ehdr = Elf::Ehdr;
using Phdr = Elf::Phdr;
using Sym = Elf::Sym;
using Dyn = Elf::Dyn;
using TlsDescGot = Elf::TlsDescGot;

// StartupLoadModule::Load returns this.
struct StartupLoadResult {
  // This is the number of DT_NEEDED entries seen.  Their strings can't be
  // decoded without a second elfldltl::DecodeDynamic scan since the first one
  // has to find DT_STRTAB and it might not be first.  But the first scan
  // counts how many entries there are, so the second scan can be
  // short-circuited rather than always doing a full O(n) scan of all entries.
  size_t needed_count = 0;

  // These are only of interest for the main executable.
  uintptr_t entry = 0;               // Runtime entry point address.
  std::optional<size_t> stack_size;  // Requested initial stack size.
};

class TlsDescResolver {
 public:
  // Handle an undefined weak TLSDESC reference.  There are special runtime
  // resolvers for this case: one for zero addend, and one for nonzero addend.
  TlsDescGot operator()(Addend addend) const {
    if (addend == 0) {
      return {.function = kRuntimeUndefinedWeak};
    }
    return {
        .function = kRuntimeUndefinedWeakAddend,
        .value = cpp20::bit_cast<size_type>(addend),
    };
  }

  // Handle a TLSDESC reference to a defined symbol.  The runtime resolver just
  // loads the static TLS offset from the value slot.  When the relocation is
  // applied the addend will be added to the value computed here from the
  // symbol's value and module's PT_TLS offset.
  template <class Definition>
  fit::result<bool, TlsDescGot> operator()(Diagnostics& diag, const Definition& defn) const {
    assert(!defn.undefined_weak());
    return fit::ok(TlsDescGot{
        .function = kRuntimeStatic,
        .value = defn.symbol().value + defn.static_tls_bias(),
    });
  }

 private:
  static inline const Addr kRuntimeStatic{
      reinterpret_cast<uintptr_t>(_ld_tlsdesc_runtime_static),
  };
  static inline const Addr kRuntimeUndefinedWeak{
      reinterpret_cast<uintptr_t>(_ld_tlsdesc_runtime_undefined_weak),
  };
  static inline const Addr kRuntimeUndefinedWeakAddend{
      reinterpret_cast<uintptr_t>(_ld_tlsdesc_runtime_undefined_weak_addend),
  };
};

inline constexpr TlsDescResolver kTlsDescResolver{};

// StartupLoadModule is the LoadModule type used in the startup dynamic linker.
// Its LoadInfo uses fixed storage bounded by kMaxSegments.  The Module is
// allocated separately using the initial-exec allocator.

using StartupLoadModuleBase = LoadModule<
    DecodedModule<Elf, elfldltl::StaticVector<kMaxSegments>::Container, AbiModuleInline::kNo>>;

template <class Loader>
struct StartupLoadModule : public StartupLoadModuleBase,
                           fbl::DoublyLinkedListable<StartupLoadModule<Loader>*> {
 public:
  using List = fbl::DoublyLinkedList<StartupLoadModule*>;
  using PreloadedModulesList = std::pair<List, cpp20::span<const Dyn>>;

  StartupLoadModule() = delete;

  StartupLoadModule(StartupLoadModule&&) = default;

  template <typename... LoaderArgs>
  explicit StartupLoadModule(const elfldltl::Soname<>& name, LoaderArgs&&... loader_args)
      : StartupLoadModuleBase{name}, loader_{std::forward<LoaderArgs>(loader_args)...} {}

  // This uses the given scratch allocator to create a new module object.
  template <class Allocator, typename... LoaderArgs>
  static StartupLoadModule* New(Diagnostics& diag, Allocator& allocator,
                                const elfldltl::Soname<>& name, LoaderArgs&&... loader_args) {
    fbl::AllocChecker ac;
    StartupLoadModule* module =
        new (allocator, ac) StartupLoadModule{name, std::forward<LoaderArgs>(loader_args)...};
    CheckAlloc(diag, ac, "temporary module data structure");
    return module;
  }

  // Read the file and use Loader::Load on it.  If at least partially
  // successful, this uses the given initial-exec allocator to set up the
  // passive ABI module in this->module().  The allocator only guarantees two
  // mutable allocations at a time, so the caller must then promptly splice it
  // into the link_map list before the next Load call allocates the next one.
  template <class Allocator, class File>
  [[nodiscard]] StartupLoadResult Load(Diagnostics& diag, Allocator& allocator, File&& file,
                                       uint32_t symbolizer_modid, size_type& max_tls_modid) {
    // Diagnostics sent to diag during loading will be prefixed with the module
    // name, unless the name is empty as it is for the main executable.
    ScopedModuleDiagnostics module_diag(diag, this->name().str());

    // Allocate the Module object first.
    fbl::AllocChecker ac;
    this->NewModule(symbolizer_modid, allocator, ac);
    CheckAlloc(diag, ac, "passive ABI module");

    // All modules allocated by StartupModule are part of the initial exec set
    // and their symbols are inherently visible.
    decoded().module().symbols_visible = true;

    // Read the file header and program headers into stack buffers and map in
    // the image.  This fills in load_info() as well as the module vaddr bounds
    // and phdrs fields.  Note that module().phdrs might remain empty if the
    // phdrs aren't in the load image, so keep using the stack copy read from
    // the file instead.
    auto headers = decoded().LoadFromFile(diag, loader_, std::forward<File>(file));
    if (!headers) [[unlikely]] {
      return {};
    }
    auto& [ehdr_owner, phdrs_owner] = *headers;
    const Ehdr& ehdr = ehdr_owner;
    const cpp20::span<const Phdr> phdrs = phdrs_owner;

    // Now the module's image is in memory!  Decode phdrs for everything else
    // but the PT_LOADs for load_info_, already filled by LoadFromFile.
    auto result = DecodeModulePhdrs(  //
        diag, phdrs, PhdrMemoryBuildIdObserver(memory(), decoded().module()));
    if (!result) [[unlikely]] {
      return {};
    }

    auto [dyn_phdr, tls_phdr, relro_phdr, stack_size] = *result;
    set_relro(relro_phdr);

    // Start filling in module() with pointers into the loaded image.

    // If there was a PT_TLS, fill in tls_module() to be published later.
    if (tls_phdr) {
      decoded().SetTls(diag, memory(), *tls_phdr, ++max_tls_modid);
    }

    // Now that there is a Memory object to use, decode the dynamic section.
    size_t needed_count = DecodeDynamic(diag, dyn_phdr);

    // Everything is now prepared to proceed with loading dependencies
    // and performing relocation.
    return {
        .needed_count = needed_count,
        .entry = ehdr.entry + loader_.load_bias(),
        .stack_size = stack_size,
    };
  }

  void set_relro(std::optional<Phdr> relro_phdr) {
    if (relro_phdr) {
      relro_ = this->load_info().RelroBounds(*relro_phdr, loader_.page_size());
    }
  }

  void Relocate(Diagnostics& diag, const List& modules) {
    elfldltl::RelocateRelative(diag, memory(), reloc_info(), load_bias());
    auto resolver = elfldltl::MakeSymbolResolver(*this, modules, diag, kTlsDescResolver);
    elfldltl::RelocateSymbolic(memory(), diag, reloc_info(), symbol_info(), load_bias(), resolver);
  }

  // Returns number of DT_NEEDED entries. See StartupLoadResult, for why that is useful.
  size_t DecodeDynamic(Diagnostics& diag, const std::optional<Phdr>& dyn_phdr) {
    using NeededObserver = elfldltl::DynamicTagCountObserver<Elf, elfldltl::ElfDynTag::kNeeded>;
    size_t count = 0;
    // Save the span of Dyn entries for LoadDeps to scan later.
    dynamic_ =
        *DecodeModuleDynamic(decoded().module(), diag, memory(), dyn_phdr, NeededObserver(count),
                             elfldltl::DynamicRelocationInfoObserver(decoded().reloc_info()));
    return count;
  }

  // Since later failures will be fatal anyway, we can go ahead and commit the
  // mappings so the Loader destructor won't unmap the module.  Transferring
  // ownership of the mappings and ending the lifetime of the Loader object is
  // part of preparing to apply RELRO protections.  But we have no need to hold
  // onto the Loader::Relro capability any longer.
  void CommitAndProtectRelro(Diagnostics& diag) {
    std::ignore = std::move(loader_).Commit(relro_).Commit(diag);
  }

  List MakeList() {
    List list;
    list.push_back(this);
    return list;
  }

  // This is only valid until CommitAndProtectRelro() is called.
  auto& memory() { return loader_.memory(); }

  template <typename ScratchAllocator, typename InitialExecAllocator, typename GetDepFile,
            typename... LoaderArgs>
  static void LinkModules(Diagnostics& diag, ScratchAllocator& scratch,
                          InitialExecAllocator& initial_exec, StartupLoadModule* main_executable,
                          GetDepFile&& get_dep_file,
                          std::initializer_list<BootstrapModule> preloaded_module_list,
                          size_t executable_needed_count, LoaderArgs&&... loader_args) {
    main_executable->decoded().module().symbols_visible = true;

    // The main executable implicitly can use static TLS and doesn't have to
    // have DF_STATIC_TLS set at link time.
    main_executable->decoded().module().symbols.set_flags(
        main_executable->module().symbols.flags() | elfldltl::ElfDynFlags::kStaticTls);

    List modules = main_executable->MakeList();
    List preloaded_modules =
        MakePreloadedList(diag, scratch, preloaded_module_list, loader_args...);

    // This will be incremented by each Load() of a module that has a PT_TLS.
    size_type max_tls_modid = main_executable->tls_module_id();

    LoadDeps(diag, scratch, initial_exec, modules, preloaded_modules, executable_needed_count,
             std::forward<GetDepFile>(get_dep_file), max_tls_modid, loader_args...);
    CheckErrors(diag);

    // This assigns static TLS offsets, so it must happen before relocation.
    PopulateAbiTls(diag, initial_exec, modules, max_tls_modid);

    RelocateModules(diag, modules);
    CheckErrors(diag);

    PopulateAbiLoadedModules(modules, std::move(preloaded_modules));
    PopulateAbiRdebug(modules);

    CommitModules(diag, std::move(modules));
  }

 private:
  void Preload(Diagnostics& diag, Module& module, cpp20::span<const Dyn> dynamic) {
    decoded().set_module(module);
    dynamic_ = dynamic;

    // Scan the phdrs to populate the LoadInfo just so it can be used for
    // things like symbolizer markup.
    elfldltl::DecodePhdrs(diag, module.phdrs.get(),
                          decoded().load_info().GetPhdrObserver(loader_.page_size()));
  }

  bool IsLoaded() const { return decoded().HasModule(); }

  template <typename Allocator, typename... LoaderArgs>
  static List MakePreloadedList(Diagnostics& diag, Allocator& allocator,
                                std::initializer_list<BootstrapModule> modules,
                                LoaderArgs&&... loader_args) {
    List preloaded_modules;
    for (const auto& [module, dyn] : modules) {
      StartupLoadModule* m = New(diag, allocator, module.soname, loader_args...);
      m->Preload(diag, module, dyn);
      preloaded_modules.push_back(m);
    }
    return preloaded_modules;
  }

  void AddToPassiveAbi(typename List::iterator it, bool symbols_visible) {
    decoded().module().symbols_visible = symbols_visible;
    auto& ins_link_map = it->decoded().module().link_map;
    auto& this_link_map = decoded().module().link_map;
    ins_link_map.next = &this_link_map;
    this_link_map.prev = &ins_link_map;
  }

  // If `soname` is found in `preloaded_modules` it will be removed from that
  // list and pushed into `modules`, making the symbols from those modules
  // visible to the program.
  static bool FindModule(List& modules, List& preloaded_modules, const elfldltl::Soname<>& soname) {
    if (std::find(modules.begin(), modules.end(), soname) != modules.end()) {
      return true;
    }
    if (auto found = std::find(preloaded_modules.begin(), preloaded_modules.end(), soname);
        found != preloaded_modules.end()) {
      // TODO(https://fxbug.dev/42080760): Mark this preloaded_module as having it's symbols visible
      // to the program.
      modules.push_back(preloaded_modules.erase(found));
      return true;
    }
    return false;
  }

  template <typename Allocator, typename... LoaderArgs>
  void EnqueueDeps(Diagnostics& diag, Allocator& allocator, List& modules, List& preloaded_modules,
                   size_t needed_count, LoaderArgs&&... loader_args) {
    auto handle_needed = [&](std::string_view soname_str) {
      assert(needed_count > 0);
      elfldltl::Soname<> soname{soname_str};
      if (!FindModule(modules, preloaded_modules, soname)) {
        modules.push_back(New(diag, allocator, soname, loader_args...));
      }
      return --needed_count > 0;
    };

    auto observer = elfldltl::DynamicNeededObserver(symbol_info(), handle_needed);
    elfldltl::DecodeDynamic(diag, memory(), dynamic_, observer);
  }

  // `get_dep_file` is called as `std::optional<File>(std::string_view)`.
  // File must meet the requirements of a File type described in
  // lib/elfldltl/memory.h.
  template <typename ScratchAllocator, typename InitialExecAllocator, typename GetDepFile,
            typename... LoaderArgs>
  static void LoadDeps(Diagnostics& diag, ScratchAllocator& scratch,
                       InitialExecAllocator& initial_exec, List& modules, List& preloaded_modules,
                       size_t needed_count, GetDepFile&& get_dep_file, size_type& max_tls_modid,
                       LoaderArgs&&... loader_args) {
    // Note, this assumes that ModuleList iterators are not invalidated after
    // push_back(), done by `EnqueueDeps`. This is true of lists and
    // StaticVector. No assumptions are made on the validity of the end()
    // iterator, so it is checked at every iteration.
    uint32_t symbolizer_modid = 0;
    for (auto it = modules.begin(); it != modules.end(); it++) {
      const bool was_already_loaded = it->IsLoaded();
      if (was_already_loaded) {
        it->decoded().module().symbolizer_modid = symbolizer_modid++;
      } else if (auto file = get_dep_file(it->name())) {
        needed_count =
            it->Load(diag, initial_exec, *file, symbolizer_modid++, max_tls_modid).needed_count;
        assert(it->IsLoaded());
      } else {
        diag.MissingDependency(it->name().str());
        return;
      }
      // The main executable is always first in the list, so its prev is
      // already correct and adding the second module will set its next.
      if (it != modules.begin()) {
        it->AddToPassiveAbi(std::prev(it), true);
        // Referenced preloaded modules can't have DT_NEEDED, do don't bother
        // enqueuing their deps.
        if (was_already_loaded) {
          continue;
        }
      }
      it->EnqueueDeps(diag, scratch, modules, preloaded_modules, needed_count, loader_args...);
    }
  }

  static void RelocateModules(Diagnostics& diag, List& modules) {
    for (auto& module : modules) {
      ScopedModuleDiagnostics module_diag{diag, module.name().str()};
      module.Relocate(diag, modules);
      module.CommitAndProtectRelro(diag);
    }
  }

  static void CommitModules(Diagnostics& diag, List modules) {
    while (!modules.is_empty()) {
      auto* module = modules.pop_front();
      diag.report().ReportModuleLoaded(*module);
      // The `operator delete` this calls does nothing since the scratch
      // allocator doesn't support deallocation per se since the scratch
      // memory will be deallocated en masse, but this calls destructors.
      delete module;
    }
  }

  static void PopulateAbiLoadedModules(List& modules, List preloaded_modules) {
    // We want to add the remaining modules to the list. Their symbols aren't
    // visible for symbolic resolution, but the program can still use their
    // functions even with no relocations resolving to their symbols.
    // Therefore, we need to add these modules to the global module list so
    // they can still be seen by dl_iterate_phdr for unwinding purposes.  For
    // example, TLSDESC implementation code lives in the dynamic linker and
    // will be called as part of the TLS implementation without ever having a
    // DT_NEEDED on ld.so. On systems other than Fuchsia it may also be
    // possible to get code from the vDSO without an explicit DT_NEEDED, which
    // is common on Linux.
    auto last = std::prev(modules.end());
    modules.splice(modules.end(), preloaded_modules);
    for (auto next = std::next(last), end = modules.end(); next != end; last = next, next++) {
      // Assign increasing symbolizer module IDs to the preloaded module now,
      // so the ID order matches the list order.  Its module() is still mutable
      // since it's in .bss rather than coming from the InitialExecAllocator.
      next->decoded().module().symbolizer_modid = last->module().symbolizer_modid + 1;
      next->AddToPassiveAbi(last, false);
    }

    ld::mutable_abi.loaded_modules = &modules.begin()->module();
  }

  static void PopulateAbiRdebug(const List& modules) {
    ld::mutable_r_debug.version = elfldltl::kRDebugVersion;
    ld::mutable_r_debug.map = &modules.begin()->module().link_map;
    assert(ld::mutable_r_debug.state == elfldltl::RDebugState::kConsistent);
    ld::mutable_r_debug.ldbase = elfldltl::Self<>::LoadBias();
  }

  // The passive ABI's TlsModule structs are allocated in a contiguous array
  // indexed by TLS module ID, so they cannot be built up piecemeal in their
  // final locations.  Instead, they're stored directly in the LoadModule when
  // a module has one.  This collects all those and copies them into the
  // passive ABI's array.
  template <typename InitialExecAllocator>
  static void PopulateAbiTls(Diagnostics& diag, InitialExecAllocator& initial_exec_allocator,
                             List& modules, size_type max_tls_modid) {
    if (max_tls_modid > 0) {
      auto new_array = [&diag, &initial_exec_allocator, max_tls_modid](auto& result) {
        using T = typename std::decay_t<decltype(result.front())>;
        fbl::AllocChecker ac;
        T* array = new (initial_exec_allocator, ac) T[max_tls_modid];
        CheckAlloc(diag, ac, "passive ABI for TLS modules");
        result = {array, max_tls_modid};
      };

      cpp20::span<TlsModule> tls_modules;
      cpp20::span<Addr> tls_offsets;
      new_array(tls_modules);
      new_array(tls_offsets);

      for (StartupLoadModule& module : modules) {
        if (auto tls = module.AssignStaticTls(mutable_abi.static_tls_layout)) {
          tls_modules[*tls] = module.tls_module();
          tls_offsets[*tls] = module.static_tls_bias();
        }

#ifdef NDEBUG
        if (module.tls_module_id() == tls_modules.size()) {
          // Don't keep scanning the list if there aren't any more, but skip
          // this optimization if it would prevent a later iteration from
          // hitting an assertion failure if there's a bug that causes an
          // invalid index into the span.
          break;
        }
#endif
      }

      mutable_abi.static_tls_modules = tls_modules;
      mutable_abi.static_tls_offsets = tls_offsets;
    }
  }

  Loader loader_;  // Must be initialized by constructor.
  cpp20::span<const Dyn> dynamic_;
  LoadInfo::Region relro_{};
};

}  // namespace ld

#endif  // LIB_LD_STARTUP_LOAD_H_
