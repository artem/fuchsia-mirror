// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"fmt"
	"math"
	"sort"
	"strconv"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

const (
	ProxySuffix            = "Interface"
	StubSuffix             = "Stub"
	EventProxySuffix       = "EventProxy"
	TransitionalBaseSuffix = "TransitionalBase"
	ServiceNameSuffix      = "Name"
	RequestSuffix          = "InterfaceRequest"
	TagSuffix              = "Tag"
	WithCtxSuffix          = "WithCtx"

	SyscallZxPackage = "syscall/zx"
	SyscallZxAlias   = "_zx"

	BindingsPackage = "syscall/zx/fidl"
	BindingsAlias   = "_bindings"

	StringsPackage = "strings"
	StringsAlias   = "_strings"
)

// Type represents a golang type.
type Type string

// Const represents the idiomatic representation of a constant in golang.
type Const struct {
	fidlgen.Attributes

	// Name is the name of the constant.
	Name string

	// Type is the constant's type.
	Type Type

	// Value is the constant's value.
	Value string
}

// Bits represents the idiomatic representation of an bits in golang.
//
// That is, something like:
//
//	type MyBits int32
//	const (
//		MyBitsMember1 MyBits = 1
//		MyBitsMember2        = 4
//		...
//	)
type Bits struct {
	fidlgen.Bits

	// Name is the name of the bits type alias.
	Name string

	// Type is the underlying primitive type for the bits.
	Type Type

	// Members is the list of bits variants that are a part of this bits.
	// The values of the Members must not overlap.
	Members []BitsMember
}

// BitsMember represents a single bits variant. See Bits for more details.
type BitsMember struct {
	fidlgen.Attributes

	// Name is the name of the bits variant without any prefix.
	Name string

	// Value is the raw value of the bits variant, represented as a string
	// to support many types.
	Value string
}

// Enum represents the idiomatic representation of an enum in golang.
//
// That is, something like:
// type MyEnum int32
// const (
//
//	MyEnumMember1 MyEnum = 1
//	MyEnumMember2        = 2
//	...
//
// )
type Enum struct {
	fidlgen.Enum

	// Name is the name of the enum type alias.
	Name string

	// Type is the underlying primitive type for the enum.
	Type Type

	// Members is the list of enum variants that are a part of this enum.
	// The values of the Members must not overlap.
	Members []EnumMember
}

// EnumMember represents a single enum variant. See Enum for more details.
type EnumMember struct {
	fidlgen.EnumMember

	// Name is the name of the enum variant without any prefix.
	Name string

	// Value is the raw value of the enum variant, represented as a string
	// to support many types.
	Value string
}

// Struct represents a golang struct.
type Struct struct {
	fidlgen.Struct
	Name string

	// Members is a list of the golang struct members.
	Members []StructMember

	Tags Tags
}

// StackOfBoundsTag corresponds to the original "fidl" tag.
type StackOfBoundsTag struct {
	reverseOfBounds []int
}

// String generates a string representation for the tag.
func (t StackOfBoundsTag) String() string {
	elems := make([]string, 0, len(t.reverseOfBounds))
	allEmpty := true
	for i := len(t.reverseOfBounds) - 1; 0 <= i; i-- {
		bound := t.reverseOfBounds[i]
		if bound == math.MaxInt32 {
			elems = append(elems, "")
		} else {
			elems = append(elems, strconv.Itoa(bound))
			allEmpty = false
		}
	}
	if allEmpty {
		return ""
	}
	return strings.Join(elems, ",")
}

func (t StackOfBoundsTag) IsEmpty() bool {
	return len(t.reverseOfBounds) == 0
}

// Tag represents a go tag in the generated code.
type Tag int32

const (
	_       Tag = iota
	FidlTag     // "fidl" tag with value from StackOfBoundsTag
	FidlSizeV2Tag
	FidlOffsetV2Tag
	FidlAlignmentV2Tag
	FidlHandleSubtypeTag
	FidlHandleRightsTag
	FidlBoundsTag
	FidlOrdinalTag
	FidlIsResourceTag
	EndTag   // This value must be last in the list to allow iteration over all tags.
	StartTag = FidlTag
)

func (t Tag) String() string {
	switch t {
	case FidlTag:
		return "fidl"
	case FidlSizeV2Tag:
		return "fidl_size_v2"
	case FidlOffsetV2Tag:
		return "fidl_offset_v2"
	case FidlAlignmentV2Tag:
		return "fidl_alignment_v2"
	case FidlHandleSubtypeTag:
		return "fidl_handle_subtype"
	case FidlHandleRightsTag:
		return "fidl_handle_rights"
	case FidlBoundsTag:
		return "fidl_bounds"
	case FidlOrdinalTag:
		return "fidl_ordinal"
	case FidlIsResourceTag:
		return "fidl_resource"
	}
	panic("unknown tag")
}

