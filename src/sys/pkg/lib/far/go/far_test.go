// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package far

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
)

func TestWrite(t *testing.T) {
	files := []string{"a", "b", "dir/c"}
	d := create(t, files)
	inputs := map[string]string{}
	for _, path := range files {
		inputs[path] = filepath.Join(d, path)
	}

	w := bytes.NewBuffer(nil)
	if err := Write(w, inputs); err != nil {
		t.Fatal(err)
	}

	far := w.Bytes()

	if !bytes.Equal(far[0:8], []byte(Magic)) {
		t.Errorf("got %x, want %x", far[0:8], []byte(Magic))
	}

	if !bytes.Equal(far, exampleArchive()) {
		t.Errorf("archives didn't match, got:\n%s", hex.Dump(far))
	}
}

func TestLengths(t *testing.T) {
	if want := 16; IndexLen != want {
		t.Errorf("IndexLen: got %d, want %d", IndexLen, want)
	}
	if want := 24; IndexEntryLen != want {
		t.Errorf("IndexEntryLen: got %d, want %d", IndexEntryLen, want)
	}
	if want := 32; DirectoryEntryLen != want {
		t.Errorf("DirectoryEntryLen: got %d, want %d", DirectoryEntryLen, want)
	}
}

// create makes a temporary directory and populates it with the files in
// the given slice. The files will contain their name as content. The path of
// the created directory is returned.
func create(t *testing.T, files []string) string {
	d := t.TempDir()
	for _, path := range files {
		absPath := filepath.Join(d, path)
		if err := os.MkdirAll(filepath.Dir(absPath), os.ModePerm); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(absPath, []byte(path+"\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	return d
}

func TestReader(t *testing.T) {
	if _, err := NewReader(bytes.NewReader(exampleArchive())); err != nil {
		t.Fatal(err)
	}

	corruptors := []func([]byte){
		// corrupt magic
		func(b []byte) { b[0] = 0 },
		// corrupt index length
		func(b []byte) { binary.LittleEndian.PutUint64(b[8:], 1) },
		// corrupt dirindex type
		func(b []byte) { b[IndexLen] = 255 },
		// TODO(raggi): corrupt index entry offset
		// TODO(raggi): remove index entries
	}

	for i, corrupt := range corruptors {
		far := exampleArchive()
		corrupt(far)
		_, err := NewReader(bytes.NewReader(far))
		if _, ok := err.(ErrInvalidArchive); !ok {
			t.Errorf("corrupt archive %d, got unexpected error %v", i, err)
		}
	}
}

func TestListFiles(t *testing.T) {
	far := exampleArchive()
	r, err := NewReader(bytes.NewReader(far))
	if err != nil {
		t.Fatal(err)
	}

	files := r.List()

	want := []string{"a", "b", "dir/c"}

	if len(files) != len(want) {
		t.Fatalf("listfiles: got %v, want %v", files, want)
	}

	sort.Strings(files)

	for i, want := range want {
		if got := files[i]; got != want {
			t.Errorf("listfiles: got %q, want %q at %d", got, want, i)
		}
	}
}

func TestReaderOpen(t *testing.T) {
	far := exampleArchive()
	r, err := NewReader(bytes.NewReader(far))
	if err != nil {
		t.Fatal(err)
	}

	if got, want := len(r.dirEntries), 3; got != want {
		t.Errorf("got %v, want %v", got, want)
	}

	expectedOffsets := []uint64{4096, 8192, 12288}
	expectedLengths := []uint64{2, 2, 6}

	for i, f := range []string{"a", "b", "dir/c"} {
		ra, err := r.Open(f)
		if err != nil {
			t.Fatal(err)
		}
		if ra.Offset != expectedOffsets[i] {
			t.Errorf("Expected offset %v, got %v for entry %v", expectedOffsets[i], ra.Offset, f)
		}
		if ra.Length != expectedLengths[i] {
			t.Errorf("Expected length %v, got %v for entry %v", expectedLengths[i], ra.Length, f)
		}
		// buffer past the far content padding to check clamping of the readat range
		want := make([]byte, 10*1024)
		copy(want, []byte(f))
		want[len(f)] = '\n'
		got := make([]byte, 10*1024)

		n, err := ra.ReadAt(got, 0)
		if err != nil {
			t.Fatal(err)
		}
		if want := len(f) + 1; n != want {
			t.Errorf("got %d, want %d", n, want)
		}
		if !bytes.Equal(got, want) {
			t.Errorf("got %x, want %x", got, want)
		}
	}

	ra, err := r.Open("a")
	if err != nil {
		t.Fatal(err)
	}
	// ensure that negative offsets are rejected
	n, err := ra.ReadAt(make([]byte, 10), -10)
	if err != io.EOF || n != 0 {
		t.Errorf("got %d %v, want %d, %v", n, err, 0, io.EOF)
	}
	// ensure that offsets beyond length are rejected
	n, err = ra.ReadAt(make([]byte, 10), 10)
	if err != io.EOF || n != 0 {
		t.Errorf("got %d %v, want %d, %v", n, err, 0, io.EOF)
	}
}

func TestReaderReadFile(t *testing.T) {
	far := exampleArchive()
	r, err := NewReader(bytes.NewReader(far))
	if err != nil {
		t.Fatal(err)
	}

	if got, want := len(r.dirEntries), 3; got != want {
		t.Errorf("got %v, want %v", got, want)
	}

	for _, f := range []string{"a", "b", "dir/c"} {
		got, err := r.ReadFile(f)
		if err != nil {
			t.Fatal(err)
		}
		// buffer past the far content padding to check clamping of the readat range
		want := []byte(f + "\n")
		if !bytes.Equal(got, want) {
			t.Errorf("got %x, want %x", got, want)
		}
	}

	ra, err := r.Open("a")
	if err != nil {
		t.Fatal(err)
	}
	// ensure that negative offsets are rejected
	n, err := ra.ReadAt(make([]byte, 10), -10)
	if err != io.EOF || n != 0 {
		t.Errorf("got %d %v, want %d, %v", n, err, 0, io.EOF)
	}
	// ensure that offsets beyond length are rejected
	n, err = ra.ReadAt(make([]byte, 10), 10)
	if err != io.EOF || n != 0 {
		t.Errorf("got %d %v, want %d, %v", n, err, 0, io.EOF)
	}
}

func TestReaderGetSize(t *testing.T) {
	far := exampleArchive()

	r, err := NewReader(bytes.NewReader(far))
	if err != nil {
		t.Fatal(err)
	}

	if got, want := r.GetSize("a"), uint64(2); got != want {
		t.Errorf("GetSize() = %d, want %d", got, want)
	}
}

func TestReadEmpty(t *testing.T) {
	r, err := NewReader(bytes.NewReader(emptyArchive()))
	if err != nil {
		t.Fatal(err)
	}
	if len(r.List()) != 0 {
		t.Error("empty archive should contain no files")
	}
}

func TestIsFAR(t *testing.T) {
	type args struct {
		r io.Reader
	}
	tests := []struct {
		name string
		args args
		want bool
	}{
		{
			name: "valid magic",
			args: args{strings.NewReader(Magic)},
			want: true,
		},
		{
			name: "empty",
			args: args{strings.NewReader("")},
			want: false,
		},
		{
			name: "not magic",
			args: args{strings.NewReader("ohai")},
			want: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := IsFAR(tt.args.r); got != tt.want {
				t.Errorf("IsFAR() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestValidateName(t *testing.T) {
	tests := []struct {
		arg       []byte
		wantError bool
	}{
		{
			arg:       []byte("a"),
			wantError: false,
		},
		{
			arg:       []byte("a/a"),
			wantError: false,
		},
		{
			arg:       []byte("a/a/a"),
			wantError: false,
		},
		{
			arg:       []byte(".a"),
			wantError: false,
		},
		{
			arg:       []byte("a."),
			wantError: false,
		},
		{
			arg:       []byte("..a"),
			wantError: false,
		},
		{
			arg:       []byte("a.."),
			wantError: false,
		},
		{
			arg:       []byte("a./a"),
			wantError: false,
		},
		{
			arg:       []byte("a../a"),
			wantError: false,
		},
		{
			arg:       []byte("a/.a"),
			wantError: false,
		},
		{
			arg:       []byte("a/..a"),
			wantError: false,
		},
		{
			arg:       []byte("/"),
			wantError: true,
		},
		{
			arg:       []byte("/a"),
			wantError: true,
		},
		{
			arg:       []byte("a/"),
			wantError: true,
		},
		{
			arg:       []byte("aa/"),
			wantError: true,
		},
		{
			arg:       []byte{0x0},
			wantError: true,
		},
		{
			arg:       []byte{'a', 0x0},
			wantError: true,
		},
		{
			arg:       []byte{0x0, 'a'},
			wantError: true,
		},
		{
			arg:       []byte{'a', '/', 0x0},
			wantError: true,
		},
		{
			arg:       []byte{0x0, '/', 'a'},
			wantError: true,
		},
		{
			arg:       []byte("a//a"),
			wantError: true,
		},
		{
			arg:       []byte("a/a//a"),
			wantError: true,
		},
		{
			arg:       []byte("."),
			wantError: true,
		},
		{
			arg:       []byte("./a"),
			wantError: true,
		},
		{
			arg:       []byte("a/./"),
			wantError: true,
		},
		{
			arg:       []byte("a/./a"),
			wantError: true,
		},
		{
			arg:       []byte(".."),
			wantError: true,
		},
		{
			arg:       []byte("../a"),
			wantError: true,
		},
		{
			arg:       []byte("../a"),
			wantError: true,
		},
		{
			arg:       []byte("a/../"),
			wantError: true,
		},
		{
			arg:       []byte("a/../a"),
			wantError: true,
		},
		{
			// Valid 2-byte UTF-8 sequence.
			arg:       []byte{0xc3, 0xb1},
			wantError: false,
		},
		{
			// Valid 4-byte UTF-8 sequence.
			arg:       []byte{0xf0, 0x90, 0x8c, 0xbc},
			wantError: false,
		},
		/*
			Currently, validation of UTF-8 is not enforced, see
			https://fxbug.dev/42136376#c6
			Should this change in the future, these are test cases
			for invalid UTF-8 strings:
				{
					// Invalid 2-byte UTF-8 sequence (second octet).
					arg:       []byte{0xc3, 0x20},
					wantError: true,
				},
				{
					// Invalid 4-byte UTF-8 sequence (fourth octet).
					arg:       []byte{0xf0, 0x90, 0x8c, 0x20},
					wantError: true,
				},
		*/
	}
	for _, tt := range tests {
		got := validateName(tt.arg)
		gotError := got != nil
		if gotError != tt.wantError {
			if got != nil {
				t.Errorf("validateName(%#v) = %v, want nil", string(tt.arg), got)
			} else {
				t.Errorf("validateName(%#v) = %v, want nil", string(tt.arg), got)
			}
		}
	}
}

// exampleArchive produces an archive similar to far(1) output
func exampleArchive() []byte {
	b := make([]byte, 16384)
	copy(b, []byte{
		// The magic header.
		0xc8, 0xbf, 0x0b, 0x48, 0xad, 0xab, 0xc5, 0x11,
		// The length of the index entries.
		0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The chunk type.
		0x44, 0x49, 0x52, 0x2d, 0x2d, 0x2d, 0x2d, 0x2d,
		// The offset to the chunk.
		0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The length of the chunk.
		0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The chunk type.
		0x44, 0x49, 0x52, 0x4e, 0x41, 0x4d, 0x45, 0x53,
		// The offset to the chunk.
		0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The length of the chunk.
		0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// A directory chunk.
		// The directory table entry for path "a".
		0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
		0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The directory table entry for path "b".
		0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
		0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The directory table entry for path "c".
		0x02, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00,
		0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The directory names chunk with one byte of padding:
		// 'a','b',  'd',  'i',  'r',  '/',  'c', 0x00
		0x61, 0x62, 0x64, 0x69, 0x72, 0x2f, 0x63, 0x00,
	})
	copy(b[4096:], []byte("a\n"))
	copy(b[8192:], []byte("b\n"))
	copy(b[12288:], []byte("dir/c\n"))
	return b
}

func emptyArchive() []byte {
	b := make([]byte, 16384)
	copy(b, []byte{
		// The magic header.
		0xc8, 0xbf, 0x0b, 0x48, 0xad, 0xab, 0xc5, 0x11,
		// The length of the index entries.
		0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The index entry for the directory chunk.
		//   The chunk type.
		0x44, 0x49, 0x52, 0x2d, 0x2d, 0x2d, 0x2d, 0x2d,
		//   The chunk offset.
		0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		//   The chunk length.
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		// The index entry for the directory names chunk.
		//   The chunk type.
		0x44, 0x49, 0x52, 0x4e, 0x41, 0x4d, 0x45, 0x53,
		//   The chunk offset.
		0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		//   The chunk length.
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	})
	return b
}
