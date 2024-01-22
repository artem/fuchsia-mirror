#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""PowerSwitch auxiliary device implementation using DMC."""

import enum
import logging
import os
import platform
import subprocess

from honeydew.interfaces.auxiliary_devices import power_switch

_LOGGER: logging.Logger = logging.getLogger(__name__)

_TIMEOUTS: dict[str, float] = {
    "COMMAND_RESPONSE": 60,
}

DMC_PATH_KEY: str = "DMC_PATH"


class PowerSwitchDmcError(power_switch.PowerSwitchError):
    """Custom exception class for raising DMC related errors."""


class PowerState(enum.StrEnum):
    """Different power states supported by `dmc set-power-state`."""

    ON = "on"
    OFF = "off"
    CYCLE = "cycle"


class PowerSwitchDmc(power_switch.PowerSwitch):
    """PowerSwitch auxiliary device implementation using DMC.

    Note: `PowerSwitchDmc` is implemented to do power off/on a Fuchsia device
    that is hosted in Fuchsia Infra labs and Fuchsia Infra workflows are
    different from local workflows. As a result, `PowerSwitchDmc` will only work
    for FuchsiaInfra labs and will not work for local set ups or any other
    setups for that matter.

    Args:
        device_name: Device name returned by `ffx target list`.
    """

    def __init__(self, device_name: str) -> None:
        self._name: str = device_name

        try:
            self._dmc_path: str = os.environ[DMC_PATH_KEY]
        except KeyError as error:
            raise PowerSwitchDmcError(
                f"{DMC_PATH_KEY} environmental variable is not set on "
                f"{platform.node()}"
            ) from error

    # List all the public methods
    def power_off(self, outlet: int | None = None) -> None:
        """Turns off the power of the Fuchsia device.

        Args:
            outlet: None. Not being used by this implementation.

        Raises:
            PowerSwitchError: In case of failure.
        """
        _LOGGER.info("Powering off %s...", self._name)
        self._run(
            command=self._generate_dmc_power_state_cmd(
                power_state=PowerState.OFF
            )
        )
        _LOGGER.info("Successfully powered off %s...", self._name)

    def power_on(self, outlet: int | None = None) -> None:
        """Turns on the power of the Fuchsia device.

        Args:
            outlet: None. Not being used by this implementation.

        Raises:
            PowerSwitchError: In case of failure.
        """
        _LOGGER.info("Powering on %s...", self._name)
        self._run(
            command=self._generate_dmc_power_state_cmd(
                power_state=PowerState.ON
            )
        )
        _LOGGER.info("Successfully powered on %s...", self._name)

    def _generate_dmc_power_state_cmd(
        self, power_state: PowerState
    ) -> list[str]:
        """Helper method that takes the PowerState and generates the DMC
        power_state command in the format accepted by `_run()` method.

        Args:
            power_state: PowerState

        Returns:
            DMC power_state command
        """
        return (
            f"{self._dmc_path} set-power-state "
            f"-nodename {self._name} "
            f"-state {power_state}"
        ).split()

    def _run(
        self, command: list[str], timeout: float = _TIMEOUTS["COMMAND_RESPONSE"]
    ) -> str:
        """Helper method to run a command and returns the output.

        Args:
            command: Command to run.
            timeout: How long in sec to wait for command to complete.

        Returns:
            Command output.

        Raises:
            PowerSwitchError: In case of failure.
        """
        try:
            _LOGGER.debug("Running the command: '%s'...", command)

            output: str = subprocess.check_output(
                command, timeout=timeout
            ).decode()

            _LOGGER.debug(
                "Output returned by command '%s' is: '%s'",
                command,
                output,
            )
            return output
        except subprocess.CalledProcessError as err:
            message: str = (
                f"Command '{command}' failed. returncode = {err.returncode}"
            )
            if err.stdout:
                message += f", stdout = {err.stdout}"
            if err.stderr:
                message += f", stderr = {err.stderr}."
            _LOGGER.debug(message)

            raise power_switch.PowerSwitchError(err) from err
        except Exception as err:  # pylint: disable=broad-except
            raise power_switch.PowerSwitchError(err) from err
