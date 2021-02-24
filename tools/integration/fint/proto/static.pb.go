// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Code generated by protoc-gen-go. DO NOT EDIT.
// versions:
// 	protoc-gen-go v1.25.0
// 	protoc        v3.8.0
// source: static.proto

package proto

import (
	proto "github.com/golang/protobuf/proto"
	protoreflect "google.golang.org/protobuf/reflect/protoreflect"
	protoimpl "google.golang.org/protobuf/runtime/protoimpl"
	reflect "reflect"
	sync "sync"
)

const (
	// Verify that this generated code is sufficiently up-to-date.
	_ = protoimpl.EnforceVersion(20 - protoimpl.MinVersion)
	// Verify that runtime/protoimpl is sufficiently up-to-date.
	_ = protoimpl.EnforceVersion(protoimpl.MaxVersion - 20)
)

// This is a compile-time assertion that a sufficiently up-to-date version
// of the legacy proto package is being used.
const _ = proto.ProtoPackageIsVersion4

type Static_Optimize int32

const (
	// If new values are added to an enum, a client using an old version of the
	// protobuf definition will have all new values mapped to the enum's zero
	// value. So it's important that the zero value be "special" rather than a
	// regular value so the client can easily detect that something is wrong.
	// See http://go/totw-4 for more info.
	// TODO(olivernewman): Link to a public explanation if and when it becomes
	// available.
	Static_OPTIMIZE_UNSPECIFIED Static_Optimize = 0
	Static_DEBUG                Static_Optimize = 1
	Static_RELEASE              Static_Optimize = 2
)

// Enum value maps for Static_Optimize.
var (
	Static_Optimize_name = map[int32]string{
		0: "OPTIMIZE_UNSPECIFIED",
		1: "DEBUG",
		2: "RELEASE",
	}
	Static_Optimize_value = map[string]int32{
		"OPTIMIZE_UNSPECIFIED": 0,
		"DEBUG":                1,
		"RELEASE":              2,
	}
)

func (x Static_Optimize) Enum() *Static_Optimize {
	p := new(Static_Optimize)
	*p = x
	return p
}

func (x Static_Optimize) String() string {
	return protoimpl.X.EnumStringOf(x.Descriptor(), protoreflect.EnumNumber(x))
}

func (Static_Optimize) Descriptor() protoreflect.EnumDescriptor {
	return file_static_proto_enumTypes[0].Descriptor()
}

func (Static_Optimize) Type() protoreflect.EnumType {
	return &file_static_proto_enumTypes[0]
}

func (x Static_Optimize) Number() protoreflect.EnumNumber {
	return protoreflect.EnumNumber(x)
}

// Deprecated: Use Static_Optimize.Descriptor instead.
func (Static_Optimize) EnumDescriptor() ([]byte, []int) {
	return file_static_proto_rawDescGZIP(), []int{0, 0}
}

type Static_Arch int32

const (
	Static_ARCH_UNSPECIFIED Static_Arch = 0 // See OPTIMIZE_UNSPECIFIED for rationale.
	Static_ARM64            Static_Arch = 1
	Static_X64              Static_Arch = 2
)

// Enum value maps for Static_Arch.
var (
	Static_Arch_name = map[int32]string{
		0: "ARCH_UNSPECIFIED",
		1: "ARM64",
		2: "X64",
	}
	Static_Arch_value = map[string]int32{
		"ARCH_UNSPECIFIED": 0,
		"ARM64":            1,
		"X64":              2,
	}
)

func (x Static_Arch) Enum() *Static_Arch {
	p := new(Static_Arch)
	*p = x
	return p
}

func (x Static_Arch) String() string {
	return protoimpl.X.EnumStringOf(x.Descriptor(), protoreflect.EnumNumber(x))
}

func (Static_Arch) Descriptor() protoreflect.EnumDescriptor {
	return file_static_proto_enumTypes[1].Descriptor()
}

func (Static_Arch) Type() protoreflect.EnumType {
	return &file_static_proto_enumTypes[1]
}

func (x Static_Arch) Number() protoreflect.EnumNumber {
	return protoreflect.EnumNumber(x)
}

// Deprecated: Use Static_Arch.Descriptor instead.
func (Static_Arch) EnumDescriptor() ([]byte, []int) {
	return file_static_proto_rawDescGZIP(), []int{0, 1}
}

