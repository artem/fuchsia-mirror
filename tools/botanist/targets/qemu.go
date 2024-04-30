// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"context"
	"debug/pe"
	"encoding/hex"
	"fmt"
	"io"
	"math/rand"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"go.fuchsia.dev/fuchsia/tools/bootserver"
	"go.fuchsia.dev/fuchsia/tools/botanist"
	"go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	"go.fuchsia.dev/fuchsia/tools/net/sshutil"
	"go.fuchsia.dev/fuchsia/tools/qemu"
	testrunnerconstants "go.fuchsia.dev/fuchsia/tools/testing/testrunner/constants"

	"github.com/creack/pty"
)

const (
	// qemuSystemPrefix is the prefix of the QEMU binary name, which is of the
	// form qemu-system-<QEMU arch suffix>.
	qemuSystemPrefix = "qemu-system"

	// DefaultInterfaceName is the name given to the emulated tap interface.
	defaultInterfaceName = "qemu"

	// DefaultQEMUNodename is the default nodename given to a QEMU target.
	DefaultQEMUNodename = "botanist-target-qemu"

	// The size in bytes of minimimum desired size for the storage-full image.
	// The image should be large enough to hold all downloaded test packages
	// for a given test shard.
	//
	// No host-side disk blocks are allocated on extension (by use of the `fvm`
	// host tool), so the operation is cheap regardless of the size we extend to.
	storageFullMinSize int64 = 17179869184 // 16 GiB

	// Minimum number of bytes of entropy bits required for the kernel's PRNG.
	minEntropyBytes uint = 32 // 256 bits

	// The experiment level at which to enable `ffx emu`.
	ffxEmuExperimentLevel = 1
)

// qemuTargetMapping maps the Fuchsia target name to the name recognized by QEMU.
var qemuTargetMapping = map[string]qemu.Target{
	"x64":     qemu.TargetEnum.X86_64,
	"arm64":   qemu.TargetEnum.AArch64,
	"riscv64": qemu.TargetEnum.RiscV64,
}

// MinFS is the configuration for the MinFS filesystem image.
type MinFS struct {
	// Image is the path to the filesystem image.
	Image string `json:"image"`

	// PCIAddress is the PCI address to map the device at.
	PCIAddress string `json:"pci_address"`
}

// QEMUConfig is a QEMU configuration.
type QEMUConfig struct {
	// Path is a path to a directory that contains QEMU system binary.
	Path string `json:"path"`

	// EDK2Dir is a path to a directory of EDK II (UEFI) prebuilts.
	EDK2Dir string `json:"edk2_dir"`

	// Target is the QEMU target to emulate.
	Target string `json:"target"`

	// CPU is the number of processors to emulate.
	CPU int `json:"cpu"`

	// Memory is the amount of memory (in MB) to provide.
	Memory int `json:"memory"`

	// KVM specifies whether to enable hardware virtualization acceleration.
	KVM bool `json:"kvm"`

	// Serial gives whether to create a 'serial device' for the QEMU instance.
	// This option should be used judiciously, as it can slow the process down.
	Serial bool `json:"serial"`

	// Logfile saves emulator standard output to a file if set.
	Logfile string `json:"logfile"`

	// Whether User networking is enabled; if false, a Tap interface will be used.
	UserNetworking bool `json:"user_networking"`

	// MinFS is the filesystem to mount as a device.
	MinFS *MinFS `json:"minfs,omitempty"`

	// Path to the fvm host tool.
	FVMTool string `json:"fvm_tool"`

	// Path to the zbi host tool.
	ZBITool string `json:"zbi_tool"`
}

// QEMU is a QEMU target.
type QEMU struct {
	*genericFuchsiaTarget
	binary  string
	builder EMUCommandBuilder
	config  QEMUConfig
	opts    Options
	c       chan error
	process *os.Process
	mac     [6]byte
	serial  io.ReadWriteCloser
	ptm     *os.File
	// isQEMU distinguishes a QEMU from an AEMU since AEMU inherits from QEMU.
	// TODO(ihuh): Refactor this to be a general EMU target so that a QEMU would
	// always have isQEMU=true, as its name implies.
	isQEMU bool
}

