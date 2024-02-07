// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package fuchsia_controller

import (
	"bytes"
	_ "embed"
	"fmt"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/config"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/ir"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/mixer"
	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

var (
	//go:embed conformance.tmpl
	conformanceTmplText string
	conformanceTmpl     = template.Must(template.New("conformanceTmpl").Parse(conformanceTmplText))
)

type conformanceTmplInput struct {
	EncodeSuccessCases []encodeSuccessCase
	DecodeSuccessCases []decodeSuccessCase
	EncodeFailureCases []encodeFailureCase
	DecodeFailureCases []decodeFailureCase
}

type encodeSuccessCase struct {
	Name, Context, HandleDefs, Value, Bytes string
}

type decodeSuccessCase struct{}

type encodeFailureCase struct{}

type decodeFailureCase struct{}

func GenerateConformanceTests(gidl ir.All, fidl fidlgen.Root, config config.GeneratorConfig) ([]byte, error) {
	schema := mixer.BuildSchema(fidl)
	encodeSuccessCases, err := encodeSuccessCases(gidl.EncodeSuccess, schema)
	if err != nil {
		return nil, err
	}
	var buf bytes.Buffer
	err = conformanceTmpl.Execute(&buf, conformanceTmplInput{
		EncodeSuccessCases: encodeSuccessCases,
	})
	return buf.Bytes(), err
}

func declName(decl mixer.NamedDeclaration) string {
	return identifierName(decl.Name())
}

func identifierName(qualifiedName string) string {
	parts := strings.Split(qualifiedName, "/")
	library_parts := strings.Split(parts[0], ".")
	return fmt.Sprintf("%s.%s", strings.Join(library_parts, "_"),
		fidlgen.ToUpperCamelCase(parts[1]))
}

func encodeSuccessCases(gidlEncodeSuccesses []ir.EncodeSuccess, schema mixer.Schema) ([]encodeSuccessCase, error) {
	var encodeSuccessCases []encodeSuccessCase
	for _, encodeSuccess := range gidlEncodeSuccesses {
		decl, err := schema.ExtractDeclarationEncodeSuccess(encodeSuccess.Value, encodeSuccess.HandleDefs)
		if err != nil {
			return nil, fmt.Errorf("encode success %s: %s", encodeSuccess.Name, err)
		}
		value := visit(encodeSuccess.Value, decl)
		for _, encoding := range encodeSuccess.Encodings {
			if !wireFormatSupported(encoding.WireFormat) {
				continue
			}
			newCase := encodeSuccessCase{
				Name:       testCaseName(encodeSuccess.Name, encoding.WireFormat),
				Context:    encodingContext(encoding.WireFormat),
				HandleDefs: buildHandleDefs(encodeSuccess.HandleDefs),
				Value:      value,
				Bytes:      buildBytes(encoding.Bytes),
			}
			encodeSuccessCases = append(encodeSuccessCases, newCase)
		}
	}
	return encodeSuccessCases, nil
}

func handleTypeName(subtype fidlgen.HandleSubtype) string {
	switch subtype {
	case fidlgen.HandleSubtypeNone:
		return "Handle"
	case fidlgen.HandleSubtypeChannel:
		return "Channel"
	case fidlgen.HandleSubtypeEvent:
		return "Event"
	default:
		panic(fmt.Sprintf("unsupported handle subtype: %s", subtype))
	}
}

func buildHandleDefs(defs []ir.HandleDef) string {
	if len(defs) == 0 {
		return ""
	}
	var builder strings.Builder
	builder.WriteString("[\n")
	for _, d := range defs {
		builder.WriteString(fmt.Sprintf("create_handle(fuchsia_controller_py.%s).take(),\n", handleTypeName(d.Subtype)))
	}
	builder.WriteString("]")
	return builder.String()
}

func testCaseName(baseName string, wireFormat ir.WireFormat) string {
	return fidlgen.ToSnakeCase(fmt.Sprintf("%s_%s", baseName, wireFormat))
}

var supportedWireFormats = []ir.WireFormat{
	ir.V2WireFormat,
}

func wireFormatSupported(wireFormat ir.WireFormat) bool {
	for _, wf := range supportedWireFormats {
		if wireFormat == wf {
			return true
		}
	}
	return false
}

func encodingContext(wireFormat ir.WireFormat) string {
	switch wireFormat {
	case ir.V2WireFormat:
		return "_V2_CONTEXT"
	default:
		panic(fmt.Sprintf("unexpected wire format %v", wireFormat))
	}
}

func primitiveTypeName(subtype fidlgen.PrimitiveSubtype) string {
	switch subtype {
	case fidlgen.Int8, fidlgen.Uint8, fidlgen.Int16, fidlgen.Uint16,
		fidlgen.Int32, fidlgen.Uint32, fidlgen.Int64, fidlgen.Uint64:
		return "int"
	case fidlgen.Float32, fidlgen.Float64:
		return "float"
	case fidlgen.Bool:
		return "bool"
	default:
		panic(fmt.Sprintf("unexpected subtype %v", subtype))
	}
}

func formatPyBool(value bool) string {
	if value {
		return "True"
	}
	return "False"
}

func onStruct(value ir.Record, decl *mixer.StructDecl) string {
	var structFields []string
	providedKeys := make(map[string]struct{}, len(value.Fields))
	for _, field := range value.Fields {
		if field.Key.IsUnknown() {
			panic(fmt.Sprintf("unknown field not supported %+v", field.Key))
		}
		providedKeys[field.Key.Name] = struct{}{}
		fieldName := fidlgen.ToSnakeCase(field.Key.Name)
		fieldValueStr := visit(field.Value, decl.Field(field.Key.Name))
		structFields = append(structFields, fmt.Sprintf("%s=%s", fieldName, fieldValueStr))
	}
	for _, key := range decl.FieldNames() {
		if _, ok := providedKeys[key]; !ok {
			fieldName := fidlgen.ToSnakeCase(key)
			structFields = append(structFields, fmt.Sprintf("%s=None", fieldName))
		}
	}
	valueStr := fmt.Sprintf("%s(%s)", declName(decl), strings.Join(structFields, ", "))
	return valueStr
}

func onTable(value ir.Record, decl *mixer.TableDecl) string {
	var tableFields []string
	for _, field := range value.Fields {
		if field.Key.IsUnknown() {
			panic(fmt.Sprintf("table %s: unknown ordinal %d: Rust cannot construct tables with unknown fields",
				decl.Name(), field.Key.UnknownOrdinal))
		}
		fieldName := fidlgen.ToSnakeCase(field.Key.Name)
		fieldValueStr := visit(field.Value, decl.Field(field.Key.Name))
		tableFields = append(tableFields, fmt.Sprintf("%s=%s", fieldName, fieldValueStr))
	}
	tableName := declName(decl)
	valueStr := fmt.Sprintf("%s(%s)", tableName, strings.Join(tableFields, ", "))
	return valueStr
}

func onUnion(value ir.Record, decl *mixer.UnionDecl) string {
	if len(value.Fields) != 1 {
		panic(fmt.Sprintf("union has %d fields, expected 1", len(value.Fields)))
	}
	field := value.Fields[0]
	var valueStr string
	if field.Key.IsUnknown() {
		if field.Key.UnknownOrdinal != 0 {
			panic(fmt.Sprintf("union %s: unknown ordinal %d: Rust can only construct unknowns with the ordinal 0",
				decl.Name(), field.Key.UnknownOrdinal))
		}
		if field.Value != nil {
			panic(fmt.Sprintf("union %s: unknown ordinal %d: Rust cannot construct union with unknown bytes/handles",
				decl.Name(), field.Key.UnknownOrdinal))
		}
		valueStr = fmt.Sprintf("%s()", declName(decl))
	} else {
		fieldName := fidlgen.ToSnakeCase(field.Key.Name)
		fieldValueStr := visit(field.Value, decl.Field(field.Key.Name))
		valueStr = fmt.Sprintf("%s.%s_variant(%s)", declName(decl), fieldName, fieldValueStr)
	}
	return valueStr
}

func onList(value []ir.Value, decl mixer.ListDeclaration) string {
	var elements []string
	elemDecl := decl.Elem()
	for _, item := range value {
		elements = append(elements, visit(item, elemDecl))
	}
	elementsStr := strings.Join(elements, ", ")
	var valueStr string
	switch decl.(type) {
	case *mixer.ArrayDecl:
		valueStr = fmt.Sprintf("[%s]", elementsStr)
	case *mixer.VectorDecl:
		valueStr = fmt.Sprintf("[%s]", elementsStr)
	default:
		panic(fmt.Sprintf("unexpected decl %v", decl))
	}
	return valueStr
}

func visit(value ir.Value, decl mixer.Declaration) string {
	switch value := value.(type) {
	case bool:
		return formatPyBool(value)
	case int64, uint64, float64:
		switch decl := decl.(type) {
		case mixer.PrimitiveDeclaration:
			return fmt.Sprintf("%v", value)
		case *mixer.BitsDecl:
			primitive := visit(value, &decl.Underlying)
			return fmt.Sprintf("%v", primitive)
		case *mixer.EnumDecl:
			primitive := visit(value, &decl.Underlying)
			return fmt.Sprintf("%v", primitive)
		}
	case ir.RawFloat:
		switch decl.(*mixer.FloatDecl).Subtype() {
		case fidlgen.Float32:
			return fmt.Sprintf("struct.unpack('>f', bytes.fromhex('%08x'))[0]", value)
		case fidlgen.Float64:
			return fmt.Sprintf("struct.unpack('>d', bytes.fromhex('%016x'))[0]", value)
		}
	case string:
		return fmt.Sprintf("%q", value)
	case nil:
		if !decl.IsNullable() {
			if _, ok := decl.(*mixer.HandleDecl); ok {
				return "0"
			}
			panic(fmt.Sprintf("got nil for non-nullable type: %T", decl))
		}
		return "None"
	case ir.Handle:
		return fmt.Sprintf("handle_defs[%d]", int(value))
	case ir.Record:
		switch decl := decl.(type) {
		case *mixer.StructDecl:
			return onStruct(value, decl)
		case *mixer.TableDecl:
			return onTable(value, decl)
		case *mixer.UnionDecl:
			return onUnion(value, decl)
		}
	case []ir.Value:
		switch decl := decl.(type) {
		case *mixer.ArrayDecl:
			return onList(value, decl)
		case *mixer.VectorDecl:
			return onList(value, decl)
		}
	}
	panic(fmt.Sprintf("not implemented: %T", value))
}

func buildBytes(bytes []byte) string {
	var builder strings.Builder
	builder.WriteString("bytearray([\n")
	for i, b := range bytes {
		builder.WriteString(fmt.Sprintf("0x%02x,", b))
		if i%8 == 7 {
			builder.WriteString("\n")
		}
	}
	builder.WriteString("\n])")
	return builder.String()
}
