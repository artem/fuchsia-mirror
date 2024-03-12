// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_LOAD_MODULE_H_
#define LIB_LD_LOAD_MODULE_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/tls-layout.h>
#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <lib/ld/tls.h>

#include <cassert>
#include <functional>

#if __cpp_impl_three_way_comparison >= 201907L
#include <compare>
#endif

#include <fbl/alloc_checker.h>

namespace ld {

// The ld::DecodedModule template class provides a base class for a dynamic
// linker's internal data structure describing a module's ELF metadata.  This
// holds the ld::abi::Abi<...>::Module object that describes the module in the
// passive ABI, but also other information the only the dynamic linker itself
// needs.  It's parameterized by a container template for elfldltl::LoadInfo
// (see <lib/elfldltl/load.h> and <lib/elfldltl/container.h>).
//
// This base class intentionally describes only information that is extracted
// directly from the ELF file's contents; it does not include things like a
// runtime load address, TLS module ID assignment, or even the name by which
// the file was loaded (only any DT_SONAME that might be embedded in the ELF
// metadata itself).  This can be appropriate to use as the base class for
// objects describing a cached ELF file.
//
// See ld::LoadModule (below) for how DecodedModule can be used in a particular
// dynamic linking session.

// This template parameter indicates whether the ld::abi::Abi<...>::Module is
// stored directly (inline) in the ld::DecodedModule or is allocated
// separately.  For in-process dynamic linking, it's allocated separately to
// survive in the passive ABI after the LoadModule object is no longer needed.
// For other cases like out-of-process, that's not needed since publishing its
// data to the passive ABI requires separate work anyway.
enum class AbiModuleInline : bool { kNo, kYes };

// This template parameter indicates whether an elfldltl::RelocationInfo
// object is included.  For simple dynamic linking, the whole LoadModule
// object is ephemeral and discarded shortly after relocation is finished.
// For a zygote model, the LoadModule might be kept indefinitely after
// relocation to be used for repeated loading or symbol resolution.
enum class DecodedModuleRelocInfo : bool { kNo, kYes };

// This is used as a base class for all DecodedModule<...> instantiations just
// as a marker so they can be detected using std::is_base_of.
struct DecodedModuleBase {};

template <class ElfLayout, template <typename> class SegmentContainer, AbiModuleInline InlineModule,
          DecodedModuleRelocInfo WithRelocInfo = DecodedModuleRelocInfo::kYes,
          template <class SegmentType> class SegmentWrapper = elfldltl::NoSegmentWrapper>
class DecodedModule : public DecodedModuleBase {
 public:
  using Elf = ElfLayout;
  using Addr = typename Elf::Addr;
  using size_type = typename Elf::size_type;
  using Module = typename abi::Abi<Elf>::Module;
  using TlsModule = typename abi::Abi<Elf>::TlsModule;
  using LoadInfo =
      elfldltl::LoadInfo<Elf, SegmentContainer, elfldltl::PhdrLoadPolicy::kBasic, SegmentWrapper>;
  using RelocationInfo = elfldltl::RelocationInfo<Elf>;
  using Soname = elfldltl::Soname<Elf>;
  using Phdr = typename Elf::Phdr;
  using Sym = typename Elf::Sym;

  static_assert(std::is_move_constructible_v<LoadInfo> || std::is_copy_constructible_v<LoadInfo>);

  static constexpr bool kModuleInline = InlineModule == AbiModuleInline::kYes;
  static constexpr bool kRelocInfo = WithRelocInfo == DecodedModuleRelocInfo::kYes;

  // Derived types might or might not be default-constructed.
  constexpr DecodedModule() = default;

  // Derived types might or might not be copyable and/or movable, depending on
  // the SegmentContainer and SegmentWrapper semantics.
  constexpr DecodedModule(const DecodedModule&) = default;
  constexpr DecodedModule(DecodedModule&&) noexcept = default;
  constexpr DecodedModule& operator=(const DecodedModule&) = default;
  constexpr DecodedModule& operator=(DecodedModule&&) noexcept = default;

  // When a module has been actually decoded at all, this returns true.
  // In default-constructed state, module() cannot be called.  Once
  // HasModule() is true, then module() can be inspected and its
  // contains will be safe, but may be incomplete if decoding hit errors
  // but the Diagnostics object said to keep going.
  constexpr bool HasModule() const { return static_cast<bool>(module_); }

