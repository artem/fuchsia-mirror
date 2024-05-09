// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package upgrade

import (
	"context"
	"fmt"
	"math/rand"
	"strings"
	"time"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/artifacts"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/packages"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type otaData struct {
	name          string
	build         artifacts.Build
	repo          *packages.Repository
	updatePackage *packages.UpdatePackage
}

func (o *otaData) String() string {
	return fmt.Sprintf("%s(%s)", o.name, o.build.String())
}

func newOtas(
	ctx context.Context,
	rand *rand.Rand,
	ffx *ffx.FFXTool,
	builds []artifacts.Build,
) ([]*otaData, error) {
	logger.Infof(ctx, "Preparing OTA packages")

	startTime := time.Now()

	// Fetch all the build artifacts up front so it's not included in the
	// `cycleTimeout` time limit.
	otas := []*otaData{}
	for i, build := range builds {
		var name string
		nth := len(builds) - 1 - i
		if nth == 0 {
			name = "N"
		} else {
			name = fmt.Sprintf("N-%d", nth)
		}

		// We'll initialize with the first build, so we don't need to download the
		// blobs.
		var blobFetchMode artifacts.BlobFetchMode

		// If this isn't the first build, we'll want to modify it to
		// make sure it's unique.
		var addRandomData bool

		if i == 0 {
			blobFetchMode = artifacts.LazilyFetchBlobs
			addRandomData = false
		} else {
			blobFetchMode = artifacts.PrefetchBlobs
			addRandomData = true
		}

		ota, err := newOta(
			ctx,
			rand,
			ffx,
			build,
			name,
			blobFetchMode,
			addRandomData,
		)
		if err != nil {
			return []*otaData{}, err
		}

		otas = append(otas, ota)
	}

	// Finally, redo the last build as our prime build.
	ota, err := newOta(
		ctx,
		rand,
		ffx,
		builds[len(builds)-1],
		"N-prime",
		artifacts.LazilyFetchBlobs,
		true,
	)
	if err != nil {
		return []*otaData{}, err
	}

	otas = append(otas, ota)

	logger.Infof(ctx, "OTAs prepared in %s", time.Now().Sub(startTime))

	return otas, nil
}

func newOta(
	ctx context.Context,
	rand *rand.Rand,
	ffx *ffx.FFXTool,
	build artifacts.Build,
	name string,
	blobFetchMode artifacts.BlobFetchMode,
	addRandomData bool,
) (*otaData, error) {
	logger.Debugf(ctx, "Creating OTA %s", name)

	repo, err := build.GetPackageRepository(ctx, blobFetchMode, ffx.IsolateDir())
	if err != nil {
		return nil, fmt.Errorf("error getting repository: %w", err)
	}

	// Refresh with the latest ffx to make sure the metadata hasn't expired.
	// FIXME(https://fxbug.dev/336897946): We need to use the latest ffx because
	// F11's ffx doesn't actually refresh metadata.
	if err := repo.RefreshMetadataWithFfx(ctx, ffx); err != nil {
		return nil, err
	}

	updatePath := "update/0"
	updatePackage, err := repo.OpenUpdatePackage(ctx, updatePath)
	if err != nil {
		return nil, fmt.Errorf("error opening %s package: %w", updatePath, err)
	}

	// If this isn't the first build, we'll want to modify it to make sure it's
	// unique.
	if addRandomData {
		dstUpdatePath := util.AddSuffixToPackageName(
			updatePackage.Path(),
			strings.ToLower(fmt.Sprintf("ota-test-%s", name)),
		)
		updatePackage, _, err = addRandomFilesToUpdate(
			ctx,
			rand,
			updatePackage,
			dstUpdatePath,
		)
		if err != nil {
			return nil, fmt.Errorf("failed to create update package %s: %w", dstUpdatePath, err)
		}
	}

	ota := &otaData{
		name:          name,
		build:         build,
		repo:          repo,
		updatePackage: updatePackage,
	}

	logger.Infof(ctx, "OTA %s update package: %s", ota, ota.updatePackage)

	return ota, nil
}

// addRandomFilesToUpdate creates a new update package with a system image that
// contains a number of extra files filled with random bytes, which should be
// incompressible. It will loop until it has created an update package that is
// smaller than `-max-ota-size`.
func addRandomFilesToUpdate(
	ctx context.Context,
	rand *rand.Rand,
	srcUpdate *packages.UpdatePackage,
	dstUpdatePath string,
) (*packages.UpdatePackage, *packages.SystemImagePackage, error) {
	avbTool, err := c.installerConfig.AVBTool()
	if err != nil {
		return nil, nil, fmt.Errorf("failed to intialize AVBTool: %w", err)
	}

	zbiTool, err := c.installerConfig.ZBITool()
	if err != nil {
		return nil, nil, fmt.Errorf("failed to initialize ZBITool: %w", err)
	}

	srcSystemImage, err := srcUpdate.OpenSystemImagePackage(ctx)
	if err != nil {
		return nil, nil, err
	}

	systemImageSize, err := srcSystemImage.SystemImageAlignedBlobSize(ctx)
	if err != nil {
		return nil, nil, fmt.Errorf("error determining system image size: %w", err)
	}

	dstSystemImagePath := util.AddSuffixToPackageName(dstUpdatePath, "system-image")

	// Add random files to the system image package in the update. Clamp the
	// package size to the upper bound if we have one, otherwise we'll just add
	// a single block to make it unique.
	dstUpdate, dstSystemImage, err := srcUpdate.EditSystemImagePackage(
		ctx,
		avbTool,
		zbiTool,
		"fuchsia.com",
		dstUpdatePath,
		c.bootfsCompression,
		func(systemImage *packages.SystemImagePackage) (*packages.SystemImagePackage, error) {
			if c.maxSystemImageSize == 0 {
				return systemImage.AddRandomFilesWithAdditionalBytes(
					ctx,
					rand,
					dstSystemImagePath,
					packages.BlobBlockSize,
				)
			} else if c.maxSystemImageSize < systemImageSize {
				return nil, fmt.Errorf(
					"max system image size %d is smaller than the size of the system image %d",
					c.maxSystemImageSize,
					systemImageSize,
				)
			} else {
				return systemImage.AddRandomFilesWithUpperBound(
					ctx,
					rand,
					dstSystemImagePath,
					c.maxSystemImageSize,
				)
			}
		},
	)
	if err != nil {
		return nil, nil, fmt.Errorf(
			"failed to add random files to system images %q in update package %q: %w",
			dstSystemImagePath,
			dstUpdatePath,
			err,
		)
	}

	// Optionally add random files to zbi package in the update images.
	if c.maxUpdateImagesSize != 0 {
		dstZbiPath := util.AddSuffixToPackageName(dstUpdatePath, "update-images-zbi")
		dstUpdate, _, err = dstUpdate.EditUpdateImages(
			ctx,
			dstUpdatePath,
			func(updateImages *packages.UpdateImages) (*packages.UpdateImages, error) {
				return updateImages.AddRandomFilesWithUpperBound(
					ctx,
					rand,
					dstZbiPath,
					c.maxUpdateImagesSize,
				)
			},
		)
		if err != nil {
			return nil, nil, fmt.Errorf(
				"failed to add random files to zbi package %q in update package %q: %w",
				dstZbiPath,
				dstUpdatePath,
				err,
			)
		}
	}

	// Optionally add random files to the update package.
	if c.maxUpdatePackageSize != 0 {
		dstUpdate, err = dstUpdate.EditPackage(
			ctx,
			func(p packages.Package) (packages.Package, error) {
				return p.AddRandomFilesWithUpperBound(
					ctx,
					rand,
					dstUpdatePath,
					c.maxUpdatePackageSize,
				)
			},
		)
		if err != nil {
			return nil, nil, fmt.Errorf(
				"failed to add random files to update package %q: %w",
				dstUpdatePath,
				err,
			)
		}
	}

	return dstUpdate, dstSystemImage, nil
}