var _ FuchsiaTarget = (*QEMU)(nil)

// EMUCommandBuilder defines the common set of functions used to build up an
// EMU command-line.
type EMUCommandBuilder interface {
	SetFlag(...string)
	SetBinary(string)
	SetKernel(string)
	SetInitrd(string)
	SetUEFIVolumes(string, string, string)
	SetTarget(qemu.Target, bool)
	SetMemory(int)
	SetCPUCount(int)
	AddVirtioBlkPciDrive(qemu.Drive)
	AddRNG(qemu.RNGdev)
	AddSerial(qemu.Chardev)
	AddNetwork(qemu.Netdev)
	AddKernelArg(string)
	Build() ([]string, error)
	BuildConfig() (qemu.Config, error)
}

// NewQEMU returns a new QEMU target with a given configuration.
func NewQEMU(ctx context.Context, config QEMUConfig, opts Options) (*QEMU, error) {
	qemuTarget, ok := qemuTargetMapping[config.Target]
	if !ok {
		return nil, fmt.Errorf("invalid target %q", config.Target)
	}

	t := &QEMU{
		binary:  fmt.Sprintf("%s-%s", qemuSystemPrefix, qemuTarget),
		builder: &qemu.QEMUCommandBuilder{},
		config:  config,
		opts:    opts,
		c:       make(chan error),
		isQEMU:  true,
	}
	r := rand.New(rand.NewSource(time.Now().UnixNano()))
	if _, err := r.Read(t.mac[:]); err != nil {
		return nil, fmt.Errorf("failed to generate random MAC: %w", err)
	}
	// Ensure that the generated MAC address is unicast
	// https://en.wikipedia.org/wiki/MAC_address#Unicast_vs._multicast_(I/G_bit)
	t.mac[0] &= ^uint8(0x01)

	if config.Serial {
		// We can run QEMU 'in a terminal' by creating a pseudoterminal slave and
		// attaching it as the process' std(in|out|err) streams. Running it in a
		// terminal - and redirecting serial to stdio - allows us to use the
		// associated pseudoterminal master as the 'serial device' for the
		// instance.
		var err error
		// TODO(joshuaseaton): Figure out how to manage ownership so that this may
		// be closed.
		t.ptm, t.serial, err = pty.Open()
		if err != nil {
			return nil, fmt.Errorf("failed to create ptm/pts pair: %w", err)
		}
	}
	base, err := newGenericFuchsia(ctx, DefaultQEMUNodename, "", []string{opts.SSHKey}, t.serial)
	if err != nil {
		return nil, err
	}
	t.genericFuchsiaTarget = base
	return t, nil
}

// Nodename returns the name of the target node.
func (t *QEMU) Nodename() string {
	return DefaultQEMUNodename
}

// Serial returns the serial device associated with the target for serial i/o.
func (t *QEMU) Serial() io.ReadWriteCloser {
	return t.serial
}

// SSHKey returns the private SSH key path associated with a previously embedded authorized key.
func (t *QEMU) SSHKey() string {
	return t.opts.SSHKey
}

// SSHClient creates and returns an SSH client connected to the QEMU target.
func (t *QEMU) SSHClient() (*sshutil.Client, error) {
	addr, err := t.IPv6()
	if err != nil {
		return nil, err
	}
	return t.sshClient(addr, "qemu")
}

