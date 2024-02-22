// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/**
 * @file
 * This file implements the spinel based radio transceiver.
 *
 */

#include "radio.h"

#include <openthread/platform/radio.h>

#ifdef OPENTHREAD_231010
static ot::Spinel::RadioSpinel<ot::Fuchsia::SpinelFidlInterface> sRadioSpinel;
static const otRadioSpinelContext radio_ctx{};
const void *radio_ctx_ptr = &radio_ctx;

extern "C" void otPlatRadioGetIeeeEui64(otInstance *a_instance, uint8_t *a_ieee_eui64) {
  OT_UNUSED_VARIABLE(a_instance);
  SuccessOrDie(sRadioSpinel.GetIeeeEui64(a_ieee_eui64));
}

extern "C" void otPlatRadioSetPanId(otInstance *a_instance, uint16_t panid) {
  OT_UNUSED_VARIABLE(a_instance);
  SuccessOrDie(sRadioSpinel.SetPanId(panid));
}

extern "C" void otPlatRadioSetExtendedAddress(otInstance *a_instance,
                                              const otExtAddress *a_address) {
  OT_UNUSED_VARIABLE(a_instance);
  otExtAddress addr;

  for (size_t i = 0; i < sizeof(addr); i++) {
    addr.m8[i] = a_address->m8[sizeof(addr) - 1 - i];
  }

  SuccessOrDie(sRadioSpinel.SetExtendedAddress(addr));
}

extern "C" void otPlatRadioSetShortAddress(otInstance *a_instance, uint16_t a_address) {
  OT_UNUSED_VARIABLE(a_instance);
  SuccessOrDie(sRadioSpinel.SetShortAddress(a_address));
}

extern "C" void otPlatRadioSetPromiscuous(otInstance *a_instance, bool a_enable) {
  OT_UNUSED_VARIABLE(a_instance);
  SuccessOrDie(sRadioSpinel.SetPromiscuous(a_enable));
}

extern "C" void platformRadioInit(const otPlatformConfig *a_platform_config) {
  SuccessOrDie(sRadioSpinel.GetSpinelInterface().Init());
  sRadioSpinel.Init(a_platform_config->reset_rcp,
                    /* aSkipRcpCompatibilityCheck */ false);
}

extern "C" otError otPlatRadioEnable(otInstance *a_instance) {
  otError ret_val = sRadioSpinel.Enable(a_instance);
  return ret_val;
}

extern "C" otError otPlatRadioDisable(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.Disable();
}

extern "C" otError otPlatRadioSleep(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.Sleep();
}

extern "C" otError otPlatRadioReceive(otInstance *a_instance, uint8_t a_channel) {
  OT_UNUSED_VARIABLE(a_instance);

  otError error;
  SuccessOrExit(error = sRadioSpinel.Receive(a_channel));

exit:
  return error;
}

extern "C" otError otPlatRadioTransmit(otInstance *a_instance, otRadioFrame *a_frame) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.Transmit(*a_frame);
}

extern "C" otRadioFrame *otPlatRadioGetTransmitBuffer(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return &sRadioSpinel.GetTransmitFrame();
}

extern "C" int8_t otPlatRadioGetRssi(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetRssi();
}

extern "C" otRadioCaps otPlatRadioGetCaps(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetRadioCaps();
}

extern "C" bool otPlatRadioGetPromiscuous(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.IsPromiscuous();
}

extern "C" void otPlatRadioEnableSrcMatch(otInstance *a_instance, bool a_enable) {
  OT_UNUSED_VARIABLE(a_instance);
  SuccessOrDie(sRadioSpinel.EnableSrcMatch(a_enable));
}

extern "C" otError otPlatRadioAddSrcMatchShortEntry(otInstance *a_instance,
                                                    uint16_t a_short_address) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.AddSrcMatchShortEntry(a_short_address);
}

extern "C" otError otPlatRadioAddSrcMatchExtEntry(otInstance *a_instance,
                                                  const otExtAddress *a_ext_address) {
  OT_UNUSED_VARIABLE(a_instance);
  otExtAddress addr;

  for (size_t i = 0; i < sizeof(addr); i++) {
    addr.m8[i] = a_ext_address->m8[sizeof(addr) - 1 - i];
  }

  return sRadioSpinel.AddSrcMatchExtEntry(addr);
}
extern "C" otError otPlatRadioClearSrcMatchShortEntry(otInstance *a_instance,
                                                      uint16_t a_short_address) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.ClearSrcMatchShortEntry(a_short_address);
}

extern "C" otError otPlatRadioClearSrcMatchExtEntry(otInstance *a_instance,
                                                    const otExtAddress *a_ext_address) {
  OT_UNUSED_VARIABLE(a_instance);
  otExtAddress addr;

  for (size_t i = 0; i < sizeof(addr); i++) {
    addr.m8[i] = a_ext_address->m8[sizeof(addr) - 1 - i];
  }

  return sRadioSpinel.ClearSrcMatchExtEntry(addr);
}

extern "C" void otPlatRadioClearSrcMatchShortEntries(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  SuccessOrDie(sRadioSpinel.ClearSrcMatchShortEntries());
}

extern "C" void otPlatRadioClearSrcMatchExtEntries(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  SuccessOrDie(sRadioSpinel.ClearSrcMatchExtEntries());
}

extern "C" otError otPlatRadioEnergyScan(otInstance *a_instance, uint8_t a_scan_channel,
                                         uint16_t a_scan_duration) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.EnergyScan(a_scan_channel, a_scan_duration);
}

extern "C" int8_t otPlatRadioGetReceiveSensitivity(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetReceiveSensitivity();
}

void platformRadioProcess(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  sRadioSpinel.Process(radio_ctx_ptr);
}

otError otPlatRadioGetCcaEnergyDetectThreshold(otInstance *a_instance, int8_t *a_threshold) {
  OT_UNUSED_VARIABLE(a_instance);
  assert(a_threshold != NULL);
  return sRadioSpinel.GetCcaEnergyDetectThreshold(*a_threshold);
}

otError otPlatRadioSetCcaEnergyDetectThreshold(otInstance *a_instance, int8_t a_threshold) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.SetCcaEnergyDetectThreshold(a_threshold);
}