// Tags is a collection containing the tag definitions.
type Tags map[Tag]interface{}

func (t Tags) String() string {
	var tagPairs []string
	for tag := StartTag; tag < EndTag; tag++ {
		if val, ok := t[tag]; ok {
			tagPairs = append(tagPairs, fmt.Sprintf(`%s:"%v"`, tag, val))
		}
	}
	return strings.Join(tagPairs, " ")
}

// StructMember represents the member of a golang struct.
type StructMember struct {
	fidlgen.Attributes

	// Name is the name of the golang struct member.
	Name string

	// PrivateName is the unexported version of the name of the struct member.
	PrivateName string

	// Type is the type of the golang struct member.
	Type Type

	// Corresponds to fidl tag in generated go.
	Tags Tags
}

type Union struct {
	fidlgen.Union
	Name    string
	TagName string
	Members []UnionMember
	Tags    Tags
	fidlgen.Strictness
	UnknownDataType string
}

func (u *Union) setTagName(c *compiler, protocol fidlgen.EncodedCompoundIdentifier, ext string) {
	u.TagName = "I_" + c.compileCompoundIdentifier(protocol, false, ext+TagSuffix)
}

type UnionMember struct {
	fidlgen.Attributes
	Ordinal     uint64
	Name        string
	PrivateName string
	Type        Type
	Tags        Tags
}

// Table represents a FIDL table as a golang struct.
type Table struct {
	fidlgen.Table
	Name            string
	Members         []TableMember
	Tags            Tags
	UnknownDataType string
}

// TableMember represents a FIDL table member as two golang struct members, one
// for the member itself, and one to indicate presence or absence.
type TableMember struct {
	fidlgen.Attributes

	// DataField is the exported name of the FIDL table member.
	DataField string

	// PrivateDataField is an unexported name of the FIDL table member, used as
	// argument.
	PrivateDataField string

	// PresenceField is the exported name of boolean indicating presence of
	// the FIDL table member.
	PresenceField string

	// Setter is the exported name of the FIDL table member setter.
	Setter string

	// Getter is the exported name of the FIDL table member getter.
	Getter string

	// GetterWithDefault is the exported name of the FIDL table member getter
	// with a default value.
	GetterWithDefault string

	// Clearer is the exported name of the FIDL table member clearer.
	Clearer string

	// Haser is the exported name of the presence checker of the FIDL table
	// member.
	Haser string

	// Type is the golang type of the table member.
	Type Type

	// Corresponds to fidl: tag in generated go.
	Tags Tags
}

// Protocol represents a FIDL protocol in terms of golang structures.
type Protocol struct {
	fidlgen.Attributes

	// Name is the Golang name of the protocol.
	Name string

	// Openness of this protocol.
	Openness fidlgen.Openness

	// ProxyName is the name of the proxy type for this FIDL protocol.
	ProxyName string

	// ProxyType is concrete type of proxy used for this FIDL protocol.
	ProxyType string

	// StubName is the name of the stub type for this FIDL protocol.
	StubName string

	// EventProxyName is the name of the event proxy type for this FIDL protocol.
	EventProxyName string

	// TransitionalBaseName is the name of the base implementation for transitional methods
	// for this FIDL protocol.
	TransitionalBaseName string

	// RequestName is the name of the protocol request type for this FIDL protocol.
	RequestName string

	// ProtocolNameString is the string service name for this FIDL protocol.
	ProtocolNameString string

	// ProtocolNameConstant is the name of the service name constant for this FIDL protocol.
	ProtocolNameConstant string

	// Methods is a list of methods for this FIDL protocol.
	Methods []Method
}

// Method represents a method of a FIDL protocol in terms of golang structures.
type Method struct {
	fidlgen.Method

	// Name of the method.
	Name string

	// Name for the top-level ordinal constant.
	OrdinalName string

	// Request represents a golang interface exposing the request parameters.
	Request *Payload

	// Response represents an optional golang interface exposing the response parameters.
	Response *Payload

	// EventExpectName is the name of the method for the client-side event proxy.
	// Only relevant if the method is an event.
	EventExpectName string

	// IsEvent is set to true if the method is an event. In this case, Response will always be
	// non-nil while Request will always be nil. EventExpectName will also be non-empty.
	IsEvent bool

	// IsTransitional is set to true if the method has the Transitional attribute.
	IsTransitional bool
}

