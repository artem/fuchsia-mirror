// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GICV2_TEST_DTS_INTERRUPTS_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GICV2_TEST_DTS_INTERRUPTS_H_

#define IRQ1_SPI 2
#define IRQ1_MODE 4  // active high level-sensitive
#define IRQ1_MODE_FUCHSIA ZX_INTERRUPT_MODE_LEVEL_HIGH
#define IRQ2_PPI 11
#define IRQ2_MODE 2  // high-to-low edge triggered
#define IRQ2_MODE_FUCHSIA ZX_INTERRUPT_MODE_EDGE_LOW

#define IRQ3_SPI 50
#define IRQ3_MODE 1  // low-to-high edge triggered
#define IRQ3_MODE_FUCHSIA ZX_INTERRUPT_MODE_EDGE_HIGH
#define IRQ4_PPI 6
#define IRQ4_MODE 8  // active low level-sensitive
#define IRQ4_MODE_FUCHSIA ZX_INTERRUPT_MODE_LEVEL_LOW

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_INTERRUPT_CONTROLLERS_ARM_GICV2_TEST_DTS_INTERRUPTS_H_