otError otPlatRadioGetTransmitPower(otInstance *a_instance, int8_t *a_power) {
  OT_UNUSED_VARIABLE(a_instance);
  assert(a_power != NULL);
  return sRadioSpinel.GetTransmitPower(*a_power);
}

otError otPlatRadioSetTransmitPower(otInstance *a_instance, int8_t a_power) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.SetTransmitPower(a_power);
}

extern "C" void platformRadioDeinit(void) { sRadioSpinel.Deinit(); }

#if OPENTHREAD_CONFIG_DIAG_ENABLE

extern "C" otError otPlatDiagProcess(otInstance *a_instance, uint8_t a_args_length, char *a_args[],
                                     char *a_output, size_t a_output_max_len) {
  OT_UNUSED_VARIABLE(a_instance);
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE] = {'\0'};
  char *cur = cmd;
  char *end = cmd + sizeof(cmd);

  for (uint8_t index = 0; (index < a_args_length) && (cur < end); index++) {
    cur += snprintf(cur, static_cast<size_t>(end - cur), "%s ", a_args[index]);
  }

  return sRadioSpinel.PlatDiagProcess(cmd, a_output, a_output_max_len);
}

extern "C" void otPlatDiagModeSet(bool a_mode) {
  SuccessOrExit(sRadioSpinel.PlatDiagProcess(a_mode ? "start" : "stop", nullptr, 0));
  sRadioSpinel.SetDiagEnabled(a_mode);

exit:
  return;
}

bool otPlatDiagModeGet(void) { return sRadioSpinel.IsDiagEnabled(); }

extern "C" void otPlatDiagTxPowerSet(int8_t a_tx_power) {
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "power %d", a_tx_power);
  SuccessOrExit(sRadioSpinel.PlatDiagProcess(cmd, nullptr, 0));

exit:
  return;
}

extern "C" void otPlatDiagChannelSet(uint8_t a_channel) {
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "channel %d", a_channel);
  SuccessOrExit(sRadioSpinel.PlatDiagProcess(cmd, nullptr, 0));

exit:
  return;
}

extern "C" void otPlatDiagRadioReceived(otInstance *a_instance, otRadioFrame *a_frame,
                                        otError a_error) {
  OT_UNUSED_VARIABLE(a_instance);
  OT_UNUSED_VARIABLE(a_frame);
  OT_UNUSED_VARIABLE(a_error);
}

extern "C" void otPlatDiagAlarmCallback(otInstance *a_instance) { OT_UNUSED_VARIABLE(a_instance); }

#endif  // OPENTHREAD_CONFIG_DIAG_ENABLE

otError otPlatRadioSetRegion(otInstance *a_instance, uint16_t a_region_code) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.SetRadioRegion(a_region_code);
}

otError otPlatRadioGetRegion(otInstance *a_instance, uint16_t *a_region_code) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetRadioRegion(a_region_code);
}

#if OPENTHREAD_CONFIG_MLE_LINK_METRICS_SUBJECT_ENABLE
otError otPlatRadioConfigureEnhAckProbing(otInstance *aInstance, otLinkMetrics aLinkMetrics,
                                          const otShortAddress aShortAddress,
                                          const otExtAddress *aExtAddress) {
  OT_UNUSED_VARIABLE(aInstance);

  return sRadioSpinel.ConfigureEnhAckProbing(aLinkMetrics, aShortAddress, *aExtAddress);
}
#endif

#if OPENTHREAD_API_VERSION < 175
void otPlatRadioSetMacKey(otInstance *a_instance, uint8_t a_key_id_mode, uint8_t a_key_id,
                          const otMacKey *a_prev_key, const otMacKey *a_curr_key,
                          const otMacKey *a_next_key) {
  SuccessOrDie(
      sRadioSpinel.SetMacKey(a_key_id_mode, a_key_id, *a_prev_key, *a_curr_key, *a_next_key));
  OT_UNUSED_VARIABLE(a_instance);
}
#else
void otPlatRadioSetMacKey(otInstance *a_instance, uint8_t a_key_id_mode, uint8_t a_key_id,
                          const otMacKeyMaterial *a_prev_key, const otMacKeyMaterial *a_curr_key,
                          const otMacKeyMaterial *a_next_key, otRadioKeyType a_key_type) {
  SuccessOrDie(sRadioSpinel.SetMacKey(a_key_id_mode, a_key_id, a_prev_key, a_curr_key, a_next_key));
  OT_UNUSED_VARIABLE(a_instance);
  OT_UNUSED_VARIABLE(a_key_type);
}
#endif

extern "C" bool otPlatRadioIsEnabled(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.IsEnabled();
}

extern "C" const char *otPlatRadioGetVersionString(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetVersion();
}

extern "C" otError otPlatRadioGetFemLnaGain(otInstance *a_instance, int8_t *a_gain) {
  OT_UNUSED_VARIABLE(a_instance);
  assert(a_gain != nullptr);
  return sRadioSpinel.GetFemLnaGain(*a_gain);
}

extern "C" otError otPlatRadioSetFemLnaGain(otInstance *a_instance, int8_t a_gain) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.SetFemLnaGain(a_gain);
}

#if OPENTHREAD_CONFIG_PLATFORM_RADIO_COEX_ENABLE
extern "C" otError otPlatRadioSetCoexEnabled(otInstance *a_instance, bool a_enabled) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.SetCoexEnabled(a_enabled);
}

extern "C" bool otPlatRadioIsCoexEnabled(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.IsCoexEnabled();
}

extern "C" otError otPlatRadioGetCoexMetrics(otInstance *a_instance,
                                             otRadioCoexMetrics *a_coex_metrics) {
  OT_UNUSED_VARIABLE(a_instance);

  otError error = OT_ERROR_NONE;

  VerifyOrExit(a_coex_metrics != nullptr, error = OT_ERROR_INVALID_ARGS);

  error = sRadioSpinel.GetCoexMetrics(*a_coex_metrics);

exit:
  return error;
}
#endif

extern "C" uint32_t otPlatRadioGetSupportedChannelMask(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetRadioChannelMask(false);
}

extern "C" uint32_t otPlatRadioGetPreferredChannelMask(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetRadioChannelMask(true);
}

