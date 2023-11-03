// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package artifacts

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path"
	"path/filepath"
	"strconv"
	"strings"

	"golang.org/x/crypto/ssh"

	pmBuild "go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/build"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/avb"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/flasher"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/omaha_tool"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/packages"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/paver"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/zbi"
	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type BlobFetchMode int

const (
	// PrefetchBlobs will download all the blobs from a build when `GetPackageRepository()` is called.
	PrefetchBlobs BlobFetchMode = iota

	// LazilyFetchBlobs will only download blobs when they are accessed.
	LazilyFetchBlobs

	// Product Bundle manifest which is used to locate VBmeta
	ProductBundleManifest = "product_bundle.json"
)

type Build interface {
	// GetBootserver returns the path to the bootserver used for paving.
	GetBootserver(ctx context.Context) (string, error)

	// GetFfx returns the FFXTool from this build.
	GetFfx(ctx context.Context) (*ffx.FFXTool, error)

	// GetFlashManifest returns the path to the flash manifest used for flashing.
	GetFlashManifest(ctx context.Context) (string, error)

	// GetPackageRepository returns a Repository for this build.
	GetPackageRepository(ctx context.Context, blobFetchMode BlobFetchMode, ffx *ffx.FFXTool) (*packages.Repository, error)

	// GetPaverDir downloads and returns the directory containing the images
	// and image manifest.
	GetPaverDir(ctx context.Context) (string, error)

	// GetPaver downloads and returns a paver for the build.
	GetPaver(ctx context.Context) (paver.Paver, error)

	// GetFlasher downloads and returns a paver for the build.
	GetFlasher(ctx context.Context) (flasher.Flasher, error)

	// GetSshPublicKey returns the SSH public key used by this build's paver.
	GetSshPublicKey() ssh.PublicKey

	// GetVbmetaPath downloads and returns a path to the zircon-a vbmeta image.
	GetVbmetaPath(ctx context.Context) (string, error)
}

// ArtifactsBuild represents the build artifacts for a specific build.
type ArtifactsBuild struct {
	id            string
	archive       *Archive
	dir           string
	packages      *packages.Repository
	buildImageDir string
	sshPublicKey  ssh.PublicKey
	srcs          map[string]struct{}
	ffxPath       string
}

func (b *ArtifactsBuild) GetBootserver(ctx context.Context) (string, error) {
	buildPaver, err := b.getPaver(ctx)
	if err != nil {
		return "", err
	}
	return buildPaver.BootserverPath, nil
}

func (b *ArtifactsBuild) GetFfx(ctx context.Context) (*ffx.FFXTool, error) {
	buildFlasher, err := b.getFfxFlasher(ctx)
	if err != nil {
		return nil, err
	}
	return buildFlasher.Ffx, nil
}

func (b *ArtifactsBuild) GetFlashManifest(ctx context.Context) (string, error) {
	ffxFlasher, err := b.getFfxFlasher(ctx)
	if err == nil {
		return ffxFlasher.FlashManifest, nil
	}

	scriptFlasher, err := b.getScriptFlasher(ctx)
	if err != nil {
		return "", err
	}

	return scriptFlasher.FlashManifest, nil
}

