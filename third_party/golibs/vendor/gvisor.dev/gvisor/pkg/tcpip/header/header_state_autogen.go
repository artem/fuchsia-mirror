// automatically generated by stateify.

package header

import (
	"context"

	"gvisor.dev/gvisor/pkg/state"
)

func (t *TCPSynOptions) StateTypeName() string {
	return "pkg/tcpip/header.TCPSynOptions"
}

func (t *TCPSynOptions) StateFields() []string {
	return []string{
		"MSS",
		"WS",
		"TS",
		"TSVal",
		"TSEcr",
		"SACKPermitted",
		"Flags",
	}
}

func (t *TCPSynOptions) beforeSave() {}

// +checklocksignore
func (t *TCPSynOptions) StateSave(stateSinkObject state.Sink) {
	t.beforeSave()
	stateSinkObject.Save(0, &t.MSS)
	stateSinkObject.Save(1, &t.WS)
	stateSinkObject.Save(2, &t.TS)
	stateSinkObject.Save(3, &t.TSVal)
	stateSinkObject.Save(4, &t.TSEcr)
	stateSinkObject.Save(5, &t.SACKPermitted)
	stateSinkObject.Save(6, &t.Flags)
}

func (t *TCPSynOptions) afterLoad(context.Context) {}

// +checklocksignore
func (t *TCPSynOptions) StateLoad(ctx context.Context, stateSourceObject state.Source) {
	stateSourceObject.Load(0, &t.MSS)
	stateSourceObject.Load(1, &t.WS)
	stateSourceObject.Load(2, &t.TS)
	stateSourceObject.Load(3, &t.TSVal)
	stateSourceObject.Load(4, &t.TSEcr)
	stateSourceObject.Load(5, &t.SACKPermitted)
	stateSourceObject.Load(6, &t.Flags)
}

func (r *SACKBlock) StateTypeName() string {
	return "pkg/tcpip/header.SACKBlock"
}

func (r *SACKBlock) StateFields() []string {
	return []string{
		"Start",
		"End",
	}
}

func (r *SACKBlock) beforeSave() {}

// +checklocksignore
func (r *SACKBlock) StateSave(stateSinkObject state.Sink) {
	r.beforeSave()
	stateSinkObject.Save(0, &r.Start)
	stateSinkObject.Save(1, &r.End)
}

func (r *SACKBlock) afterLoad(context.Context) {}

// +checklocksignore
func (r *SACKBlock) StateLoad(ctx context.Context, stateSourceObject state.Source) {
	stateSourceObject.Load(0, &r.Start)
	stateSourceObject.Load(1, &r.End)
}

func (t *TCPOptions) StateTypeName() string {
	return "pkg/tcpip/header.TCPOptions"
}

func (t *TCPOptions) StateFields() []string {
	return []string{
		"TS",
		"TSVal",
		"TSEcr",
		"SACKBlocks",
	}
}

func (t *TCPOptions) beforeSave() {}

// +checklocksignore
func (t *TCPOptions) StateSave(stateSinkObject state.Sink) {
	t.beforeSave()
	stateSinkObject.Save(0, &t.TS)
	stateSinkObject.Save(1, &t.TSVal)
	stateSinkObject.Save(2, &t.TSEcr)
	stateSinkObject.Save(3, &t.SACKBlocks)
}

func (t *TCPOptions) afterLoad(context.Context) {}

// +checklocksignore
func (t *TCPOptions) StateLoad(ctx context.Context, stateSourceObject state.Source) {
	stateSourceObject.Load(0, &t.TS)
	stateSourceObject.Load(1, &t.TSVal)
	stateSourceObject.Load(2, &t.TSEcr)
	stateSourceObject.Load(3, &t.SACKBlocks)
}

func init() {
	state.Register((*TCPSynOptions)(nil))
	state.Register((*SACKBlock)(nil))
	state.Register((*TCPOptions)(nil))
}
