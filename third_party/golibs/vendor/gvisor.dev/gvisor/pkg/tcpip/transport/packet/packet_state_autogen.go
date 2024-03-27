// automatically generated by stateify.

package packet

import (
	"context"

	"gvisor.dev/gvisor/pkg/state"
)

func (p *packet) StateTypeName() string {
	return "pkg/tcpip/transport/packet.packet"
}

func (p *packet) StateFields() []string {
	return []string{
		"packetEntry",
		"data",
		"receivedAt",
		"senderAddr",
		"packetInfo",
	}
}

func (p *packet) beforeSave() {}

// +checklocksignore
func (p *packet) StateSave(stateSinkObject state.Sink) {
	p.beforeSave()
	var receivedAtValue int64
	receivedAtValue = p.saveReceivedAt()
	stateSinkObject.SaveValue(2, receivedAtValue)
	stateSinkObject.Save(0, &p.packetEntry)
	stateSinkObject.Save(1, &p.data)
	stateSinkObject.Save(3, &p.senderAddr)
	stateSinkObject.Save(4, &p.packetInfo)
}

func (p *packet) afterLoad(context.Context) {}

// +checklocksignore
func (p *packet) StateLoad(ctx context.Context, stateSourceObject state.Source) {
	stateSourceObject.Load(0, &p.packetEntry)
	stateSourceObject.Load(1, &p.data)
	stateSourceObject.Load(3, &p.senderAddr)
	stateSourceObject.Load(4, &p.packetInfo)
	stateSourceObject.LoadValue(2, new(int64), func(y any) { p.loadReceivedAt(ctx, y.(int64)) })
}

func (ep *endpoint) StateTypeName() string {
	return "pkg/tcpip/transport/packet.endpoint"
}

func (ep *endpoint) StateFields() []string {
	return []string{
		"DefaultSocketOptionsHandler",
		"waiterQueue",
		"cooked",
		"ops",
		"stats",
		"rcvList",
		"rcvBufSize",
		"rcvClosed",
		"rcvDisabled",
		"closed",
		"boundNetProto",
		"boundNIC",
		"lastError",
	}
}

// +checklocksignore
func (ep *endpoint) StateSave(stateSinkObject state.Sink) {
	ep.beforeSave()
	stateSinkObject.Save(0, &ep.DefaultSocketOptionsHandler)
	stateSinkObject.Save(1, &ep.waiterQueue)
	stateSinkObject.Save(2, &ep.cooked)
	stateSinkObject.Save(3, &ep.ops)
	stateSinkObject.Save(4, &ep.stats)
	stateSinkObject.Save(5, &ep.rcvList)
	stateSinkObject.Save(6, &ep.rcvBufSize)
	stateSinkObject.Save(7, &ep.rcvClosed)
	stateSinkObject.Save(8, &ep.rcvDisabled)
	stateSinkObject.Save(9, &ep.closed)
	stateSinkObject.Save(10, &ep.boundNetProto)
	stateSinkObject.Save(11, &ep.boundNIC)
	stateSinkObject.Save(12, &ep.lastError)
}

// +checklocksignore
func (ep *endpoint) StateLoad(ctx context.Context, stateSourceObject state.Source) {
	stateSourceObject.Load(0, &ep.DefaultSocketOptionsHandler)
	stateSourceObject.Load(1, &ep.waiterQueue)
	stateSourceObject.Load(2, &ep.cooked)
	stateSourceObject.Load(3, &ep.ops)
	stateSourceObject.Load(4, &ep.stats)
	stateSourceObject.Load(5, &ep.rcvList)
	stateSourceObject.Load(6, &ep.rcvBufSize)
	stateSourceObject.Load(7, &ep.rcvClosed)
	stateSourceObject.Load(8, &ep.rcvDisabled)
	stateSourceObject.Load(9, &ep.closed)
	stateSourceObject.Load(10, &ep.boundNetProto)
	stateSourceObject.Load(11, &ep.boundNIC)
	stateSourceObject.Load(12, &ep.lastError)
	stateSourceObject.AfterLoad(func() { ep.afterLoad(ctx) })
}

func (l *packetList) StateTypeName() string {
	return "pkg/tcpip/transport/packet.packetList"
}

func (l *packetList) StateFields() []string {
	return []string{
		"head",
		"tail",
	}
}

func (l *packetList) beforeSave() {}

// +checklocksignore
func (l *packetList) StateSave(stateSinkObject state.Sink) {
	l.beforeSave()
	stateSinkObject.Save(0, &l.head)
	stateSinkObject.Save(1, &l.tail)
}

func (l *packetList) afterLoad(context.Context) {}

// +checklocksignore
func (l *packetList) StateLoad(ctx context.Context, stateSourceObject state.Source) {
	stateSourceObject.Load(0, &l.head)
	stateSourceObject.Load(1, &l.tail)
}

func (e *packetEntry) StateTypeName() string {
	return "pkg/tcpip/transport/packet.packetEntry"
}

func (e *packetEntry) StateFields() []string {
	return []string{
		"next",
		"prev",
	}
}

func (e *packetEntry) beforeSave() {}

// +checklocksignore
func (e *packetEntry) StateSave(stateSinkObject state.Sink) {
	e.beforeSave()
	stateSinkObject.Save(0, &e.next)
	stateSinkObject.Save(1, &e.prev)
}

func (e *packetEntry) afterLoad(context.Context) {}

// +checklocksignore
func (e *packetEntry) StateLoad(ctx context.Context, stateSourceObject state.Source) {
	stateSourceObject.Load(0, &e.next)
	stateSourceObject.Load(1, &e.prev)
}

func init() {
	state.Register((*packet)(nil))
	state.Register((*endpoint)(nil))
	state.Register((*packetList)(nil))
	state.Register((*packetEntry)(nil))
}