// GetPackageRepository returns a Repository for this build. It tries to
// download a package when all the artifacts are stored in individual files,
// which is how modern builds publish their build artifacts.
func (b *ArtifactsBuild) GetPackageRepository(ctx context.Context, fetchMode BlobFetchMode, ffx *ffx.FFXTool) (*packages.Repository, error) {
	if b.packages != nil {
		return b.packages, nil
	}

	logger.Infof(ctx, "downloading package repository")

	// Make sure the blob contains the `packages/all_blobs.json`.
	if _, ok := b.srcs["packages/all_blobs.json"]; !ok {
		logger.Errorf(ctx, "blobs manifest doesn't exist for build %s", b.id)
		return nil, fmt.Errorf("blob manifest doesn't exist for build %s", b.id)
	}

	packageSrcs := []string{}
	for src := range b.srcs {
		if strings.HasPrefix(src, "packages/") {
			packageSrcs = append(packageSrcs, src)
		}
	}

	packagesDir := filepath.Join(b.dir, b.id, "packages")
	if err := b.archive.download(ctx, b.id, false, filepath.Dir(packagesDir), packageSrcs); err != nil {
		logger.Errorf(ctx, "failed to download packages for build %s to %s: %v", packagesDir, b.id, err)
		return nil, fmt.Errorf("failed to download packages for build %s to %s: %w", packagesDir, b.id, err)
	}

	blobsManifest := filepath.Join(packagesDir, "all_blobs.json")
	blobsData, err := os.ReadFile(blobsManifest)
	if err != nil {
		return nil, fmt.Errorf("failed to read blobs manifest: %w", err)
	}

	var blobs []build.Blob
	err = json.Unmarshal(blobsData, &blobs)
	if err != nil {
		return nil, fmt.Errorf("failed to unmarshal blobs JSON: %w", err)
	}

	deliveryBlobConfigPath := filepath.Join(packagesDir, "delivery_blob_config.json")
	blobsDir, err := build.GetBlobsDir(deliveryBlobConfigPath)
	if err != nil {
		return nil, fmt.Errorf("failed to get blobs dir: %w", err)
	}

	var blobsList []string
	for _, blob := range blobs {
		blobsList = append(blobsList, filepath.Join(blobsDir, blob.Merkle))
	}
	logger.Infof(ctx, "all_blobs contains %d blobs", len(blobs))

	blobsDir = filepath.Join(b.dir, "blobs")

	if fetchMode == PrefetchBlobs {
		if err := b.archive.download(ctx, b.id, true, filepath.Dir(blobsDir), blobsList); err != nil {
			logger.Errorf(ctx, "failed to download blobs to %s: %v", blobsDir, err)
			return nil, fmt.Errorf("failed to download blobs to %s: %w", blobsDir, err)
		}
	}

	if ffx == nil {
		build_ffx, err := b.GetFfx(ctx)
		if err != nil {
			return nil, fmt.Errorf("failed to get ffx: %w", err)
		}
		ffx = build_ffx
	}

	blobType, err := build.GetDeliveryBlobType(deliveryBlobConfigPath)
	if err != nil {
		return nil, fmt.Errorf("failed to get delivery blob type: %w", err)
	}

	p, err := packages.NewRepository(ctx, packagesDir, &proxyBlobStore{b, blobsDir}, ffx, blobType)
	if err != nil {
		return nil, err
	}
	b.packages = p

	return b.packages, nil
}

type proxyBlobStore struct {
	b   *ArtifactsBuild
	dir string
}

func (fs *proxyBlobStore) BlobPath(ctx context.Context, deliveryBlobType *int, merkle pmBuild.MerkleRoot) (string, error) {
	var path string
	if deliveryBlobType == nil {
		path = filepath.Join(fs.dir, merkle.String())
	} else {
		path = filepath.Join(fs.dir, strconv.Itoa(*deliveryBlobType), merkle.String())
	}

	// First, try to read the blob from the directory
	if _, err := os.Stat(path); err == nil {
		return path, nil
	}

	// Otherwise, start downloading the blob. The package resolver will only
	// fetch a blob once, so we don't need to deduplicate requests on our side.

	var src string
	if deliveryBlobType == nil {
		src = filepath.Join("blobs", merkle.String())
	} else {
		src = filepath.Join("blobs", strconv.Itoa(*deliveryBlobType), merkle.String())
	}

	logger.Infof(ctx, "downloading %s from build %s", src, fs.b.id)

	if err := fs.b.archive.download(ctx, fs.b.id, true, path, []string{src}); err != nil {
		return "", err
	}

	return path, nil
}

func (fs *proxyBlobStore) OpenBlob(ctx context.Context, deliveryBlobType *int, merkle pmBuild.MerkleRoot) (*os.File, error) {
	path, err := fs.BlobPath(ctx, deliveryBlobType, merkle)
	if err != nil {
		return nil, err
	}

	return os.Open(path)
}

func (fs *proxyBlobStore) BlobSize(ctx context.Context, deliveryBlobType *int, merkle pmBuild.MerkleRoot) (int64, error) {
	f, err := fs.OpenBlob(ctx, deliveryBlobType, merkle)
	if err != nil {
		return 0, err
	}
	defer f.Close()

	if s, err := f.Stat(); err == nil {
		return s.Size(), nil
	} else {
		return 0, err
	}
}

func (fs *proxyBlobStore) Dir() string {
	return fs.dir
}

