// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package rust

import (
	"bytes"
	"fmt"
	"sort"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
	"go.fuchsia.dev/fuchsia/tools/fidl/measure-tape/src/measurer"
	"go.fuchsia.dev/fuchsia/tools/fidl/measure-tape/src/utils"
)

func WriteRs(buf *bytes.Buffer,
	m *measurer.Measurer,
	targetMts []*measurer.MeasuringTape,
	allMethods map[measurer.MethodID]*measurer.Method) {

	if err := topOfRs.Execute(buf, newTmplParams(m, targetMts)); err != nil {
		panic(err)
	}

	cb := codeBuffer{buf: buf}
	utils.ForAllMethodsInOrder(allMethods, func(m *measurer.Method) {
		buf.WriteString("\n")
		cb.writeMethod(m)
	})

	buf.WriteRune('}') // and with that, we close the inner module
}

type codeBuffer struct {
	level int
	buf   *bytes.Buffer
}

func (cb *codeBuffer) writeMethod(m *measurer.Method) {
	traitName, methodName := toTraitAndMethodName(m.ID.Kind)
	cb.writef("impl %s for %s {\n", traitName, toTypeName(m.ID.TargetType))
	cb.indent(func() {
		// TODO(https://fxbug.dev/42128549): With improved locals handling, we could
		// conditionally define the alias below. Of course, this would be
		// superseded by https://fxbug.dev/42128551 but both should happen.
		cb.writef("#[inline]\n")
		cb.writef("#[allow(unused_variables)]\n")
		cb.writef("fn %s(&self, size_agg: &mut SizeAgg) {\n", methodName)
		cb.indent(func() {
			// TODO(https://fxbug.dev/42128551): Variable naming should be defered to printing.
			// Here, we should bind m.Arg to the name 'self' therefore avoiding
			// this local.
			cb.writef("let %s = self;\n", formatExpr{m.Arg})
			cb.writeBlock(m.Body)
		})
		cb.writef("}\n")
	})
	cb.writef("}\n")
}

func toTraitAndMethodName(kind measurer.MethodKind) (string, string) {
	switch kind {
	case measurer.Measure:
		return "MeasurableAll", "measure_all"
	case measurer.MeasureOutOfLine:
		return "MeasurableOutOfLine", "measure_out_of_line"
	case measurer.MeasureHandles:
		return "MeasurableHandles", "measure_handles"
	default:
		panic(fmt.Sprintf("should not be reachable for method kind %v", kind))
	}
}

func (cb *codeBuffer) writeBlock(b *measurer.Block) {
	b.ForAllStatements(func(stmt *measurer.Statement) {
		stmt.Visit(cb)
	})
}

var _ measurer.StatementFormatter = (*codeBuffer)(nil)

func (cb *codeBuffer) CaseMaxOut() {
	cb.writef("size_agg.maxed_out = true;\n")
}

func (cb *codeBuffer) CaseAddNumBytes(val measurer.Expression) {
	cb.writef("size_agg.add_num_bytes(%s);\n", formatExpr{val})
}

func (cb *codeBuffer) CaseAddNumHandles(val measurer.Expression) {
	cb.writef("size_agg.add_num_handles(%s);\n", formatExpr{val})
}

func (cb *codeBuffer) CaseInvoke(id measurer.MethodID, val measurer.Expression) {
	_, methodName := toTraitAndMethodName(id.Kind)
	cb.writef("%s.%s(size_agg);\n", formatExpr{val}, methodName)
}

func (cb *codeBuffer) CaseGuard(cond measurer.Expression, body *measurer.Block) {
	// TODO(https://fxbug.dev/42128824): Improve guard statement.
	cb.writef("match %s {\n", formatExpr{cond})
	cb.indent(func() {
		cb.writef("Some(_) => {\n")
		cb.indent(func() {
			cb.writeBlock(body)
		})
		cb.writef("}\n")
		cb.writef("_ => {}\n")
	})
	cb.writef("}\n")
}

func (cb *codeBuffer) CaseIterate(local, val measurer.Expression, body *measurer.Block) {
	var iter string
	if kind := val.AssertKind(measurer.String, measurer.Vector, measurer.Array); kind == measurer.Array || kind == measurer.Vector {
		iter = ".iter()"
	}
	cb.writef("for %s in %s%s%s {\n", formatExpr{local}, formatExpr{val}, maybeUnwrap(val), iter)
	cb.indent(func() {
		cb.writeBlock(body)
	})
	cb.writef("}\n")
}

func (cb *codeBuffer) CaseSelectVariant(
	val measurer.Expression,
	targetType fidlgen.Name,
	variants map[string]measurer.LocalWithBlock) {

	cb.writef("match %s {\n", formatExpr{val})
	cb.indent(func() {
		utils.ForAllVariantsInOrder(variants, func(member string, localWithBlock measurer.LocalWithBlock) {
			if member != measurer.UnknownVariant {
				cb.writef("%s::%s(%s) => {\n",
					toTypeName(targetType), fidlgen.ToUpperCamelCase(member),
					formatExpr{localWithBlock.Local})
			} else {
				cb.writef("%sUnknown!() => {\n", toTypeName(targetType))
			}
			cb.indent(func() {
				cb.writeBlock(localWithBlock.Body)
			})
			cb.writef("}\n")
		})
	})
	cb.writef("}\n")
}

func (cb *codeBuffer) CaseDeclareMaxOrdinal(local measurer.Expression) {
	cb.writef("let mut %s: usize = 0;\n", formatExpr{local})
}

