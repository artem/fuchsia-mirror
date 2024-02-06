// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package orchestrate

import (
	"errors"
	"reflect"
	"testing"
)

func TestReadDeviceConfig(t *testing.T) {
	testCases := []struct {
		name        string
		readFileErr bool
		jsonData    string
		wantErr     bool
		wantResult  *DeviceConfig
	}{
		{
			name:        "ReadFileFails",
			readFileErr: true,
			wantErr:     true,
		},
		{
			name:     "Invalid JSON",
			jsonData: "!invalid!",
			wantErr:  true,
		},
		{
			name:     "TooManyDevices",
			jsonData: "[{\"fastboot_sernum\":\"foo\"}, {\"fastboot_sernum\":\"bar\"}]",
			wantErr:  true,
		},
		{
			name:     "Success",
			jsonData: "[{\"fastboot_sernum\":\"foo\"}]",
			wantResult: &DeviceConfig{
				FastbootSerial: "foo",
			},
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			oc := &OrchestrateConfig{
				ReadFile: func(name string) ([]byte, error) {
					if tc.readFileErr {
						return nil, errors.New("Error reading file")
					}
					return []byte(tc.jsonData), nil
				},
			}
			result, err := oc.ReadDeviceConfig("foo.json")
			if tc.wantErr && err == nil {
				t.Errorf("ReadDeviceConfig %s failed: got no error, want error", tc.name)
			} else if !tc.wantErr && err != nil {
				t.Errorf("ReadDeviceConfig %s failed: got error: %s, want no error", tc.name, err)
			} else if tc.wantResult != nil && !reflect.DeepEqual(tc.wantResult, result) {
				t.Errorf("ReadDeviceConfig %s failed: want %v, got %v", tc.name, tc.wantResult, result)
			}
		})
	}
}

func TestReadRunInput(t *testing.T) {
	testCases := []struct {
		name        string
		readFileErr bool
		jsonData    string
		wantErr     bool
		wantResult  *RunInput
	}{
		{
			name:        "ReadFileFails",
			readFileErr: true,
			wantErr:     true,
		},
		{
			name:     "Invalid JSON",
			jsonData: "!invalid!",
			wantErr:  true,
		},
		{
			name:     "Success",
			jsonData: "{\"hardware\": {\"ffx_path\":\"foo\"}}",
			wantResult: &RunInput{
				Hardware: TargetRunInput{
					FfxPath: "foo",
				},
			},
		},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			oc := &OrchestrateConfig{
				ReadFile: func(name string) ([]byte, error) {
					if tc.readFileErr {
						return nil, errors.New("Error reading file")
					}
					return []byte(tc.jsonData), nil
				},
			}
			result, err := oc.ReadRunInput("foofile.json")
			if tc.wantErr && err == nil {
				t.Errorf("ReadRunInput %s failed: got no error, want error", tc.name)
			} else if !tc.wantErr && err != nil {
				t.Errorf("ReadRunInput %s failed: got error: %s, want no error", tc.name, err)
			} else if tc.wantResult != nil && !reflect.DeepEqual(tc.wantResult, result) {
				t.Errorf("ReadRunInput %s failed: want %v, got %v", tc.name, tc.wantResult, result)
			}
		})
	}
}