  // This should be used only after EmplaceModule or (successful) NewModule.
  constexpr Module& module() {
    assert(module_);
    return *module_;
  }
  constexpr const Module& module() const {
    assert(module_);
    return *module_;
  }

  // **NOTE:** Most methods below use module() and cannot be called unless
  // HasModule() returns true, i.e. after EmplaceModule / NewModule.

  constexpr const auto& symbol_info() const { return module().symbols; }

  // This is the DT_SONAME in the file or empty if there was none (or if
  // decoding was incomplete).  This often matches the name by which the
  // module is known, but need not.  There may be no SONAME at all (normal for
  // an executable or loadable module rather than a shared library), or the
  // embedded SONAME is not the same as the lookup name (file name, usually)
  // used to get the file.
  constexpr const Soname& soname() const { return module().soname; }

  // See <lib/elfldtl/load.h> for details; the LoadInfo provides information
  // on address layout.  This is used to map file segments into memory and/or
  // to convert memory addresses to file offsets.  The SegmentWrapper template
  // class may extend the load_info().segments() element types with holders of
  // segment data mapped or copied from the file (and maybe further prepared).
  constexpr LoadInfo& load_info() { return load_info_; }
  constexpr const LoadInfo& load_info() const { return load_info_; }

  // See <lib/elfldtl/relocation.h> for details; the RelocationInfo provides
  // information need by the <lib/elfldtl/link.h> layer to drive dynamic
  // linking of this module.  It doesn't need to be stored at all if a module
  // is being cached after relocation without concern for a separate dynamic
  // linking session reusing the same data.
  template <auto R = WithRelocInfo, typename = std::enable_if_t<R == DecodedModuleRelocInfo::kYes>>
  constexpr RelocationInfo& reloc_info() {
    return reloc_info_;
  }
  template <auto R = WithRelocInfo, typename = std::enable_if_t<R == DecodedModuleRelocInfo::kYes>>
  constexpr const RelocationInfo& reloc_info() const {
    return reloc_info_;
  }

  // Set up the Abi<>::TlsModule in tls_module() based on the PT_TLS segment.
  // The modid must be nonzero, but its only actual use is in the module() and
  // tls_module_id() values returned by this object.  In a derived object only
  // used as a pure cache of the ELF file's metadata, constant 1 is fine.
  template <class Diagnostics, class Memory>
  bool SetTls(Diagnostics& diag, Memory& memory, const Phdr& tls_phdr, size_type modid) {
    using PhdrError = elfldltl::internal::PhdrError<elfldltl::ElfPhdrType::kTls>;

    assert(modid != 0);
    module().tls_modid = modid;

    size_type alignment = std::max<size_type>(tls_phdr.align, 1);
    if (!cpp20::has_single_bit(alignment)) [[unlikely]] {
      if (!diag.FormatError(PhdrError::kBadAlignment)) {
        return false;
      }
    } else {
      tls_module_.tls_alignment = alignment;
    }

    if (tls_phdr.filesz > tls_phdr.memsz) [[unlikely]] {
      if (!diag.FormatError("PT_TLS header `p_filesz > p_memsz`")) {
        return false;
      }
    } else {
      tls_module_.tls_bss_size = tls_phdr.memsz - tls_phdr.filesz;
    }

    auto initial_data = memory.template ReadArray<std::byte>(tls_phdr.vaddr, tls_phdr.filesz);
    if (!initial_data) [[unlikely]] {
      return diag.FormatError("PT_TLS has invalid p_vaddr", elfldltl::FileAddress{tls_phdr.vaddr},
                              " or p_filesz ", tls_phdr.filesz());
    }
    tls_module_.tls_initial_data = *initial_data;

    return true;
  }

  // This returns the TLS module ID assigned by SetTls, or zero if none is
  // set.  In a derived type representing a pure cache of the ELF file's
  // metadata, this is just a flag where nonzero indicates it has a PT_TLS.
  // In a derived type for a module in a specific dynamic linking session,
  // this is the ABI's meaning of TLS module ID at runtime.  In either case,
  // it's always nonzero if the module has a PT_TLS and zero if it doesn't.
  constexpr size_type tls_module_id() const { return module().tls_modid; }