// Payload is a method request or response. It can be a struct, table, or union.
// Structs get flattened, meaning each field is a separate parameter.
// Tables and unions are treated as a single parameter called "payload".
type Payload struct {
	// Name of the struct, table, or union.
	Name string
	// Comma-separated parameter names.
	Names string
	// Comma-separated parameter types.
	Types string
	// Comma-separated parameter names and types.
	NamesAndTypes string
	// Assuming the parameters are in scope, constructs the payload object.
	Construct string
	// Given a payload object, produces comma-separated parameters.
	Destructure func(variable string) string
}

// Library represents a FIDL library as a golang package.
type Library struct {
	// Alias is the alias of the golang package referring to a FIDL library.
	Alias string

	// Path is the path to the golang package referring to a FIDL library.
	Path string
}

// Root is the root of the golang backend IR structure.
//
// The golang backend IR structure is loosely modeled after an abstract syntax
// tree, and is used to generate golang code from templates.
type Root struct {
	// Experiments that have been enabled upstream in fidlc.
	Experiments fidlgen.Experiments

	// Name is the name of the library.
	Name string

	// PackageName is the name of the golang package as other Go programs would
	// import it.
	PackageName string

	// BindingsAlias is the alias name of the golang package of the FIDL
	// bindings.
	BindingsAlias string

	// Bits represents a list of FIDL bits represented as Go bits.
	Bits []Bits

	// Consts represents a list of FIDL constants represented as Go constants.
	Consts []Const

	// Enums represents a list of FIDL enums represented as Go enums.
	Enums []Enum

	// Structs represents the list of FIDL structs represented as Go structs.
	Structs []Struct

	// Unions represents the list of FIDL unions represented as Go structs.
	Unions []Union

	// Table represents the list of FIDL tables represented as Go structs.
	Tables []Table

	// Protocols represents the list of FIDL protocols represented as Go types.
	Protocols []Protocol

	// Libraries represents the set of library dependencies for this FIDL library.
	Libraries []Library
}

// compiler contains the state necessary for recursive compilation.
type compiler struct {
	// decls contains all top-level declarations for the FIDL source.
	decls fidlgen.DeclInfoMap

	// library is the identifier for the current library.
	library fidlgen.LibraryIdentifier

	// libraryDeps is a mapping of compiled library identifiers (go package paths)
	// to aliases, which is used to resolve references to types outside of the current
	// FIDL library.
	libraryDeps map[string]string

	// Map of all structs, including external structs.
	// We use this to flatten method requests and responses.
	structs map[fidlgen.EncodedCompoundIdentifier]Struct

	// usedLibraryDeps is identical to libraryDeps except it is built up as references
	// are made into libraryDeps. Thus, after Compile is run, it contains the subset of
	// libraryDeps that's actually being used. The purpose is to figure out which
	// dependencies need to be imported.
	usedLibraryDeps map[string]string
}

// Contains the full set of reserved golang keywords, in addition to a set of
// primitive named types. Note that this will result in potentially unnecessary
// identifier renaming, but this isn't a big deal for generated code.
var reservedWords = map[string]struct{}{
	// Officially reserved keywords.
	"break":       {},
	"case":        {},
	"chan":        {},
	"const":       {},
	"continue":    {},
	"default":     {},
	"defer":       {},
	"else":        {},
	"fallthrough": {},
	"for":         {},
	"func":        {},
	"go":          {},
	"goto":        {},
	"if":          {},
	"int":         {},
	"interface":   {},
	"map":         {},
	"package":     {},
	"range":       {},
	"return":      {},
	"select":      {},
	"struct":      {},
	"switch":      {},
	"try":         {},
	"type":        {},
	"var":         {},

	// Reserved types.
	"bool":   {},
	"byte":   {},
	"int8":   {},
	"int16":  {},
	"int32":  {},
	"int64":  {},
	"rune":   {},
	"string": {},
	"uint8":  {},
	"uint16": {},
	"uint32": {},
	"uint64": {},

	// Reserved values.
	"false": {},
	"true":  {},
}

var primitiveTypes = map[fidlgen.PrimitiveSubtype]string{
	fidlgen.Bool:    "bool",
	fidlgen.Int8:    "int8",
	fidlgen.Int16:   "int16",
	fidlgen.Int32:   "int32",
	fidlgen.Int64:   "int64",
	fidlgen.Uint8:   "uint8",
	fidlgen.Uint16:  "uint16",
	fidlgen.Uint32:  "uint32",
	fidlgen.Uint64:  "uint64",
	fidlgen.Float32: "float32",
	fidlgen.Float64: "float64",
}

var internalTypes = map[fidlgen.InternalSubtype]string{
	// No actual internal type is defined because we don't support unknown
	// interactions in Go currently.
	fidlgen.TransportErr: "int32",
}

