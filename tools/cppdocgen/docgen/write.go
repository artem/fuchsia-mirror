// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package docgen

import (
	"fmt"
	"go.fuchsia.dev/fuchsia/tools/cppdocgen/clangdoc"
	"io"
	"log"
	"path/filepath"
	"strings"
)

type WriteSettings struct {
	// User visible library name ("fdio")
	LibName string

	// The source root relative to the build directory.
	BuildRelSourceRoot string

	// The include directory relative to the build directory.
	BuildRelIncludeDir string

	// Browsable URL of the source code repo. File paths get appended to this to generate source
	// links.
	RepoBaseUrl string

	// Contents of the toplevel documentation (typically from a README.md file) if any.
	// If nonempty, this data will be inserted at the top of the index.
	OverviewContents []byte

	// The absolute doc path where the docs will be installed on devsite. This is used to
	// generate the paths in _toc.yaml which must be absolute. It will end in a slash.
	TocPath string
}

// Identifies the heading level in Markdown. Level 0 is not a real heading level but is used to
// indicate something is happening outside of any heading level.
const (
	markdownHeading0 int = 0
	markdownHeading1 int = 1
	markdownHeading2 int = 2
	markdownHeading3 int = 3
	markdownHeading4 int = 4
)

// Gets a user-visible include path given the path in the build.
func (s WriteSettings) GetUserIncludePath(path string) string {
	result, err := filepath.Rel(s.BuildRelIncludeDir, path)
	if err != nil {
		log.Fatal(err)
	}
	return result
}

// Returns the given build-relative path as being relative to the source root.
func (s WriteSettings) GetSourceRootPath(path string) string {
	result, err := filepath.Rel(s.BuildRelSourceRoot, path)
	if err != nil {
		log.Fatal(err)
	}
	return result
}

func (s WriteSettings) fileSourceLink(file string) string {
	return s.RepoBaseUrl + s.GetSourceRootPath(file)
}

func (s WriteSettings) locationSourceLink(d clangdoc.Location) string {
	return fmt.Sprintf("%s#%d", s.fileSourceLink(d.Filename), d.LineNumber)
}

func writePreHeader(f io.Writer) {
	fmt.Fprintf(f, "<pre class=\"devsite-disable-click-to-copy\">\n")
}
func writePreFooter(f io.Writer) {
	fmt.Fprintf(f, "</pre>\n\n")
}

func stripPathLeftElements(path string, stripElts int) string {
	if stripElts == 0 {
		return path
	}

	norm := filepath.ToSlash(path)
	cur_slash := 0
	for i := 0; i < len(norm); i++ {
		if norm[i] == '/' {
			cur_slash++
			if cur_slash == stripElts {
				return norm[i+1:]
			}
		}
	}

	// Not enough slashes to strip, return the original.
	return norm
}

func makeIndent(length int) (out []byte) {
	out = make([]byte, length)
	for i := 0; i < length; i++ {
		out[i] = ' '
	}
	return
}

func headingMarkerAtLevel(lv int) string {
	return strings.Repeat("#", lv)
}

var htmlEscapes = map[rune]string{
	'<': "&lt;",
	'>': "&gt;",
	'&': "&amp;",
	// Required in attribute contexts, it can't hurt to always escape quotes.
	'"': "&quot;",
}

func escapeHtml(s string) string {
	escaped := ""
	for _, b := range s {
		if be := htmlEscapes[b]; len(be) > 0 {
			escaped += be
		} else {
			escaped += string(b)
		}
	}
	return escaped
}

var typeRenames = map[string]string{
	// Clang emits C-style "_Bool" for some reason.
	"_Bool":                  "bool",
	"std::basic_string":      "std::string",
	"std::basic_string_view": "std::string_view",
}

// Handles some name rewriting and escaping for type names. Returns the escaped string and the
// number of bytes in the unescaped version.
func getEscapedTypeName(t clangdoc.Type) (string, int) {
	tn := t.QualifiedName()

	rewritten := typeRenames[tn]
	if len(rewritten) > 0 {
		return escapeHtml(rewritten), len(rewritten)
	} else {
		return escapeHtml(tn), len(tn)
	}
}

// Given a list of namespace references (as on FunctionInfo.Namespace), returns the opening and
// closing namespace declarations. This will be formatted such that the beginning and end can be
// printed unconditionally around a declaration.
//
// If there is no namespace, this will return empty strings.
//
// For example, for one namespace
//   - begin = "namespace my_namespace {\n\n"
//   - end = "\n} // my_namespace\n"
//
// See also getNamespaceQualifier().
func getNamespaceDecl(n []clangdoc.Reference) (begin, end string) {
	for i := len(n) - 1; i >= 0; i-- {
		ns := n[i]
		if ns.Type == "Namespace" {
			// Omit "GlobalNamespace". See docs on clangdoc.RecordInfo.Namespace.
			if ns.Name != "GlobalNamespace" {
				begin += fmt.Sprintf("<span class=\"kwd\">namespace</span> %s {\n", ns.Name)
				end = fmt.Sprintf("}  <span class=\"com\">// namespace %s</span>\n", ns.Name) + end
			}
		}
	}
	if begin != "" {
		// We generated something, separate it from the content with blank lines.
		begin += "\n"
		end = "\n" + end
	}
	return
}

// Given a list of namespace and nested class references (as on FunctionInfo.Namespace), returns the
// name qualifier for things inside that namespace and class, so "my_namespace::MyClass::". If there
// are no namespaces, this returns the empty string. It can be unconditionally prepended to names.
//
// includeNamespaces specifies whether namespace qualifications should be included. This would be
// omitted if writing something already qualified with that namespace, as inside a block returned by
// getNamespaceDecl().
func getScopeQualifier(n []clangdoc.Reference, includeNamespaces bool) (result string) {
	for i := len(n) - 1; i >= 0; i-- {
		ns := n[i]
		if ns.Type == "Namespace" {
			// Omit "GlobalNamespace". See docs on clangdoc.RecordInfo.Namespace.
			if includeNamespaces && ns.Name != "GlobalNamespace" {
				result += ns.Name + "::"
			}
		} else {
			// Class/struct qualification.
			result += ns.Name + "::"
		}
	}
	return
}
