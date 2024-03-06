// Copyright 2021 The gVisor Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

package stack

import "unsafe"

// PacketBufferStructSize is the minimal size of the packet buffer overhead.
const PacketBufferStructSize = int(unsafe.Sizeof(PacketBuffer{}))

// ID returns a unique ID for the underlying storage of the packet.
//
// Two *PacketBuffers have the same IDs if and only if they point to the same
// location in memory.
func (pk *PacketBuffer) ID() uintptr {
	return uintptr(unsafe.Pointer(pk))
}