// Start starts the QEMU target.
// TODO(https://fxbug.dev/42177999): Add logic to use PB with ffx emu
func (t *QEMU) Start(ctx context.Context, images []bootserver.Image, args []string, pbPath string, isBootTest bool) (err error) {
	if t.process != nil {
		return fmt.Errorf("a process has already been started with PID %d", t.process.Pid)
	}
	qemuCmd := t.builder

	qemuTarget, ok := qemuTargetMapping[t.config.Target]
	if !ok {
		return fmt.Errorf("invalid target %q", t.config.Target)
	}
	qemuCmd.SetTarget(qemuTarget, t.config.KVM)

	if t.config.Path == "" {
		return fmt.Errorf("directory must be set")
	}
	qemuSystem := filepath.Join(t.config.Path, t.binary)
	absQEMUSystemPath, err := normalizeFile(qemuSystem)
	if err != nil {
		return fmt.Errorf("could not find qemu binary %q: %w", qemuSystem, err)
	}
	qemuCmd.SetBinary(absQEMUSystemPath)

	if pbPath == "" {
		return fmt.Errorf("missing product bundle")
	}

	// If a QEMU kernel was specified, use that; else, a UEFI disk image (which does not
	// require a QEMU kernel) must be specified in the product bundle to use.
	var qemuKernel, efiDisk *bootserver.Image
	// `ffx product get-image-path` prints an error message if it can't find the
	// requested image in the product bundle, which is ok. Since the error message
	// is confusing, we'll discard the output and only return an error if a
	// required image is missing.
	resetStdoutStderr := func() {
		origStdout := t.ffx.Stdout()
		origStderr := t.ffx.Stderr()
		t.ffx.SetStdoutStderr(origStdout, origStderr)
	}
	t.ffx.SetStdoutStderr(io.Discard, io.Discard)
	qemuKernel, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "qemu-kernel", "")
	if err != nil {
		return err
	}
	if qemuKernel == nil {
		if !isBootTest {
			return fmt.Errorf("failed to find qemu kernel from product bundle")
		}
		efiDisk, err = t.ffx.GetImageFromPB(ctx, pbPath, "", "", "firmware_fat")
		if err != nil {
			return err
		}
		if efiDisk == nil {
			return fmt.Errorf("failed to find either qemu kernel or efi disk from product bundle")
		}
	}

	var zbi *bootserver.Image
	zbi, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "zbi", "")
	if err != nil {
		return err
	}
	// The zbi image may not exist as part of the
	// product bundle for a boot test, which is ok.
	if zbi == nil && !isBootTest {
		return fmt.Errorf("failed to find zbi from product bundle")
	}

	// The QEMU command needs to be invoked within an empty directory, as QEMU
	// will attempt to pick up files from its working directory, one notable
	// culprit being multiboot.bin. This can result in strange behavior.
	workdir, err := os.MkdirTemp("", "qemu-working-dir")
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			os.RemoveAll(workdir)
		}
	}()

	var fvmImage *bootserver.Image
	var fxfsImage *bootserver.Image
	fvmImage, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "fvm", "")
	if err != nil {
		return err
	}
	fxfsImage, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "fxfs", "")
	if err != nil {
		return err
	}
	resetStdoutStderr()

	if err := copyImagesToDir(ctx, workdir, false, qemuKernel, zbi, efiDisk, fvmImage, fxfsImage); err != nil {
		return err
	}

	if zbi != nil && t.config.ZBITool != "" {
		if err := embedZBIWithKey(ctx, zbi, t.config.ZBITool, t.opts.AuthorizedKey); err != nil {
			return fmt.Errorf("failed to embed zbi with key: %w", err)
		}
	}

	// Now that the images hav successfully been copied to the working
	// directory, Path points to their path on disk.
	if qemuKernel != nil {
		qemuCmd.SetKernel(qemuKernel.Path)
	}
	if zbi != nil {
		qemuCmd.SetInitrd(zbi.Path)
	}

	// Checks whether the reader represents a PE (Portable Executable) file, the
	// format of UEFI applications.
	isPE := func(r io.ReaderAt) bool {
		_, err := pe.NewFile(r)
		return err == nil
	}
	// If a UEFI filesystem/disk image was specified, or if the provided QEMU
	// kernel is a UEFI application, ensure that QEMU boots through UEFI.
	if efiDisk != nil || (qemuKernel != nil && isPE(qemuKernel.Reader)) {
		edk2Dir := filepath.Join(t.config.EDK2Dir, "qemu-"+t.config.Target)
		var code, data string
		switch t.config.Target {
		case "x64":
			code = filepath.Join(edk2Dir, "OVMF_CODE.fd")
			data = filepath.Join(edk2Dir, "OVMF_VARS.fd")
		case "arm64":
			code = filepath.Join(edk2Dir, "QEMU_EFI.fd")
			data = filepath.Join(edk2Dir, "QEMU_VARS.fd")
		}
		code, err = normalizeFile(code)
		if err != nil {
			return err
		}
		data, err = normalizeFile(data)
		if err != nil {
			return err
		}
		diskPath := ""
		if efiDisk != nil {
			diskPath = efiDisk.Path
		}
		qemuCmd.SetUEFIVolumes(code, data, diskPath)
	}

	if fxfsImage != nil {
		if err := extendImage(ctx, fxfsImage, storageFullMinSize); err != nil {
			return fmt.Errorf("%s to %d bytes: %w", constants.FailedToExtendBlkMsg, storageFullMinSize, err)
		}
		qemuCmd.AddVirtioBlkPciDrive(qemu.Drive{
			ID:   "maindisk",
			File: fxfsImage.Path,
		})
	} else if fvmImage != nil {
		if t.config.FVMTool != "" {
			if err := extendFvmImage(ctx, fvmImage, t.config.FVMTool, storageFullMinSize); err != nil {
				return fmt.Errorf("%s to %d bytes: %w", constants.FailedToExtendFVMMsg, storageFullMinSize, err)
			}
		}
		qemuCmd.AddVirtioBlkPciDrive(qemu.Drive{
			ID:   "maindisk",
			File: fvmImage.Path,
		})
	}

	if t.config.MinFS != nil {
		absMinFsPath, err := normalizeFile(t.config.MinFS.Image)
		if err != nil {
			return fmt.Errorf("could not find minfs image %q: %w", t.config.MinFS.Image, err)
		}
		// Swarming hard-links Isolate downloads with a cache and the very same
		// cached minfs image will be used across multiple tasks. To ensure
		// that it remains blank, we must break its link.
		if err := overwriteFileWithCopy(absMinFsPath); err != nil {
			return err
		}
		qemuCmd.AddVirtioBlkPciDrive(qemu.Drive{
			ID:   "testdisk",
			File: absMinFsPath,
			Addr: t.config.MinFS.PCIAddress,
		})
	}

	netdev := qemu.Netdev{
		ID:     "net0",
		Device: qemu.Device{Model: qemu.DeviceModelVirtioNetPCI},
	}
	netdev.Device.AddOption("mac", net.HardwareAddr(t.mac[:]).String())
	netdev.Device.AddOption("vectors", "8")
	if t.config.UserNetworking {
		netdev.User = &qemu.NetdevUser{}
	} else {
		netdev.Tap = &qemu.NetdevTap{Name: defaultInterfaceName}
	}

	qemuCmd.AddNetwork(netdev)

	// If we're running on RISC-V, we need to add an RNG device.
	if t.config.Target == "riscv64" {
		rngdev := qemu.RNGdev{
			ID:       "rng0",
			Device:   qemu.Device{Model: qemu.DeviceModelRNG},
			Filename: "/dev/urandom",
		}
		qemuCmd.AddRNG(rngdev)
	}

	chardev := qemu.Chardev{
		ID:     "char0",
		Signal: false,
	}
	qemuCmd.AddSerial(chardev)

	// Manually set nodename, since MAC is randomly generated.
	qemuCmd.AddKernelArg("zircon.nodename=" + DefaultQEMUNodename)
	// Disable the virtcon.
	qemuCmd.AddKernelArg("virtcon.disable=true")
	// The system will halt on a kernel panic instead of rebooting.
	qemuCmd.AddKernelArg("kernel.halt-on-panic=true")
	// Print a message if `dm poweroff` times out.
	qemuCmd.AddKernelArg("devmgr.suspend-timeout-debug=true")
	// Disable kernel lockup detector in emulated environments to prevent false alarms from
	// potentially oversubscribed hosts.
	qemuCmd.AddKernelArg("kernel.lockup-detector.critical-section-threshold-ms=0")
	qemuCmd.AddKernelArg("kernel.lockup-detector.critical-section-fatal-threshold-ms=0")
	qemuCmd.AddKernelArg("kernel.lockup-detector.heartbeat-period-ms=0")
	qemuCmd.AddKernelArg("kernel.lockup-detector.heartbeat-age-threshold-ms=0")
	qemuCmd.AddKernelArg("kernel.lockup-detector.heartbeat-age-fatal-threshold-ms=0")
	// TODO(https://fxbug.dev/42083858): Remove when set by default.
	if !strings.Contains(strings.Join(args, " "), "kernel.experimental.serial_migration") {
		qemuCmd.AddKernelArg("kernel.experimental.serial_migration=true")
	}

	// Add entropy to simulate bootloader entropy.
	entropy := make([]byte, minEntropyBytes)
	if _, err := rand.Read(entropy); err == nil {
		qemuCmd.AddKernelArg("kernel.entropy-mixin=" + hex.EncodeToString(entropy))
	}
	// Do not print colors.
	qemuCmd.AddKernelArg("TERM=dumb")
	if t.config.Target == "x64" {
		// Necessary to redirect to stdout.
		qemuCmd.AddKernelArg("kernel.serial=legacy")
	}
	for _, arg := range args {
		qemuCmd.AddKernelArg(arg)
	}

	qemuCmd.SetCPUCount(t.config.CPU)
	qemuCmd.SetMemory(t.config.Memory)
	qemuCmd.SetFlag("-nographic")
	qemuCmd.SetFlag("-monitor", "none")

	var cmd *exec.Cmd
	if t.UseFFXExperimental(ffxEmuExperimentLevel) {
		config, err := qemuCmd.BuildConfig()
		if err != nil {
			return err
		}
		configFile := filepath.Join(os.Getenv(testrunnerconstants.TestOutDirEnvKey), "ffx_emu_config.json")
		absConfigFile, err := filepath.Abs(configFile)
		if err != nil {
			return err
		}
		if err := jsonutil.WriteToFile(absConfigFile, config); err != nil {
			return err
		}
		cwd, err := os.Getwd()
		if err != nil {
			return err
		}
		tools := ffxutil.EmuTools{
			Emulator: absQEMUSystemPath,
			FVM:      t.config.FVMTool,
			ZBI:      t.config.ZBITool,
		}
		cmd, err = t.ffx.EmuStartConsole(ctx, cwd, DefaultQEMUNodename, t.isQEMU, absConfigFile, tools)
		if err != nil {
			return err
		}
	} else {
		invocation, err := qemuCmd.Build()
		if err != nil {
			return err
		}
		cmd = exec.Command(invocation[0], invocation[1:]...)
	}

	cmd.Dir = workdir
	stdout, stderr, flush := botanist.NewStdioWriters(ctx, "qemu")
	// Since serial is already printed to stdout, we can copy it to the
	// serial logfile as well.
	var serialLog *os.File
	closeSerialLog := func() {
		if serialLog == nil {
			return
		}
		if err := serialLog.Close(); err != nil {
			logger.Debugf(ctx, "failed to close %s", serialLog.Name())
		}
	}
	if t.config.Logfile != "" {
		logfile, err := filepath.Abs(t.config.Logfile)
		if err != nil {
			return fmt.Errorf("cannot get absolute path for %q: %w", t.config.Logfile, err)
		}
		if err := os.MkdirAll(filepath.Dir(logfile), os.ModePerm); err != nil {
			return fmt.Errorf("failed to make parent dirs of %q: %w", logfile, err)
		}
		serialLog, err := os.Create(logfile)
		if err != nil {
			return fmt.Errorf("failed to create %s", logfile)
		}
		if err := os.Setenv(constants.SerialLogEnvKey, logfile); err != nil {
			logger.Debugf(ctx, "failed to set %s to %s", constants.SerialLogEnvKey, logfile)
		}
		serialWriter := botanist.NewLineWriter(botanist.NewTimestampWriter(serialLog), "")
		stdout = io.MultiWriter(stdout, serialWriter)
		stderr = io.MultiWriter(stderr, serialWriter)
	}
	if t.ptm != nil {
		cmd.Stdin = t.ptm
		cmd.Stdout = io.MultiWriter(t.ptm, stdout)
		cmd.Stderr = io.MultiWriter(t.ptm, stderr)
		cmd.SysProcAttr = &syscall.SysProcAttr{
			Setctty: true,
			Setsid:  true,
		}
	} else {
		cmd.Stdout = stdout
		cmd.Stderr = stderr
		cmd.SysProcAttr = &syscall.SysProcAttr{
			// Set a process group ID so we can kill the entire group, meaning
			// the process and any of its children.
			Setpgid: true,
		}
	}
	logger.Debugf(ctx, "QEMU invocation:\n%s", strings.Join(append([]string{cmd.Path}, cmd.Args...), " "))

	if err := cmd.Start(); err != nil {
		flush()
		closeSerialLog()
		return fmt.Errorf("failed to start: %w", err)
	}
	t.process = cmd.Process

	go func() {
		err := cmd.Wait()
		flush()
		closeSerialLog()
		if err != nil {
			err = fmt.Errorf("%s: %w", constants.QEMUInvocationErrorMsg, err)
		}
		t.c <- err
		os.RemoveAll(workdir)
	}()
	return nil
}

