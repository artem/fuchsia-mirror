#!/bin/bash
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -e

# Formats the code as per coding guidelines
# This file was adapted from //src/testing/end_to_end/honeydew/scripts/...

FCT_SRC="$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller"

VENV_ROOT_PATH="$FCT_SRC/.venvs"
VENV_NAME="fuchsia_python_venv"
VENV_PATH="$VENV_ROOT_PATH/$VENV_NAME"

if [ -d $VENV_PATH ]
then
    echo "Activating the virtual environment..."
    source $VENV_PATH/bin/activate
else
    echo "Directory '$VENV_PATH' does not exists. Run the 'install.sh' script first..."
    exit 1
fi

cd $FUCHSIA_DIR

echo "Removing unused code..."
autoflake \
    --in-place \
    --remove-unused-variables \
    --remove-all-unused-imports \
    --remove-duplicate-keys \
    --recursive \
    --exclude "__init__.py,.venvs**" \
    $FCT_SRC

echo "Sorting imports..."
isort $FCT_SRC \
    --sg '.venvs**'
echo "Formatting code..."
black  \
    $FCT_SRC \
    --exclude '\.venvs'
echo "Checking types..."
mypy \
    $FCT_SRC \
    --config-file=${FCT_SRC}/pyproject.toml