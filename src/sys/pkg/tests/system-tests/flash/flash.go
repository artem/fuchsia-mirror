// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package flash

import (
	"context"
	"fmt"
	"time"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/artifacts"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/device"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"golang.org/x/crypto/ssh"
)

func FlashDevice(
	ctx context.Context,
	d *device.Client,
	ffx *ffx.FFXTool,
	build artifacts.Build,
	publicKey ssh.PublicKey,
) error {
	logger.Infof(ctx, "Starting to flash device")
	startTime := time.Now()

	if err := d.Flash(ctx, ffx, build, publicKey); err != nil {
		return fmt.Errorf("device failed to flash: %w", err)
	}

	if err := d.Reconnect(ctx); err != nil {
		return fmt.Errorf("device failed to connect after flash: %w", err)
	}

	logger.Infof(ctx, "device booted")
	logger.Infof(ctx, "Flashing successful in %s", time.Now().Sub(startTime))

	return nil
}
