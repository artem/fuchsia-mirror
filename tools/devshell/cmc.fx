# Copyright 2020 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

#### CATEGORY=Build
#### EXECUTABLE=${HOST_TOOLS_DIR}/cmc
### Component manifest compiler
## USAGE:
##     cmc [OPTIONS] <SUBCOMMAND>
##
## FLAGS:
##     -h, --help       Prints help information
##     -V, --version    Prints version information
##
## OPTIONS:
##     -s, --stamp <stamp>    Stamp this file on success
##
## SUBCOMMANDS:
##    check-includes         check if given includes are present in a given component manifest
##    compile                compile a CML file
##    format                 format a json file
##    help                   Prints this message or the help of the given subcommand(s)
##    include                recursively process contents from includes, and optionally validate the result
##    merge                  merge the listed manifest files. Does NOT validate the resulting manifest.
##    print-cml-reference    print generated .cml reference documentation
##    validate               validate that one or more cml files are valid
##    validate-references    validate a component manifest against package manifest.