extern "C" otRadioState otPlatRadioGetState(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetState();
}

extern "C" void otPlatRadioSetMacFrameCounter(otInstance *a_instance,
                                              uint32_t a_mac_frame_counter) {
  SuccessOrDie(sRadioSpinel.SetMacFrameCounter(a_mac_frame_counter, /* aSetIfLarger */ false));
  OT_UNUSED_VARIABLE(a_instance);
}

extern "C" void otPlatRadioSetMacFrameCounterIfLarger(otInstance *aInstance,
                                                      uint32_t aMacFrameCounter) {
  SuccessOrDie(sRadioSpinel.SetMacFrameCounter(aMacFrameCounter, /* aSetIfLarger */ true));
  OT_UNUSED_VARIABLE(aInstance);
}

extern "C" uint64_t otPlatRadioGetNow(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.GetNow();
}

extern "C" uint8_t otPlatRadioGetCslAccuracy(otInstance *a_instance) {
  OT_UNUSED_VARIABLE(a_instance);

  return 0;
}

extern "C" otError otPlatRadioSetChannelMaxTransmitPower(otInstance *a_instance, uint8_t a_channel,
                                                         int8_t a_max_power) {
  OT_UNUSED_VARIABLE(a_instance);
  return sRadioSpinel.SetChannelMaxTransmitPower(a_channel, a_max_power);
}

extern "C" otError otPlatRadioReceiveAt(otInstance *a_instance, uint8_t a_channel, uint32_t a_start,
                                        uint32_t a_duration) {
  OT_UNUSED_VARIABLE(a_instance);
  OT_UNUSED_VARIABLE(a_channel);
  OT_UNUSED_VARIABLE(a_start);
  OT_UNUSED_VARIABLE(a_duration);
  return OT_ERROR_NOT_IMPLEMENTED;
}
#endif  // OPENTHREAD_231010

#ifdef OPENTHREAD_240214
// modified POSIX implementation for Fuchsia
#include <openthread/platform/diag.h>
#include <platform/exit_code.h>

#include <utils/parse_cmdline.hpp>

static ot::Posix::Radio sRadio;

