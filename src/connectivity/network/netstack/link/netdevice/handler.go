// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Code generated by go generate; DO NOT EDIT.

package netdevice

import (
	"context"
	"fmt"
	"reflect"
	"syscall/zx"
	"syscall/zx/zxwait"
	"unsafe"

	"fidl/fuchsia/hardware/network"

	"gvisor.dev/gvisor/pkg/tcpip"
	"gvisor.dev/gvisor/pkg/tcpip/stack"
)

// Historical note: this file contains part of the Client implementation to
// better maintain git history from when this contained code shared with the
// Ethernet client implementation.

func (c *Client) txReceiverLoop() error {
	scratch := make([]uint16, c.txDepth)
	for {
		c.tx.mu.Lock()
		detached := c.tx.mu.detached
		c.tx.mu.Unlock()
		if detached {
			return nil
		}

		if err := zxwait.WithRetryContext(context.Background(), func() error {
			status, count := fifoRead(c.txFifo, scratch)
			if status != zx.ErrOk {
				return &zx.Error{Status: status, Text: "fifoRead(TX)"}
			}
			var notSent uint64
			for i := range scratch[:count] {
				descriptor := c.getDescriptor(scratch[i])
				if network.TxReturnFlags(descriptor.return_flags)&network.TxReturnFlagsTxRetError != 0 {
					notSent++
				}
			}
			c.stats.tx.Drops.IncrementBy(notSent)
			c.stats.tx.Reads(count).Increment()
			c.tx.mu.Lock()
			n := c.tx.mu.entries.addReadied(scratch[:count])
			c.tx.mu.entries.incrementReadied(uint16(n))
			c.tx.mu.Unlock()
			c.tx.cond.Broadcast()

			if n := uint32(n); count != n {
				return fmt.Errorf("fifoRead(TX): tx_depth invariant violation; observed=%d expected=%d", c.txDepth-n+count, c.txDepth)
			}
			return nil
		}, c.txFifo, zx.SignalFIFOReadable, zx.SignalFIFOPeerClosed); err != nil {
			return err
		}
	}
}

func (c *Client) rxLoop() error {
	scratch := make([]uint16, c.rxDepth)
	for {
		if batchSize := len(scratch) - int(c.rx.inFlight()); batchSize != 0 && c.rx.haveQueued() {
			n := c.rx.getQueued(scratch[:batchSize])
			c.rx.incrementSent(uint16(n))

			status, count := fifoWrite(c.rxFifo, scratch[:n])
			switch status {
			case zx.ErrOk:
				c.stats.rx.Writes(count).Increment()
				if n := uint32(n); count != n {
					return fmt.Errorf("fifoWrite(RX): rx_depth invariant violation; observed=%d expected=%d", c.rxDepth-n+count, c.rxDepth)
				}
			default:
				return &zx.Error{Status: status, Text: "fifoWrite(RX)"}
			}
		}

		for c.rx.haveReadied() {
			entry := c.rx.getReadied()
			c.processRxDescriptor(*entry)
			c.rx.incrementQueued(1)
		}

		for {
			signals := zx.Signals(zx.SignalFIFOReadable | zx.SignalFIFOPeerClosed)
			if int(c.rx.inFlight()) != len(scratch) && c.rx.haveQueued() {
				signals |= zx.SignalFIFOWritable
			}
			obs, err := zxwait.WaitContext(context.Background(), c.rxFifo, signals)
			if err != nil {
				return err
			}

			if obs&zx.SignalFIFOPeerClosed != 0 {
				return fmt.Errorf("fifoRead(RX): peer closed")
			}
			if obs&zx.SignalFIFOReadable != 0 {
				switch status, count := fifoRead(c.rxFifo, scratch); status {
				case zx.ErrOk:
					c.stats.rx.Reads(count).Increment()
					n := c.rx.addReadied(scratch[:count])
					c.rx.incrementReadied(uint16(n))

					if n := uint32(n); count != n {
						return fmt.Errorf("fifoRead(RX): rx_depth invariant violation; observed=%d expected=%d", c.rxDepth-n+count, c.rxDepth)
					}
				default:
					return &zx.Error{Status: status, Text: "fifoRead(RX)"}
				}
				break
			}
			if obs&zx.SignalFIFOWritable != 0 {
				break
			}
		}
	}
}