  // This should only be called after SetTls.
  constexpr const TlsModule& tls_module() const {
    assert(tls_module_id() != 0);
    return tls_module_;
  }

  template <bool Inline = InlineModule == AbiModuleInline::kYes,
            typename = std::enable_if_t<!Inline>>
  constexpr void set_module(Module& module) {
    module_ = &module;
  }

  // A derived class should wrap EmplaceModule and NewModule with calls to
  // SetAbiName passing the derived class's notion of primary name for the
  // module, so that the AbiModule representation always matches that name.
  constexpr void SetAbiName(const Soname& name) {
    if (module_) {
      module_->link_map.name = name.c_str();
    }
  }

  // In an instantiation with InlineModule=kYes, EmplaceModule(..) just
  // constructs Module{...}).  The modid is always required, though it need
  // not be meaningful.  In a derived type representing a pure cache of the
  // ELF file's metadta, it could be zero in every module.  In a derived type
  // representing a module in a specific dynamic linking scenario, it's useful
  // to assign the ld::abi::Abi::Module::symbolizer_modid value early and rely
  // on it corresponding to the module's order in the linking session's
  // "load-order" list of modules; this allows the index to serve as a means
  // of back-pointer from the module() object to a containing LoadModule sort
  // of object via reference to the session's primary module list.
  template <typename... Args, bool Inline = InlineModule == AbiModuleInline::kYes,
            typename = std::enable_if_t<Inline>>
  constexpr void EmplaceModule(uint32_t modid, Args&&... args) {
    assert(!module_);
    module_.emplace(std::forward<Args>(args)...);
    module_->symbolizer_modid = modid;
  }

  // In an instantiation with InlineModule=false, NewModule(a..., c...) does
  // new (a...) Module{c...}.  See above about the modid argument.  The last
  // argument in a... must be a fbl::AllocChecker that indicates whether `new`
  // succeeded.
  template <typename... Args, bool Inline = InlineModule == AbiModuleInline::kYes,
            typename = std::enable_if_t<!Inline>>
  constexpr void NewModule(uint32_t modid, Args&&... args) {
    assert(!module_);
    module_ = new (std::forward<Args>(args)...) Module;
    module_->symbolizer_modid = modid;
  }

 private:
  using ModuleStorage =
      std::conditional_t<InlineModule == AbiModuleInline::kYes, std::optional<Module>, Module*>;

  struct Empty {};

  using RelocInfoStorage =
      std::conditional_t<WithRelocInfo == DecodedModuleRelocInfo::kYes, RelocationInfo, Empty>;

  ModuleStorage module_{};
  LoadInfo load_info_;
  [[no_unique_address]] RelocInfoStorage reloc_info_;
  TlsModule tls_module_;
};

// Forward declaration for a helper class defined below.
// See the name_ref() and soname_ref() methods in ld::LoadModule, below.
template <class LoadModule>
class LoadModuleRef;

// The ld::LoadModule template class provides a base class for a
// information about a given ELF file's use in a particular dynamic linking
// scenario.  This includes things like its name and runtime load bias.
//
// The template parameter can either be a ld::DecodedModule<...> instantiation
// or subclass of one, or it can be some pointer-like type that can be
// dereferenced with `*` to get to one.  That can be a plain pointer, a pointer
// to `const`, a smart pointer type of some kind, or even something like
// `std::optional`.
//
// This provides proxy methods for read-only access to the object derived from
// ld::DecodedModule.  The ld::LoadModule object itself holds little more than
// the module name.  A mutable decoded module object can be modified via the
// `decoded()` accessor.
//
// TODO(https://fxbug.dev/326524302): Flesh out the indirect case with a
// read-only decoded().
template <typename DecodedStorage>
class LoadModule {
 private:
  static_assert(!std::is_const_v<DecodedStorage>);

  // Determine whether the template parameter is derived from an instantiation
  // of ld::DecodedModule<...>.  If not, it must be some pointer-like type.
  static constexpr bool kDecodedDirect = []() -> bool {
    if constexpr (std::is_class_v<DecodedStorage>) {
      return std::is_base_of_v<DecodedModuleBase, DecodedStorage>;
    }
    return false;
  }();