namespace ot {
namespace Posix {

namespace {
extern "C" void platformRadioInit(const char *aUrl) { sRadio.Init(aUrl); }
}  // namespace

Radio::Radio(void) : mRadioUrl(nullptr), mRadioSpinel(), mSpinelInterface(nullptr) {}

void Radio::Init(const char *aUrl) {
  bool resetRadio;
  bool skipCompatibilityCheck;
  spinel_iid_t iidList[Spinel::kSpinelHeaderMaxNumIid];
  struct ot::Spinel::RadioSpinelCallbacks callbacks;

  mRadioUrl.Init(aUrl);
  VerifyOrDie(mRadioUrl.GetPath() != nullptr, OT_EXIT_INVALID_ARGUMENTS);

  memset(&callbacks, 0, sizeof(callbacks));
#if OPENTHREAD_CONFIG_DIAG_ENABLE
  callbacks.mDiagReceiveDone = otPlatDiagRadioReceiveDone;
  callbacks.mDiagTransmitDone = otPlatDiagRadioTransmitDone;
#endif  // OPENTHREAD_CONFIG_DIAG_ENABLE
  callbacks.mEnergyScanDone = otPlatRadioEnergyScanDone;
  callbacks.mReceiveDone = otPlatRadioReceiveDone;
  callbacks.mTransmitDone = otPlatRadioTxDone;
  callbacks.mTxStarted = otPlatRadioTxStarted;

  GetIidListFromRadioUrl(iidList);

#if OPENTHREAD_POSIX_VIRTUAL_TIME
  VirtualTimeInit();
#endif

  mSpinelInterface = CreateSpinelInterface(mRadioUrl.GetProtocol());
  VerifyOrDie(mSpinelInterface != nullptr, OT_EXIT_FAILURE);

  resetRadio = !mRadioUrl.HasParam("no-reset");
  skipCompatibilityCheck = mRadioUrl.HasParam("skip-rcp-compatibility-check");

  mRadioSpinel.SetCallbacks(callbacks);
  mRadioSpinel.Init(*mSpinelInterface, resetRadio, skipCompatibilityCheck, iidList,
                    OT_ARRAY_LENGTH(iidList));
  otPlatLog(OT_LOG_LEVEL_INFO, OT_LOG_REGION_PLATFORM, "instance init:%p - iid = %d",
            (void *)&mRadioSpinel, iidList[0]);
  otPlatLog(OT_LOG_LEVEL_INFO, OT_LOG_REGION_PLATFORM, "resetRadio:%d,skipCompatibilityCheck:%d",
            resetRadio, skipCompatibilityCheck);
  ProcessRadioUrl(mRadioUrl);
}

#if OPENTHREAD_POSIX_VIRTUAL_TIME
void Radio::VirtualTimeInit(void) {
  // The last argument must be the node id
  const char *nodeId = nullptr;

  for (const char *arg = nullptr; (arg = mRadioUrl.GetValue("forkpty-arg", arg)) != nullptr;
       nodeId = arg) {
  }

  virtualTimeInit(static_cast<uint16_t>(atoi(nodeId)));
}
#endif

Spinel::SpinelInterface *Radio::CreateSpinelInterface(const char *aInterfaceName) {
  Spinel::SpinelInterface *interface;

  if (aInterfaceName == nullptr) {
    DieNow(OT_ERROR_FAILED);
  }
#if OPENTHREAD_POSIX_CONFIG_SPINEL_HDLC_INTERFACE_ENABLE
  else if (HdlcInterface::IsInterfaceNameMatch(aInterfaceName)) {
    interface = new (&mSpinelInterfaceRaw) HdlcInterface(mRadioUrl);
  }
#endif
#if OPENTHREAD_POSIX_CONFIG_SPINEL_SPI_INTERFACE_ENABLE
  else if (Posix::SpiInterface::IsInterfaceNameMatch(aInterfaceName)) {
    interface = new (&mSpinelInterfaceRaw) SpiInterface(mRadioUrl);
  }
#endif
#if OPENTHREAD_POSIX_CONFIG_SPINEL_VENDOR_INTERFACE_ENABLE
  else if (VendorInterface::IsInterfaceNameMatch(aInterfaceName)) {
    interface = new (&mSpinelInterfaceRaw) VendorInterface(mRadioUrl);
  }
#endif
  else {
    otPlatLog(OT_LOG_LEVEL_WARN, OT_LOG_REGION_PLATFORM,
              "The Spinel interface name \"%s\" is not supported!", aInterfaceName);
    DieNow(OT_ERROR_FAILED);
  }

  return interface;
}

void Radio::ProcessRadioUrl(const RadioUrl &aRadioUrl) {
  const char *region;
  int8_t value;

  if (aRadioUrl.HasParam("ncp-dataset")) {
    otPlatLog(OT_LOG_LEVEL_CRIT, OT_LOG_REGION_PLATFORM,
              "The argument \"ncp-dataset\" is no longer supported");
    DieNow(OT_ERROR_FAILED);
  }

  if (aRadioUrl.HasParam("fem-lnagain")) {
    SuccessOrDie(aRadioUrl.ParseInt8("fem-lnagain", value));
    SuccessOrDie(mRadioSpinel.SetFemLnaGain(value));
  }

  if (aRadioUrl.HasParam("cca-threshold")) {
    SuccessOrDie(aRadioUrl.ParseInt8("cca-threshold", value));
    SuccessOrDie(mRadioSpinel.SetCcaEnergyDetectThreshold(value));
  }

  OT_UNUSED_VARIABLE(region);
#if 0
    if ((region = aRadioUrl.GetValue("region")) != nullptr)
    {
        uint16_t regionCode;

        VerifyOrDie(strnlen(region, 3) == 2, OT_EXIT_INVALID_ARGUMENTS);
        regionCode = static_cast<uint16_t>(static_cast<uint16_t>(region[0]) << 8) + static_cast<uint16_t>(region[1]);
        SuccessOrDie(otPlatRadioSetRegion(gInstance, regionCode));
    }
#endif

  ProcessMaxPowerTable(aRadioUrl);

#if OPENTHREAD_CONFIG_PLATFORM_RADIO_COEX_ENABLE
  {
    const char *enableCoex = aRadioUrl.GetValue("enable-coex");
    if (enableCoex != nullptr) {
      SuccessOrDie(mRadioSpinel.SetCoexEnabled(enableCoex[0] != '0'));
    }
  }
#endif  // OPENTHREAD_CONFIG_PLATFORM_RADIO_COEX_ENABLE
}

void Radio::ProcessMaxPowerTable(const RadioUrl &aRadioUrl) {
  OT_UNUSED_VARIABLE(aRadioUrl);

#if OPENTHREAD_POSIX_CONFIG_MAX_POWER_TABLE_ENABLE
  otError error;
  constexpr int8_t kPowerDefault = 30;  // Default power 1 watt (30 dBm).
  const char *str = nullptr;
  char *pSave = nullptr;
  uint8_t channel = ot::Radio::kChannelMin;
  int8_t power = kPowerDefault;
  const char *maxPowerTable;

  VerifyOrExit((maxPowerTable = aRadioUrl.GetValue("max-power-table")) != nullptr);

  for (str = strtok_r(const_cast<char *>(maxPowerTable), ",", &pSave);
       str != nullptr && channel <= ot::Radio::kChannelMax; str = strtok_r(nullptr, ",", &pSave)) {
    power = static_cast<int8_t>(strtol(str, nullptr, 0));
    error = mRadioSpinel.SetChannelMaxTransmitPower(channel, power);
    VerifyOrDie((error == OT_ERROR_NONE) || (error == OT_ERROR_NOT_IMPLEMENTED), OT_EXIT_FAILURE);
    if (error == OT_ERROR_NOT_IMPLEMENTED) {
      otPlatLog(OT_LOG_LEVEL_WARN, OT_LOG_REGION_PLATFORM,
                "The RCP doesn't support setting the max transmit power");
    }

    ++channel;
  }

  // Use the last power if omitted.
  while (channel <= ot::Radio::kChannelMax) {
    error = mRadioSpinel.SetChannelMaxTransmitPower(channel, power);
    VerifyOrDie((error == OT_ERROR_NONE) || (error == OT_ERROR_NOT_IMPLEMENTED), OT_ERROR_FAILED);
    if (error == OT_ERROR_NOT_IMPLEMENTED) {
      otPlatLog(OT_LOG_LEVEL_WARN, OT_LOG_REGION_PLATFORM,
                "The RCP doesn't support setting the max transmit power");
    }

    ++channel;
  }

  VerifyOrDie(str == nullptr, OT_EXIT_INVALID_ARGUMENTS);

exit:
  return;
#endif  // OPENTHREAD_POSIX_CONFIG_MAX_POWER_TABLE_ENABLE
}

void Radio::GetIidListFromRadioUrl(spinel_iid_t (&aIidList)[Spinel::kSpinelHeaderMaxNumIid]) {
  const char *iidString;
  const char *iidListString;

  memset(aIidList, SPINEL_HEADER_INVALID_IID, sizeof(aIidList));

  iidString = (mRadioUrl.GetValue("iid"));
  iidListString = (mRadioUrl.GetValue("iid-list"));

#if OPENTHREAD_CONFIG_MULTIPAN_RCP_ENABLE
  // First entry to the aIidList must be the IID of the host application.
  VerifyOrDie(iidString != nullptr, OT_EXIT_INVALID_ARGUMENTS);
  aIidList[0] = static_cast<spinel_iid_t>(atoi(iidString));

  if (iidListString != nullptr) {
    // Convert string to an array of integers.
    // Integer i is for traverse the iidListString.
    // Integer j is for aIidList array offset location.
    // First entry of aIidList is for host application iid hence j start from 1.
    for (uint8_t i = 0, j = 1; iidListString[i] != '\0' && j < Spinel::kSpinelHeaderMaxNumIid;
         i++) {
      if (iidListString[i] == ',') {
        j++;
        continue;
      }

      if (iidListString[i] < '0' || iidListString[i] > '9') {
        DieNow(OT_EXIT_INVALID_ARGUMENTS);
      } else {
        aIidList[j] = iidListString[i] - '0';
        VerifyOrDie(aIidList[j] < Spinel::kSpinelHeaderMaxNumIid, OT_EXIT_INVALID_ARGUMENTS);
      }
    }
  }
#else   // !OPENTHREAD_CONFIG_MULTIPAN_RCP_ENABLE
  VerifyOrDie(iidString == nullptr, OT_EXIT_INVALID_ARGUMENTS);
  VerifyOrDie(iidListString == nullptr, OT_EXIT_INVALID_ARGUMENTS);
  aIidList[0] = 0;
#endif  // OPENTHREAD_CONFIG_MULTIPAN_RCP_ENABLE
}

}  // namespace Posix
}  // namespace ot

