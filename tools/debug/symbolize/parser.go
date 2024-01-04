// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package symbolize

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"strconv"
	"strings"
)

// TODO: Implement a reflection based means of automatically doing these conversions.
func str2dec(what string) uint64 {
	what = strings.TrimLeft(what, "0")
	if len(what) == 0 {
		return 0
	}
	out, err := strconv.ParseUint(what, 10, 64)
	if err != nil {
		panic(err.Error())
	}
	return out
}

func str2int(what string) uint64 {
	// str2int assumes that the |what| matches either decRegex or ptrRegex.
	// If we come across a hex value, we don't want to trim the leading zero.
	// If we come across anything else and it still matched one of dec or ptr
	// regexes then we want to trim leading zeros. This lets us match things like
	// "01234" which is important for pids and things like that but also prevents
	// panics like those seen in https://fxbug.dev/27032.
	if !strings.HasPrefix(what, "0x") {
		what = strings.TrimLeft(what, "0")
		if len(what) == 0 {
			return 0
		}
	}
	out, err := strconv.ParseUint(what, 0, 64)
	if err != nil {
		panic(err.Error())
	}
	return out
}

func str2float(what string) float64 {
	out, err := strconv.ParseFloat(what, 64)
	if err != nil {
		panic(err.Error())
	}
	return out
}

const (
	decRegexp   = "(?:[[:digit:]]+)"
	ptrRegexp   = "(?:0|0x[[:xdigit:]]{1,16})"
	strRegexp   = "(?:[^{}:]*)"
	spaceRegexp = `(?:\s*)`
	floatRegexp = `(?:[[:digit:]]+)\.(?:[[:digit:]]+)`
	// Matches datetimes like "2020-10-08 16:12:44".
	datetimeRegexp = `(?:\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})`
)

type ParseLineFunc func(msg string) []Node

func GetLineParser() ParseLineFunc {
	var b regexpTokenizerBuilder
	out := []Node{}
	dec := decRegexp
	ptr := ptrRegexp
	str := strRegexp
	num := fmt.Sprintf("(?:%s|%s)", dec, ptr)
	b.addRule(fmt.Sprintf("{{{bt:(%s):(%s)(?::(%s))?}}}", dec, ptr, str), func(args ...string) {
		out = append(out, &BacktraceElement{
			num:   str2dec(args[1]),
			vaddr: str2int(args[2]),
			msg:   args[3],
		})
	})
	b.addRule(fmt.Sprintf("{{{pc:(%s)}}}", ptr), func(args ...string) {
		out = append(out, &PCElement{vaddr: str2int(args[1])})
	})
	b.addRule(fmt.Sprintf("\033\\[(%s)m", dec), func(args ...string) {
		out = append(out, &ColorCode{color: str2dec(args[1])})
	})
	b.addRule(fmt.Sprintf("{{{dumpfile:(%s):(%s)}}}", str, str), func(args ...string) {
		out = append(out, &DumpfileElement{sinkType: args[1], name: args[2]})
	})
	b.addRule(fmt.Sprintf(`{{{module:(%s):(%s):elf:(%s)}}}`, num, str, str), func(args ...string) {
		out = append(out, &ModuleElement{mod: Module{
			Id:    str2int(args[1]),
			Name:  args[2],
			Build: args[3],
		}})
	})
	b.addRule(fmt.Sprintf(`{{{mmap:(%s):(%s):load:(%s):(%s):(%s)}}}`, ptr, num, num, str, ptr), func(args ...string) {
		out = append(out, &MappingElement{seg: Segment{
			Vaddr:      str2int(args[1]),
			Size:       str2int(args[2]),
			Mod:        str2int(args[3]),
			Flags:      args[4],
			ModRelAddr: str2int(args[5]),
		}})
	})
	b.addRule(`{{{reset}}}`, func(args ...string) {
		out = append(out, &ResetElement{})
	})
	tokenizer, err := b.compile(func(text string) {
		out = append(out, &Text{text: text})
	})
	if err != nil {
		panic(err.Error())
	}
	return func(msg string) []Node {
		out = nil
		tokenizer.run(msg)
		return out
	}
}

func StartParsing(ctx context.Context, reader io.Reader) <-chan InputLine {
	out := make(chan InputLine)
	// This is not used for demuxing. It is a human readable line number.
	var lineno uint64 = 1
	var b regexpTokenizerBuilder
	space := spaceRegexp
	float := floatRegexp
	datetime := datetimeRegexp
	dec := decRegexp
	tags := `[^\[\]]*`
	b.addRule(fmt.Sprintf(`\[(%s)\]%s(%s)[.:](%s)>(.*)$`, float, space, dec, dec), func(args ...string) {
		var hdr logHeader
		var line InputLine
		hdr.time = str2float(args[1])
		hdr.process = str2dec(args[2])
		hdr.thread = str2dec(args[3])
		line.header = hdr
		line.source = Process(hdr.process)
		line.lineno = lineno
		line.msg = args[4]
		out <- line
	})
	b.addRule(fmt.Sprintf(`\[(%s)\]\[(%s)\]\[(%s)\]\[(%s)\] ([A-Z]+):?(.*)$`, float, dec, dec, tags), func(args ...string) {
		var hdr sysLogHeader
		var line InputLine
		hdr.uptime = str2float(args[1])
		hdr.process = str2dec(args[2])
		hdr.thread = str2dec(args[3])
		hdr.tags = args[4]
		hdr.typ = args[5]
		line.header = hdr
		line.source = Process(hdr.process)
		line.lineno = lineno
		line.msg = args[6]
		out <- line
	})
	b.addRule(fmt.Sprintf(`\[(%s)\]\[(%s)\]\[(%s)\]\[(%s)\] ([A-Z]+):?(.*)$`, datetime, dec, dec, tags), func(args ...string) {
		var hdr sysLogHeader
		var line InputLine
		hdr.datetime = args[1]
		hdr.process = str2dec(args[2])
		hdr.thread = str2dec(args[3])
		hdr.tags = args[4]
		hdr.typ = args[5]
		line.header = hdr
		line.source = Process(hdr.process)
		line.lineno = lineno
		line.msg = args[6]
		out <- line
	})
	tokenizer, err := b.compile(func(text string) {
		var line InputLine
		line.source = DummySource{}
		line.msg = text
		line.lineno = lineno
		out <- line
	})
	if err != nil {
		panic(err.Error())
	}
	go func() {
		defer close(out)
		scanner := bufio.NewScanner(reader)
		scanner.Buffer(nil, 1024*1024)
		for ; scanner.Scan(); lineno++ {
			select {
			case <-ctx.Done():
				return
			default:
				tokenizer.run(scanner.Text())
			}
		}
	}()
	return out
}