  // This is a static method so it can easily be called from both const and
  // non-const overloads of decoded(), below, where Storage will be const
  // DecodedStorage or DecodedStorage respectively.  It's also used to deduce
  // the underlying Decoded type, whether direct or pointed-to.
  template <class Storage>
  static constexpr auto& GetDecodedRef(Storage& storage) {
    if constexpr (kDecodedDirect) {
      return storage;
    } else {
      return *storage;
    }
  }

 public:
  // In the simple case, this will be the same as the template parameter.  But
  // if the template parameter is pointer-like, this is the pointed-to type,
  // which might be const.
  using Decoded = std::remove_reference_t<decltype(GetDecodedRef(std::declval<DecodedStorage&>()))>;

  // For convenience alias all the interesting types from Decoded.
  using Elf = typename Decoded::Elf;
  using Addr = typename Decoded::Addr;
  using size_type = typename Decoded::size_type;
  using Phdr = typename Decoded::Phdr;
  using Sym = typename Decoded::Sym;
  using Module = typename Decoded::Module;
  using TlsModule = typename Decoded::TlsModule;
  using LoadInfo = typename Decoded::LoadInfo;
  using RelocationInfo = typename Decoded::RelocationInfo;
  using Soname = typename Decoded::Soname;

  // This is returned by the name_ref() and soname_ref() methods.
  using Ref = LoadModuleRef<LoadModule>;

  constexpr LoadModule() = default;

  constexpr LoadModule(const LoadModule&) = delete;

  constexpr LoadModule(LoadModule&&) noexcept = default;

  // The LoadModule is initially constructed just with a name, which is what
  // will eventually appear in Module::link_map::name.  This is the name by
  // which the module is initially found (in the filesystem or whatever).
  // When the object has a DT_SONAME (Module::soname), this is usually the
  // same; but nothing guarantees that.
  constexpr explicit LoadModule(std::string_view name) : name_(name) {}
  constexpr explicit LoadModule(const Soname& name) : name_(name) {}

  constexpr LoadModule& operator=(LoadModule&&) noexcept = default;

  constexpr const Soname& name() const { return name_; }

  constexpr void set_name(const Soname& name) {
    name_ = name;
    SetAbiName();
  }
  constexpr void set_name(std::string_view name) {
    name_ = name;
    SetAbiName();
  }

  // If the template parameter is pointer-like, then HasDecoded() is initially
  // false and this must be used to install a pointer.
  constexpr void set_decoded(DecodedStorage decoded) { decoded_ = std::move(decoded); }

  // For convenient container searches, equality comparison against a (hashed)
  // name checks both name fields.  An unloaded module only has a load name.
  // A loaded module may also have a SONAME.
  constexpr bool operator==(const Soname& name) const {
    return name == name_ || (HasModule() && name == this->module().soname);
  }

  // This returns an object that can be used like a LoadModule* pointing at
  // this, but is suitable for use in a container like std::unordered_set or
  // fbl::HashTable keyed by name().
  Ref name_ref() const { return {this, &LoadModule::name}; }

  // This returns an object that can be used like a LoadModule* pointing at
  // this, but is suitable for use in a container like std::unordered_set or
  // fbl::HashTable keyed by soname().
  Ref soname_ref() const { return {this, &LoadModule::soname}; }

  // This returns the offset from the thread pointer to this module's static
  // TLS block if it has one.  The value is assigned by AssignStaticTls, below.
  // This method should not be called unless AssignStaticTls has been called.
  constexpr size_t static_tls_bias() const { return static_tls_bias_; }

  // Use ths TlsLayout object to assign a static TLS offset for this module's
  // PT_TLS segment, if it has one.  SetTls() has already been called if it
  // will be, so the module ID is known.  If the result is not std::nullopt,
  // then the Abi array fields at the returned index should be filled using
  // .tls_module() and .static_tls_bias().
  template <elfldltl::ElfMachine Machine = elfldltl::ElfMachine::kNative, size_type RedZone = 0>
  constexpr std::optional<size_type> AssignStaticTls(elfldltl::TlsLayout<Elf>& tls_layout) {
    if (!HasModule()) [[unlikely]] {
      return std::nullopt;
    }

    if (this->tls_module_id() == 0) {
      return std::nullopt;
    }

    // These correspond to the p_memsz and p_align of the PT_TLS.
    const size_type memsz = this->tls_module().tls_size();
    const size_type align = this->tls_module().tls_alignment;

    // Save the offset for use in resolving IE relocations.
    static_tls_bias_ = tls_layout.template Assign<Machine, RedZone>(memsz, align);

    return this->tls_module_id() - 1;
  }