ot::Spinel::RadioSpinel &GetRadioSpinel(void) { return sRadio.GetRadioSpinel(); }

extern "C" void platformRadioDeinit(void) { GetRadioSpinel().Deinit(); }

void otPlatRadioGetIeeeEui64(otInstance *aInstance, uint8_t *aIeeeEui64) {
  OT_UNUSED_VARIABLE(aInstance);
  SuccessOrDie(GetRadioSpinel().GetIeeeEui64(aIeeeEui64));
}

void otPlatRadioSetPanId(otInstance *aInstance, uint16_t panid) {
  OT_UNUSED_VARIABLE(aInstance);
  SuccessOrDie(GetRadioSpinel().SetPanId(panid));
}

void otPlatRadioSetExtendedAddress(otInstance *aInstance, const otExtAddress *aAddress) {
  OT_UNUSED_VARIABLE(aInstance);
  otExtAddress addr;

  for (size_t i = 0; i < sizeof(addr); i++) {
    addr.m8[i] = aAddress->m8[sizeof(addr) - 1 - i];
  }

  SuccessOrDie(GetRadioSpinel().SetExtendedAddress(addr));
}

void otPlatRadioSetShortAddress(otInstance *aInstance, uint16_t aAddress) {
  OT_UNUSED_VARIABLE(aInstance);
  SuccessOrDie(GetRadioSpinel().SetShortAddress(aAddress));
}

void otPlatRadioSetPromiscuous(otInstance *aInstance, bool aEnable) {
  OT_UNUSED_VARIABLE(aInstance);
  SuccessOrDie(GetRadioSpinel().SetPromiscuous(aEnable));
}

bool otPlatRadioIsEnabled(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().IsEnabled();
}

otError otPlatRadioEnable(otInstance *aInstance) { return GetRadioSpinel().Enable(aInstance); }

otError otPlatRadioDisable(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().Disable();
}

otError otPlatRadioSleep(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().Sleep();
}

otError otPlatRadioReceive(otInstance *aInstance, uint8_t aChannel) {
  OT_UNUSED_VARIABLE(aInstance);

  otError error;

  SuccessOrExit(error = GetRadioSpinel().Receive(aChannel));

exit:
  return error;
}

otError otPlatRadioTransmit(otInstance *aInstance, otRadioFrame *aFrame) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().Transmit(*aFrame);
}

otRadioFrame *otPlatRadioGetTransmitBuffer(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return &GetRadioSpinel().GetTransmitFrame();
}

int8_t otPlatRadioGetRssi(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().GetRssi();
}

otRadioCaps otPlatRadioGetCaps(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().GetRadioCaps();
}

const char *otPlatRadioGetVersionString(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().GetVersion();
}

bool otPlatRadioGetPromiscuous(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().IsPromiscuous();
}

#if 0
void platformRadioUpdateFdSet(otSysMainloopContext *aContext)
{
    uint64_t now      = otPlatTimeGet();
    uint64_t deadline = GetRadioSpinel().GetNextRadioTimeRecalcStart();

    if (GetRadioSpinel().IsTransmitting())
    {
        uint64_t txRadioEndUs = GetRadioSpinel().GetTxRadioEndUs();

        if (txRadioEndUs < deadline)
        {
            deadline = txRadioEndUs;
        }
    }

    if (now < deadline)
    {
        uint64_t remain = deadline - now;

        if (remain < (static_cast<uint64_t>(aContext->mTimeout.tv_sec) * US_PER_S +
                      static_cast<uint64_t>(aContext->mTimeout.tv_usec)))
        {
            aContext->mTimeout.tv_sec  = static_cast<time_t>(remain / US_PER_S);
            aContext->mTimeout.tv_usec = static_cast<suseconds_t>(remain % US_PER_S);
        }
    }
    else
    {
        aContext->mTimeout.tv_sec  = 0;
        aContext->mTimeout.tv_usec = 0;
    }

    sRadio.GetSpinelInterface().UpdateFdSet(aContext);

    if (GetRadioSpinel().HasPendingFrame() || GetRadioSpinel().IsTransmitDone())
    {
        aContext->mTimeout.tv_sec  = 0;
        aContext->mTimeout.tv_usec = 0;
    }
}
#endif

#if OPENTHREAD_POSIX_VIRTUAL_TIME
void virtualTimeRadioSpinelProcess(otInstance *aInstance, const struct VirtualTimeEvent *aEvent) {
  OT_UNUSED_VARIABLE(aInstance);
  GetRadioSpinel().Process(aEvent);
}
#else
extern "C" void platformRadioProcess(otInstance *aInstance, const otSysMainloopContext *aContext) {
  OT_UNUSED_VARIABLE(aInstance);

  GetRadioSpinel().Process(aContext);
}
#endif  // OPENTHREAD_POSIX_VIRTUAL_TIME

void otPlatRadioEnableSrcMatch(otInstance *aInstance, bool aEnable) {
  OT_UNUSED_VARIABLE(aInstance);
  SuccessOrDie(GetRadioSpinel().EnableSrcMatch(aEnable));
}

otError otPlatRadioAddSrcMatchShortEntry(otInstance *aInstance, uint16_t aShortAddress) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().AddSrcMatchShortEntry(aShortAddress);
}

otError otPlatRadioAddSrcMatchExtEntry(otInstance *aInstance, const otExtAddress *aExtAddress) {
  OT_UNUSED_VARIABLE(aInstance);
  otExtAddress addr;

  for (size_t i = 0; i < sizeof(addr); i++) {
    addr.m8[i] = aExtAddress->m8[sizeof(addr) - 1 - i];
  }

  return GetRadioSpinel().AddSrcMatchExtEntry(addr);
}