// GetBuildImages downloads the build images for a specific build id.
// Returns a path to the directory of the downloaded images or an error if it
// fails to download.
func (b *ArtifactsBuild) GetBuildImages(ctx context.Context) (string, error) {
	if b.buildImageDir != "" {
		return b.buildImageDir, nil
	}

	logger.Infof(ctx, "downloading build images")

	imageDir := filepath.Join(b.dir, b.id, "images")
	if err := b.archive.download(ctx, b.id, false, filepath.Join(imageDir, paver.ImageManifest), []string{path.Join("images", paver.ImageManifest)}); err != nil {
		return "", fmt.Errorf("failed to download image manifest: %w", err)
	}
	imagesJSON := filepath.Join(imageDir, paver.ImageManifest)
	f, err := os.Open(imagesJSON)
	if err != nil {
		return "", fmt.Errorf("failed to open %q: %w", imagesJSON, err)
	}
	defer f.Close()

	var items []build.Image
	if err := json.NewDecoder(f).Decode(&items); err != nil {
		return "", fmt.Errorf("failed to parse %q: %w", imagesJSON, err)
	}

	// Get list of all available images to download and only download
	// the ones needed for flashing or paving.
	imageSrcMap := make(map[string]struct{})
	for src := range b.srcs {
		if strings.HasPrefix(src, "images/") {
			imageSrcMap[src] = struct{}{}
		}
	}
	imageSrcs := []string{}
	for _, item := range items {
		src := path.Join("images", item.Path)
		if _, ok := imageSrcMap[src]; ok {
			imageSrcs = append(imageSrcs, src)
		}
	}

	if err := b.archive.download(ctx, b.id, false, filepath.Dir(imageDir), imageSrcs); err != nil {
		return "", fmt.Errorf("failed to download images to %s: %w", imageDir, err)
	}

	b.buildImageDir = imageDir
	return b.buildImageDir, nil
}

func (b *ArtifactsBuild) GetPaverDir(ctx context.Context) (string, error) {
	return b.GetBuildImages(ctx)
}

// GetPaver downloads and returns a paver for the build.
func (b *ArtifactsBuild) GetPaver(ctx context.Context) (paver.Paver, error) {
	return b.getPaver(ctx)
}

func (b *ArtifactsBuild) getPaver(ctx context.Context) (*paver.BuildPaver, error) {
	buildImageDir, err := b.GetBuildImages(ctx)
	if err != nil {
		return nil, err
	}

	currentBuildId := os.Getenv("BUILDBUCKET_ID")
	if currentBuildId == "" {
		currentBuildId = b.id
	}
	// Use the latest bootserver if possible because the one uploaded with the artifacts may not include bug fixes.
	bootserverPath := filepath.Join(buildImageDir, "bootserver")
	if err := b.archive.download(ctx, currentBuildId, false, bootserverPath, []string{"tools/linux-x64/bootserver"}); err != nil {
		return nil, fmt.Errorf("failed to download bootserver: %w", err)
	}
	// Make bootserver executable.
	if err := os.Chmod(bootserverPath, os.ModePerm); err != nil {
		return nil, fmt.Errorf("failed to make bootserver executable: %w", err)
	}

	return paver.NewBuildPaver(bootserverPath, buildImageDir, paver.SSHPublicKey(b.sshPublicKey))
}

// GetFlasher downloads and returns a flasher for the build.
func (b *ArtifactsBuild) GetFlasher(ctx context.Context) (flasher.Flasher, error) {
	if flasher, err := b.getFfxFlasher(ctx); err == nil {
		return flasher, nil
	}

	return b.getScriptFlasher(ctx)
}

func (b *ArtifactsBuild) getFfxFlasher(ctx context.Context) (*flasher.FfxFlasher, error) {
	buildImageDir, err := b.GetBuildImages(ctx)
	if err != nil {
		return nil, err
	}

	currentBuildId := os.Getenv("BUILDBUCKET_ID")
	if currentBuildId == "" {
		currentBuildId = b.id
	}
	// Use the latest ffx
	ffxPath := filepath.Join(buildImageDir, "ffx")
	if b.ffxPath != "" {
		ffxPath = b.ffxPath
	}

	flashManifest := filepath.Join(buildImageDir, "flash.json")
	if err := b.archive.download(ctx, currentBuildId, false, ffxPath, []string{"tools/linux-x64/ffx"}); err != nil {
		return nil, fmt.Errorf("failed to download ffxPath: %w", err)
	}

	if err := b.archive.download(ctx, currentBuildId, false, flashManifest, []string{"images/flash.json"}); err != nil {
		return nil, fmt.Errorf("failed to download flash.json for flasher: %w", err)
	}

	// Make ffx executable.
	if err := os.Chmod(ffxPath, os.ModePerm); err != nil {
		return nil, fmt.Errorf("failed to make ffxPath executable: %w", err)
	}

	ffx, err := ffx.NewFFXTool(ffxPath)
	if err != nil {
		return nil, err
	}
	return flasher.NewFfxFlasher(ffx, flashManifest, false, flasher.SSHPublicKey(b.sshPublicKey))
}

