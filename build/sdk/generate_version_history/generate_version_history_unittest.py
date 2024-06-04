# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import generate_version_history


class GenerateVersionHistoryTests(unittest.TestCase):
    def test_replace_api(self) -> None:
        version_history = {
            "data": {
                "name": "Platform version map",
                "type": "version_history",
                "api_levels": {
                    "9": {
                        "abi_revision": "0xECDB841C251A8CB9",
                        "status": "unsupported",
                    },
                    "10": {
                        "abi_revision": "0xED74D73009C2B4E3",
                        "status": "supported",
                    },
                },
                "special_api_levels": {
                    "HEAD": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4292870144,
                        "status": "in-development",
                    },
                    "PLATFORM": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4293918720,
                        "status": "in-development",
                    },
                },
            },
        }
        generate_version_history.replace_special_abi_revisions(version_history)

        self.assertEqual(
            version_history,
            {
                "data": {
                    "name": "Platform version map",
                    "type": "version_history",
                    "api_levels": {
                        "9": {
                            "abi_revision": "0xECDB841C251A8CB9",
                            "status": "unsupported",
                        },
                        "10": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "status": "supported",
                        },
                    },
                    "special_api_levels": {
                        "HEAD": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "as_u32": 4292870144,
                            "status": "in-development",
                        },
                        "PLATFORM": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "as_u32": 4293918720,
                            "status": "in-development",
                        },
                    },
                },
            },
        )


if __name__ == "__main__":
    unittest.main()