var handleTypes = map[fidlgen.HandleSubtype]string{
	// TODO(fxbug.dev/118283): Add support here for process, thread, job, resource,
	// interrupt, eventpair, fifo, guest, and time once these are actually
	// supported in the Go runtime.
	fidlgen.HandleSubtypeNone:     "_zx.Handle",
	fidlgen.HandleSubtypeVmo:      "_zx.VMO",
	fidlgen.HandleSubtypeChannel:  "_zx.Channel",
	fidlgen.HandleSubtypeEvent:    "_zx.Event",
	fidlgen.HandleSubtypePort:     "_zx.Port",
	fidlgen.HandleSubtypeDebugLog: "_zx.Log",
	fidlgen.HandleSubtypeSocket:   "_zx.Socket",
	fidlgen.HandleSubtypeVmar:     "_zx.VMAR",
}

func isReservedWord(str string) bool {
	_, ok := reservedWords[str]
	return ok
}

func changeIfReserved(val fidlgen.Identifier, ext string) string {
	// TODO(fxbug.dev/118283): Detect name collision within a scope as a result of transforming.
	str := string(val) + ext
	if isReservedWord(str) {
		return str + "_"
	}
	return str
}

func (c *compiler) inExternalLibrary(ci fidlgen.CompoundIdentifier) bool {
	if len(ci.Library) != len(c.library) {
		return true
	}
	for i, part := range c.library {
		if ci.Library[i] != part {
			return true
		}
	}
	return false
}

// Handle rights annotations are added to fields that contain handles
// or arrays and vectors of handles (recursively).
func (c *compiler) computeHandleRights(t fidlgen.Type) (fidlgen.HandleRights, bool) {
	switch t.Kind {
	case fidlgen.HandleType:
		return t.HandleRights, true
	case fidlgen.ArrayType, fidlgen.VectorType:
		return c.computeHandleRights(*t.ElementType)
	}
	return 0, false
}

// Handle subtype annotations are added to fields that contain handles
// or arrays and vectors of handles (recursively).
func (c *compiler) computeHandleSubtype(t fidlgen.Type) (fidlgen.ObjectType, bool) {
	switch t.Kind {
	case fidlgen.HandleType:
		return fidlgen.ObjectType(t.ObjType), true
	case fidlgen.RequestType:
		// TODO(fxbug.dev/45998 & fxb/64629): Currently, fidlc does not emit an
		// object type for request types. Internally, fidlc does not interpret
		// request types as special channel handles.
		return fidlgen.ObjectTypeChannel, true
	case fidlgen.IdentifierType:
		// TODO(fxbug.dev/45998 & fxb/64629): Same issue as above, but for the
		// reciprocal. Once we properly represent `client_end:P` and
		// `server_end:P` as further constraints on a handle, we can solve this
		// cleanly.
		declInfo, ok := c.decls[t.Identifier]
		if !ok {
			panic(fmt.Sprintf("unknown identifier: %v", t.Identifier))
		}
		if declInfo.Type == fidlgen.ProtocolDeclType {
			return fidlgen.ObjectTypeChannel, true
		}
	case fidlgen.ArrayType, fidlgen.VectorType:
		return c.computeHandleSubtype(*t.ElementType)
	}
	return fidlgen.ObjectTypeNone, false
}

func (*compiler) compileIdentifier(id fidlgen.Identifier, export bool, ext string) string {
	str := string(id)
	if export {
		str = fidlgen.ToUpperCamelCase(str)
	} else {
		str = fidlgen.ToLowerCamelCase(str)
	}
	return changeIfReserved(fidlgen.Identifier(str), ext)
}

func (c *compiler) compileCompoundIdentifier(eci fidlgen.EncodedCompoundIdentifier, export bool, ext string) string {
	ci := eci.Parse()
	var name string
	if export {
		name = fidlgen.ToUpperCamelCase(string(ci.Name))
	} else {
		name = fidlgen.ToLowerCamelCase(string(ci.Name))
	}
	pkg := compileLibraryIdentifier(ci.Library)
	var strs []string
	if c.inExternalLibrary(ci) {
		pkgAlias := c.libraryDeps[pkg]
		strs = append(strs, pkgAlias)
		c.usedLibraryDeps[pkg] = pkgAlias
	}
	strs = append(strs, changeIfReserved(fidlgen.Identifier(name), ext))
	if ci.Member != "" {
		strs[len(strs)-1] += c.compileIdentifier(ci.Member, true, "")
	}
	return strings.Join(strs, ".")
}

func (*compiler) compileLiteral(val fidlgen.Literal) string {
	switch val.Kind {
	case fidlgen.NumericLiteral:
		return val.Value
	case fidlgen.BoolLiteral:
		return val.Value
	case fidlgen.StringLiteral:
		return strconv.Quote(val.Value)
	default:
		panic(fmt.Sprintf("unknown literal kind: %v", val.Kind))
	}
}