otError otPlatRadioClearSrcMatchShortEntry(otInstance *aInstance, uint16_t aShortAddress) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().ClearSrcMatchShortEntry(aShortAddress);
}

otError otPlatRadioClearSrcMatchExtEntry(otInstance *aInstance, const otExtAddress *aExtAddress) {
  OT_UNUSED_VARIABLE(aInstance);
  otExtAddress addr;

  for (size_t i = 0; i < sizeof(addr); i++) {
    addr.m8[i] = aExtAddress->m8[sizeof(addr) - 1 - i];
  }

  return GetRadioSpinel().ClearSrcMatchExtEntry(addr);
}

void otPlatRadioClearSrcMatchShortEntries(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  SuccessOrDie(GetRadioSpinel().ClearSrcMatchShortEntries());
}

void otPlatRadioClearSrcMatchExtEntries(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  SuccessOrDie(GetRadioSpinel().ClearSrcMatchExtEntries());
}

otError otPlatRadioEnergyScan(otInstance *aInstance, uint8_t aScanChannel, uint16_t aScanDuration) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().EnergyScan(aScanChannel, aScanDuration);
}

otError otPlatRadioGetTransmitPower(otInstance *aInstance, int8_t *aPower) {
  OT_UNUSED_VARIABLE(aInstance);
  assert(aPower != nullptr);
  return GetRadioSpinel().GetTransmitPower(*aPower);
}

otError otPlatRadioSetTransmitPower(otInstance *aInstance, int8_t aPower) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().SetTransmitPower(aPower);
}

otError otPlatRadioGetCcaEnergyDetectThreshold(otInstance *aInstance, int8_t *aThreshold) {
  OT_UNUSED_VARIABLE(aInstance);
  assert(aThreshold != nullptr);
  return GetRadioSpinel().GetCcaEnergyDetectThreshold(*aThreshold);
}

otError otPlatRadioSetCcaEnergyDetectThreshold(otInstance *aInstance, int8_t aThreshold) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().SetCcaEnergyDetectThreshold(aThreshold);
}

otError otPlatRadioGetFemLnaGain(otInstance *aInstance, int8_t *aGain) {
  OT_UNUSED_VARIABLE(aInstance);
  assert(aGain != nullptr);
  return GetRadioSpinel().GetFemLnaGain(*aGain);
}

otError otPlatRadioSetFemLnaGain(otInstance *aInstance, int8_t aGain) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().SetFemLnaGain(aGain);
}

int8_t otPlatRadioGetReceiveSensitivity(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().GetReceiveSensitivity();
}

#if OPENTHREAD_CONFIG_PLATFORM_RADIO_COEX_ENABLE
otError otPlatRadioSetCoexEnabled(otInstance *aInstance, bool aEnabled) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().SetCoexEnabled(aEnabled);
}

bool otPlatRadioIsCoexEnabled(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().IsCoexEnabled();
}

otError otPlatRadioGetCoexMetrics(otInstance *aInstance, otRadioCoexMetrics *aCoexMetrics) {
  OT_UNUSED_VARIABLE(aInstance);

  otError error = OT_ERROR_NONE;

  VerifyOrExit(aCoexMetrics != nullptr, error = OT_ERROR_INVALID_ARGS);

  error = GetRadioSpinel().GetCoexMetrics(*aCoexMetrics);

exit:
  return error;
}
#endif

#if OPENTHREAD_CONFIG_DIAG_ENABLE
otError otPlatDiagProcess(otInstance *aInstance, uint8_t aArgsLength, char *aArgs[], char *aOutput,
                          size_t aOutputMaxLen) {
  // deliver the platform specific diags commands to radio only ncp.
  OT_UNUSED_VARIABLE(aInstance);
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE] = {'\0'};
  char *cur = cmd;
  char *end = cmd + sizeof(cmd);

  for (uint8_t index = 0; (index < aArgsLength) && (cur < end); index++) {
    cur += snprintf(cur, static_cast<size_t>(end - cur), "%s ", aArgs[index]);
  }

  return GetRadioSpinel().PlatDiagProcess(cmd, aOutput, aOutputMaxLen);
}

void otPlatDiagModeSet(bool aMode) {
  SuccessOrExit(GetRadioSpinel().PlatDiagProcess(aMode ? "start" : "stop", nullptr, 0));
  GetRadioSpinel().SetDiagEnabled(aMode);

exit:
  return;
}

bool otPlatDiagModeGet(void) { return GetRadioSpinel().IsDiagEnabled(); }

void otPlatDiagTxPowerSet(int8_t aTxPower) {
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "power %d", aTxPower);
  SuccessOrExit(GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0));

exit:
  return;
}

void otPlatDiagChannelSet(uint8_t aChannel) {
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "channel %d", aChannel);
  SuccessOrExit(GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0));

exit:
  return;
}

otError otPlatDiagGpioSet(uint32_t aGpio, bool aValue) {
  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "gpio set %d %d", aGpio, aValue);
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0));

exit:
  return error;
}

otError otPlatDiagGpioGet(uint32_t aGpio, bool *aValue) {
  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];
  char output[OPENTHREAD_CONFIG_DIAG_OUTPUT_BUFFER_SIZE];
  char *str;

  snprintf(cmd, sizeof(cmd), "gpio get %d", aGpio);
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, output, sizeof(output)));
  VerifyOrExit((str = strtok(output, "\r")) != nullptr, error = OT_ERROR_FAILED);
  *aValue = static_cast<bool>(atoi(str));

exit:
  return error;
}

otError otPlatDiagGpioSetMode(uint32_t aGpio, otGpioMode aMode) {
  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "gpio mode %d %s", aGpio, aMode == OT_GPIO_MODE_INPUT ? "in" : "out");
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0));

exit:
  return error;
}