func (b *ArtifactsBuild) getScriptFlasher(ctx context.Context) (*flasher.ScriptFlasher, error) {
	buildImageDir, err := b.GetBuildImages(ctx)
	if err != nil {
		return nil, err
	}
	currentBuildId := os.Getenv("BUILDBUCKET_ID")
	if currentBuildId == "" {
		currentBuildId = b.id
	}
	flashManifest := filepath.Join(buildImageDir, "flash.json")
	flashScript := filepath.Join(buildImageDir, "flash.sh")
	if err := b.archive.download(ctx, currentBuildId, false, flashScript, []string{"images/flash.sh"}); err != nil {
		return nil, fmt.Errorf("failed to download flash.sh flasher: %w", err)
	}

	if err := b.archive.download(ctx, currentBuildId, false, flashManifest, []string{"images/flash.json"}); err != nil {
		return nil, fmt.Errorf("failed to download flash.json for flasher: %w", err)
	}

	if err := os.Chmod(flashScript, os.ModePerm); err != nil {
		return nil, fmt.Errorf("failed to make flash.sh executable: %w", err)
	}

	return flasher.NewScriptFlasher(flashScript, flashManifest, flasher.ScriptFlasherSSHPublicKey(b.sshPublicKey))
}

func (b *ArtifactsBuild) GetSshPublicKey() ssh.PublicKey {
	return b.sshPublicKey
}

func (b *ArtifactsBuild) GetVbmetaPath(ctx context.Context) (string, error) {
	buildImageDir, err := b.GetBuildImages(ctx)
	if err != nil {
		return "", err
	}
	imagesJSON := filepath.Join(buildImageDir, paver.ImageManifest)
	f, err := os.Open(imagesJSON)
	if err != nil {
		return "", fmt.Errorf("failed to open %q: %w", imagesJSON, err)
	}
	defer f.Close()

	var items []struct {
		Name string `json:"name"`
		Path string `json:"path"`
		Type string `json:"type"`
	}
	if err := json.NewDecoder(f).Decode(&items); err != nil {
		return "", fmt.Errorf("failed to parse %q: %w", imagesJSON, err)
	}

	for _, item := range items {
		if item.Name == "zircon-a" && item.Type == "vbmeta" {
			return filepath.Join(buildImageDir, item.Path), nil
		}
	}

	return "", fmt.Errorf("failed to file zircon-a vbmeta in %q", imagesJSON)
}

func (b *ArtifactsBuild) Pave(ctx context.Context, deviceName string) error {
	paver, err := b.GetPaver(ctx)
	if err != nil {
		return err
	}

	return paver.Pave(ctx, deviceName)
}

func (b *ArtifactsBuild) String() string {
	return b.id
}

type FuchsiaDirBuild struct {
	dir          string
	sshPublicKey ssh.PublicKey
}

func NewFuchsiaDirBuild(dir string, publicKey ssh.PublicKey) *FuchsiaDirBuild {
	return &FuchsiaDirBuild{dir: dir, sshPublicKey: publicKey}
}

func (b *FuchsiaDirBuild) String() string {
	return b.dir
}

func (b *FuchsiaDirBuild) GetBootserver(ctx context.Context) (string, error) {
	return filepath.Join(b.dir, "host_x64/bootserver_new"), nil
}

func (b *FuchsiaDirBuild) GetFfx(ctx context.Context) (*ffx.FFXTool, error) {
	ffxPath := filepath.Join(b.dir, "host_x64/ffx")
	return ffx.NewFFXTool(ffxPath)
}

func (b *FuchsiaDirBuild) GetFlashManifest(ctx context.Context) (string, error) {
	return filepath.Join(b.dir, "flash.json"), nil
}