func (c *compiler) compileConstant(val fidlgen.Constant) string {
	switch val.Kind {
	case fidlgen.IdentifierConstant:
		return c.compileCompoundIdentifier(val.Identifier, true, "")
	case fidlgen.LiteralConstant:
		return c.compileLiteral(*val.Literal)
	case fidlgen.BinaryOperator:
		return val.Value
	default:
		panic(fmt.Sprintf("unknown constant kind: %v", val.Kind))
	}
}

func (c *compiler) compilePrimitiveSubtype(val fidlgen.PrimitiveSubtype) Type {
	t, ok := primitiveTypes[val]
	if !ok {
		panic(fmt.Sprintf("unknown primitive type: %v", val))
	}
	return Type(t)
}

func (c *compiler) compileInternalSubtype(val fidlgen.InternalSubtype) Type {
	t, ok := internalTypes[val]
	if !ok {
		panic(fmt.Sprintf("unknown internal type: %v", val))
	}
	return Type(t)
}

func (c *compiler) compileType(val fidlgen.Type) (r Type, t StackOfBoundsTag) {
	switch val.Kind {
	case fidlgen.ArrayType:
		e, et := c.compileType(*val.ElementType)
		r = Type(fmt.Sprintf("[%d]%s", *val.ElementCount, e))
		t = et
	case fidlgen.StringType:
		if val.ElementCount == nil {
			t.reverseOfBounds = append(t.reverseOfBounds, math.MaxInt32)
		} else {
			t.reverseOfBounds = append(t.reverseOfBounds, *val.ElementCount)
		}
		if val.Nullable {
			r = Type("*string")
		} else {
			r = Type("string")
		}
	case fidlgen.HandleType:
		// Note here that we require the SyscallZx package.
		c.usedLibraryDeps[SyscallZxPackage] = SyscallZxAlias
		e, ok := handleTypes[val.HandleSubtype]
		if !ok {
			// Fall back onto a generic handle if we don't support that particular
			// handle subtype.
			e = handleTypes[fidlgen.HandleSubtypeNone]
		}
		var nullability int
		if val.Nullable {
			nullability = 1
		}
		t.reverseOfBounds = append(t.reverseOfBounds, nullability)
		r = Type(e)
	case fidlgen.RequestType:
		e := c.compileCompoundIdentifier(val.RequestSubtype, true, WithCtxSuffix+RequestSuffix)
		var nullability int
		if val.Nullable {
			nullability = 1
		}
		t.reverseOfBounds = append(t.reverseOfBounds, nullability)
		r = Type(e)
	case fidlgen.VectorType:
		e, et := c.compileType(*val.ElementType)
		if val.ElementCount == nil {
			et.reverseOfBounds = append(et.reverseOfBounds, math.MaxInt32)
		} else {
			et.reverseOfBounds = append(et.reverseOfBounds, *val.ElementCount)
		}
		if val.Nullable {
			r = Type(fmt.Sprintf("*[]%s", e))
		} else {
			r = Type(fmt.Sprintf("[]%s", e))
		}
		t = et
	case fidlgen.PrimitiveType:
		r = c.compilePrimitiveSubtype(val.PrimitiveSubtype)
	case fidlgen.InternalType:
		r = c.compileInternalSubtype(val.InternalSubtype)
	case fidlgen.IdentifierType:
		e := c.compileCompoundIdentifier(val.Identifier, true, "")
		declInfo, ok := c.decls[val.Identifier]
		if !ok {
			panic(fmt.Sprintf("unknown identifier: %v", val.Identifier))
		}
		switch declInfo.Type {
		case fidlgen.BitsDeclType:
			fallthrough
		case fidlgen.EnumDeclType:
			r = Type(e)
		case fidlgen.ProtocolDeclType:
			r = Type(e + WithCtxSuffix + ProxySuffix)
		case fidlgen.StructDeclType:
			fallthrough
		case fidlgen.UnionDeclType:
			fallthrough
		case fidlgen.TableDeclType:
			if val.Nullable {
				r = Type("*" + e)
			} else {
				r = Type(e)
			}
		default:
			panic(fmt.Sprintf("unknown declaration type: %v", declInfo.Type))
		}
	default:
		panic(fmt.Sprintf("unknown type kind: %v", val.Kind))
	}
	return
}

func (c *compiler) compileBitsMember(val fidlgen.BitsMember) BitsMember {
	return BitsMember{
		Attributes: val.Attributes,
		Name:       c.compileIdentifier(val.Name, true, ""),
		Value:      c.compileConstant(val.Value),
	}
}