func (cb *codeBuffer) CaseSetMaxOrdinal(local, ordinal measurer.Expression) {
	cb.writef("%s = %s;\n", formatExpr{local}, formatExpr{ordinal})
}

func (cb *codeBuffer) writef(format string, a ...interface{}) {
	for i := 0; i < cb.level; i++ {
		cb.buf.WriteString(indent)
	}
	cb.buf.WriteString(fmt.Sprintf(format, a...))
}

const indent = "  "

func (cb *codeBuffer) indent(fn func()) {
	cb.level++
	fn()
	cb.level--
}

type formatExpr struct {
	measurer.Expression
}

func (val formatExpr) String() string {
	return val.Fmt(val)
}

var _ measurer.ExpressionFormatter = formatExpr{}

func (formatExpr) CaseNum(num int) string {
	return fmt.Sprintf("%d", num)
}

func (formatExpr) CaseLocal(name string, _ measurer.TapeKind) string {
	return name
}

func (formatExpr) CaseMemberOf(val measurer.Expression, member string, _ measurer.TapeKind, nullable bool) string {
	var maybeUnwrap string
	if nullable {
		maybeUnwrap = ".as_ref()"
	} else if kind := val.AssertKind(measurer.Struct, measurer.Union, measurer.Table); kind == measurer.Table {
		maybeUnwrap = ".as_ref().unwrap()"
	}
	return fmt.Sprintf("%s.%s%s", formatExpr{val}, member, maybeUnwrap)
}

func (formatExpr) CaseFidlAlign(val measurer.Expression) string {
	return fmt.Sprintf("round_up_to_align(%s, 8)", formatExpr{val})
}

func (formatExpr) CaseLength(val measurer.Expression) string {
	return fmt.Sprintf("%s%s.len()", formatExpr{val}, maybeUnwrap(val))
}

func (formatExpr) CaseHasMember(val measurer.Expression, member string) string {
	return fmt.Sprintf("%s.%s", formatExpr{val}, member)
}

func (formatExpr) CaseMult(lhs, rhs measurer.Expression) string {
	return fmt.Sprintf("%s * %s", formatExpr{lhs}, formatExpr{rhs})
}

func maybeUnwrap(val measurer.Expression) string {
	if val.Nullable() {
		return ".unwrap()"
	}
	return ""
}

func toCrateName(libraryName fidlgen.LibraryName) string {
	return fmt.Sprintf("fidl_%s", strings.Join(libraryName.Parts(), "_"))
}

func toTypeName(declName fidlgen.Name) string {
	return fmt.Sprintf("%s::%s",
		toCrateName(declName.LibraryName()),
		fidlgen.ToUpperCamelCase(declName.DeclarationName()))
}

type tmplParams struct {
	Uses        []string
	TargetTypes []string
}

func newTmplParams(m *measurer.Measurer,
	targetMts []*measurer.MeasuringTape) tmplParams {

	var uses []string
	for _, libraryName := range m.RootLibraries() {
		uses = append(uses, toCrateName(libraryName))
	}
	sort.Strings(uses)

	var targetTypes []string
	for _, targetMt := range targetMts {
		targetTypes = append(targetTypes, toTypeName(targetMt.Name()))
	}
	return tmplParams{
		Uses:        uses,
		TargetTypes: targetTypes,
	}
}

var topOfRs = template.Must(template.New("topOfRs").Parse(
	`// WARNING: This file is machine generated by measure-tape.

use inner::MeasurableAll;

#[derive(Debug, Eq, PartialEq)]
pub struct Size {
  pub num_bytes: usize,
  pub num_handles: usize,
}

pub trait Measurable {
  fn measure(&self) -> Size;
}

{{ range $targetType := .TargetTypes }}
impl Measurable for {{ $targetType }} {
  fn measure(&self) -> Size {
    let mut size_agg = inner::SizeAgg { maxed_out: false, num_bytes: 0, num_handles: 0 };
    self.measure_all(&mut size_agg);
    size_agg.to_size()
  }
}
{{ end }}

mod inner {
#![allow(unused_imports)]
#![allow(unused_mut)]

use {
  crate::Size,
  fidl::encoding::round_up_to_align,
{{- range .Uses }}
  {{ . }},
{{- end }}
  fuchsia_zircon_types as zx,
};

pub struct SizeAgg {
  pub maxed_out: bool,
  pub num_bytes: usize,
  pub num_handles: usize,
}

impl SizeAgg {
  #[inline(always)]
  fn add_num_bytes(&mut self, num_bytes: usize) {
    self.num_bytes += num_bytes;
  }

  #[inline(always)]
  #[allow(dead_code)]
  fn add_num_handles(&mut self, num_handles: usize) {
    self.num_handles += num_handles;
  }

  #[inline(always)]
  pub fn to_size(&self) -> Size {
    if self.maxed_out {
      return Size {
        num_bytes: zx::ZX_CHANNEL_MAX_MSG_BYTES as usize,
        num_handles: zx::ZX_CHANNEL_MAX_MSG_HANDLES as usize,
      };
    }
    return Size { num_bytes: self.num_bytes, num_handles: self.num_handles };
  }
}

pub trait MeasurableAll {
  fn measure_all(&self, size_agg: &mut SizeAgg);
}

trait MeasurableOutOfLine {
  fn measure_out_of_line(&self, size_agg: &mut SizeAgg);
}

trait MeasurableHandles {
  fn measure_handles(&self, size_agg: &mut SizeAgg);
}
`))
