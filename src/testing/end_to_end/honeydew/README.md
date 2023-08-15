# HoneyDew

[TOC]

HoneyDew is a test framework agnostic device controller written in Python that
provides Host-(Fuchsia)Target interaction.

Supported host operating systems:
* Linux

Assumptions:
* FFX CLI is present on the host and is included in `$PATH` environmental
  variable.
* Fastboot CLI is present on the host and is included in `$PATH` environmental
  variable, if you need to use [Fastboot transport].

* This tool was built to be run locally. Remote workflows (i.e. where the Target
  and Host are not collocated) are in limited support, and have the following
  assumptions:
    * You use a tool like `fssh tunnel` or `funnel` to forward the Target from
      your local machine to the remote machine over a SSH tunnel
    * Only one device is currently supported over the SSH tunnel.
    * If the device reboots during the test, it may be necessary to re-run
      the `fssh tunnel` command manually again in order to re-establish the
      appropriate port forwards.

## Installation
If you are contributing to HoneyDew or if you like to try HoneyDew locally in a
python interpreter, you can follow the guide below to [pip] install HoneyDew
(and all of its dependencies) in a [python virtual environment].

### Prerequisites
The Honeydew library depends on some Fuchsia build artifacts that must be built
for Honeydew to be successfully imported in Python.

```shell
~/fuchsia$ fx set core.qemu-x64 \
--with-host //src/testing/end_to_end/honeydew \
--with //src/testing/sl4f --with //src/sys/bin/start_sl4f \
--args='core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]'

~/fuchsia$ fx build
```

