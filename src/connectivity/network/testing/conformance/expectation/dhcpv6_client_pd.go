// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package expectation

import "go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/expectation/outcome"

var dhcpv6ClientPDExpectations map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 1}: Pass,
	{2, 1}: Pass,
	{2, 2}: Pass,
	{2, 3}: Pass,
	{2, 4}: Fail,
	{3, 1}: Pass,
	{3, 2}: Pass,
	{3, 3}: Pass,
	{3, 4}: Pass,
	{3, 5}: Fail,
	{3, 6}: Pass,
	{3, 7}: Pass,
	{3, 8}: Pass,
	{4, 1}: Pass,
	{4, 2}: Pass,
	{4, 3}: Pass,
	{5, 1}: Inconclusive,
	{5, 2}: Inconclusive,
	{5, 3}: Inconclusive,
	{6, 1}: Pass,
	{6, 2}: Pass,
	{6, 3}: Pass,
	{7, 1}: Fail,
	{7, 2}: Fail,
}