// Stop stops the QEMU target.
func (t *QEMU) Stop() error {
	if t.UseFFXExperimental(ffxEmuExperimentLevel) {
		return t.ffx.EmuStop(context.Background())
	}
	if t.process == nil {
		return fmt.Errorf("QEMU target has not yet been started")
	}
	logger.Debugf(t.targetCtx, "Sending SIGKILL to %d", t.process.Pid)
	err := t.process.Kill()
	t.process = nil
	t.genericFuchsiaTarget.Stop()
	return err
}

// Wait waits for the QEMU target to stop.
func (t *QEMU) Wait(ctx context.Context) error {
	select {
	case err := <-t.c:
		return err
	case <-ctx.Done():
		return ctx.Err()
	}
}

// Config returns fields describing the target.
func (t *QEMU) TestConfig(netboot bool) (any, error) {
	return TargetInfo(t, netboot, nil)
}

func normalizeFile(path string) (string, error) {
	if _, err := os.Stat(path); err != nil {
		return "", err
	}
	absPath, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return absPath, nil
}

func overwriteFileWithCopy(path string) error {
	tmpfile, err := os.CreateTemp(filepath.Dir(path), "botanist")
	if err != nil {
		return err
	}
	defer tmpfile.Close()
	if err := osmisc.CopyFile(path, tmpfile.Name()); err != nil {
		return err
	}
	return os.Rename(tmpfile.Name(), path)
}