func (c *compiler) compileBits(val fidlgen.Bits) Bits {
	// all bits must implement fidl.Bits
	c.usedLibraryDeps[BindingsPackage] = BindingsAlias
	// bits implementation uses bytes package
	c.usedLibraryDeps[StringsPackage] = StringsAlias

	t, _ := c.compileType(val.Type)
	r := Bits{
		Bits: val,
		Name: c.compileCompoundIdentifier(val.Name, true, ""),
		Type: t,
	}
	for _, v := range val.Members {
		r.Members = append(r.Members, c.compileBitsMember(v))
	}
	return r
}

func (c *compiler) compileConst(val fidlgen.Const) Const {
	// It's OK to ignore the tag because this type is guaranteed by the frontend
	// to be either an enum, a primitive, or a string.
	t, _ := c.compileType(val.Type)
	return Const{
		Attributes: val.Attributes,
		Name:       c.compileCompoundIdentifier(val.Name, true, ""),
		Type:       t,
		Value:      c.compileConstant(val.Value),
	}
}

func (c *compiler) compileEnumMember(val fidlgen.EnumMember) EnumMember {
	return EnumMember{
		EnumMember: val,
		Name:       c.compileIdentifier(val.Name, true, ""),
		Value:      c.compileConstant(val.Value),
	}
}

func (c *compiler) compileEnum(val fidlgen.Enum) Enum {
	// all enums must implement fidl.Enum
	c.usedLibraryDeps[BindingsPackage] = BindingsAlias

	r := Enum{
		Enum: val,
		Name: c.compileCompoundIdentifier(val.Name, true, ""),
		Type: c.compilePrimitiveSubtype(val.Type),
	}
	for _, v := range val.Members {
		r.Members = append(r.Members, c.compileEnumMember(v))
	}
	return r
}

func (c *compiler) compileStructMember(val fidlgen.StructMember) StructMember {
	tags := Tags{
		FidlOffsetV2Tag: val.FieldShapeV2.Offset,
	}
	ty, rbtag := c.compileType(val.Type)
	if !rbtag.IsEmpty() {
		tags[FidlBoundsTag] = rbtag.String()
	}
	if handleRights, ok := c.computeHandleRights(val.Type); ok {
		tags[FidlHandleRightsTag] = int(handleRights)
	}
	if handleSubtype, ok := c.computeHandleSubtype(val.Type); ok {
		tags[FidlHandleSubtypeTag] = handleSubtype
	}

	return StructMember{
		Attributes:  val.Attributes,
		Type:        ty,
		Name:        c.compileIdentifier(val.Name, true, ""),
		PrivateName: c.compileIdentifier(val.Name, false, ""),
		Tags:        tags,
	}
}

func (c *compiler) compileStruct(val fidlgen.Struct) Struct {
	// required for fidl.CreateLazymarshaler which is called on all structs
	c.usedLibraryDeps[BindingsPackage] = BindingsAlias

	tags := Tags{
		FidlTag:            "s",
		FidlSizeV2Tag:      val.TypeShapeV2.InlineSize,
		FidlAlignmentV2Tag: val.TypeShapeV2.Alignment,
	}

	r := Struct{
		Name:   c.compileCompoundIdentifier(val.Name, true, ""),
		Struct: val,
		Tags:   tags,
	}

	for _, v := range val.Members {
		r.Members = append(r.Members, c.compileStructMember(v))
	}

	c.structs[val.Name] = r
	return r
}

func (c *compiler) compileUnion(val fidlgen.Union) Union {
	// required for fidl.CreateLazymarshaler which is called on all unions.
	c.usedLibraryDeps[BindingsPackage] = BindingsAlias

	if val.IsFlexible() {
		// fidl.UnknownData is needed only for flexible unions
		c.usedLibraryDeps[BindingsPackage] = BindingsAlias
	}
	var members []UnionMember
	for _, member := range val.Members {
		if member.Reserved {
			continue
		}
		tags := Tags{
			FidlOrdinalTag: member.Ordinal,
		}
		ty, rbtag := c.compileType(*member.Type)
		if !rbtag.IsEmpty() {
			tags[FidlBoundsTag] = rbtag.String()
		}
		if handleRights, ok := c.computeHandleRights(*member.Type); ok {
			tags[FidlHandleRightsTag] = handleRights
		}
		if handleSubtype, ok := c.computeHandleSubtype(*member.Type); ok {
			tags[FidlHandleSubtypeTag] = handleSubtype
		}
		members = append(members, UnionMember{
			Attributes:  member.Attributes,
			Ordinal:     uint64(member.Ordinal),
			Type:        ty,
			Name:        c.compileIdentifier(member.Name, true, ""),
			PrivateName: c.compileIdentifier(member.Name, false, ""),
			Tags:        tags,
		})
	}
	fidlTag := "x"
	if val.Strictness == fidlgen.IsStrict {
		fidlTag += "!"
	}
	tags := Tags{
		FidlTag:            fidlTag,
		FidlSizeV2Tag:      val.TypeShapeV2.InlineSize,
		FidlAlignmentV2Tag: val.TypeShapeV2.Alignment,
		FidlIsResourceTag:  val.IsResourceType(),
	}
	return Union{
		Union:           val,
		Name:            c.compileCompoundIdentifier(val.Name, true, ""),
		TagName:         "I_" + c.compileCompoundIdentifier(val.Name, false, TagSuffix),
		Members:         members,
		Strictness:      val.Strictness,
		Tags:            tags,
		UnknownDataType: fmt.Sprintf("%s.UnknownData", BindingsAlias),
	}
}