func (c *Client) processWrite(port network.PortId, pbList stack.PacketBufferList) (int, tcpip.Error) {
	pkts := pbList.AsSlice()
	i := 0

	for i < len(pkts) {
		c.tx.mu.Lock()
		for {
			if c.tx.mu.detached {
				c.tx.mu.Unlock()
				return i, &tcpip.ErrClosedForSend{}
			}

			if c.tx.mu.entries.haveReadied() {
				break
			}

			c.tx.mu.waiters++
			c.tx.cond.Wait()
			c.tx.mu.waiters--
		}

		// Queue as many remaining packets as possible; if we run out of space,
		// we'll return to the waiting state in the outer loop.
		for ; i < len(pkts) && c.tx.mu.entries.haveReadied(); i++ {
			entry := c.tx.mu.entries.getReadied()
			c.prepareTxDescriptor(*entry, port, pkts[i])
			c.tx.mu.entries.incrementQueued(1)
		}

		batch := c.tx.mu.scratch[:len(c.tx.mu.scratch)-int(c.tx.mu.entries.inFlight())]
		n := c.tx.mu.entries.getQueued(batch)
		c.tx.mu.entries.incrementSent(uint16(n))
		// We must write to the FIFO under lock because `batch` is aliased from
		// `h.tx.mu.scratch`.
		status, count := fifoWrite(c.txFifo, batch[:n])
		c.tx.mu.Unlock()

		switch status {
		case zx.ErrOk:
			if n := uint32(n); count != n {
				panic(fmt.Sprintf("fifoWrite(TX): tx_depth invariant violation; observed=%d expected=%d", c.txDepth-n+count, c.txDepth))
			}
			c.stats.tx.Writes(count).Increment()
		case zx.ErrPeerClosed:
			c.detachTx()
			return i, &tcpip.ErrClosedForSend{}
		case zx.ErrBadHandle:
			// We may have detached then closed the FIFO since we last unlocked before
			// writing to the FIFO.
			c.tx.mu.Lock()
			detached := c.tx.mu.detached
			c.tx.mu.Unlock()
			if detached {
				return i, &tcpip.ErrClosedForSend{}
			}
			fallthrough
		default:
			panic(fmt.Sprintf("fifoWrite(TX): (%v, %d)", status, count))
		}
	}

	return i, nil
}

func (c *Client) detachTx() {
	c.tx.mu.Lock()
	c.tx.mu.detached = true
	c.tx.mu.Unlock()
	c.tx.cond.Broadcast()
}

func fifoWrite(handle zx.Handle, b []uint16) (zx.Status, uint32) {
	var actual uint
	var _x uint16
	data := unsafe.Pointer((*reflect.SliceHeader)(unsafe.Pointer(&b)).Data)

	// TODO(https://fxbug.dev/32098): We're assuming that writing to the FIFO
	// here is a sufficient memory barrier for the other end to access the data.
	// That is currently true but not really guaranteed by the API.

	status := zx.Sys_fifo_write(handle, uint(unsafe.Sizeof(_x)), data, uint(len(b)), &actual)
	return status, uint32(actual)
}

func fifoRead(handle zx.Handle, b []uint16) (zx.Status, uint32) {
	var actual uint
	var _x uint16
	data := unsafe.Pointer((*reflect.SliceHeader)(unsafe.Pointer(&b)).Data)
	status := zx.Sys_fifo_read(handle, uint(unsafe.Sizeof(_x)), data, uint(len(b)), &actual)
	return status, uint32(actual)
}
