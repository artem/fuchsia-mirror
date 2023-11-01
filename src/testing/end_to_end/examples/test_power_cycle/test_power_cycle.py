#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Power Cycle test."""

import importlib
import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.interfaces.auxiliary_devices import power_switch
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


_DMC_MODULE: str = "honeydew.auxiliary_devices.power_switch_dmc"
_DMC_CLASS: str = "PowerSwitchDmc"


class PowerCycleTest(fuchsia_base_test.FuchsiaBaseTest):
    """power cycle test.

    Attributes:
        dut: FuchsiaDevice object.

    Required Mobly Test Params:
        num_power_cycles (int): Number of times power_cycle test need to be
            executed.
    """

    def pre_run(self) -> None:
        """Mobly method used to generate the test cases at run time."""
        test_arg_tuple_list: list[tuple[int]] = []

        for iteration in range(
            1, int(self.user_params["num_power_cycles"]) + 1
        ):
            test_arg_tuple_list.append((iteration,))

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self._name_func,
            arg_sets=test_arg_tuple_list,
        )

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        self._device_config: dict[str, str] = self._get_device_config(
            controller_type="FuchsiaDevice",
            identifier_key="name",
            identifier_value=self.dut.device_name,
        )
        self._power_switch_hw: dict[str, str] | None = self._device_config.get(
            "power_switch_hw"
        )
        self._power_switch_impl: dict[
            str, str
        ] | None = self._device_config.get("power_switch_impl")

        self._power_switch: power_switch.PowerSwitch
        self._port: int | None = None

        # ToDo(b/307970573): Once supported, update below logic to look for
        # test execution environment (local or remote). Accordingly raise
        # exception if power_switch_hw and power_switch_impl are not provided
        # in testbed yaml file when running in local mode.
        if self._power_switch_hw and self._power_switch_impl:
            _LOGGER.info("Test execution is in local mode...")

            _LOGGER.debug(
                "Importing %s.%s module",
                self._power_switch_impl["module"],
                self._power_switch_impl["class"],
            )
            power_switch_class: power_switch.PowerSwitch = getattr(
                importlib.import_module(self._power_switch_impl["module"]),
                self._power_switch_impl["class"],
            )

            _LOGGER.debug(
                "Instantiating %s.%s module",
                self._power_switch_impl["module"],
                self._power_switch_impl["class"],
            )
            self._power_switch = power_switch_class(**self._power_switch_hw)

            self._port = int(self._power_switch_hw["port"])
        else:
            _LOGGER.info("Test execution is in infra mode...")

            _LOGGER.debug("Importing %s.%s module", _DMC_MODULE, _DMC_CLASS)
            power_switch_class: power_switch.PowerSwitch = getattr(
                importlib.import_module(_DMC_MODULE), _DMC_CLASS
            )

            _LOGGER.debug("Instantiating %s.%s module", _DMC_MODULE, _DMC_CLASS)
            self._power_switch = power_switch_class(
                device_name=self.dut.device_name
            )

    def _test_logic(self, iteration: int) -> None:
        """Test case logic that performs power cycle of fuchsia device."""
        _LOGGER.info("Starting the Power Cycle test iteration# %s", iteration)
        self.dut.power_cycle(power_switch=self._power_switch, outlet=self._port)
        _LOGGER.info(
            "Successfully ended the Power Cycle test iteration# %s", iteration
        )

    def _name_func(self, iteration: int) -> str:
        """This function generates the names of each test case based on each
        argument set.

        The name function should have the same signature as the actual test
        logic function.

        Returns:
            Test case name
        """
        return f"test_power_cycle_{iteration}"


if __name__ == "__main__":
    test_runner.main()