func (b *FuchsiaDirBuild) GetPackageRepository(ctx context.Context, blobFetchMode BlobFetchMode, ffx *ffx.FFXTool) (*packages.Repository, error) {
	blobFS := packages.NewDirBlobStore(filepath.Join(b.dir, "amber-files", "repository", "blobs"))
	if ffx == nil {
		build_ffx, err := b.GetFfx(ctx)
		if err != nil {
			return nil, fmt.Errorf("failed to get ffx: %w", err)
		}
		ffx = build_ffx
	}
	blobType, err := build.GetDeliveryBlobType(filepath.Join(b.dir, "delivery_blob_config.json"))
	if err != nil {
		return nil, fmt.Errorf("failed to get delivery blob type: %w", err)
	}
	return packages.NewRepository(ctx, filepath.Join(b.dir, "amber-files"), blobFS, ffx, blobType)
}

func (b *FuchsiaDirBuild) GetPaverDir(ctx context.Context) (string, error) {
	return b.dir, nil
}

func (b *FuchsiaDirBuild) GetPaver(ctx context.Context) (paver.Paver, error) {
	return paver.NewBuildPaver(
		filepath.Join(b.dir, "host_x64/bootserver_new"),
		b.dir,
		paver.SSHPublicKey(b.sshPublicKey),
	)
}

func (b *FuchsiaDirBuild) GetFlasher(ctx context.Context) (flasher.Flasher, error) {
	ffx, err := b.GetFfx(ctx)
	if err != nil {
		return nil, err
	}
	return flasher.NewFfxFlasher(
		ffx,
		filepath.Join(b.dir, "flash.json"),
		false,
		flasher.SSHPublicKey(b.sshPublicKey),
	)
}

func (b *FuchsiaDirBuild) GetSshPublicKey() ssh.PublicKey {
	return b.sshPublicKey
}

func (b *FuchsiaDirBuild) GetVbmetaPath(ctx context.Context) (string, error) {
	imagesJSON := filepath.Join(b.dir, paver.ImageManifest)
	f, err := os.Open(imagesJSON)
	if err != nil {
		return "", fmt.Errorf("failed to open %q: %w", imagesJSON, err)
	}
	defer f.Close()

	var items []build.Image
	if err := json.NewDecoder(f).Decode(&items); err != nil {
		return "", fmt.Errorf("failed to parse %q: %w", imagesJSON, err)
	}

	for _, item := range items {
		if item.Name == "zircon-a" && item.Type == "vbmeta" {
			return filepath.Join(b.dir, item.Path), nil
		}
	}

	return "", fmt.Errorf("failed to file zircon-a vbmeta in %q", imagesJSON)
}

type ProductBundleDirBuild struct {
	dir          string
	sshPublicKey ssh.PublicKey
}

func NewProductBundleDirBuild(dir string, publicKey ssh.PublicKey) *ProductBundleDirBuild {
	return &ProductBundleDirBuild{dir: dir, sshPublicKey: publicKey}
}

func (b *ProductBundleDirBuild) String() string {
	return b.dir
}

func (b *ProductBundleDirBuild) GetBootserver(ctx context.Context) (string, error) {
	// Only flashing is supported for ProductBundle
	return "", nil
}

func (b *ProductBundleDirBuild) GetFfx(ctx context.Context) (*ffx.FFXTool, error) {
	ffxPath := filepath.Join(b.dir, "ffx")
	return ffx.NewFFXTool(ffxPath)
}

func (b *ProductBundleDirBuild) GetFlashManifest(ctx context.Context) (string, error) {
	return b.dir, nil
}

func (b *ProductBundleDirBuild) GetPackageRepository(ctx context.Context, blobFetchMode BlobFetchMode, ffx *ffx.FFXTool) (*packages.Repository, error) {
	// TODO (fxb/114760) Change to use ffx tool to start package server
	pbJSON := filepath.Join(b.dir, ProductBundleManifest)
	f, err := os.Open(pbJSON)
	if err != nil {
		return nil, fmt.Errorf("failed to open %q: %w", pbJSON, err)
	}
	defer f.Close()

	var productBundle ProductBundle
	if err := json.NewDecoder(f).Decode(&productBundle); err != nil {
		return nil, fmt.Errorf("failed to parse %q: %w", pbJSON, err)
	}
	if productBundle.Version != "2" {
		return nil, fmt.Errorf("Product bundle version is not 2 %q", pbJSON)
	}

	blobFS := packages.NewDirBlobStore(filepath.Join(b.dir, productBundle.Repositories[0].BlobsPath))
	if ffx == nil {
		build_ffx, err := b.GetFfx(ctx)
		if err != nil {
			return nil, fmt.Errorf("failed to get ffx: %w", err)
		}
		ffx = build_ffx
	}
	// TODO(fxbug.dev/126107): Read delivery blob type from product bundle.
	return packages.NewRepository(ctx, b.dir, blobFS, ffx, nil)
}