func (c *compiler) compileTable(val fidlgen.Table) Table {
	// required for fidl.CreateLazymarshaler which is called on all tables.
	c.usedLibraryDeps[BindingsPackage] = BindingsAlias

	var members []TableMember
	for _, member := range val.SortedMembersNoReserved() {
		ty, rbtag := c.compileType(*member.Type)
		tags := Tags{
			FidlOrdinalTag: member.Ordinal,
		}
		if !rbtag.IsEmpty() {
			tags[FidlBoundsTag] = rbtag.String()
		}
		if handleRights, ok := c.computeHandleRights(*member.Type); ok {
			tags[FidlHandleRightsTag] = handleRights
		}
		if handleSubtype, ok := c.computeHandleSubtype(*member.Type); ok {
			tags[FidlHandleSubtypeTag] = handleSubtype
		}
		name := c.compileIdentifier(member.Name, true, "")
		members = append(members, TableMember{
			Attributes:        member.Attributes,
			DataField:         name,
			PrivateDataField:  c.compileIdentifier(member.Name, false, ""),
			PresenceField:     name + "Present",
			Setter:            "Set" + name,
			Getter:            "Get" + name,
			GetterWithDefault: "Get" + name + "WithDefault",
			Haser:             "Has" + name,
			Clearer:           "Clear" + name,
			Type:              ty,
			Tags:              tags,
		})
	}
	tags := Tags{
		FidlTag:            "t",
		FidlSizeV2Tag:      val.TypeShapeV2.InlineSize,
		FidlAlignmentV2Tag: val.TypeShapeV2.Alignment,
		FidlIsResourceTag:  val.IsResourceType(),
	}
	return Table{
		Table:           val,
		Name:            c.compileCompoundIdentifier(val.Name, true, ""),
		Members:         members,
		Tags:            tags,
		UnknownDataType: fmt.Sprintf("%s.UnknownData", BindingsAlias),
	}
}

func (c *compiler) compileMethod(protocolName fidlgen.EncodedCompoundIdentifier, val fidlgen.Method) Method {
	methodName := c.compileIdentifier(val.Name, true, "")
	return Method{
		Method:          val,
		Name:            methodName,
		OrdinalName:     c.compileCompoundIdentifier(protocolName, true, methodName+"Ordinal"),
		Request:         c.compilePayload(val.RequestPayload),
		Response:        c.compilePayload(val.ResponsePayload),
		EventExpectName: "Expect" + methodName,
		IsEvent:         !val.HasRequest && val.HasResponse,
		IsTransitional:  val.IsTransitional(),
	}
}

func (c *compiler) compilePayload(payloadType *fidlgen.Type) *Payload {
	if payloadType == nil {
		return nil
	}
	if s, ok := c.structs[payloadType.Identifier]; ok {
		var names, types, namesAndTypes, construct []string
		for _, member := range s.Members {
			names = append(names, member.PrivateName)
			types = append(types, string(member.Type))
			namesAndTypes = append(namesAndTypes, fmt.Sprintf("%s %s", member.PrivateName, member.Type))
			construct = append(construct, fmt.Sprintf("%s: %s", member.Name, member.PrivateName))
		}
		return &Payload{
			Name:          s.Name,
			Names:         strings.Join(names, ", "),
			Types:         strings.Join(types, ", "),
			NamesAndTypes: strings.Join(namesAndTypes, ", "),
			Construct:     fmt.Sprintf("%s{%s}", s.Name, strings.Join(construct, ", ")),
			Destructure: func(v string) string {
				var fields []string
				for _, member := range s.Members {
					fields = append(fields, fmt.Sprintf("%s.%s", v, member.Name))
				}
				return strings.Join(fields, ", ")
			},
		}
	}
	// Table or union.
	name := c.compileCompoundIdentifier(payloadType.Identifier, true, "")
	parameter := "payload"
	return &Payload{
		Name:          name,
		Names:         parameter,
		Types:         name,
		NamesAndTypes: parameter + " " + name,
		Construct:     parameter,
		Destructure:   func(v string) string { return v },
	}
}

