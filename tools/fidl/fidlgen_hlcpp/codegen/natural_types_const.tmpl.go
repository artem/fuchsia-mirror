// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

const constTemplate = `
{{- define "ConstDeclaration" }}
{{range .DocComments}}
///{{ . }}
{{- end}}
{{- if .Extern }}
extern {{ .Decorator }} {{ .Type.NatDecl }} {{ .Name }};
{{- else }}
{{ .Decorator }} {{ .Type.NatDecl }} {{ .Name }} = {{ .Value }};
{{- end }}
{{- end }}

{{- define "ConstDefinition" }}
{{- if .Extern }}
{{ .Decorator }} {{ .Type.NatDecl }} {{ .Name }} = {{ .Value }};
{{- end }}
{{- end }}
`