func (b *ProductBundleDirBuild) GetPaverDir(ctx context.Context) (string, error) {
	// Only flashing is supported for ProductBundle
	return "", nil
}

func (b *ProductBundleDirBuild) GetPaver(ctx context.Context) (paver.Paver, error) {
	// Only flashing is supported for ProductBundle
	return nil, nil
}

func (b *ProductBundleDirBuild) GetFlasher(ctx context.Context) (flasher.Flasher, error) {
	ffx, err := b.GetFfx(ctx)
	if err != nil {
		return nil, err
	}
	return flasher.NewFfxFlasher(
		ffx,
		b.dir,
		true,
		flasher.SSHPublicKey(b.sshPublicKey),
	)
}

func (b *ProductBundleDirBuild) GetSshPublicKey() ssh.PublicKey {
	return b.sshPublicKey
}

type ProductBundle struct {
	SystemA      []build.Image  `json:"system_a"`
	Version      string         `json:"version"`
	Repositories []PBRepository `json:"repositories"`
}

type PBRepository struct {
	MetadataPath string `json:"metadata_path"`
	BlobsPath    string `json:"blobs_path"`
}

func (b *ProductBundleDirBuild) GetVbmetaPath(ctx context.Context) (string, error) {
	pbJSON := filepath.Join(b.dir, ProductBundleManifest)
	f, err := os.Open(pbJSON)
	if err != nil {
		return "", fmt.Errorf("failed to open %q: %w", pbJSON, err)
	}
	defer f.Close()

	var productBundle ProductBundle
	if err := json.NewDecoder(f).Decode(&productBundle); err != nil {
		return "", fmt.Errorf("failed to parse %q: %w", pbJSON, err)
	}
	if productBundle.Version != "2" {
		return "", fmt.Errorf("Product bundle version is not 2 %q", pbJSON)
	}

	for _, item := range productBundle.SystemA {
		if item.Name == "zircon-a" && item.Type == "vbmeta" {
			return filepath.Join(b.dir, item.Path), nil
		}
	}

	return "", fmt.Errorf("failed to find zircon-a vbmeta in %q", pbJSON)
}

type OmahaBuild struct {
	build     Build
	omahatool *omaha_tool.OmahaTool
	avbtool   *avb.AVBTool
	zbitool   *zbi.ZBITool
}

func NewOmahaBuild(build Build, omahatool *omaha_tool.OmahaTool, avbtool *avb.AVBTool, zbitool *zbi.ZBITool) *OmahaBuild {
	return &OmahaBuild{build: build, omahatool: omahatool, avbtool: avbtool, zbitool: zbitool}
}

func (b *OmahaBuild) GetBootserver(ctx context.Context) (string, error) {
	return b.build.GetBootserver(ctx)
}

func (b *OmahaBuild) GetFfx(ctx context.Context) (*ffx.FFXTool, error) {
	return b.build.GetFfx(ctx)
}

func (b *OmahaBuild) GetFlashManifest(ctx context.Context) (string, error) {
	return b.build.GetFlashManifest(ctx)
}

// GetPackageRepository returns a Repository for this build.
func (b *OmahaBuild) GetPackageRepository(ctx context.Context, blobFetchMode BlobFetchMode, ffx *ffx.FFXTool) (*packages.Repository, error) {
	return b.build.GetPackageRepository(ctx, blobFetchMode, ffx)
}

func (b *OmahaBuild) GetPaverDir(ctx context.Context) (string, error) {
	return b.build.GetPaverDir(ctx)
}

