// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package component

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"syscall/zx"
	"syscall/zx/fidl"
	"syscall/zx/zxwait"

	"golang.org/x/sync/errgroup"
)

var (
	// ErrTooManyBytesInResponse indicates too many bytes in a response message.
	ErrTooManyBytesInResponse = errors.New("too many bytes in a response")
	// ErrTooManyHandlesInResponse indicates too many handles in a response message.
	ErrTooManyHandlesInResponse = errors.New("too many handles in a response")
)

// ServeOptions contains options for Serve.
type ServeOptions struct {
	// Concurrent controls if requests on the channel are handled concurrently or
	// serially.
	Concurrent bool
	// KeepChannelAlive controls if the channel is closed when Serve returns.
	KeepChannelAlive bool
	// OnError is a logging hook that will be called with errors that cannot be
	// propagated. Must be non-nil.
	OnError func(error)
}

// Serve serves requests from req using stub.
//
// opts contains behavior-modifying options.
func Serve(ctx context.Context, stub fidl.Stub, req zx.Channel, opts ServeOptions) {
	if opts.Concurrent {
		g, ctx := errgroup.WithContext(ctx)
		serveExclusive(g, ctx, stub, req, opts)
		if err := g.Wait(); err != nil {
			opts.handleServeError(ctx, err)
		}
		return
	}
	serveExclusive(nil, ctx, stub, req, opts)
}

var bytesPool = sync.Pool{
	New: func() interface{} {
		return make([]byte, zx.ChannelMaxMessageBytes)
	},
}

var handleInfosPool = sync.Pool{
	New: func() interface{} {
		return make([]zx.HandleInfo, zx.ChannelMaxMessageHandles)
	},
}

var handleDispositionsPool = sync.Pool{
	New: func() interface{} {
		return make([]zx.HandleDisposition, zx.ChannelMaxMessageHandles)
	},
}

func serveOne(g *errgroup.Group, ctx context.Context, stub fidl.Stub, req zx.Channel, onError func(error)) error {
	bRaw := bytesPool.Get()
	hiRaw := handleInfosPool.Get()

	// Set up deferred cleanup before type-asserting (which can panic).
	var hi []zx.HandleInfo
	cleanup := func() {
		// Arrange for unconsumed handles to close on return.
		for _, hi := range hi {
			if err := hi.Handle.Close(); err != nil {
				onError(fmt.Errorf("failed to close unconsumed inbound handle: %w", err))
			}
		}
		bytesPool.Put(bRaw)
		handleInfosPool.Put(hiRaw)
	}
	defer func() {
		if fn := cleanup; fn != nil {
			fn()
		}
	}()

	b := bRaw.([]byte)
	bOrig := b
	hi = hiRaw.([]zx.HandleInfo)[:0]

	var nb, nhi uint32
	if err := zxwait.WithRetryContext(ctx, func() error {
		var err error
		nb, nhi, err = req.ReadEtc(b, hi[:cap(hi)], 0)
		return err
	}, *req.Handle(), zx.SignalChannelReadable, zx.SignalChannelPeerClosed); err != nil {
		return err
	}
	b = b[:nb]
	hi = hi[:nhi]

	if len(b) < fidl.MessageHeaderSize {
		return fidl.ErrPayloadTooSmall
	}

	var reqHeader fidl.MessageHeader
	if err := fidl.Unmarshal(fidl.NewCtx(), b[:fidl.MessageHeaderSize], nil, &reqHeader); err != nil {
		return err
	}
	if err := reqHeader.ValidateWireFormat(); err != nil {
		return err
	}
	b = b[fidl.MessageHeaderSize:]

	marshalerCtx := reqHeader.NewCtx()

	// We've done all the synchronous work we can. This closure calls into the
	// stub, which the caller may have requested to happen concurrently.
	//
	// Move deferred cleanup into the closure.
	movedCleanup := cleanup
	cleanup = nil
	dispatch := func() error {
		defer movedCleanup()

		p, shouldRespond, err := stub.Dispatch(fidl.DispatchArgs{
			Ctx:         fidl.WithMarshalerContext(ctx, marshalerCtx),
			Ordinal:     reqHeader.Ordinal,
			Bytes:       b,
			HandleInfos: hi,
		})
		if err != nil {
			return err
		}

		// Consumed, prevent cleanup.
		hi = hi[:0]

		if shouldRespond {
			hdRaw := handleDispositionsPool.Get()
			defer handleDispositionsPool.Put(hdRaw)

			// Arrange for unconsumed handles to close on return.
			hd := hdRaw.([]zx.HandleDisposition)[:0]
			defer func() {
				for _, hd := range hd {
					if err := hd.Handle.Close(); err != nil {
						onError(fmt.Errorf("failed to close unconsumed outbound handle: %w", err))
					}
				}
			}()

			b = bOrig

			respHeader := marshalerCtx.NewHeader()
			respHeader.Ordinal = reqHeader.Ordinal
			respHeader.Txid = reqHeader.Txid
			cnb, cnh, err := fidl.MarshalHeaderThenMessage(&respHeader, p, b, hd[:cap(hd)])
			if err != nil {
				return err
			}
			if cnb > cap(b) {
				return fmt.Errorf("slice bounds out of range [:%d] with capacity %d (%w)", cnb, cap(b), ErrTooManyBytesInResponse)
			}
			b = b[:cnb]
			if cnh > cap(hd) {
				return fmt.Errorf("slice bounds out of range [:%d] with capacity %d (%w)", cnh, cap(hd), ErrTooManyHandlesInResponse)
			}
			hd = hd[:cnh]
			if err := req.WriteEtc(b, hd, 0); err != nil {
				return err
			}

			// Consumed, prevent cleanup.
			hd = hd[:0]
		}

		return nil
	}

	// The caller requested concurrent dispatch.
	if g != nil {
		g.Go(dispatch)
		return nil
	}
	return dispatch()
}

