// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package expectation

import "go.fuchsia.dev/fuchsia/src/connectivity/network/testing/conformance/expectation/outcome"

var tcpcorev6Expectations map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 1}:   Pass,
	{1, 2}:   Pass,
	{1, 3}:   Pass,
	{1, 4}:   Pass,
	{1, 5}:   Pass,
	{1, 17}:  Pass,
	{1, 18}:  Fail,
	{1, 19}:  Pass,
	{1, 20}:  Pass,
	{1, 21}:  Pass,
	{1, 22}:  Pass,
	{1, 23}:  Pass,
	{1, 24}:  Pass,
	{1, 25}:  Pass,
	{1, 26}:  Pass,
	{1, 27}:  Pass,
	{2, 1}:   Pass,
	{2, 2}:   Pass,
	{2, 17}:  Pass,
	{2, 18}:  Pass,
	{2, 19}:  Pass,
	{2, 20}:  Pass,
	{3, 1}:   Pass,
	{3, 2}:   Pass,
	{3, 3}:   Pass,
	{3, 4}:   Fail,
	{3, 6}:   Pass,
	{3, 7}:   Pass,
	{3, 8}:   Pass,
	{3, 17}:  Fail,
	{3, 18}:  Pass,
	{3, 19}:  Pass,
	{3, 20}:  Pass,
	{3, 21}:  Pass,
	{3, 22}:  Pass,
	{3, 23}:  Pass,
	{4, 1}:   Pass,
	{4, 20}:  Pass,
	{4, 21}:  Pass,
	{4, 22}:  Fail,
	{4, 24}:  Pass,
	{4, 25}:  Pass,
	{4, 26}:  Pass,
	{4, 27}:  Pass,
	{4, 28}:  Pass,
	{4, 29}:  Pass,
	{4, 30}:  Pass,
	{5, 18}:  Pass,
	{5, 19}:  Pass,
	{5, 20}:  Pass,
	{5, 22}:  Pass,
	{5, 23}:  Pass,
	{5, 24}:  Fail,
	{5, 25}:  Fail,
	{5, 26}:  Pass,
	{6, 17}:  Fail,
	{6, 18}:  Pass,
	{6, 20}:  Pass,
	{6, 22}:  Pass,
	{6, 23}:  Pass,
	{6, 24}:  Fail,
	{6, 25}:  Fail,
	{6, 26}:  Fail,
	{6, 27}:  Fail,
	{6, 28}:  Fail,
	{6, 29}:  Fail,
	{6, 31}:  Pass,
	{6, 32}:  Fail,
	{7, 18}:  Fail,
	{7, 22}:  Pass,
	{7, 26}:  Fail,
	{8, 1}:   Pass,
	{8, 2}:   Pass,
	{8, 17}:  Pass,
	{8, 18}:  Flaky, // TODO(https://fxbug.dev/98953): fix flake
	{8, 19}:  Fail,
	{8, 20}:  Fail,
	{8, 21}:  Pass,
	{8, 22}:  Pass,
	{8, 23}:  Pass,
	{8, 24}:  Pass,
	{8, 25}:  Pass,
	{8, 26}:  Pass,
	{8, 27}:  Pass,
	{8, 28}:  Pass,
	{8, 29}:  Fail,
	{9, 17}:  Pass,
	{9, 18}:  Pass,
	{9, 19}:  Pass,
	{9, 20}:  Pass,
	{9, 21}:  Fail,
	{9, 22}:  Fail,
	{9, 27}:  Pass,
	{10, 21}: Pass,
	{10, 22}: Fail,
	{10, 23}: Pass,
	{10, 24}: Fail,
	{10, 25}: Pass,
	{10, 26}: Pass,
	{11, 18}: Pass,
	{11, 19}: Pass,
	{11, 20}: Pass,
	{11, 21}: Pass,
	{11, 22}: Pass,
	{11, 23}: Pass,
	{11, 24}: Pass,
	{11, 25}: Pass,
	{11, 26}: Pass,
	{11, 27}: Pass,
	{11, 28}: Pass,
	{11, 29}: Fail,
	{12, 1}:  Pass,
	{12, 2}:  Pass,
	{12, 3}:  Pass,
	{12, 4}:  Fail,
	{12, 21}: Pass,
	{12, 23}: Pass,
	{12, 26}: Pass,
	{12, 27}: Fail,
	{12, 28}: Pass,
	{12, 29}: Pass,
	{12, 30}: Pass,
	{13, 1}:  Pass,
	{13, 2}:  Pass,
	{13, 4}:  Pass,
	{13, 5}:  Pass,
	{13, 17}: Pass,
	{13, 18}: Pass,
	{13, 19}: Pass,
	{13, 20}: Pass,
	{13, 21}: Pass,
	{14, 19}: Pass,
	{14, 20}: Pass,
	{15, 22}: Pass,
	{15, 24}: Pass,
	{15, 25}: Pass,
	{15, 26}: Fail,
	{15, 27}: Fail,
	{15, 28}: Fail,
	{15, 29}: Pass,
	{16, 17}: Pass,
	{16, 18}: Pass,
	{16, 19}: Pass,
	{16, 20}: Pass,
	{16, 21}: Pass,
	{16, 22}: Pass,
	{17, 17}: Pass,
	{17, 18}: Pass,
	{17, 19}: Fail,
	{17, 20}: Pass,
	{18, 17}: Pass,
	{18, 21}: Pass,
	{19, 17}: Fail,
	{19, 20}: Fail,
	{20, 17}: Pass,
	{20, 18}: Pass,
	{20, 19}: Pass,
	{20, 20}: Pass,
	{23, 1}:  Pass,
	{23, 2}:  Pass,
	{23, 4}:  Pass,
}