  // This returns true if it's safe to call the decoded() method.  This is
  // always true if the template parameter is a DecodedModule type itself.
  // If it's instead some pointer-like type, then this can return false
  // before the pointer has been installed.
  // TODO(https://fxbug.dev/326524302): There is not yet any way to install it!
  constexpr bool HasDecoded() const {
    if constexpr (kDecodedDirect) {
      return true;
    } else {
      return static_cast<bool>(decoded_);
    }
  }

  // Access the underlying DecodedModule object.  This must not be called if
  // HasDecoded() returns false.  There are both const and mutable overloads
  // for this.  But note that Decoded might be a const type itself when the
  // template parameter is a pointer-like type, in which case decoded() always
  // returns a const reference even when called on a mutable LoadModule.
  constexpr Decoded& decoded() { return GetDecodedRef(decoded_); }
  constexpr const Decoded& decoded() const { return GetDecodedRef(decoded_); }

  // When Decoded is mutable and uses AbiModuleInline::kYes, EmplaceModule(..)
  // just constructs Module{...}).
  template <typename... Args, bool CanEmplace = Decoded::kModuleInline && !std::is_const_v<Decoded>,
            typename = std::enable_if_t<CanEmplace>>
  constexpr void EmplaceModule(uint32_t modid, Args&&... args) {
    decoded().EmplaceModule(modid, std::forward<Args>(args)...);
    SetAbiName();
  }

  // When Decoded is mutable and uses AbiModuleInline::kNo, then calling
  // NewModule(a..., c...)  does new (a...) Module{c...}.  The last argument in
  // a... must be a fbl::AllocChecker that indicates whether `new` succeeded.
  template <typename... Args, bool CanNew = !Decoded::kModuleInline && !std::is_const_v<Decoded>,
            typename = std::enable_if_t<CanNew>>
  constexpr void NewModule(uint32_t modid, Args&&... args) {
    decoded().NewModule(modid, std::forward<Args>(args)...);
    SetAbiName();
  }

  // The following methods are simple proxies for the same method on decoded().
  // Note only const overloads are provided here, for reading information from
  // the DecodedModule to use in loading and dynamic linking.  For filling in
  // the information from the file in the first place, use decoded() for the
  // mutable reference.

  constexpr bool HasModule() const { return HasDecoded() && decoded().HasModule(); }

  constexpr const Module& module() const { return decoded().module(); }

  constexpr const Soname& soname() const { return decoded().soname(); }

  constexpr const LoadInfo& load_info() const { return decoded().load_info(); }

  template <bool RI = Decoded::kRelocInfo, typename = std::enable_if_t<RI>>
  constexpr const RelocationInfo& reloc_info() const {
    return decoded().reloc_info();
  }

  constexpr size_type tls_module_id() const { return module().tls_modid; }

  constexpr const TlsModule& tls_module() const { return decoded().tls_module(); }

  // The following methods satisfy the Module template API for use with
  // elfldltl::ResolverDefinition (see <lib/elfldltl/resolve.h>).

  constexpr const auto& symbol_info() const { return module().symbols; }

  // This is only provided when Decoded is mutable.  In that case, after
  // decoding and choosing load address, module() will be updated via
  // ld::SetModuleVaddrBounds.  In other cases, a derived class must implement
  // load_bias() itself.
  template <bool Mutable = !std::is_const_v<Decoded>, typename = std::enable_if_t<Mutable>>
  constexpr size_type load_bias() const {
    return module().link_map.addr;
  }

  constexpr bool uses_static_tls() const {
    return (module().symbols.flags() & elfldltl::ElfDynFlags::kStaticTls) ||
           (module().symbols.flags1() & elfldltl::ElfDynFlags1::kPie);
  }