// Static contains all of the non-dynamic configuration values for building
// Fuchsia. These values are "static" in the sense that they don't vary
// depending on things like git history or local environment, so they can be
// checked into version control.
type Static struct {
	state         protoimpl.MessageState
	sizeCache     protoimpl.SizeCache
	unknownFields protoimpl.UnknownFields

	// The optimization level for the build.
	Optimize Static_Optimize `protobuf:"varint,1,opt,name=optimize,proto3,enum=fint.Static_Optimize" json:"optimize,omitempty"`
	// The board to build.
	Board string `protobuf:"bytes,2,opt,name=board,proto3" json:"board,omitempty"`
	// The product file to build.
	Product string `protobuf:"bytes,3,opt,name=product,proto3" json:"product,omitempty"`
	// Whether to skip building the basic images needed to boot and test on
	// Fuchsia.
	ExcludeImages bool `protobuf:"varint,4,opt,name=exclude_images,json=excludeImages,proto3" json:"exclude_images,omitempty"`
	// Extra args to pass to gn gen.
	GnArgs []string `protobuf:"bytes,5,rep,name=gn_args,json=gnArgs,proto3" json:"gn_args,omitempty"`
	// Extra targets to pass to Ninja.
	NinjaTargets []string `protobuf:"bytes,6,rep,name=ninja_targets,json=ninjaTargets,proto3" json:"ninja_targets,omitempty"`
	// Fuchsia packages to build and include in the base set.
	BasePackages []string `protobuf:"bytes,7,rep,name=base_packages,json=basePackages,proto3" json:"base_packages,omitempty"`
	// Fuchsia packages to build and include in the cache set.
	CachePackages []string `protobuf:"bytes,8,rep,name=cache_packages,json=cachePackages,proto3" json:"cache_packages,omitempty"`
	// Fuchsia packages to build and include in the universe set.
	UniversePackages []string `protobuf:"bytes,9,rep,name=universe_packages,json=universePackages,proto3" json:"universe_packages,omitempty"`
	// Whether to build host tests.
	IncludeHostTests bool `protobuf:"varint,10,opt,name=include_host_tests,json=includeHostTests,proto3" json:"include_host_tests,omitempty"`
	// The target CPU architecture.
	TargetArch Static_Arch `protobuf:"varint,11,opt,name=target_arch,json=targetArch,proto3,enum=fint.Static_Arch" json:"target_arch,omitempty"`
	// Values of select_variant GN argument.
	Variants []string `protobuf:"bytes,12,rep,name=variants,proto3" json:"variants,omitempty"`
	// Whether to include archives in the build.
	IncludeArchives bool `protobuf:"varint,13,opt,name=include_archives,json=includeArchives,proto3" json:"include_archives,omitempty"`
	// Whether to skip the ninja build if we're running in CQ and none of the
	// changed files affect the build.
	SkipIfUnaffected bool `protobuf:"varint,14,opt,name=skip_if_unaffected,json=skipIfUnaffected,proto3" json:"skip_if_unaffected,omitempty"`
	// The path within the checkout of a file containing historical test duration
	// data specific to the current build config.
	TestDurationsFile string `protobuf:"bytes,15,opt,name=test_durations_file,json=testDurationsFile,proto3" json:"test_durations_file,omitempty"`
	// If `test_durations_file` doesn't exist within the checkout, use this file
	// instead. It's not specific to the current build config, but it can be
	// assumed to always exist.
	DefaultTestDurationsFile string `protobuf:"bytes,16,opt,name=default_test_durations_file,json=defaultTestDurationsFile,proto3" json:"default_test_durations_file,omitempty"`
	// Whether to use goma for running ninja. Will be ignored (and goma will not
	// be used) when building with some experimental toolchain versions.
	UseGoma bool `protobuf:"varint,17,opt,name=use_goma,json=useGoma,proto3" json:"use_goma,omitempty"`
	// Whether to generate a listing of the commands run during the build.
	GenerateCompdb bool `protobuf:"varint,18,opt,name=generate_compdb,json=generateCompdb,proto3" json:"generate_compdb,omitempty"`
	// Host-only targets to build.
	HostLabels []string `protobuf:"bytes,21,rep,name=host_labels,json=hostLabels,proto3" json:"host_labels,omitempty"`
	// Whether to use a go cache when building.
	EnableGoCache bool `protobuf:"varint,22,opt,name=enable_go_cache,json=enableGoCache,proto3" json:"enable_go_cache,omitempty"`
	// Whether to use a rust cache when building.
	EnableRustCache bool `protobuf:"varint,23,opt,name=enable_rust_cache,json=enableRustCache,proto3" json:"enable_rust_cache,omitempty"`
	// Which IDE files to generate.
	IdeFiles []string `protobuf:"bytes,24,rep,name=ide_files,json=ideFiles,proto3" json:"ide_files,omitempty"`
	// Passed to --json-ide-script GN flag; GN will execute each of these scripts
	// after regenerating the project.json IDE file.
	JsonIdeScripts []string `protobuf:"bytes,25,rep,name=json_ide_scripts,json=jsonIdeScripts,proto3" json:"json_ide_scripts,omitempty"`
}