* The `core.qemu-x64` product config can be updated out to match the
`product.board` combination you want to test against.
* `--with-host //src/testing/end_to_end/honeydew` is required so that
Honeydew dependencies are built (e.g. Fuchsia controller's shared libraries).
* (Optional) `--with //src/testing/sl4f --with //src/sys/bin/start_sl4f \
--args='core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]'` is only
required if you wish to use Honeydew's SL4F-based affordances.

### Automated installation (recommended)
**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/install.sh`
**will automatically perform the below
[Manual installation](#manual-installation) steps**

After the installation succeeds, follow the script's instruction message to
start a Python interpreter and import Honeydew.

### Manual installation
Note - Use `fuchsia-vendored-python` while creating the virtual environment
as all of the in-tree code is developed using `fuchsia-vendored-python`

```shell
# cd to Fuchsia root directory
~$ cd $FUCHSIA_DIR

# Configure PYTHONPATH
~/fuchsia$ BUILD_DIR=$(cat "$FUCHSIA_DIR"/.fx-build-dir)
~/fuchsia$ PYTHONPATH=$FUCHSIA_DIR/$BUILD_DIR/host_x64:$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python:$PYTHONPATH

# create a virtual environment using `fuchsia-vendored-python`
~/fuchsia$ mkdir -p $FUCHSIA_DIR/src/testing/end_to_end/.venvs/
~/fuchsia$ fuchsia-vendored-python -m venv $FUCHSIA_DIR/src/testing/end_to_end/.venvs/fuchsia_python_venv

# activate the virtual environment
~/fuchsia$ source $FUCHSIA_DIR/src/testing/end_to_end/.venvs/fuchsia_python_venv/bin/activate

# upgrade the `pip` module
(fuchsia_python_venv)~/fuchsia$ python -m pip install --upgrade pip

# install honeydew
(fuchsia_python_venv)~/fuchsia$ cd $FUCHSIA_DIR/src/testing/end_to_end/honeydew
(fuchsia_python_venv)~/fuchsia/src/testing/end_to_end/honeydew$ python -m pip install --editable ".[test,guidelines]"

# verify you are able to import honeydew from a python terminal running inside this virtual environment
(fuchsia_python_venv)~/fuchsia/src/testing/end_to_end/honeydew$ cd $FUCHSIA_DIR
(fuchsia_python_venv)~/fuchsia$
(fuchsia_python_venv)~/fuchsia$ python
Python 3.8.8+chromium.12 (tags/v3.8.8-dirty:024d8058b0, Feb 19 2021, 16:18:16)
[GCC 4.8.2 20140120 (Red Hat 4.8.2-15)] on linux
Type "help", "copyright", "credits" or "license" for more information.
# Update `sys.path` to include Fuchsia Controller and FIDL IR paths before
# importing Honeydew.
>>> import os
>>> import sys
>>> import subprocess
>>> FUCHSIA_ROOT = os.environ.get("FUCHSIA_DIR")
>>> OUT_DIR = subprocess.check_output( \
  "echo $(cat \"$FUCHSIA_DIR\"/.fx-build-dir)", \
  shell=True).strip().decode('utf-8')
>>> sys.path.append(f"{FUCHSIA_ROOT}/src/developer/ffx/lib/fuchsia-controller/python")
>>> sys.path.append(f"{FUCHSIA_ROOT}/{OUT_DIR}/host_x64")

# You can now import honeydew
>>> import honeydew
```

## Uninstallation
**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/uninstall.sh`
**will automatically perform the below mentioned uninstallation steps**

To fully uninstall HoneyDew, delete the virtual environment that was crated
```shell
(fuchsia_python_venv)~/fuchsia$ deactivate
~/fuchsia$ rm -rf $FUCHSIA_DIR/src/testing/end_to_end/.venvs/fuchsia_python_venv
```

## Usage
Before proceeding further, please complete the [Installation](#Installation) and
activate the python virtual environment created during the installation.

### Device object creation
```python
# Update `sys.path` to include Fuchsia Controller and FIDL IR paths before
# importing Honeydew.
>>> import os
>>> import sys
>>> import subprocess
>>> FUCHSIA_ROOT = os.environ.get("FUCHSIA_DIR")
>>> OUT_DIR = subprocess.check_output( \
  "echo $(cat \"$FUCHSIA_DIR\"/.fx-build-dir)", \
  shell=True).strip().decode('utf-8')
>>> sys.path.append(f"{FUCHSIA_ROOT}/src/developer/ffx/lib/fuchsia-controller/python")
>>> sys.path.append(f"{FUCHSIA_ROOT}/{OUT_DIR}/host_x64")

# Enable Info logging
>>> import logging
>>> logging.basicConfig(level=logging.INFO)

# You can now import honeydew
>>> import honeydew

# Setup HoneyDew to run using isolated FFX and collect the logs
# Call this first prior to calling any other HoneyDew API
>>> from honeydew.transports import ffx
>>> ffx.setup(logs_dir="/tmp/foo/logs")

# honeydew.create_device() will look for a specific Fuchsia device class implementation that matches the device type specified and if it finds, it returns that specific device type object, else returns GenericFuchsiaDevice object.
# In below examples,
#   * "fuchsia-54b2-038b-6e90" is a x64 device whose implementation is present in HoneyDew. Hence returning honeydew.device_classes.x64.X64 object.
#   * "fuchsia-d88c-79a3-aa1d" is Google's 1p device whose implementation is not present in HoneyDew. Hence returning a generic_fuchsia_device.GenericFuchsiaDevice object.
#   * "fuchsia-emulator" is an emulator device whose implementation is not present in HoneyDew. Hence returning a generic_fuchsia_device.GenericFuchsiaDevice object.
#   * "[::1]:8022" is a fuchsia device whose SSH port is proxied via SSH from a local machine to a remote workstation.

>>> fd_1p = honeydew.create_device("fuchsia-ac67-847a-2e50", ssh_private_key=os.environ.get("SSH_PRIVATE_KEY_FILE"))
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Didn't find any matching device class implementation for 'fuchsia-ac67-847a-2e50'. So returning 'GenericFuchsiaDevice'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-ac67-847a-2e50...

>>> type(fd_1p)
honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice

>>> ws = honeydew.create_device("fuchsia-54b2-038b-6e90", ssh_private_key=os.environ.get("SSH_PRIVATE_KEY_FILE"))
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Found matching device class implementation for 'fuchsia-54b2-038b-6e90' as 'X64'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-54b2-038b-6e90...

>>> type(ws)
honeydew.device_classes.x64.X64

>>> emu = honeydew.create_device("fuchsia-emulator", ssh_private_key=os.environ.get("SSH_PRIVATE_KEY_FILE"))
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>}'
INFO:honeydew:Didn't find any matching device class implementation for 'fuchsia-emulator'. So returning 'GenericFuchsiaDevice'
INFO:honeydew.device_classes.fuchsia_device_base:Starting SL4F server on fuchsia-emulator...

>>> type(emu)
honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice

>>> fd_remote = honeydew.create_device("fuchsia-d88c-796c-e57e", ssh_private_key=os.environ.get("SSH_PRIVATE_KEY_FILE"), device_ip_port=custom_types.IpPort.parse("[::1]:8022"))
INFO:honeydew:Registered device classes with HoneyDew '{<class 'honeydew.device_classes.fuchsia_device_base.FuchsiaDeviceBase'>, <class 'honeydew.device_classes.x64.X64'>, <class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>}'
INFO:honeydew:Didn't find any matching device class implementation for 'fuchsia-d88c-796c-e57e'. So returning 'GenericFuchsiaDevice'
INFO:honeydew.transports.ssh:Waiting for fuchsia-d88c-796c-e57e to allow ssh connection...
Warning: Permanently added '[::1]:8022' (ED25519) to the list of known hosts.
INFO:honeydew.transports.ssh:fuchsia-d88c-796c-e57e is available via ssh.
INFO:honeydew.transports.sl4f:Starting SL4F server on fuchsia-d88c-796c-e57e...

>>> type(fd_remote)
<class 'honeydew.device_classes.generic_fuchsia_device.GenericFuchsiaDevice'>
```

### Access the static properties
```python
>>> emu.name
'fuchsia-emulator'

>>> emu.device_type
'qemu-x64'

>>> emu.product_name
'default-fuchsia'

>>> emu.manufacturer
'default-manufacturer'

>>> emu.model
'default-model'

>>> emu.serial_number
```

### Access the dynamic properties
```python
>>> emu.firmware_version
'2023-02-01T17:26:40+00:00'
```

### Access the public methods
```python
>>> emu.reboot()
INFO:honeydew.device_classes.base_fuchsia_device:Rebooting fuchsia-emulator...
INFO:honeydew.device_classes.base_fuchsia_device:Waiting for fuchsia-emulator to go offline...
INFO:honeydew.device_classes.base_fuchsia_device:fuchsia-emulator is offline.
INFO:honeydew.device_classes.base_fuchsia_device:Waiting for fuchsia-emulator to go online...
INFO:honeydew.device_classes.base_fuchsia_device:fuchsia-emulator is online.
INFO:honeydew.transports.sl4f:Starting SL4F server on fuchsia-emulator...


>>> emu.log_message_to_device(message="This is a test INFO message logged by HoneyDew", level=honeydew.custom_types.LEVEL.INFO)

>>> emu.snapshot(directory="/tmp/")
INFO:honeydew.device_classes.fuchsia_device_base:Snapshot file has been saved @ '/tmp/Snapshot_fuchsia-emulator_2023-03-01-01-09-43-PM.zip'
'/tmp/Snapshot_fuchsia-emulator_2023-03-01-01-09-43-PM.zip'
```

### Access the affordances
* [Bluetooth affordance](markdowns/bluetooth.md)
* [Tracing affordance](markdowns/tracing.md)

### Access the transports
* [Fastboot transport]

### Device object destruction
```python
>>> emu.close()
>>> del emu
```

## HoneyDew code guidelines
**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/conformance.sh`
**will automatically ensure you have followed the guidelines. Run this script**
**and fix any errors it suggests.**

**These guidelines need to be run at the least on the following patchsets:**
1. Initial patchset just before adding reviewers
2. Final patchset just before merging the CL
On all other patchsets, it is recommended but optional to run these guidelines.

### Install dependencies
Below guidelines requires certain dependencies that are not yet available in
[Fuchsia third-party]. So for the time being you need to [pip] install
these dependencies inside a [python virtual environment].

Follow [Installation](#Installation) to install HoneyDew which will also install
all of these dependencies.

Before proceeding further, ensure you are inside the virtual environment created
by above installation step.
```shell
# activate the virtual environment
~/fuchsia$ source $FUCHSIA_DIR/src/testing/end_to_end/.venvs/fuchsia_python_venv/bin/activate
(fuchsia_python_venv)~/fuchsia$
```

### Python Style Guide
**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/format.sh`
**will automatically perform all of the below mentioned style guide checks. Run this script**
**and fix any errors it suggests.**

HoneyDew code follows [Google Python Style Guide] and it is important to ensure
any new code written continue to follow these guidelines.

At this point, we do not have an automated way (in CQ) for identifying this and
alerting the CL author prior to submitting the CL. Until then CL author need to
follow the below instructions every time HoneyDew code is changed.

#### formatting
* Remove unused code by running below command
    ```shell
    (fuchsia_python_venv)~/fuchsia$ autoflake --in-place --remove-unused-variables --remove-all-unused-imports --remove-duplicate-keys --recursive $FUCHSIA_DIR/src/testing/end_to_end/honeydew/
    ```
* Sort the imports within python sources by running below command
    ```shell
    (fuchsia_python_venv)~/fuchsia$ isort $FUCHSIA_DIR/src/testing/end_to_end/honeydew/
    ```
* Ensure code is formatted using [yapf]
    * `fx format-code` underneath uses [yapf] for formatting the python code.
    * Run below command to format the code
    ```shell
    (fuchsia_python_venv)~/fuchsia$ fx format-code
    ```

#### linting
* Ensure code is [pylint] compliant
* Verify `pylint` is properly installed by running `pylint --help`
* Run below commands and fix all the issues pointed by `pylint`
```shell
(fuchsia_python_venv)~/fuchsia$ pylint --rcfile=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/linter/pylintrc $FUCHSIA_DIR/src/testing/end_to_end/honeydew/honeydew/

(fuchsia_python_venv)~/fuchsia$ pylint --rcfile=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/linter/pylintrc $FUCHSIA_DIR/src/testing/end_to_end/honeydew/tests/
```

#### type-checking
* Ensure code is [mypy] compliant
* Verify `mypy` is properly installed by running `mypy --help`
* Run below command and fix all the issues pointed by `mypy`
```shell
(fuchsia_python_venv)~/fuchsia$ mypy --config-file=$FUCHSIA_DIR/src/testing/end_to_end/honeydew/pyproject.toml $FUCHSIA_DIR/src/testing/end_to_end/honeydew/
```

### Code Coverage
**Running** `cd $FUCHSIA_DIR && sh $FUCHSIA_DIR/src/testing/end_to_end/honeydew/scripts/coverage.sh`
**will automatically perform the below mentioned coverage steps which shows
comprehensive coverage on the entire Honeydew codebase.**

**For a targeted report on only the locally modified files , run the command
above with the `--affected` flag.**

It is a hard requirement that HoneyDew code is well tested using a combination
of unit and functional tests.

Broadly this is how we can define the scope of each tests:
* Unit test cases
  * Tests individual code units (such as functions) in isolation from the rest
    of the system by mocking all of the dependencies.
  * Makes it easy to test different error conditions, corner cases etc
  * Minimum of 70% of HoneyDew code is tested using these unit tests
* Functional test cases
  * Aims to ensure that a given API works as intended and indeed does what it is
    supposed to do (that is, `<device>.reboot()` actually reboots Fuchsia
    device) which can’t be ensured using unit test cases
  * Every single HoneyDew’s Host-(Fuchsia)Target interaction API should have at
    least one functional test case

We use [coverage] tool for measuring the code coverage requirement of HoneyDew.

At this point, we do not have an automated way (in CQ) for identifying this and
alerting the CL author prior to submitting the CL. Until then CL author need to
follow the below instructions every time HoneyDew code is changed:

* Verify `coverage` is properly installed by running `coverage --help`
* Verify `parameterized` is properly installed by running
  `pip show parameterized`
* Run the below commands and ensure code you have touched has unit test coverage
```shell
# Set up environment for running coverage tool
(fuchsia_python_venv)~/fuchsia$ BUILD_DIR=$(cat "$FUCHSIA_DIR"/.fx-build-dir)
(fuchsia_python_venv)~/fuchsia$ OLD_PYTHONPATH=$PYTHONPATH
(fuchsia_python_venv)~/fuchsia$ PYTHONPATH=$FUCHSIA_DIR/$BUILD_DIR/host_x64:$FUCHSIA_DIR/src/developer/ffx/lib/fuchsia-controller/python:$PYTHONPATH

# Run unit tests using coverage tool
(fuchsia_python_venv)~/fuchsia$ coverage run -m unittest discover --top-level-directory $FUCHSIA_DIR/src/testing/end_to_end/honeydew --start-directory $FUCHSIA_DIR/src/testing/end_to_end/honeydew/tests/unit_tests --pattern "*_test.py"

# Run below command to generate code coverage stats
(fuchsia_python_venv)~/fuchsia$ coverage report -m

# Delete the .coverage file
(fuchsia_python_venv)~/fuchsia$ rm -rf .coverage

# Restore environment
(fuchsia_python_venv)~/fuchsia$ PYTHONPATH=$OLD_PYTHONPATH
```

### Cleanup
Finally, follow [Uninstallation](#Uninstallation) for the cleanup.

## Contributing
One of the primary goal while designing HoneyDew was to make it easy to
contribute for anyone working on Host-(Fuchsia)Target interactions.

HoneyDew is meant to be the one stop solution for any Host-(Fuchsia)Target
interactions. We can only make this possible when more people contribute to
HoneyDew and add more and more interactions that others can also benefit.

### Getting started
* HoneyDew is currently supported only on Linux. So please use a Linux machine
  for the development and testing of HoneyDew
* Follow [instructions on how to submit contributions to the Fuchsia project]
  for the Gerrit developer work flow
* If you are using [vscode IDE] then I recommend installing these
  [vscode extensions] from [vscode extension marketplace] and using these
  [vscode settings]

### Best Practices
Here are some of the best practices that should be followed while contributing
to HoneyDew:
* If contribution involves adding a new class method or new class itself, you
  may have to update the [interfaces] definitions
* Ensure there is both [unit tests] and [functional tests] coverage for
  contribution and have run the impacted [functional tests] either locally or
  in infra to make sure contribution is indeed working
* If a new unit test is added,
  * ensure [unit tests README] has the instructions to run this new test
  * ensure this new test is included in `group("tests")` section in the
    `BUILD.gn` file (located in the same directory as unit test)
  * ensure this new test is included in `group("tests")` section in the
    [top level HoneyDew unit tests BUILD] file
* If a new functional test is added,
  * ensure [functional tests README] has the instructions to run this new test
  * ensure this new test is included in `group("tests")` section in the
    [top level HoneyDew functional tests BUILD] file
* Ensure code is meeting all the [HoneyDew code guidelines]
* Before merging the CL, ensure CL does not introduce any regressions by
  successfully running the [honeydew builders using try-jobs]
* At least one of the [HoneyDew OWNERS] should be added as a reviewer

### Code Review Expectations
Here are some of the things to follow during HoneyDew CL review process as a
CL author/contributor (or) CL reviewer/approver:

#### Author
* On the initial patchset where reviewers will be added, do the following before
  starting the review:
  1. Make sure you have followed all of the [Best Practices]
  2. Include the following information in the commit message:
    ```
    ...

    Verified the following on Patchset: <initial patchset number>
    * HoneyDew code guidelines
    * Successfully ran the impacted functional tests using [LocalRun|InfraRun]
    ```
* On final patchset that will be used for merging, do the following before
  merging the CL:
  1. Re-run the [Honeydew code conformance scripts]
  2. Ensure CL does not introduce any regressions by successfully running the
     [honeydew builders using try-jobs]
  3. Update the commit message with the final patchset number:
    ```
    ...

    Verified the following on Patchset: <final patchset number>
    * HoneyDew code guidelines
    * Successfully ran the impacted functional tests using [LocalRun|InfraRun]
    * Successfully ran Honeydew builders using try-jobs
    ```

#### Reviewer
* Remind the CL author to follow [Best Practices] section by opening a comment
  and asking author to resolve this comment only after they verify on absolute
  final patchset that will be merged
* Verify the author has included all the information in commit message as
  mentioned [here](#Author)

[HoneyDew OWNERS]: ../OWNERS

[Best Practices]: #Best-Practices

[HoneyDew code guidelines]: #honeydew-code-guidelines

[Honeydew code conformance scripts]: #honeydew-code-guidelines

[interfaces]: interfaces/

[unit tests]: tests/unit_tests/

[unit tests README]: tests/unit_tests/README.md

[unit tests BUILD.gn]: tests/unit_tests/BUILD.gn#10

[top level HoneyDew unit tests BUILD]: tests/unit_tests/BUILD.gn

[functional tests]: tests/functional_tests/

[functional tests README]: tests/functional_tests/README.md

[top level HoneyDew functional tests BUILD]: tests/functional_tests/BUILD.gn

[honeydew builders using try-jobs]: images/pye2e_builders.png

[instructions on how to submit contributions to the Fuchsia project]: https://fuchsia.dev/fuchsia-src/development/source_code/contribute_changes

[Fastboot transport]: markdowns/fastboot.md

[//third_party]: https://fuchsia.googlesource.com/third_party/

[pylint]: https://pypi.org/project/pylint/

[mypy]: https://mypy.readthedocs.io/en/stable/

[yapf]: https://github.com/google/yapf

[vscode IDE]: https://code.visualstudio.com/docs/python/python-tutorial

[vscode extension marketplace]: https://code.visualstudio.com/docs/editor/extension-marketplace

[vscode extensions]: vscode/extensions.json

[vscode settings]: vscode/settings.json

[Google Python Style Guide]: https://google.github.io/styleguide/pyguide.html

[Fuchsia third-party]: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/third_party/

[python virtual environment]: https://docs.python.org/3/tutorial/venv.html

[coverage]: https://coverage.readthedocs.io/

[pip]: https://pip.pypa.io/en/stable/getting-started/