 protected:
  constexpr void SetAbiName() {
    if constexpr (!std::is_const_v<Decoded>) {
      if (HasDecoded()) {
        decoded().SetAbiName(name_);
      }
    }
  }

 private:
  DecodedStorage decoded_{};
  Soname name_;
  size_type static_tls_bias_ = 0;
};

// This object is returned by the name_ref() and soname_ref() methods of
// ld::LoadModule.  It's meant to be used in containers keyed by the module
// name.  Both name_ref() and soname_ref() of the same module should be
// inserted into the container so that it can be looked up by either name.
//
// The object can be used like LoadModule* or a similar smart pointer type.
// It also has methods for accessing the module's name (with hash), which is
// either LoadModule::name() or LoadModule::soname(); an operator== that takes
// a Soname (name with hash) object; and it supports both the std::hash and
// <fbl/intrusive_container_utils.h> APIs.  This makes it easy to use in
// containers like std::unordered_set or fbl::HashTable.
template <class LoadModule>
class LoadModuleRef {
 public:
  using Soname = typename LoadModule::Soname;

  constexpr LoadModuleRef() = default;

  constexpr LoadModuleRef(const LoadModuleRef&) = default;

  constexpr LoadModuleRef(LoadModule* module, const Soname& (LoadModule::*name)())
      : module_(module), name_(name) {}

  constexpr LoadModuleRef& operator=(const LoadModuleRef&) = default;

  constexpr LoadModule& operator*() const { return *module_; }

  constexpr LoadModule* operator->() const { return module_; }

  constexpr LoadModule* get() const { return module_; }

  constexpr const Soname& name() const { return module_->*name_(); }

  constexpr uint32_t hash() const { return name().hash(); }

  // This is the API contract used by fbl::HashTable.
  static const Soname& GetKey(const LoadModuleRef& ref) { return ref.name(); }
  static constexpr uint32_t GetHash(const LoadModuleRef& ref) { return ref.hash(); }

  constexpr bool operator==(const Soname& other_name) const { return name() == other_name; }

  constexpr bool operator!=(const Soname& other_name) const { return !(*this == other_name); }

  constexpr bool operator==(const LoadModuleRef& other) const {
    return module_ == other.module_ || (module_ && other.module_ && *this == other.name());
  }

  constexpr bool operator!=(const LoadModuleRef& other) const { return !(*this == other); }

  // While elfldltl::Soname is strictly ordered on the str() value, i.e. ASCII
  // string sorting, ld::LoadModuleRef is has a strong but arbitrary ordering.
  // That is, it's ordered first by hash value (which is precomputed), and
  // only ordered by string comparison among strings with the same hash value.
  //
  // This allows LoadModuleRef is to be used efficiently in either sorted or
  // hashed containers, but only for purposes of lookup, never for enumeration
  // where ordering should be useful.

#if __cpp_impl_three_way_comparison >= 201907L

  constexpr std::strong_ordering operator<=>(const LoadModuleRef& other) const {
    return Ordering<std::compare_three_way>(other);
  }

#else  // No operator<=>.

  constexpr bool operator<(const LoadModuleRef& other) const { return Ordering<std::less>(other); }

  constexpr bool operator<=(const LoadModuleRef& other) const {
    return Ordering<std::less_equal>(other);
  }

  constexpr bool operator>(const LoadModuleRef& other) const {
    return Ordering<std::greater>(other);
  }

  constexpr bool operator>=(const LoadModuleRef& other) const {
    return Ordering<std::greater_equal>(other);
  }

#endif  // operator<=>.

 private:
  template <class Compare>
  constexpr auto Ordering(const LoadModuleRef& other) const {
    return hash() == other.hash() ? Compare{}(name(), other.name())
                                  : Compare{}(hash(), other.hash());
  }

  LoadModule* module_ = nullptr;
  const Soname& (LoadModule::*name_)() = nullptr;
};

}  // namespace ld

// This is the API contract for standard C++ hash-based containers.
template <class LoadModule>
struct std::hash<ld::LoadModuleRef<LoadModule>> {
  constexpr uint32_t operator()(ld::LoadModuleRef<LoadModule> ref) const { return ref.hash(); }
};

#endif  // LIB_LD_LOAD_MODULE_H_
