package main

import (
	"encoding/hex"
	"fmt"
	"math/rand"
	"strings"
)

// genString generates a random UTF8 string with a byte length equal or just
// over targetLen.
func genString(maxRune int, seed int64, targetLen int) string {
	random := rand.New(rand.NewSource(seed))
	var r rune
	var b strings.Builder
	for b.Len() < targetLen {
		r = rune(random.Intn(maxRune))
		b.WriteRune(r)
	}
	return b.String()
}

const hexPerLine int = 32

func print(name, src string) {
	dst := make([]byte, hex.EncodedLen(len(src)))
	hex.Encode(dst, []byte(src))

	fmt.Printf("const char %s_S_%d[] = \n", name, len(src))
	for i := 0; i < len(dst); i += 2 {
		if i%hexPerLine == 0 {
			fmt.Printf("\"")
		}
		fmt.Printf("\\x%s", dst[i:i+2])
		if i != len(dst)-2 && i%hexPerLine == hexPerLine-2 {
			fmt.Printf("\"\n")
		}
	}
	fmt.Printf("\";\n")
}

func main() {
	fmt.Print(top)
	// Seeds must be set to preserve benchmark size.
	for _, s := range []struct {
		seed int64
		size int
	}{
		{
			seed: 45,
			size: 257,
		},
		{
			seed: 23,
			size: 1024,
		},
		{
			seed: 34,
			size: 4097,
		},
		{
			seed: 2,
			size: 16384,
		},
		{
			seed: 118,
			size: 65535,
		},
	} {
		s := genString(0x1fffff, s.seed, s.size)
		print("Utf8", s)
	}
	for _, size := range []int{
		258,
		1025,
		4098,
		16385,
		65536,
	} {
		s := genString(128, 0, size)
		print("Ascii", s)
	}
	fmt.Print(bottom)
}

const top = `// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// File is automatically generated; do not edit. To update, use:
//
// go run src/tests/benchmarks/fidl/lib-fidl/gen.go > src/tests/benchmarks/fidl/lib-fidl/data.h
// fx format-code --files=src/tests/benchmarks/fidl/lib-fidl/data.h

#ifndef SRC_TESTS_BENCHMARKS_FIDL_LIB_FIDL_DATA_H_
#define SRC_TESTS_BENCHMARKS_FIDL_LIB_FIDL_DATA_H_

namespace lib_fidl_microbenchmarks {

`

const bottom = `

}  // namespace lib_fidl_microbenchmarks

#endif  // SRC_TESTS_BENCHMARKS_FIDL_LIB_FIDL_DATA_H_
`
