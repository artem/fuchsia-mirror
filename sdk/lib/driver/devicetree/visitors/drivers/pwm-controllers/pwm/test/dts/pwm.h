// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_PWM_CONTROLLERS_PWM_TEST_DTS_PWM_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_PWM_CONTROLLERS_PWM_TEST_DTS_PWM_H_

#define PIN1 12
#define PIN1_NAME "ENCODER"
#define PIN1_PERIOD 4000000
#define PIN1_FLAG 0x1  // PWM_POLARITY_INVERTED

#define PIN2 30
#define PIN2_NAME "DECODER"
#define PIN2_PERIOD 30000
#define PIN2_FLAG 0x3  // PWM_POLARITY_INVERTED | PWM_SKIP_INIT

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_PWM_CONTROLLERS_PWM_TEST_DTS_PWM_H_
