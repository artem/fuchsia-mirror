#!/bin/bash
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# usage: cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/coverage.sh [--affected]
#
# Ensures unit test coverage meets Honeydew's coding guideline.
#
# Arguments:
#   --affected  If set, only consider modified files w.r.t to upstream;
#               otherwise, consider all files under the Honeydew directory.

INCLUDE_FILES="*"
if [[ "$1" == "--affected" ]]; then
    # Generate comma-seperated list using file names from an upstream diff.
    # Note:
    # * `coverage` expects a comma-seperated string for output filtering.
    # * Non-Python files are ignored by `coverage` so no need to omit them here.
    INCLUDE_FILES=$(git diff --name-only origin/main | tr '\n' ',')
fi

COVERAGE_THRESHOLD=70

LACEWING_SRC="$FUCHSIA_DIR/src/testing/end_to_end"
HONEYDEW_SRC="$LACEWING_SRC/honeydew"
BUILD_DIR=$(cat "$FUCHSIA_DIR"/.fx-build-dir)

VENV_ROOT_PATH="$LACEWING_SRC/.venvs"
VENV_NAME="fuchsia_python_venv"
VENV_PATH="$VENV_ROOT_PATH/$VENV_NAME"

if [ -d $VENV_PATH ]
then
    echo "Activating the virtual environment..."
    source $VENV_PATH/bin/activate
else
    echo "ERROR: Directory '$VENV_PATH' does not exists. Run the 'install.sh' script first..."
    exit 1
fi

echo "Configuring environment..."
OLD_PYTHONPATH=$PYTHONPATH
PYTHONPATH=$FUCHSIA_DIR/$BUILD_DIR/host_x64:$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python:$PYTHONPATH
# Set FIDL_IR_PATH inorder to successfully imoport Fuchsia-Controller
export FIDL_IR_PATH="$(fx get-build-dir)/fidling/gen/ir_root"

echo "Running coverage tool..."
coverage \
    run -m unittest discover \
    --top-level-directory $HONEYDEW_SRC \
    --start-directory $HONEYDEW_SRC/tests/unit_tests \
    --pattern "*_test.py"

if [ $? -ne 0 ]; then
    echo
    echo "ERROR: coverage tool failed while running Honeydew unit tests."
    echo "ERROR: Please fix the failrues and re-run."
    exit 1
fi

echo "Generating coverage stats..."
output=$(coverage report -m --include "$INCLUDE_FILES")
# `coverage report` returns non-zero exit code when there is no coverage data to
# report; ignore those occurrences and only bubble up other failure modes.
if [[ $? -ne 0 && $output != "No data to report"* ]]; then
    exit $?
fi
echo -e "$output"


# Iterate coverage output lines and assert coverage is sufficient on a per-file
# basis.
#
# Sample output:
#     Name       Stmts   Miss  Cover   Missing
#     ----------------------------------------
#     module.py  20      1     95%     43
#     ----------------------------------------
#     TOTAL      20      1     95%
error_msg=""
while IFS= read -r line; do
    arr=($line)
    file_path=${arr[0]}
    coverage_str=${arr[3]}
    if [[ $coverage_str == *"%"* ]]; then
        coverage_metric=${coverage_str::-1}
        if ((coverage_metric < COVERAGE_THRESHOLD )); then
            error_msg+="ERROR: $coverage_str - $file_path\n"
        fi
    fi
done <<< "$output"

rm -rf .coverage

echo "Restoring environment..."
PYTHONPATH=$OLD_PYTHONPATH

if [ -z "$error_msg" ]; then
    echo
    echo "Code is 'coverage' compliant"
else
    echo
    echo "ERROR: Code is not 'coverage' compliant."
    echo "ERROR: Coverage threshold of $COVERAGE_THRESHOLD% not reached for the files below:"
    echo -e $error_msg
    exit 1
fi