var tcpcorev6ExpectationsNS3 map[AnvlCaseNumber]outcome.Outcome = map[AnvlCaseNumber]outcome.Outcome{
	{1, 1}:   Pass,
	{1, 2}:   Pass,
	{1, 3}:   Pass,
	{1, 4}:   Pass,
	{1, 5}:   Pass,
	{1, 17}:  Pass,
	{1, 18}:  Fail,
	{1, 19}:  Pass,
	{1, 20}:  Pass,
	{1, 21}:  Pass,
	{1, 22}:  Fail,
	{1, 23}:  Fail,
	{1, 24}:  Fail,
	{1, 25}:  Pass,
	{1, 26}:  Pass,
	{1, 27}:  Pass,
	{2, 1}:   Pass,
	{2, 2}:   Pass,
	{2, 17}:  Pass,
	{2, 18}:  Pass,
	{2, 19}:  Pass,
	{2, 20}:  Pass,
	{3, 1}:   Fail,
	{3, 2}:   Pass,
	{3, 3}:   Pass,
	{3, 4}:   Pass,
	{3, 6}:   Pass,
	{3, 7}:   Fail,
	{3, 8}:   Pass,
	{3, 17}:  Fail,
	{3, 18}:  Fail,
	{3, 19}:  Fail,
	{3, 20}:  Fail,
	{3, 21}:  Fail,
	{3, 22}:  Fail,
	{3, 23}:  Fail,
	{4, 1}:   Pass,
	{4, 20}:  Pass,
	{4, 21}:  Pass,
	{4, 22}:  Fail,
	{4, 24}:  Pass,
	{4, 25}:  Pass,
	{4, 26}:  Pass,
	{4, 27}:  Pass,
	{4, 28}:  Pass,
	{4, 29}:  Pass,
	{4, 30}:  Pass,
	{5, 18}:  Pass,
	{5, 19}:  Pass,
	{5, 20}:  Pass,
	{5, 22}:  Pass,
	{5, 23}:  Pass,
	{5, 24}:  Fail,
	{5, 25}:  Fail,
	{5, 26}:  Pass,
	{6, 17}:  Fail,
	{6, 18}:  Pass,
	{6, 20}:  Pass,
	{6, 22}:  Fail,
	{6, 23}:  Pass,
	{6, 24}:  Fail,
	{6, 25}:  Fail,
	{6, 26}:  Fail,
	{6, 27}:  Pass,
	{6, 28}:  Fail,
	{6, 29}:  Fail,
	{6, 31}:  Fail,
	{6, 32}:  Fail,
	{7, 18}:  Fail,
	{7, 22}:  Fail,
	{7, 26}:  Fail,
	{8, 1}:   Pass,
	{8, 2}:   Pass,
	{8, 17}:  Pass,
	{8, 18}:  Fail,
	{8, 19}:  Fail,
	{8, 20}:  Fail,
	{8, 21}:  Pass,
	{8, 22}:  Fail,
	{8, 23}:  Fail,
	{8, 24}:  Fail,
	{8, 25}:  Fail,
	{8, 26}:  Fail,
	{8, 27}:  Fail,
	{8, 28}:  Fail,
	{8, 29}:  Fail,
	{9, 17}:  Pass,
	{9, 18}:  Pass,
	{9, 19}:  Pass,
	{9, 20}:  Pass,
	{9, 21}:  Pass,
	{9, 22}:  Fail,
	{9, 27}:  Pass,
	{10, 21}: Fail,
	{10, 22}: Pass,
	{10, 23}: Pass,
	{10, 24}: Fail,
	{10, 25}: Fail,
	{10, 26}: Pass,
	{11, 18}: Pass,
	{11, 19}: Fail,
	{11, 20}: Pass,
	{11, 21}: Fail,
	{11, 22}: Fail,
	{11, 23}: Pass,
	{11, 24}: Fail,
	{11, 25}: Fail,
	{11, 26}: Pass,
	{11, 27}: Fail,
	{11, 28}: Fail,
	{11, 29}: Fail,
	{12, 1}:  Fail,
	{12, 2}:  Pass,
	{12, 3}:  Pass,
	{12, 4}:  Fail,
	{12, 21}: Pass,
	{12, 23}: Pass,
	{12, 26}: Pass,
	{12, 27}: Pass,
	{12, 28}: Pass,
	{12, 29}: Pass,
	{12, 30}: Pass,
	{13, 1}:  Pass,
	{13, 2}:  Pass,
	{13, 4}:  Pass,
	{13, 5}:  Pass,
	{13, 17}: Pass,
	{13, 18}: Pass,
	{13, 19}: Pass,
	{13, 20}: Pass,
	{13, 21}: Pass,
	{14, 19}: Fail,
	{14, 20}: Fail,
	{15, 22}: Pass,
	{15, 24}: Pass,
	{15, 25}: Pass,
	{15, 26}: Fail,
	{15, 27}: Fail,
	{15, 28}: Flaky,
	{15, 29}: Flaky,
	{16, 17}: Pass,
	{16, 18}: Pass,
	{16, 19}: Fail,
	{16, 20}: Pass,
	{16, 21}: Pass,
	{16, 22}: Pass,
	{17, 17}: Fail,
	{17, 18}: Fail,
	{17, 19}: Pass,
	{17, 20}: Pass,
	{18, 17}: Pass,
	{18, 21}: Pass,
	{19, 17}: Fail,
	{19, 20}: Fail,
	{20, 17}: Pass,
	{20, 18}: Pass,
	{20, 19}: Pass,
	{20, 20}: Pass,
	{23, 1}:  Fail,
	{23, 2}:  Pass,
	{23, 4}:  Fail,
}