otError otPlatDiagGpioGetMode(uint32_t aGpio, otGpioMode *aMode) {
  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];
  char output[OPENTHREAD_CONFIG_DIAG_OUTPUT_BUFFER_SIZE];
  char *str;

  snprintf(cmd, sizeof(cmd), "gpio mode %d", aGpio);
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, output, sizeof(output)));
  VerifyOrExit((str = strtok(output, "\r")) != nullptr, error = OT_ERROR_FAILED);

  if (strcmp(str, "in") == 0) {
    *aMode = OT_GPIO_MODE_INPUT;
  } else if (strcmp(str, "out") == 0) {
    *aMode = OT_GPIO_MODE_OUTPUT;
  } else {
    error = OT_ERROR_FAILED;
  }

exit:
  return error;
}

otError otPlatDiagRadioGetPowerSettings(otInstance *aInstance, uint8_t aChannel,
                                        int16_t *aTargetPower, int16_t *aActualPower,
                                        uint8_t *aRawPowerSetting,
                                        uint16_t *aRawPowerSettingLength) {
  OT_UNUSED_VARIABLE(aInstance);
  static constexpr uint16_t kRawPowerStringSize =
      OPENTHREAD_CONFIG_POWER_CALIBRATION_RAW_POWER_SETTING_SIZE * 2 + 1;
  static constexpr uint16_t kFmtStringSize = 100;

  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];
  char output[OPENTHREAD_CONFIG_DIAG_OUTPUT_BUFFER_SIZE];
  int targetPower;
  int actualPower;
  char rawPowerSetting[kRawPowerStringSize];
  char fmt[kFmtStringSize];

  assert((aTargetPower != nullptr) && (aActualPower != nullptr) && (aRawPowerSetting != nullptr) &&
         (aRawPowerSettingLength != nullptr));

  snprintf(cmd, sizeof(cmd), "powersettings %d", aChannel);
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, output, sizeof(output)));
  snprintf(fmt, sizeof(fmt),
           "TargetPower(0.01dBm): %%d\r\nActualPower(0.01dBm): %%d\r\nRawPowerSetting: %%%us\r\n",
           kRawPowerStringSize);
  VerifyOrExit(sscanf(output, fmt, &targetPower, &actualPower, rawPowerSetting) == 3,
               error = OT_ERROR_FAILED);
  SuccessOrExit(error = ot::Utils::CmdLineParser::ParseAsHexString(
                    rawPowerSetting, *aRawPowerSettingLength, aRawPowerSetting));
  *aTargetPower = static_cast<int16_t>(targetPower);
  *aActualPower = static_cast<int16_t>(actualPower);

exit:
  return error;
}

otError otPlatDiagRadioSetRawPowerSetting(otInstance *aInstance, const uint8_t *aRawPowerSetting,
                                          uint16_t aRawPowerSettingLength) {
  OT_UNUSED_VARIABLE(aInstance);

  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];
  int nbytes;

  assert(aRawPowerSetting != nullptr);

  nbytes = snprintf(cmd, sizeof(cmd), "rawpowersetting ");

  for (uint16_t i = 0; i < aRawPowerSettingLength; i++) {
    nbytes += snprintf(cmd + nbytes, sizeof(cmd) - static_cast<size_t>(nbytes), "%02x",
                       aRawPowerSetting[i]);
    VerifyOrExit(nbytes < static_cast<int>(sizeof(cmd)), error = OT_ERROR_INVALID_ARGS);
  }

  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0));

exit:
  return error;
}

otError otPlatDiagRadioGetRawPowerSetting(otInstance *aInstance, uint8_t *aRawPowerSetting,
                                          uint16_t *aRawPowerSettingLength) {
  OT_UNUSED_VARIABLE(aInstance);
  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];
  char output[OPENTHREAD_CONFIG_DIAG_OUTPUT_BUFFER_SIZE];
  char *str;

  assert((aRawPowerSetting != nullptr) && (aRawPowerSettingLength != nullptr));

  snprintf(cmd, sizeof(cmd), "rawpowersetting");
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, output, sizeof(output)));
  VerifyOrExit((str = strtok(output, "\r")) != nullptr, error = OT_ERROR_FAILED);
  SuccessOrExit(error = ot::Utils::CmdLineParser::ParseAsHexString(str, *aRawPowerSettingLength,
                                                                   aRawPowerSetting));

exit:
  return error;
}

otError otPlatDiagRadioRawPowerSettingEnable(otInstance *aInstance, bool aEnable) {
  OT_UNUSED_VARIABLE(aInstance);

  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "rawpowersetting %s", aEnable ? "enable" : "disable");
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0));

exit:
  return error;
}

otError otPlatDiagRadioTransmitCarrier(otInstance *aInstance, bool aEnable) {
  OT_UNUSED_VARIABLE(aInstance);

  otError error;
  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "cw %s", aEnable ? "start" : "stop");
  SuccessOrExit(error = GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0));

exit:
  return error;
}

otError otPlatDiagRadioTransmitStream(otInstance *aInstance, bool aEnable) {
  OT_UNUSED_VARIABLE(aInstance);

  char cmd[OPENTHREAD_CONFIG_DIAG_CMD_LINE_BUFFER_SIZE];

  snprintf(cmd, sizeof(cmd), "stream %s", aEnable ? "start" : "stop");
  return GetRadioSpinel().PlatDiagProcess(cmd, nullptr, 0);
}

void otPlatDiagRadioReceived(otInstance *aInstance, otRadioFrame *aFrame, otError aError) {
  OT_UNUSED_VARIABLE(aInstance);
  OT_UNUSED_VARIABLE(aFrame);
  OT_UNUSED_VARIABLE(aError);
}

void otPlatDiagAlarmCallback(otInstance *aInstance) { OT_UNUSED_VARIABLE(aInstance); }
#endif  // OPENTHREAD_CONFIG_DIAG_ENABLE

uint32_t otPlatRadioGetSupportedChannelMask(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);

  uint32_t channelMask;

#if OPENTHREAD_POSIX_CONFIG_CONFIGURATION_FILE_ENABLE
  if (sConfig.IsValid()) {
    channelMask = sConfig.GetSupportedChannelMask();
  } else
#endif
  {
    channelMask = GetRadioSpinel().GetRadioChannelMask(false);
  }

  return channelMask;
}

uint32_t otPlatRadioGetPreferredChannelMask(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);

  uint32_t channelMask;