// GetPaver downloads and returns a paver for the build.
func (b *OmahaBuild) GetPaver(ctx context.Context) (paver.Paver, error) {
	paverDir, err := b.GetPaverDir(ctx)
	if err != nil {
		return nil, err
	}
	bootserverPath, err := b.GetBootserver(ctx)
	if err != nil {
		return nil, err
	}

	// Create a ZBI with the omaha_url argument.
	tempDir, err := os.MkdirTemp("", "")
	if err != nil {
		return nil, fmt.Errorf("failed to create temp directory: %w", err)
	}
	defer os.RemoveAll(tempDir)

	// Create a ZBI with the omaha_url argument.
	destZbiPath := path.Join(tempDir, "omaha_argument.zbi")
	imageArguments := map[string]string{
		"omaha_url":    b.omahatool.URL(),
		"omaha_app_id": b.omahatool.Args.AppId,
	}

	if err := b.zbitool.MakeImageArgsZbi(ctx, destZbiPath, imageArguments); err != nil {
		return nil, fmt.Errorf("Failed to create ZBI: %w", err)
	}

	// Create a vbmeta that includes the ZBI we just created.
	propFiles := map[string]string{
		"zbi": destZbiPath,
	}

	destVbmetaPath := filepath.Join(paverDir, "zircon-a-omaha-test.vbmeta")

	srcVbmetaPath, err := b.GetVbmetaPath(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to find zircon-a vbmeta: %w", err)
	}

	err = b.avbtool.MakeVBMetaImage(ctx, destVbmetaPath, srcVbmetaPath, propFiles)
	if err != nil {
		return nil, fmt.Errorf("failed to create vbmeta: %w", err)
	}

	return paver.NewBuildPaver(
		bootserverPath,
		paverDir,
		paver.SSHPublicKey(b.GetSshPublicKey()),
		paver.OverrideVBMetaA(destVbmetaPath),
	)
}

// GetFlasher downloads and returns a paver for the build.
func (b *OmahaBuild) GetFlasher(ctx context.Context) (flasher.Flasher, error) {
	paverDir, err := b.GetPaverDir(ctx)
	if err != nil {
		return nil, err
	}
	// Create a ZBI with the omaha_url argument.
	tempDir, err := os.MkdirTemp("", "")
	if err != nil {
		return nil, fmt.Errorf("failed to create temp directory: %w", err)
	}
	defer os.RemoveAll(tempDir)

	// Create a ZBI with the omaha_url argument.
	destZbiPath := path.Join(tempDir, "omaha_argument.zbi")
	imageArguments := map[string]string{
		"omaha_url":    b.omahatool.URL(),
		"omaha_app_id": b.omahatool.Args.AppId,
	}

	if err := b.zbitool.MakeImageArgsZbi(ctx, destZbiPath, imageArguments); err != nil {
		return nil, fmt.Errorf("Failed to create ZBI: %w", err)
	}

	// Create a vbmeta that includes the ZBI we just created.
	propFiles := map[string]string{
		"zbi": destZbiPath,
	}

	destVbmetaPath := filepath.Join(paverDir, "zircon-a-omaha-test.vbmeta")

	srcVbmetaPath, err := b.GetVbmetaPath(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to find zircon-a vbmeta: %w", err)
	}

	err = b.avbtool.MakeVBMetaImage(ctx, destVbmetaPath, srcVbmetaPath, propFiles)
	if err != nil {
		return nil, fmt.Errorf("failed to create vbmeta: %w", err)
	}

	ffx, err := b.GetFfx(ctx)
	if err != nil {
		return nil, err
	}
	flashManifest, err := b.GetFlashManifest(ctx)
	if err != nil {
		return nil, err
	}

	fileInfo, err := os.Stat(flashManifest)
	if err != nil {
		return nil, err
	}

	if fileInfo.IsDir() {
		// We are using Product Bundle instead of flash.json
		os.Rename(destVbmetaPath, srcVbmetaPath)
		return flasher.NewFfxFlasher(ffx, flashManifest, true, flasher.SSHPublicKey(b.GetSshPublicKey()))
	}

	content, err := os.ReadFile(flashManifest)
	if err != nil {
		return nil, err
	}
	updatedContent := strings.Replace(string(content), "fuchsia.vbmeta", "zircon-a-omaha-test.vbmeta", -1)

	updatedFlashManifest := filepath.Join(paverDir, "flash_new.json")
	f, err := os.Create(updatedFlashManifest)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	_, err = f.WriteString(updatedContent)
	if err != nil {
		return nil, err
	}

	return flasher.NewFfxFlasher(ffx, updatedFlashManifest, false, flasher.SSHPublicKey(b.GetSshPublicKey()))
}

func (b *OmahaBuild) GetSshPublicKey() ssh.PublicKey {
	return b.build.GetSshPublicKey()
}

func (b *OmahaBuild) GetVbmetaPath(ctx context.Context) (string, error) {
	return b.build.GetVbmetaPath(ctx)
}