func embedZBIWithKey(ctx context.Context, zbiImage *bootserver.Image, zbiTool string, authorizedKeysFile string) error {
	absToolPath, err := filepath.Abs(zbiTool)
	if err != nil {
		return err
	}
	logger.Debugf(ctx, "embedding %s with key %s", zbiImage.Name, authorizedKeysFile)
	cmd := exec.CommandContext(ctx, absToolPath, "-o", zbiImage.Path, zbiImage.Path, "--entry", fmt.Sprintf("data/ssh/authorized_keys=%s", authorizedKeysFile))
	stdout, stderr, flush := botanist.NewStdioWriters(ctx, "zbi")
	defer flush()
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	if err := cmd.Run(); err != nil {
		return err
	}
	return nil
}

func extendFvmImage(ctx context.Context, fvmImage *bootserver.Image, fvmTool string, size int64) error {
	if fvmTool == "" {
		return nil
	}
	absToolPath, err := filepath.Abs(fvmTool)
	if err != nil {
		return err
	}
	logger.Debugf(ctx, "extending fvm.blk to %d bytes", size)
	cmd := exec.CommandContext(ctx, absToolPath, fvmImage.Path, "extend", "--length", strconv.Itoa(int(size)))
	stdout, stderr, flush := botanist.NewStdioWriters(ctx, "fvm")
	defer flush()
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	if err := cmd.Run(); err != nil {
		return err
	}
	fvmImage.Size = size
	return nil
}

func extendImage(ctx context.Context, image *bootserver.Image, size int64) error {
	if image.Size >= size {
		return nil
	}
	if err := os.Truncate(image.Path, size); err != nil {
		return err
	}
	image.Size = size
	return nil
}

func getImageByNameAndCPU(imgs []bootserver.Image, name, cpu string) *bootserver.Image {
	for _, img := range imgs {
		if img.Name == name && img.CPU == cpu {
			return &img
		}
	}
	return nil
}