func (x *Static) Reset() {
	*x = Static{}
	if protoimpl.UnsafeEnabled {
		mi := &file_static_proto_msgTypes[0]
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		ms.StoreMessageInfo(mi)
	}
}

func (x *Static) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*Static) ProtoMessage() {}

func (x *Static) ProtoReflect() protoreflect.Message {
	mi := &file_static_proto_msgTypes[0]
	if protoimpl.UnsafeEnabled && x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use Static.ProtoReflect.Descriptor instead.
func (*Static) Descriptor() ([]byte, []int) {
	return file_static_proto_rawDescGZIP(), []int{0}
}

func (x *Static) GetOptimize() Static_Optimize {
	if x != nil {
		return x.Optimize
	}
	return Static_OPTIMIZE_UNSPECIFIED
}

func (x *Static) GetBoard() string {
	if x != nil {
		return x.Board
	}
	return ""
}

func (x *Static) GetProduct() string {
	if x != nil {
		return x.Product
	}
	return ""
}

func (x *Static) GetExcludeImages() bool {
	if x != nil {
		return x.ExcludeImages
	}
	return false
}

func (x *Static) GetGnArgs() []string {
	if x != nil {
		return x.GnArgs
	}
	return nil
}

func (x *Static) GetNinjaTargets() []string {
	if x != nil {
		return x.NinjaTargets
	}
	return nil
}

func (x *Static) GetBasePackages() []string {
	if x != nil {
		return x.BasePackages
	}
	return nil
}

func (x *Static) GetCachePackages() []string {
	if x != nil {
		return x.CachePackages
	}
	return nil
}

func (x *Static) GetUniversePackages() []string {
	if x != nil {
		return x.UniversePackages
	}
	return nil
}

func (x *Static) GetIncludeHostTests() bool {
	if x != nil {
		return x.IncludeHostTests
	}
	return false
}

func (x *Static) GetTargetArch() Static_Arch {
	if x != nil {
		return x.TargetArch
	}
	return Static_ARCH_UNSPECIFIED
}

func (x *Static) GetVariants() []string {
	if x != nil {
		return x.Variants
	}
	return nil
}

func (x *Static) GetIncludeArchives() bool {
	if x != nil {
		return x.IncludeArchives
	}
	return false
}

func (x *Static) GetSkipIfUnaffected() bool {
	if x != nil {
		return x.SkipIfUnaffected
	}
	return false
}

func (x *Static) GetTestDurationsFile() string {
	if x != nil {
		return x.TestDurationsFile
	}
	return ""
}

func (x *Static) GetDefaultTestDurationsFile() string {
	if x != nil {
		return x.DefaultTestDurationsFile
	}
	return ""
}

func (x *Static) GetUseGoma() bool {
	if x != nil {
		return x.UseGoma
	}
	return false
}

func (x *Static) GetGenerateCompdb() bool {
	if x != nil {
		return x.GenerateCompdb
	}
	return false
}

func (x *Static) GetHostLabels() []string {
	if x != nil {
		return x.HostLabels
	}
	return nil
}

func (x *Static) GetEnableGoCache() bool {
	if x != nil {
		return x.EnableGoCache
	}
	return false
}

func (x *Static) GetEnableRustCache() bool {
	if x != nil {
		return x.EnableRustCache
	}
	return false
}

func (x *Static) GetIdeFiles() []string {
	if x != nil {
		return x.IdeFiles
	}
	return nil
}

func (x *Static) GetJsonIdeScripts() []string {
	if x != nil {
		return x.JsonIdeScripts
	}
	return nil
}

var File_static_proto protoreflect.FileDescriptor

var file_static_proto_rawDesc = []byte{
	0x0a, 0x0c, 0x73, 0x74, 0x61, 0x74, 0x69, 0x63, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x12, 0x04,
	0x66, 0x69, 0x6e, 0x74, 0x22, 0xff, 0x07, 0x0a, 0x06, 0x53, 0x74, 0x61, 0x74, 0x69, 0x63, 0x12,
	0x31, 0x0a, 0x08, 0x6f, 0x70, 0x74, 0x69, 0x6d, 0x69, 0x7a, 0x65, 0x18, 0x01, 0x20, 0x01, 0x28,
	0x0e, 0x32, 0x15, 0x2e, 0x66, 0x69, 0x6e, 0x74, 0x2e, 0x53, 0x74, 0x61, 0x74, 0x69, 0x63, 0x2e,
	0x4f, 0x70, 0x74, 0x69, 0x6d, 0x69, 0x7a, 0x65, 0x52, 0x08, 0x6f, 0x70, 0x74, 0x69, 0x6d, 0x69,
	0x7a, 0x65, 0x12, 0x14, 0x0a, 0x05, 0x62, 0x6f, 0x61, 0x72, 0x64, 0x18, 0x02, 0x20, 0x01, 0x28,
	0x09, 0x52, 0x05, 0x62, 0x6f, 0x61, 0x72, 0x64, 0x12, 0x18, 0x0a, 0x07, 0x70, 0x72, 0x6f, 0x64,
	0x75, 0x63, 0x74, 0x18, 0x03, 0x20, 0x01, 0x28, 0x09, 0x52, 0x07, 0x70, 0x72, 0x6f, 0x64, 0x75,
	0x63, 0x74, 0x12, 0x25, 0x0a, 0x0e, 0x65, 0x78, 0x63, 0x6c, 0x75, 0x64, 0x65, 0x5f, 0x69, 0x6d,
	0x61, 0x67, 0x65, 0x73, 0x18, 0x04, 0x20, 0x01, 0x28, 0x08, 0x52, 0x0d, 0x65, 0x78, 0x63, 0x6c,
	0x75, 0x64, 0x65, 0x49, 0x6d, 0x61, 0x67, 0x65, 0x73, 0x12, 0x17, 0x0a, 0x07, 0x67, 0x6e, 0x5f,
	0x61, 0x72, 0x67, 0x73, 0x18, 0x05, 0x20, 0x03, 0x28, 0x09, 0x52, 0x06, 0x67, 0x6e, 0x41, 0x72,
	0x67, 0x73, 0x12, 0x23, 0x0a, 0x0d, 0x6e, 0x69, 0x6e, 0x6a, 0x61, 0x5f, 0x74, 0x61, 0x72, 0x67,
	0x65, 0x74, 0x73, 0x18, 0x06, 0x20, 0x03, 0x28, 0x09, 0x52, 0x0c, 0x6e, 0x69, 0x6e, 0x6a, 0x61,
	0x54, 0x61, 0x72, 0x67, 0x65, 0x74, 0x73, 0x12, 0x23, 0x0a, 0x0d, 0x62, 0x61, 0x73, 0x65, 0x5f,
	0x70, 0x61, 0x63, 0x6b, 0x61, 0x67, 0x65, 0x73, 0x18, 0x07, 0x20, 0x03, 0x28, 0x09, 0x52, 0x0c,
	0x62, 0x61, 0x73, 0x65, 0x50, 0x61, 0x63, 0x6b, 0x61, 0x67, 0x65, 0x73, 0x12, 0x25, 0x0a, 0x0e,
	0x63, 0x61, 0x63, 0x68, 0x65, 0x5f, 0x70, 0x61, 0x63, 0x6b, 0x61, 0x67, 0x65, 0x73, 0x18, 0x08,
	0x20, 0x03, 0x28, 0x09, 0x52, 0x0d, 0x63, 0x61, 0x63, 0x68, 0x65, 0x50, 0x61, 0x63, 0x6b, 0x61,
	0x67, 0x65, 0x73, 0x12, 0x2b, 0x0a, 0x11, 0x75, 0x6e, 0x69, 0x76, 0x65, 0x72, 0x73, 0x65, 0x5f,
	0x70, 0x61, 0x63, 0x6b, 0x61, 0x67, 0x65, 0x73, 0x18, 0x09, 0x20, 0x03, 0x28, 0x09, 0x52, 0x10,
	0x75, 0x6e, 0x69, 0x76, 0x65, 0x72, 0x73, 0x65, 0x50, 0x61, 0x63, 0x6b, 0x61, 0x67, 0x65, 0x73,
	0x12, 0x2c, 0x0a, 0x12, 0x69, 0x6e, 0x63, 0x6c, 0x75, 0x64, 0x65, 0x5f, 0x68, 0x6f, 0x73, 0x74,
	0x5f, 0x74, 0x65, 0x73, 0x74, 0x73, 0x18, 0x0a, 0x20, 0x01, 0x28, 0x08, 0x52, 0x10, 0x69, 0x6e,
	0x63, 0x6c, 0x75, 0x64, 0x65, 0x48, 0x6f, 0x73, 0x74, 0x54, 0x65, 0x73, 0x74, 0x73, 0x12, 0x32,
	0x0a, 0x0b, 0x74, 0x61, 0x72, 0x67, 0x65, 0x74, 0x5f, 0x61, 0x72, 0x63, 0x68, 0x18, 0x0b, 0x20,
	0x01, 0x28, 0x0e, 0x32, 0x11, 0x2e, 0x66, 0x69, 0x6e, 0x74, 0x2e, 0x53, 0x74, 0x61, 0x74, 0x69,
	0x63, 0x2e, 0x41, 0x72, 0x63, 0x68, 0x52, 0x0a, 0x74, 0x61, 0x72, 0x67, 0x65, 0x74, 0x41, 0x72,
	0x63, 0x68, 0x12, 0x1a, 0x0a, 0x08, 0x76, 0x61, 0x72, 0x69, 0x61, 0x6e, 0x74, 0x73, 0x18, 0x0c,
	0x20, 0x03, 0x28, 0x09, 0x52, 0x08, 0x76, 0x61, 0x72, 0x69, 0x61, 0x6e, 0x74, 0x73, 0x12, 0x29,
	0x0a, 0x10, 0x69, 0x6e, 0x63, 0x6c, 0x75, 0x64, 0x65, 0x5f, 0x61, 0x72, 0x63, 0x68, 0x69, 0x76,
	0x65, 0x73, 0x18, 0x0d, 0x20, 0x01, 0x28, 0x08, 0x52, 0x0f, 0x69, 0x6e, 0x63, 0x6c, 0x75, 0x64,
	0x65, 0x41, 0x72, 0x63, 0x68, 0x69, 0x76, 0x65, 0x73, 0x12, 0x2c, 0x0a, 0x12, 0x73, 0x6b, 0x69,
	0x70, 0x5f, 0x69, 0x66, 0x5f, 0x75, 0x6e, 0x61, 0x66, 0x66, 0x65, 0x63, 0x74, 0x65, 0x64, 0x18,
	0x0e, 0x20, 0x01, 0x28, 0x08, 0x52, 0x10, 0x73, 0x6b, 0x69, 0x70, 0x49, 0x66, 0x55, 0x6e, 0x61,
	0x66, 0x66, 0x65, 0x63, 0x74, 0x65, 0x64, 0x12, 0x2e, 0x0a, 0x13, 0x74, 0x65, 0x73, 0x74, 0x5f,
	0x64, 0x75, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x73, 0x5f, 0x66, 0x69, 0x6c, 0x65, 0x18, 0x0f,
	0x20, 0x01, 0x28, 0x09, 0x52, 0x11, 0x74, 0x65, 0x73, 0x74, 0x44, 0x75, 0x72, 0x61, 0x74, 0x69,
	0x6f, 0x6e, 0x73, 0x46, 0x69, 0x6c, 0x65, 0x12, 0x3d, 0x0a, 0x1b, 0x64, 0x65, 0x66, 0x61, 0x75,
	0x6c, 0x74, 0x5f, 0x74, 0x65, 0x73, 0x74, 0x5f, 0x64, 0x75, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e,
	0x73, 0x5f, 0x66, 0x69, 0x6c, 0x65, 0x18, 0x10, 0x20, 0x01, 0x28, 0x09, 0x52, 0x18, 0x64, 0x65,
	0x66, 0x61, 0x75, 0x6c, 0x74, 0x54, 0x65, 0x73, 0x74, 0x44, 0x75, 0x72, 0x61, 0x74, 0x69, 0x6f,
	0x6e, 0x73, 0x46, 0x69, 0x6c, 0x65, 0x12, 0x19, 0x0a, 0x08, 0x75, 0x73, 0x65, 0x5f, 0x67, 0x6f,
	0x6d, 0x61, 0x18, 0x11, 0x20, 0x01, 0x28, 0x08, 0x52, 0x07, 0x75, 0x73, 0x65, 0x47, 0x6f, 0x6d,
	0x61, 0x12, 0x27, 0x0a, 0x0f, 0x67, 0x65, 0x6e, 0x65, 0x72, 0x61, 0x74, 0x65, 0x5f, 0x63, 0x6f,
	0x6d, 0x70, 0x64, 0x62, 0x18, 0x12, 0x20, 0x01, 0x28, 0x08, 0x52, 0x0e, 0x67, 0x65, 0x6e, 0x65,
	0x72, 0x61, 0x74, 0x65, 0x43, 0x6f, 0x6d, 0x70, 0x64, 0x62, 0x12, 0x1f, 0x0a, 0x0b, 0x68, 0x6f,
	0x73, 0x74, 0x5f, 0x6c, 0x61, 0x62, 0x65, 0x6c, 0x73, 0x18, 0x15, 0x20, 0x03, 0x28, 0x09, 0x52,
	0x0a, 0x68, 0x6f, 0x73, 0x74, 0x4c, 0x61, 0x62, 0x65, 0x6c, 0x73, 0x12, 0x26, 0x0a, 0x0f, 0x65,
	0x6e, 0x61, 0x62, 0x6c, 0x65, 0x5f, 0x67, 0x6f, 0x5f, 0x63, 0x61, 0x63, 0x68, 0x65, 0x18, 0x16,
	0x20, 0x01, 0x28, 0x08, 0x52, 0x0d, 0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x47, 0x6f, 0x43, 0x61,
	0x63, 0x68, 0x65, 0x12, 0x2a, 0x0a, 0x11, 0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x5f, 0x72, 0x75,
	0x73, 0x74, 0x5f, 0x63, 0x61, 0x63, 0x68, 0x65, 0x18, 0x17, 0x20, 0x01, 0x28, 0x08, 0x52, 0x0f,
	0x65, 0x6e, 0x61, 0x62, 0x6c, 0x65, 0x52, 0x75, 0x73, 0x74, 0x43, 0x61, 0x63, 0x68, 0x65, 0x12,
	0x1b, 0x0a, 0x09, 0x69, 0x64, 0x65, 0x5f, 0x66, 0x69, 0x6c, 0x65, 0x73, 0x18, 0x18, 0x20, 0x03,
	0x28, 0x09, 0x52, 0x08, 0x69, 0x64, 0x65, 0x46, 0x69, 0x6c, 0x65, 0x73, 0x12, 0x28, 0x0a, 0x10,
	0x6a, 0x73, 0x6f, 0x6e, 0x5f, 0x69, 0x64, 0x65, 0x5f, 0x73, 0x63, 0x72, 0x69, 0x70, 0x74, 0x73,
	0x18, 0x19, 0x20, 0x03, 0x28, 0x09, 0x52, 0x0e, 0x6a, 0x73, 0x6f, 0x6e, 0x49, 0x64, 0x65, 0x53,
	0x63, 0x72, 0x69, 0x70, 0x74, 0x73, 0x22, 0x3c, 0x0a, 0x08, 0x4f, 0x70, 0x74, 0x69, 0x6d, 0x69,
	0x7a, 0x65, 0x12, 0x18, 0x0a, 0x14, 0x4f, 0x50, 0x54, 0x49, 0x4d, 0x49, 0x5a, 0x45, 0x5f, 0x55,
	0x4e, 0x53, 0x50, 0x45, 0x43, 0x49, 0x46, 0x49, 0x45, 0x44, 0x10, 0x00, 0x12, 0x09, 0x0a, 0x05,
	0x44, 0x45, 0x42, 0x55, 0x47, 0x10, 0x01, 0x12, 0x0b, 0x0a, 0x07, 0x52, 0x45, 0x4c, 0x45, 0x41,
	0x53, 0x45, 0x10, 0x02, 0x22, 0x30, 0x0a, 0x04, 0x41, 0x72, 0x63, 0x68, 0x12, 0x14, 0x0a, 0x10,
	0x41, 0x52, 0x43, 0x48, 0x5f, 0x55, 0x4e, 0x53, 0x50, 0x45, 0x43, 0x49, 0x46, 0x49, 0x45, 0x44,
	0x10, 0x00, 0x12, 0x09, 0x0a, 0x05, 0x41, 0x52, 0x4d, 0x36, 0x34, 0x10, 0x01, 0x12, 0x07, 0x0a,
	0x03, 0x58, 0x36, 0x34, 0x10, 0x02, 0x42, 0x35, 0x5a, 0x33, 0x67, 0x6f, 0x2e, 0x66, 0x75, 0x63,
	0x68, 0x73, 0x69, 0x61, 0x2e, 0x64, 0x65, 0x76, 0x2f, 0x66, 0x75, 0x63, 0x68, 0x73, 0x69, 0x61,
	0x2f, 0x74, 0x6f, 0x6f, 0x6c, 0x73, 0x2f, 0x69, 0x6e, 0x74, 0x65, 0x67, 0x72, 0x61, 0x74, 0x69,
	0x6f, 0x6e, 0x2f, 0x66, 0x69, 0x6e, 0x74, 0x2f, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x62, 0x06, 0x70,
	0x72, 0x6f, 0x74, 0x6f, 0x33,
}

var (
	file_static_proto_rawDescOnce sync.Once
	file_static_proto_rawDescData = file_static_proto_rawDesc
)

func file_static_proto_rawDescGZIP() []byte {
	file_static_proto_rawDescOnce.Do(func() {
		file_static_proto_rawDescData = protoimpl.X.CompressGZIP(file_static_proto_rawDescData)
	})
	return file_static_proto_rawDescData
}

var file_static_proto_enumTypes = make([]protoimpl.EnumInfo, 2)
var file_static_proto_msgTypes = make([]protoimpl.MessageInfo, 1)
var file_static_proto_goTypes = []interface{}{
	(Static_Optimize)(0), // 0: fint.Static.Optimize
	(Static_Arch)(0),     // 1: fint.Static.Arch
	(*Static)(nil),       // 2: fint.Static
}
var file_static_proto_depIdxs = []int32{
	0, // 0: fint.Static.optimize:type_name -> fint.Static.Optimize
	1, // 1: fint.Static.target_arch:type_name -> fint.Static.Arch
	2, // [2:2] is the sub-list for method output_type
	2, // [2:2] is the sub-list for method input_type
	2, // [2:2] is the sub-list for extension type_name
	2, // [2:2] is the sub-list for extension extendee
	0, // [0:2] is the sub-list for field type_name
}

func init() { file_static_proto_init() }
func file_static_proto_init() {
	if File_static_proto != nil {
		return
	}
	if !protoimpl.UnsafeEnabled {
		file_static_proto_msgTypes[0].Exporter = func(v interface{}, i int) interface{} {
			switch v := v.(*Static); i {
			case 0:
				return &v.state
			case 1:
				return &v.sizeCache
			case 2:
				return &v.unknownFields
			default:
				return nil
			}
		}
	}
	type x struct{}
	out := protoimpl.TypeBuilder{
		File: protoimpl.DescBuilder{
			GoPackagePath: reflect.TypeOf(x{}).PkgPath(),
			RawDescriptor: file_static_proto_rawDesc,
			NumEnums:      2,
			NumMessages:   1,
			NumExtensions: 0,
			NumServices:   0,
		},
		GoTypes:           file_static_proto_goTypes,
		DependencyIndexes: file_static_proto_depIdxs,
		EnumInfos:         file_static_proto_enumTypes,
		MessageInfos:      file_static_proto_msgTypes,
	}.Build()
	File_static_proto = out.File
	file_static_proto_rawDesc = nil
	file_static_proto_goTypes = nil
	file_static_proto_depIdxs = nil
}
