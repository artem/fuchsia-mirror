// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"embed"
	"text/template"

	cpp "go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen_cpp"
)

//go:embed *.tmpl
var templates embed.FS

func NewGenerator(flags *cpp.CmdlineFlags, extraFuncs template.FuncMap) *cpp.Generator {
	return cpp.NewGenerator(flags, templates, extraFuncs)
}