func serve(g *errgroup.Group, ctx context.Context, stub fidl.Stub, req zx.Channel, onError func(error)) error {
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		// Wait for an incoming message before calling serveOne, which acquires
		// pooled memory to read into. This technique avoids O(requests) memory
		// usage, which yields substantial savings when the number of idle requests
		// is high.
		if _, err := zxwait.WaitContext(ctx, *req.Handle(), zx.SignalChannelReadable|zx.SignalChannelPeerClosed); err != nil {
			return err
		}
		if err := serveOne(g, ctx, stub, req, onError); err != nil {
			return err
		}
	}
}

type Epitaph struct {
	Status zx.Status
}

var _ error = (*Epitaph)(nil)

func (e *Epitaph) Error() string {
	return fmt.Sprintf("epitaph: %s", e.Status)
}

func serveExclusive(g *errgroup.Group, ctx context.Context, stub fidl.Stub, req zx.Channel, opts ServeOptions) {
	if !opts.KeepChannelAlive {
		defer func() {
			if err := req.Close(); err != nil {
				opts.handleServeError(ctx, fmt.Errorf("failed to close request channel: %w", err))
			}
		}()
	}

	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	if err := serve(g, ctx, stub, req, func(err error) {
		opts.handleServeError(ctx, err)
	}); err != nil {
		opts.handleServeError(ctx, err)

		if !opts.KeepChannelAlive {
			var epitaph *Epitaph
			if errors.As(err, &epitaph) {
				if err := writeEpitaph(req, epitaph.Status); err != nil {
					opts.handleServeError(ctx, fmt.Errorf("failed to write epitaph(%s): %w", &epitaph.Status, err))
				}
			}
		}
	}
}

func writeEpitaph(req zx.Channel, status zx.Status) error {
	marshalerCtx := fidl.NewCtx()
	header := marshalerCtx.NewHeader()
	header.Ordinal = fidl.EpitaphOrdinal
	epitaph := fidl.EpitaphBody{
		Error: status,
	}
	var b [24]byte
	cnb, cnh, err := fidl.MarshalHeaderThenMessage(&header, &epitaph, b[:], nil)
	if err != nil {
		panic(err)
	}
	if got, want := cnb, len(b); got != want {
		panic(fmt.Sprintf("got encoded epitaph size = %d bytes, want %d", got, want))
	}
	if got, want := cnh, 0; got != want {
		panic(fmt.Sprintf("got encoded epitaph size = %d handles, want %d", got, want))
	}
	return req.WriteEtc(b[:], nil, 0)
}

func CloseWithEpitaph(req zx.Channel, status zx.Status) error {
	if err := writeEpitaph(req, status); err != nil {
		return fmt.Errorf("write epitaph: %w", err)
	}
	if err := req.Close(); err != nil {
		return fmt.Errorf("close channel: %w", err)
	}
	return nil
}

func (o *ServeOptions) handleServeError(ctx context.Context, err error) {
	if err == ctx.Err() {
		return
	}
	if err, ok := err.(*zx.Error); ok && err.Status == zx.ErrPeerClosed {
		return
	}
	o.OnError(fmt.Errorf("serving terminated: %w", err))
}