func (c *compiler) compileProtocol(val fidlgen.Protocol) Protocol {
	c.usedLibraryDeps[BindingsPackage] = BindingsAlias

	var proxyType string
	if val.Attributes.OverTransport() == "Channel" {
		proxyType = "ChannelProxy"
	}
	r := Protocol{
		Attributes:           val.Attributes,
		Name:                 c.compileCompoundIdentifier(val.Name, true, WithCtxSuffix),
		Openness:             val.Openness,
		TransitionalBaseName: c.compileCompoundIdentifier(val.Name, true, WithCtxSuffix+TransitionalBaseSuffix),
		ProxyName:            c.compileCompoundIdentifier(val.Name, true, WithCtxSuffix+ProxySuffix),
		ProxyType:            proxyType,
		StubName:             c.compileCompoundIdentifier(val.Name, true, WithCtxSuffix+StubSuffix),
		RequestName:          c.compileCompoundIdentifier(val.Name, true, WithCtxSuffix+RequestSuffix),
		EventProxyName:       c.compileCompoundIdentifier(val.Name, true, EventProxySuffix),
		ProtocolNameConstant: c.compileCompoundIdentifier(val.Name, true, ServiceNameSuffix),
		ProtocolNameString:   val.GetProtocolName(),
	}
	for _, v := range val.Methods {
		r.Methods = append(r.Methods, c.compileMethod(val.Name, v))
	}
	return r
}

func compileLibraryIdentifier(lib fidlgen.LibraryIdentifier) string {
	return "fidl/" + joinLibraryIdentifier(lib, "/")
}

func joinLibraryIdentifier(lib fidlgen.LibraryIdentifier, sep string) string {
	str := make([]string, len([]fidlgen.Identifier(lib)))
	for i, id := range lib {
		str[i] = string(id)
	}
	return strings.Join(str, sep)
}

// Compile translates parsed FIDL IR into golang backend IR for code generation.
func Compile(ir fidlgen.Root) Root {
	ir = ir.ForBindings("go")

	libraryName := ir.Name.Parse()
	libraryPath := compileLibraryIdentifier(libraryName)

	// Collect all libraries.
	godeps := make(map[string]string)
	for _, v := range ir.Libraries {
		// Don't try to import yourself.
		if v.Name == ir.Name {
			continue
		}
		libComponents := v.Name.Parse()
		path := compileLibraryIdentifier(libComponents)
		alias := changeIfReserved(
			fidlgen.Identifier(fidlgen.ToLowerCamelCase(
				joinLibraryIdentifier(libComponents, ""),
			)),
			"",
		)
		godeps[path] = alias
	}

	// Instantiate a compiler context.
	c := compiler{
		decls:           ir.DeclInfo(),
		library:         libraryName,
		libraryDeps:     godeps,
		structs:         make(map[fidlgen.EncodedCompoundIdentifier]Struct),
		usedLibraryDeps: make(map[string]string),
	}

	// Compile ir into r.
	r := Root{
		Experiments:   ir.Experiments,
		Name:          changeIfReserved(libraryName[len(libraryName)-1], ""),
		PackageName:   libraryPath,
		BindingsAlias: BindingsAlias,
	}

	for _, v := range ir.Bits {
		r.Bits = append(r.Bits, c.compileBits(v))
	}
	for _, v := range ir.Consts {
		r.Consts = append(r.Consts, c.compileConst(v))
	}
	for _, v := range ir.Enums {
		r.Enums = append(r.Enums, c.compileEnum(v))
	}
	for _, v := range ir.Structs {
		r.Structs = append(r.Structs, c.compileStruct(v))
	}
	for _, v := range ir.ExternalStructs {
		c.compileStruct(v)
	}
	for _, v := range ir.Tables {
		r.Tables = append(r.Tables, c.compileTable(v))
	}
	for _, v := range ir.Unions {
		r.Unions = append(r.Unions, c.compileUnion(v))
	}

	for _, v := range ir.Protocols {
		protocol := c.compileProtocol(v)
		r.Protocols = append(r.Protocols, protocol)
		if protocol.ProxyType == "ChannelProxy" && len(protocol.ProtocolNameString) != 0 {
			c.usedLibraryDeps[SyscallZxPackage] = SyscallZxAlias
		}
	}

	for path, alias := range c.usedLibraryDeps {
		r.Libraries = append(r.Libraries, Library{
			Path:  path,
			Alias: alias,
		})
	}

	// Sort the libraries according to Path.
	sort.Slice(r.Libraries, func(i, j int) bool {
		return strings.Compare(r.Libraries[i].Path, r.Libraries[j].Path) == -1
	})
	return r
}
