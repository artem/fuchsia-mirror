// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"math"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/pkg/sftp"
	"golang.org/x/crypto/ssh"
)

type TargetConnection struct {
	client *ssh.Client
}

func newSigner(maybeKeyFile string) (ssh.Signer, error) {
	var keyFile string
	if maybeKeyFile == "" {
		var err error
		if keyFile, err = findDefaultKeyFile(); err != nil {
			return nil, err
		}
	} else {
		keyFile = maybeKeyFile
	}
	key, err := os.ReadFile(keyFile)
	if err != nil {
		return nil, err
	}
	return ssh.ParsePrivateKey(key)
}

// Explicit key was not specified. Find the default key using the manifest.
func findDefaultKeyFile() (string, error) {
	ffx := filepath.Join(getTraceutilBuildDir(), "ffx")
	output, err := getCommandOutput(ffx, "config", "get", "ssh.priv")
	output = strings.TrimSpace(output)
	if err != nil {
		return "", errors.New(output)
	}
	// Trim the double quotes if they are still there.
	if len(output) >= 2 {
		if output[0] == '"' && output[len(output)-1] == '"' {
			output = output[1 : len(output)-1]
		}
	}
	return output, nil
}

func findDefaultTarget() (string, error) {
	netaddr := filepath.Join(getZirconBuildRoot(), "tools", "netaddr")
	output, err := getCommandOutput(netaddr, "--fuchsia")
	output = strings.TrimSpace(output)
	if err != nil {
		return "", errors.New(output)
	}
	return output, nil
}

func NewTargetConnection(hostname, port, keyFile string) (*TargetConnection, error) {
	if hostname == "" {
		defaultHostname, err := findDefaultTarget()
		if err != nil {
			return nil, fmt.Errorf("Can not look up default target: %s",
				err.Error())
		}
		hostname = defaultHostname
	}

	if port == "" {
		port = "22"
	}

	signer, err := newSigner(keyFile)
	if err != nil {
		return nil, err
	}

	config := &ssh.ClientConfig{
		User: "fuchsia",
		Auth: []ssh.AuthMethod{
			ssh.PublicKeys(signer),
		},
		HostKeyCallback: ssh.InsecureIgnoreHostKey(),
	}

	address := net.JoinHostPort(hostname, port)
	client, err := ssh.Dial("tcp", address, config)
	if err != nil {
		return nil, err
	}
	return &TargetConnection{
		client: client,
	}, nil
}

func (c *TargetConnection) Close() {
	c.client.Close()
}

func (c *TargetConnection) RunCommand(command string) error {
	session, err := c.client.NewSession()
	if err != nil {
		return err
	}
	defer session.Close()

	session.Stdin = os.Stdin
	session.Stdout = os.Stdout
	session.Stderr = os.Stderr
	fmt.Printf("Running: %s\n", command)
	return session.Run(command)
}

func (c *TargetConnection) GetFile(remotePath string, localPath string) error {
	client, err := sftp.NewClient(c.client)
	if err != nil {
		return err
	}
	defer client.Close()

	remoteFile, err := client.Open(remotePath)
	if err != nil {
		return err
	}
	defer remoteFile.Close()

	localFile, err := os.Create(localPath)
	if err != nil {
		return err
	}
	defer localFile.Close()

	_, err = io.Copy(localFile, remoteFile)
	return err
}

// SyncClk synchronizes local and remote clock over TargetConnection.
// It selects the shortest round trip of 10 attempts to read remote timestamps.
// Return is local-remote time offset, length of best round trip, error.
// If error is not nil, first two paramters should not be trusted.
func (c *TargetConnection) SyncClk() (offset time.Duration, delta time.Duration, err error) {
	delta = time.Hour

	session, err := c.client.NewSession()
	if err != nil {
		return offset, delta, err
	}
	defer session.Close()

	cin, err := session.StdinPipe()
	if err != nil {
		return offset, delta, err
	}
	cerrPipe, err := session.StderrPipe()
	if err != nil {
		return offset, delta, err
	}
	cerr := bufio.NewReader(cerrPipe)

	err = session.Start("trace time")
	if err != nil {
		return offset, delta, err
	}

	// Expect one line of "how to use timesync" output.
	_, err = cerr.ReadString('\n')
	if err != nil {
		return offset, delta, err
	}

	// Take 10 samples; each time:
	// Send "t" to request time.
	// Read one line; expect a timestamp.
	for i := 0; i < 10; i++ {
		start := time.Now()
		n, err := cin.Write([]byte("t"))
		if err != nil {
			return offset, delta, err
		}
		if n != 1 {
			return offset, delta, errors.New("Failed to send timestamp request")
		}

		// Read a line ending with '\n'.
		usBytes, err := cerr.ReadBytes('\n')

		end := time.Now()

		if err != nil {
			return offset, delta, err
		}

		// If this wasn't the fastest RTT we've seen so far, then continue.
		d := end.Sub(start)
		if d > delta {
			continue
		}

		// Store new best delta; sync local time to midpoint between start and end.
		delta = d
		t1 := start.Add(delta / 2)

		// Parse remote timestamp.
		usStr := string(usBytes[:len(usBytes)-1])
		floatUSecs, err := strconv.ParseFloat(usStr, 64)
		if err != nil {
			return offset, delta, errors.New("Failed to parse timestamp")
		}
		nanoSecs := int64(math.Round(floatUSecs * 1000.0))
		t2 := time.Unix(nanoSecs/1000000000, nanoSecs%1000000000)

		// Store offset between local and remote timestamps.
		offset = t1.Sub(t2)
	}

	// Instruct "timesync" utility to quit with "q" keystroke.
	// Wait for it to shutdown.
	n, err := cin.Write([]byte("q"))
	if err != nil {
		return offset, delta, err
	}
	if n != 1 {
		return offset, delta, errors.New("Failed to send quit request")
	}
	err = cin.Close()
	if err != nil {
		return offset, delta, err
	}
	err = session.Wait()
	if err != nil {
		return offset, delta, err
	}

	return offset, delta, nil
}
