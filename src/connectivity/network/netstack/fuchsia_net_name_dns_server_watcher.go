// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package netstack

import (
	"context"
	"errors"
	"fmt"
	"syscall/zx"
	"syscall/zx/fidl"

	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/dns"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/fidlconv"
	"go.fuchsia.dev/fuchsia/src/connectivity/network/netstack/sync"
	"go.fuchsia.dev/fuchsia/src/lib/component"
	syslog "go.fuchsia.dev/fuchsia/src/lib/syslog/go"

	"fidl/fuchsia/net/name"
)

type dnsServerWatcher struct {
	parent      *dnsServerWatcherCollection
	cancelServe context.CancelFunc
	mu          struct {
		sync.Mutex
		isHanging    bool
		lastObserved []dns.Server
	}
}

var _ name.DnsServerWatcherWithCtx = (*dnsServerWatcher)(nil)

// serverListEquals compares if two slices of configured servers the same elements in the same
// order.
func serverListEquals(a []dns.Server, b []dns.Server) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func dnsServerToFidl(s dns.Server) name.DnsServer {
	var n name.DnsServer
	n.SetAddress(fidlconv.ToNetSocketAddress(s.Address))
	n.SetSource(s.Source)
	return n
}

func (w *dnsServerWatcher) WatchServers(ctx fidl.Context) ([]name.DnsServer, error) {
	w.mu.Lock()
	defer w.mu.Unlock()

	if w.mu.isHanging {
		w.cancelServe()
		return nil, errors.New("not allowed to call DnsServerWatcher.WatchServers when a call is already pending")
	}

	for {
		servers, ch := w.parent.getServersCacheAndChannel()
		if !serverListEquals(servers, w.mu.lastObserved) {
			dnsServer := make([]name.DnsServer, 0, len(servers))
			for _, v := range servers {
				dnsServer = append(dnsServer, dnsServerToFidl(v))
			}
			// Store the last observed servers to compare in subsequent calls.
			w.mu.lastObserved = servers
			return dnsServer, nil
		}

		w.mu.isHanging = true

		w.mu.Unlock()

		var err error
		select {
		case <-ch:
		case <-ctx.Done():
			err = fmt.Errorf("context cancelled during hanging get: %w", ctx.Err())
		}
		w.mu.Lock()

		w.mu.isHanging = false

		if err != nil {
			return nil, err
		}
	}
}

type dnsServerWatcherCollection struct {
	getServersCacheAndChannel func() ([]dns.Server, <-chan struct{})
}

// newDnsServerWatcherCollection creates a new dnsServerWatcherCollection that will observe the
// server configuration provided by getServersCacheAndChannel.
func newDnsServerWatcherCollection(getServersCacheAndChannel func() ([]dns.Server, <-chan struct{})) *dnsServerWatcherCollection {
	collection := dnsServerWatcherCollection{
		getServersCacheAndChannel: getServersCacheAndChannel,
	}
	return &collection
}

// Bind binds a new fuchsia.net.name.DnsServerWatcher request to the collection of watchers and
// starts serving on its channel.
//
// TODO(https://fxbug.dev/134162): Inline this into main.go once GetDnsServerWatcher is removed.
func (c *dnsServerWatcherCollection) Bind(ctx context.Context, ch zx.Channel) error {
	go func() {
		ctx, cancel := context.WithCancel(ctx)
		defer cancel()

		watcher := dnsServerWatcher{
			parent:      c,
			cancelServe: cancel,
		}

		stub := name.DnsServerWatcherWithCtxStub{
			Impl: &watcher,
		}
		component.Serve(ctx, &stub, ch, component.ServeOptions{
			Concurrent: true,
			OnError: func(err error) {
				_ = syslog.WarnTf(name.DnsServerWatcherName, "%s", err)
			},
		})
	}()

	return nil
}
