#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Verifies the output of the license collection gathering aspect."""

import argparse
import dataclasses
import sys
import shutil
from fuchsia.tools.licenses.common_types import *
from typing import List


@dataclasses.dataclass(frozen=True)
class UnlicensedTargetInfo:
    """Container for a Unlicensed Target information"""

    label: str
    build_file_path: str
    rule_kind: str
    unlicensed_resources: List[str]
    rule_attr_names: List[str]

    def from_json_dict(input: DictReader) -> "UnlicensedTargetInfo":
        return UnlicensedTargetInfo(
            label=input.get("label"),
            build_file_path=input.get("build_file_path"),
            rule_kind=input.get("rule_kind"),
            unlicensed_resources=input.get_string_list("unlicensed_resources"),
            rule_attr_names=input.get_string_list("rule_attr_names"))

    def summary_for_error(self) -> str:
        if self.unlicensed_resources:
            resources_str = "\n    ".join(
                trim_long_str_list(
                    [f"resource={r}" for r in self.unlicensed_resources],
                    max_len=5))
            return f"{self.label} in {self.build_file_path} (rule={self.rule_kind} attrs={','.join(self.rule_attr_names)})\n    {resources_str}"
        else:
            return f"{self.label} in {self.build_file_path} (rule={self.rule_kind})"

    def example_for_fix(self, root_target) -> str:

        target_local_name = self.label
        if ":" in target_local_name:
            target_local_name = target_local_name.split(":")[-1]
        elif "/" in target_local_name:
            target_local_name = target_local_name.split("/")[-1]

        return f"""
For example, to add license information to {self.label},
edit the file {self.build_file_path} as follows:

First, declare a Bazel license target:

```
load("@rules_license//rules:license.bzl", "license")

license(
    name = "license",
    package_name = "see note 2",
    license_text = "see note 3",
)
```

Notes:
1. You may skip this step if a `license` target is already already defined.
2. `package_name` is the externally visible name of the licensed package.
    E.g. "grpc", "Fuchsia", "absl".
3. `license_text` file can be either a plain file
   (E.g. common names are `LICENSE`, `NOTICE.txt`),
   or a JSON spdx file (must be `.spdx.json`).
   If you don't know what is the license file for the target, consult
   https://fuchsia.dev/fuchsia-src/contribute/governance/policy/osrb-process
4. You may optionally also add `package_url` to specify the publicly hosted
   location of the package.

Next, associate {self.label} with the license:

You may associated ALL targets in {self.build_file_path}
using the `package` declaration:

```
package(
    ...
    default_applicable_licenses = [":license"],
)
```

Or, you may associate just `:{target_local_name}`:

```
{self.rule_kind} {{
    name = "{target_local_name}"
    applicable_licenses = [":license"]
    ...
}}
```

Note: `{self.rule_kind}` may not be explicitly called in the build file,
but indirectly invoked through a Starlark function. In that case,
make sure the function forwards the `applicable_licenses` attribute
to `{self.rule_kind}`.


Other Options:
1. To investigate why '{self.label}' is included in the build graph, run:
```
bazel cquery 'somepath({root_target},{self.label})'
```
2. To systemically remove all targets of kind `{self.rule_kind}` you
   may want to change `collection_policy.bzl`.
"""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--licenses_collection_input',
        help=
        'A json file containing the serialized output of the license collection gathering aspect.',
        required=True,
    )
    parser.add_argument(
        '--verified_licenses_collection_output',
        help='A verified copy of the input json file.',
        required=True,
    )
    args = parser.parse_args()

    reader = DictReader.create_from_file(args.licenses_collection_input)

    unlicensed_targets = [
        UnlicensedTargetInfo.from_json_dict(r)
        for r in reader.get_readers_list("unlicensed_targets")
    ]
    if unlicensed_targets:

        root_target = reader.get("root_target")
        target_summaries = "\n  ".join(
            trim_long_str_list(
                [t.summary_for_error() for t in unlicensed_targets],
                max_len=100))
        target_for_example = unlicensed_targets[0]
        example = target_for_example.example_for_fix(root_target)

        print(
            f"""ERROR: Targets are missing required license information!

The following {len(unlicensed_targets)} targets are missing license information:
  {target_summaries}

These are third_party or prebuilt targets, or otherwise depend directly on exported
files that are third_party or prebuilt targets. You should either add
license information (via the `applicable_licenses` attribute) or remove
the targets from the build graph.

{example}
""")
        sys.exit(-1)

    shutil.copyfile(
        args.licenses_collection_input,
        args.verified_licenses_collection_output)


if __name__ == '__main__':
    main()