#if OPENTHREAD_POSIX_CONFIG_CONFIGURATION_FILE_ENABLE
  if (sConfig.IsValid()) {
    channelMask = sConfig.GetPreferredChannelMask();
  } else
#endif
  {
    channelMask = GetRadioSpinel().GetRadioChannelMask(true);
  }

  return channelMask;
}

otRadioState otPlatRadioGetState(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().GetState();
}

void otPlatRadioSetMacKey(otInstance *aInstance, uint8_t aKeyIdMode, uint8_t aKeyId,
                          const otMacKeyMaterial *aPrevKey, const otMacKeyMaterial *aCurrKey,
                          const otMacKeyMaterial *aNextKey, otRadioKeyType aKeyType) {
  SuccessOrDie(GetRadioSpinel().SetMacKey(aKeyIdMode, aKeyId, aPrevKey, aCurrKey, aNextKey));
  OT_UNUSED_VARIABLE(aInstance);
  OT_UNUSED_VARIABLE(aKeyType);
}

void otPlatRadioSetMacFrameCounter(otInstance *aInstance, uint32_t aMacFrameCounter) {
  SuccessOrDie(GetRadioSpinel().SetMacFrameCounter(aMacFrameCounter, /* aSetIfLarger */ false));
  OT_UNUSED_VARIABLE(aInstance);
}

void otPlatRadioSetMacFrameCounterIfLarger(otInstance *aInstance, uint32_t aMacFrameCounter) {
  SuccessOrDie(GetRadioSpinel().SetMacFrameCounter(aMacFrameCounter, /* aSetIfLarger */ true));
  OT_UNUSED_VARIABLE(aInstance);
}

uint64_t otPlatRadioGetNow(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().GetNow();
}

uint32_t otPlatRadioGetBusSpeed(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().GetBusSpeed();
}

#if OPENTHREAD_CONFIG_MAC_CSL_RECEIVER_ENABLE || OPENTHREAD_CONFIG_MAC_CSL_TRANSMITTER_ENABLE
uint8_t otPlatRadioGetCslAccuracy(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);

  return GetRadioSpinel().GetCslAccuracy();
}
#endif

#if OPENTHREAD_CONFIG_MAC_CSL_TRANSMITTER_ENABLE
uint8_t otPlatRadioGetCslUncertainty(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);

  return GetRadioSpinel().GetCslUncertainty();
}
#endif

otError otPlatRadioSetChannelMaxTransmitPower(otInstance *aInstance, uint8_t aChannel,
                                              int8_t aMaxPower) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().SetChannelMaxTransmitPower(aChannel, aMaxPower);
}

#if OPENTHREAD_CONFIG_PLATFORM_POWER_CALIBRATION_ENABLE
otError otPlatRadioAddCalibratedPower(otInstance *aInstance, uint8_t aChannel, int16_t aActualPower,
                                      const uint8_t *aRawPowerSetting,
                                      uint16_t aRawPowerSettingLength) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().AddCalibratedPower(aChannel, aActualPower, aRawPowerSetting,
                                             aRawPowerSettingLength);
}

otError otPlatRadioClearCalibratedPowers(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().ClearCalibratedPowers();
}

otError otPlatRadioSetChannelTargetPower(otInstance *aInstance, uint8_t aChannel,
                                         int16_t aTargetPower) {
  OT_UNUSED_VARIABLE(aInstance);
  return GetRadioSpinel().SetChannelTargetPower(aChannel, aTargetPower);
}
#endif

otError otPlatRadioSetRegion(otInstance *aInstance, uint16_t aRegionCode) {
  OT_UNUSED_VARIABLE(aInstance);

  otError error;

#if OPENTHREAD_POSIX_CONFIG_CONFIGURATION_FILE_ENABLE
  if (sConfig.IsValid()) {
    error = sConfig.SetRegion(aRegionCode);
  } else
#endif
  {
    error = GetRadioSpinel().SetRadioRegion(aRegionCode);
  }

  return error;
}

otError otPlatRadioGetRegion(otInstance *aInstance, uint16_t *aRegionCode) {
  OT_UNUSED_VARIABLE(aInstance);

  otError error;

#if OPENTHREAD_POSIX_CONFIG_CONFIGURATION_FILE_ENABLE
  if (sConfig.IsValid()) {
    *aRegionCode = sConfig.GetRegion();
    error = OT_ERROR_NONE;
  } else
#endif
  {
    error = GetRadioSpinel().GetRadioRegion(aRegionCode);
  }

  return error;
}

#if OPENTHREAD_CONFIG_MLE_LINK_METRICS_SUBJECT_ENABLE
otError otPlatRadioConfigureEnhAckProbing(otInstance *aInstance, otLinkMetrics aLinkMetrics,
                                          const otShortAddress aShortAddress,
                                          const otExtAddress *aExtAddress) {
  OT_UNUSED_VARIABLE(aInstance);

  return GetRadioSpinel().ConfigureEnhAckProbing(aLinkMetrics, aShortAddress, *aExtAddress);
}
#endif

otError otPlatRadioReceiveAt(otInstance *aInstance, uint8_t aChannel, uint32_t aStart,
                             uint32_t aDuration) {
  OT_UNUSED_VARIABLE(aInstance);
  OT_UNUSED_VARIABLE(aChannel);
  OT_UNUSED_VARIABLE(aStart);
  OT_UNUSED_VARIABLE(aDuration);
  return OT_ERROR_NOT_IMPLEMENTED;
}

#if OPENTHREAD_CONFIG_PLATFORM_BOOTLOADER_MODE_ENABLE
otError otPlatResetToBootloader(otInstance *aInstance) {
  OT_UNUSED_VARIABLE(aInstance);

  return GetRadioSpinel().SendReset(SPINEL_RESET_BOOTLOADER);
}
#endif

const otRadioSpinelMetrics *otSysGetRadioSpinelMetrics(void) {
  return GetRadioSpinel().GetRadioSpinelMetrics();
}

const otRcpInterfaceMetrics *otSysGetRcpInterfaceMetrics(void) {
  return sRadio.GetSpinelInterface().GetRcpInterfaceMetrics();
}

#endif  // OPENTHREAD_240214
