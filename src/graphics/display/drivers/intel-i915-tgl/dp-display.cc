// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915-tgl/dp-display.h"

#include <endian.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/status.h>
#include <lib/zx/time.h>
#include <math.h>
#include <string.h>
#include <zircon/assert.h>

#include <algorithm>
#include <iterator>
#include <variant>

#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/intel-i915-tgl/ddi-aux-channel.h"
#include "src/graphics/display/drivers/intel-i915-tgl/dpll.h"
#include "src/graphics/display/drivers/intel-i915-tgl/intel-i915-tgl.h"
#include "src/graphics/display/drivers/intel-i915-tgl/pci-ids.h"
#include "src/graphics/display/drivers/intel-i915-tgl/pipe.h"
#include "src/graphics/display/drivers/intel-i915-tgl/poll-until.h"
#include "src/graphics/display/drivers/intel-i915-tgl/registers-ddi.h"
#include "src/graphics/display/drivers/intel-i915-tgl/registers-dpll.h"
#include "src/graphics/display/drivers/intel-i915-tgl/registers-pipe.h"
#include "src/graphics/display/drivers/intel-i915-tgl/registers-transcoder.h"
#include "src/graphics/display/drivers/intel-i915-tgl/registers.h"
#include "src/graphics/display/drivers/intel-i915/pch-engine.h"

namespace i915_tgl {
namespace {

constexpr uint32_t kBitsPerPixel = 24;  // kPixelFormat

// Recommended DDI buffer translation programming values

struct DdiPhyConfigEntry {
  uint32_t entry2;
  uint32_t entry1;
};

// The tables below have the values recommended by the documentation.
//
// Kaby Lake: IHD-OS-KBL-Vol 12-1.17 pages 187-190
// Skylake: IHD-OS-SKL-Vol 12-05.16 pages 181-183
//
// TODO(fxbug.dev/108252): Per-entry Iboost values.

constexpr DdiPhyConfigEntry kPhyConfigDpSkylakeHs[9] = {
    {0x000000a0, 0x00002016}, {0x0000009b, 0x00005012}, {0x00000088, 0x00007011},
    {0x000000c0, 0x80009010}, {0x0000009b, 0x00002016}, {0x00000088, 0x00005012},
    {0x000000c0, 0x80007011}, {0x000000df, 0x00002016}, {0x000000c0, 0x80005012},
};

constexpr DdiPhyConfigEntry kPhyConfigDpSkylakeY[9] = {
    {0x000000a2, 0x00000018}, {0x00000088, 0x00005012}, {0x000000cd, 0x80007011},
    {0x000000c0, 0x80009010}, {0x0000009d, 0x00000018}, {0x000000c0, 0x80005012},
    {0x000000c0, 0x80007011}, {0x00000088, 0x00000018}, {0x000000c0, 0x80005012},
};

constexpr DdiPhyConfigEntry kPhyConfigDpSkylakeU[9] = {
    {0x000000a2, 0x0000201b}, {0x00000088, 0x00005012}, {0x000000cd, 0x80007011},
    {0x000000c0, 0x80009010}, {0x0000009d, 0x0000201b}, {0x000000c0, 0x80005012},
    {0x000000c0, 0x80007011}, {0x00000088, 0x00002016}, {0x000000c0, 0x80005012},
};

constexpr DdiPhyConfigEntry kPhyConfigDpKabyLakeHs[9] = {
    {0x000000a0, 0x00002016}, {0x0000009b, 0x00005012}, {0x00000088, 0x00007011},
    {0x000000c0, 0x80009010}, {0x0000009b, 0x00002016}, {0x00000088, 0x00005012},
    {0x000000c0, 0x80007011}, {0x00000097, 0x00002016}, {0x000000c0, 0x80005012},
};

constexpr DdiPhyConfigEntry kPhyConfigDpKabyLakeY[9] = {
    {0x000000a1, 0x00001017}, {0x00000088, 0x00005012}, {0x000000cd, 0x80007011},
    {0x000000c0, 0x8000800f}, {0x0000009d, 0x00001017}, {0x000000c0, 0x80005012},
    {0x000000c0, 0x80007011}, {0x0000004c, 0x00001017}, {0x000000c0, 0x80005012},
};

constexpr DdiPhyConfigEntry kPhyConfigDpKabyLakeU[9] = {
    {0x000000a1, 0x0000201b}, {0x00000088, 0x00005012}, {0x000000cd, 0x80007011},
    {0x000000c0, 0x80009010}, {0x0000009d, 0x0000201b}, {0x000000c0, 0x80005012},
    {0x000000c0, 0x80007011}, {0x0000004f, 0x00002016}, {0x000000c0, 0x80005012},
};

constexpr DdiPhyConfigEntry kPhyConfigEdpKabyLakeHs[10] = {
    {0x000000a8, 0x00000018}, {0x000000a9, 0x00004013}, {0x000000a2, 0x00007011},
    {0x0000009c, 0x00009010}, {0x000000a9, 0x00000018}, {0x000000a2, 0x00006013},
    {0x000000a6, 0x00007011}, {0x000000ab, 0x00000018}, {0x0000009f, 0x00007013},
    {0x000000df, 0x00000018},
};

constexpr DdiPhyConfigEntry kPhyConfigEdpKabyLakeY[10] = {
    {0x000000a8, 0x00000018}, {0x000000ab, 0x00004013}, {0x000000a4, 0x00007011},
    {0x000000df, 0x00009010}, {0x000000aa, 0x00000018}, {0x000000a4, 0x00006013},
    {0x0000009d, 0x00007011}, {0x000000a0, 0x00000018}, {0x000000df, 0x00006012},
    {0x0000008a, 0x00000018},
};

constexpr DdiPhyConfigEntry kPhyConfigEdpKabyLakeU[10] = {
    {0x000000a8, 0x00000018}, {0x000000a9, 0x00004013}, {0x000000a2, 0x00007011},
    {0x0000009c, 0x00009010}, {0x000000a9, 0x00000018}, {0x000000a2, 0x00006013},
    {0x000000a6, 0x00007011}, {0x000000ab, 0x00002016}, {0x0000009f, 0x00005013},
    {0x000000df, 0x00000018},
};

cpp20::span<const DdiPhyConfigEntry> GetDpPhyConfigEntries(uint16_t device_id, uint8_t* i_boost) {
  if (is_skl(device_id)) {
    if (is_skl_u(device_id)) {
      *i_boost = 0x1;
      return kPhyConfigDpSkylakeU;
    }
    if (is_skl_y(device_id)) {
      *i_boost = 0x3;
      return kPhyConfigDpSkylakeY;
    }
    *i_boost = 0x1;
    return kPhyConfigDpSkylakeHs;
  }
  if (is_kbl(device_id)) {
    if (is_kbl_u(device_id)) {
      *i_boost = 0x1;
      return kPhyConfigDpKabyLakeU;
    }
    if (is_kbl_y(device_id)) {
      *i_boost = 0x3;
      return kPhyConfigDpKabyLakeY;
    }
    *i_boost = 0x3;
    return kPhyConfigDpKabyLakeHs;
  }

  zxlogf(ERROR, "Unsupported i915 device id: %x", device_id);
  *i_boost = 0;
  return {};
}

cpp20::span<const DdiPhyConfigEntry> GetEdpPhyConfigEntries(uint16_t device_id, uint8_t* i_boost) {
  *i_boost = 0x0;
  if (is_skl_u(device_id) || is_kbl_u(device_id)) {
    return kPhyConfigEdpKabyLakeU;
  }
  if (is_skl_y(device_id) || is_kbl_y(device_id)) {
    return kPhyConfigEdpKabyLakeY;
  }
  return kPhyConfigEdpKabyLakeHs;
}

// Aux port functions

// 4-bit request type in Aux channel request messages.
enum {
  DP_REQUEST_I2C_WRITE = 0,
  DP_REQUEST_I2C_READ = 1,
  DP_REQUEST_NATIVE_WRITE = 8,
  DP_REQUEST_NATIVE_READ = 9,
};

// 4-bit statuses in Aux channel reply messages.
enum {
  DP_REPLY_AUX_ACK = 0,
  DP_REPLY_AUX_NACK = 1,
  DP_REPLY_AUX_DEFER = 2,
  DP_REPLY_I2C_NACK = 4,
  DP_REPLY_I2C_DEFER = 8,
};

std::string DpcdRevisionToString(dpcd::Revision rev) {
  switch (rev) {
    case dpcd::Revision::k1_0:
      return "DPCD r1.0";
    case dpcd::Revision::k1_1:
      return "DPCD r1.1";
    case dpcd::Revision::k1_2:
      return "DPCD r1.2";
    case dpcd::Revision::k1_3:
      return "DPCD r1.3";
    case dpcd::Revision::k1_4:
      return "DPCD r1.4";
  }
  return "unknown";
}

std::string EdpDpcdRevisionToString(dpcd::EdpRevision rev) {
  switch (rev) {
    case dpcd::EdpRevision::k1_1:
      return "eDP v1.1 or lower";
    case dpcd::EdpRevision::k1_2:
      return "eDP v1.2";
    case dpcd::EdpRevision::k1_3:
      return "eDP v1.3";
    case dpcd::EdpRevision::k1_4:
      return "eDP v1.4";
    case dpcd::EdpRevision::k1_4a:
      return "eDP v1.4a";
    case dpcd::EdpRevision::k1_4b:
      return "eDP v1.4b";
  }
  return "unknown";
}

}  // namespace

zx::status<DdiAuxChannel::ReplyInfo> DpAux::DoTransaction(const DdiAuxChannel::Request& request,
                                                          cpp20::span<uint8_t> reply_data_buffer) {
  // If the DisplayPort sink device isn't ready to handle an Aux message,
  // it can return an AUX_DEFER reply, which means we should retry the
  // request. The spec added a requirement for >=7 defer retries in v1.3,
  // but there are no requirements before that nor is there a max value. 16
  // retries is pretty arbitrary and might need to be increased for slower
  // displays.
  const int kMaxDefers = 16;

  // Per table 2-43 in v1.1a, we need to retry >3 times, since some
  // DisplayPort sink devices time out on the first DP aux request
  // but succeed on later requests.
  const int kMaxTimeouts = 5;

  unsigned defers_seen = 0;
  unsigned timeouts_seen = 0;

  for (;;) {
    zx::status<DdiAuxChannel::ReplyInfo> transaction_result =
        aux_channel_.DoTransaction(request, reply_data_buffer);
    if (transaction_result.is_error()) {
      if (transaction_result.error_value() == ZX_ERR_IO_MISSED_DEADLINE) {
        if (++timeouts_seen == kMaxTimeouts) {
          zxlogf(DEBUG, "DP aux: Got too many timeouts (%d)", kMaxTimeouts);
          return transaction_result;
        }
        // Retry on timeout.
        continue;
      }

      // We do not retry if sending the raw message failed for
      // an unexpected reason.
      return transaction_result;
    }

    uint8_t header_byte = transaction_result->reply_header;
    uint8_t padding = header_byte & 0xf;
    uint8_t status = static_cast<uint8_t>(header_byte >> 4);
    // Sanity check: The padding should be zero.  If it's not, we
    // shouldn't return an error, in case this space gets used for some
    // later extension to the protocol.  But report it, in case this
    // indicates some problem.
    if (padding) {
      zxlogf(INFO, "DP aux: Reply header padding is non-zero (header byte: 0x%x)", header_byte);
    }

    switch (status) {
      case DP_REPLY_AUX_ACK:
        // The AUX_ACK implies that we got an I2C ACK too.
        return transaction_result;
      case DP_REPLY_AUX_NACK:
        zxlogf(TRACE, "DP aux: Reply was not an ack (got AUX_NACK)");
        return zx::error_status(ZX_ERR_IO_REFUSED);
      case DP_REPLY_AUX_DEFER:
        if (++defers_seen == kMaxDefers) {
          zxlogf(TRACE, "DP aux: Received too many AUX DEFERs (%d)", kMaxDefers);
          return zx::error_status(ZX_ERR_IO_MISSED_DEADLINE);
        }
        // Go around the loop again to retry.
        continue;
      case DP_REPLY_I2C_NACK:
        zxlogf(TRACE, "DP aux: Reply was not an ack (got I2C_NACK)");
        return zx::error_status(ZX_ERR_IO_REFUSED);
      case DP_REPLY_I2C_DEFER:
        // TODO(fxbug.dev/31313): Implement handling of I2C_DEFER.
        zxlogf(TRACE, "DP aux: Received I2C_DEFER (not implemented)");
        return zx::error_status(ZX_ERR_NEXT);
      default:
        zxlogf(TRACE, "DP aux: Unrecognized reply (header byte: 0x%x)", header_byte);
        return zx::error_status(ZX_ERR_IO_DATA_INTEGRITY);
    }
  }
}

zx_status_t DpAux::DpAuxRead(uint32_t dp_cmd, uint32_t addr, uint8_t* buf, size_t size) {
  while (size > 0) {
    uint32_t chunk_size = static_cast<uint32_t>(std::min<size_t>(size, DdiAuxChannel::kMaxOpSize));
    size_t bytes_read = 0;
    zx_status_t status = DpAuxReadChunk(dp_cmd, addr, buf, chunk_size, &bytes_read);
    if (status != ZX_OK) {
      return status;
    }
    if (bytes_read == 0) {
      // We failed to make progress on the last call.  To avoid the
      // risk of getting an infinite loop from that happening
      // continually, we return.
      return ZX_ERR_IO;
    }
    buf += bytes_read;
    size -= bytes_read;
  }
  return ZX_OK;
}

zx_status_t DpAux::DpAuxReadChunk(uint32_t dp_cmd, uint32_t addr, uint8_t* buf, uint32_t size_in,
                                  size_t* size_out) {
  const DdiAuxChannel::Request request = {
      .address = static_cast<int32_t>(addr),
      .command = static_cast<int8_t>(dp_cmd),
      .op_size = static_cast<int8_t>(size_in),
      .data = cpp20::span<uint8_t>(),
  };

  zx::status<DdiAuxChannel::ReplyInfo> result =
      DoTransaction(request, cpp20::span<uint8_t>(buf, size_in));
  if (result.is_error()) {
    return result.error_value();
  }

  // The cast is not UB because `reply_data_size` is guaranteed to be between
  // 1 and 16.
  const size_t bytes_read = static_cast<size_t>(result.value().reply_data_size);
  if (static_cast<size_t>(bytes_read) > size_in) {
    zxlogf(WARNING, "DP aux read: Reply was larger than requested");
    return ZX_ERR_IO;
  }
  *size_out = bytes_read;
  return ZX_OK;
}

zx_status_t DpAux::DpAuxWrite(uint32_t dp_cmd, uint32_t addr, const uint8_t* buf, size_t size) {
  // Implement this if it's ever needed
  ZX_ASSERT_MSG(size <= 16, "message too large");

  const DdiAuxChannel::Request request = {
      .address = static_cast<int32_t>(addr),
      .command = static_cast<int8_t>(dp_cmd),
      .op_size = static_cast<int8_t>(size),
      .data = cpp20::span<const uint8_t>(buf, size),
  };

  // In case of a short write, receives the amount of written bytes.
  uint8_t reply_data[1];

  zx::status<DdiAuxChannel::ReplyInfo> transaction_result = DoTransaction(request, reply_data);
  if (transaction_result.is_error()) {
    return transaction_result.error_value();
  }
  // TODO(fxbug.dev/31313): Handle the case where the hardware did a short write,
  // for which we could send the remaining bytes.
  if (transaction_result->reply_data_size != 0) {
    zxlogf(WARNING, "DP aux write: Unexpected reply size");
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

zx_status_t DpAux::I2cTransact(const i2c_impl_op_t* ops, size_t count) {
  fbl::AutoLock lock(&lock_);
  for (unsigned i = 0; i < count; i++) {
    uint8_t* buf = static_cast<uint8_t*>(ops[i].data_buffer);
    uint8_t len = static_cast<uint8_t>(ops[i].data_size);
    zx_status_t status = ops[i].is_read
                             ? DpAuxRead(DP_REQUEST_I2C_READ, ops[i].address, buf, len)
                             : DpAuxWrite(DP_REQUEST_I2C_WRITE, ops[i].address, buf, len);
    if (status != ZX_OK) {
      return status;
    }
  }
  return ZX_OK;
}

bool DpAux::DpcdRead(uint32_t addr, uint8_t* buf, size_t size) {
  fbl::AutoLock lock(&lock_);
  constexpr uint32_t kReadAttempts = 3;
  for (unsigned i = 0; i < kReadAttempts; i++) {
    if (DpAuxRead(DP_REQUEST_NATIVE_READ, addr, buf, size) == ZX_OK) {
      return true;
    }
    zx_nanosleep(zx_deadline_after(ZX_MSEC(5)));
  }
  return false;
}

bool DpAux::DpcdWrite(uint32_t addr, const uint8_t* buf, size_t size) {
  fbl::AutoLock lock(&lock_);
  return DpAuxWrite(DP_REQUEST_NATIVE_WRITE, addr, buf, size) == ZX_OK;
}

DpAux::DpAux(fdf::MmioBuffer* mmio_buffer, tgl_registers::Ddi ddi, uint16_t device_id)
    : aux_channel_(mmio_buffer, ddi, device_id) {
  ZX_ASSERT(mtx_init(&lock_, mtx_plain) == thrd_success);
}

DpCapabilities::DpCapabilities() { dpcd_.fill(0); }

DpCapabilities::Edp::Edp() { bytes.fill(0); }

DpCapabilities::DpCapabilities(inspect::Node* parent_node) {
  dpcd_.fill(0);
  node_ = parent_node->CreateChild("dpcd-capabilities");
}

// static
fpromise::result<DpCapabilities> DpCapabilities::Read(DpcdChannel* dp_aux,
                                                      inspect::Node* parent_node) {
  DpCapabilities caps(parent_node);

  if (!dp_aux->DpcdRead(dpcd::DPCD_CAP_START, caps.dpcd_.data(), caps.dpcd_.size())) {
    zxlogf(TRACE, "Failed to read dpcd capabilities");
    return fpromise::error();
  }

  auto dsp_present =
      caps.dpcd_reg<dpcd::DownStreamPortPresent, dpcd::DPCD_DOWN_STREAM_PORT_PRESENT>();
  if (dsp_present.is_branch()) {
    auto dsp_count = caps.dpcd_reg<dpcd::DownStreamPortCount, dpcd::DPCD_DOWN_STREAM_PORT_COUNT>();
    zxlogf(DEBUG, "Found branch with %d ports", dsp_count.count());
  }

  if (!dp_aux->DpcdRead(dpcd::DPCD_SINK_COUNT, caps.sink_count_.reg_value_ptr(), 1)) {
    zxlogf(ERROR, "Failed to read DisplayPort sink count");
    return fpromise::error();
  }

  caps.max_lane_count_ = caps.dpcd_reg<dpcd::LaneCount, dpcd::DPCD_MAX_LANE_COUNT>();
  if (caps.max_lane_count() != 1 && caps.max_lane_count() != 2 && caps.max_lane_count() != 4) {
    zxlogf(ERROR, "Unsupported DisplayPort lane count: %u", caps.max_lane_count());
    return fpromise::error();
  }

  if (!caps.ProcessEdp(dp_aux)) {
    return fpromise::error();
  }

  if (!caps.ProcessSupportedLinkRates(dp_aux)) {
    return fpromise::error();
  }

  ZX_ASSERT(!caps.supported_link_rates_mbps_.empty());
  caps.PublishInspect();

  return fpromise::ok(std::move(caps));
}

bool DpCapabilities::ProcessEdp(DpcdChannel* dp_aux) {
  // Check if the Display Control registers reserved for eDP are available.
  auto edp_config = dpcd_reg<dpcd::EdpConfigCap, dpcd::DPCD_EDP_CONFIG>();
  if (!edp_config.dpcd_display_ctrl_capable()) {
    return true;
  }

  zxlogf(TRACE, "eDP registers are available");

  edp_dpcd_.emplace();
  if (!dp_aux->DpcdRead(dpcd::DPCD_EDP_CAP_START, edp_dpcd_->bytes.data(),
                        edp_dpcd_->bytes.size())) {
    zxlogf(ERROR, "Failed to read eDP capabilities");
    return false;
  }

  edp_dpcd_->revision = dpcd::EdpRevision(edp_dpcd_at(dpcd::DPCD_EDP_REV));

  auto general_cap1 = edp_dpcd_reg<dpcd::EdpGeneralCap1, dpcd::DPCD_EDP_GENERAL_CAP1>();
  auto backlight_cap = edp_dpcd_reg<dpcd::EdpBacklightCap, dpcd::DPCD_EDP_BACKLIGHT_CAP>();

  edp_dpcd_->backlight_aux_power =
      general_cap1.tcon_backlight_adjustment_cap() && general_cap1.backlight_aux_enable_cap();
  edp_dpcd_->backlight_aux_brightness =
      general_cap1.tcon_backlight_adjustment_cap() && backlight_cap.brightness_aux_set_cap();

  return true;
}

bool DpCapabilities::ProcessSupportedLinkRates(DpcdChannel* dp_aux) {
  ZX_ASSERT(supported_link_rates_mbps_.empty());

  // According to eDP v1.4b, Table 4-24, a device supporting eDP version v1.4 and higher can support
  // link rate selection by way of both the DPCD MAX_LINK_RATE register and the "Link Rate Table"
  // method via DPCD SUPPORTED_LINK_RATES registers.
  //
  // The latter method can represent more values than the former (which is limited to only 4
  // discrete values). Hence we attempt to use the "Link Rate Table" method first.
  use_link_rate_table_ = false;
  if (edp_dpcd_ && edp_dpcd_->revision >= dpcd::EdpRevision::k1_4) {
    constexpr size_t kBufferSize =
        dpcd::DPCD_SUPPORTED_LINK_RATE_END - dpcd::DPCD_SUPPORTED_LINK_RATE_START + 1;
    std::array<uint8_t, kBufferSize> link_rates;
    if (dp_aux->DpcdRead(dpcd::DPCD_SUPPORTED_LINK_RATE_START, link_rates.data(), kBufferSize)) {
      for (size_t i = 0; i < link_rates.size(); i += 2) {
        uint16_t value = link_rates[i] | (static_cast<uint16_t>(link_rates[i + 1]) << 8);

        // From the eDP specification: "A table entry containing the value 0 indicates that the
        // entry and all entries at higher DPCD addressess contain invalid link rates."
        if (value == 0) {
          break;
        }

        // Each valid entry indicates a nominal per-lane link rate equal to `value * 200kHz`. We
        // convert value to MHz: `value * 200 / 1000 ==> value / 5`.
        supported_link_rates_mbps_.push_back(value / 5);
      }
    }

    use_link_rate_table_ = !supported_link_rates_mbps_.empty();
  }

  // Fall back to the MAX_LINK_RATE register if the Link Rate Table method is not supported.
  if (supported_link_rates_mbps_.empty()) {
    uint32_t max_link_rate = dpcd_reg<dpcd::LinkBw, dpcd::DPCD_MAX_LINK_RATE>().link_bw();

    // All link rates including and below the maximum are supported.
    switch (max_link_rate) {
      case dpcd::LinkBw::k8100Mbps:
        supported_link_rates_mbps_.push_back(8100);
        __FALLTHROUGH;
      case dpcd::LinkBw::k5400Mbps:
        supported_link_rates_mbps_.push_back(5400);
        __FALLTHROUGH;
      case dpcd::LinkBw::k2700Mbps:
        supported_link_rates_mbps_.push_back(2700);
        __FALLTHROUGH;
      case dpcd::LinkBw::k1620Mbps:
        supported_link_rates_mbps_.push_back(1620);
        break;
      case 0:
        zxlogf(ERROR, "Device did not report supported link rates");
        return false;
      default:
        zxlogf(ERROR, "Unsupported max link rate: %u", max_link_rate);
        return false;
    }

    // Make sure the values are in ascending order.
    std::reverse(supported_link_rates_mbps_.begin(), supported_link_rates_mbps_.end());
  }

  return true;
}

void DpCapabilities::PublishInspect() {
  node_.RecordString("dpcd_revision", DpcdRevisionToString(dpcd_revision()));
  node_.RecordUint("sink_count", sink_count());
  node_.RecordUint("max_lane_count", max_lane_count());

  {
    auto node = node_.CreateUintArray("supported_link_rates_mbps_per_lane",
                                      supported_link_rates_mbps_.size());
    for (size_t i = 0; i < supported_link_rates_mbps_.size(); ++i) {
      node.Add(i, supported_link_rates_mbps_[i]);
    }
    node_.Record(std::move(node));
  }

  {
    std::string value =
        edp_dpcd_.has_value() ? EdpDpcdRevisionToString(edp_dpcd_->revision) : "not supported";
    node_.RecordString("edp_revision", std::move(value));
  }
}

bool DpDisplay::EnsureEdpPanelIsPoweredOn() {
  // Fix the panel configuration, if necessary.
  const PchPanelParameters panel_parameters = pch_engine_->PanelParameters();
  PchPanelParameters fixed_panel_parameters = panel_parameters;
  fixed_panel_parameters.Fix();
  if (panel_parameters != fixed_panel_parameters) {
    zxlogf(WARNING, "Incorrect PCH configuration for eDP panel. Re-configuring.");
  }
  pch_engine_->SetPanelParameters(fixed_panel_parameters);
  pch_engine_->SetPanelBrightness(backlight_brightness_);
  zxlogf(TRACE, "eDP panel configured.");

  // Power up the panel, if necessary.

  // The boot firmware might have left `force_power_on` set to true. To avoid
  // turning the panel off and on (and get the associated HPD interrupts), we
  // need to leave `force_power_on` as-is while we perform PCH-managed panel
  // power sequencing. Once the PCH keeps the panel on, we can set
  // `force_power_on` to false.
  PchPanelPowerTarget power_target = pch_engine_->PanelPowerTarget();
  power_target.power_on = true;
  pch_engine_->SetPanelPowerTarget(power_target);

  // The Atlas panel takes more time to power up than required in the eDP and
  // SPWG Notebook Panel standards.
  //
  // The generous timeout is chosen because we really don't want to give up too
  // early and leave the user with a non-working system, if there's any hope.
  // The waiting code polls the panel state every few ms, so we don't waste too
  // much time if the panel wakes up early / on time.
  static constexpr int kPowerUpTimeoutUs = 1'000'000;
  if (!pch_engine_->WaitForPanelPowerState(PchPanelPowerState::kPoweredUp, kPowerUpTimeoutUs)) {
    zxlogf(ERROR, "Failed to enable panel!");
    pch_engine_->Log();
    return false;
  }

  // The PCH panel power sequence has completed. Now it's safe to set
  // `force_power_on` to false, if it was true. The PCH will keep the panel
  // powered on.
  power_target.backlight_on = true;
  power_target.brightness_pwm_counter_on = true;
  power_target.force_power_on = false;
  pch_engine_->SetPanelPowerTarget(power_target);

  zxlogf(TRACE, "eDP panel powered on.");
  return true;
}

bool DpDisplay::DpcdWrite(uint32_t addr, const uint8_t* buf, size_t size) {
  return dp_aux_->DpcdWrite(addr, buf, size);
}

bool DpDisplay::DpcdRead(uint32_t addr, uint8_t* buf, size_t size) {
  return dp_aux_->DpcdRead(addr, buf, size);
}

// Link training functions

// Tell the sink device to start link training.
bool DpDisplay::DpcdRequestLinkTraining(const dpcd::TrainingPatternSet& tp_set,
                                        const dpcd::TrainingLaneSet lane[]) {
  // The DisplayPort spec says that we are supposed to write these
  // registers with a single operation: "The AUX CH burst write must be
  // used for writing to TRAINING_LANEx_SET bytes of the enabled lanes."
  // (From section 3.5.1.3, "Link Training", in v1.1a.)
  uint8_t reg_bytes[1 + dp_lane_count_];
  reg_bytes[0] = static_cast<uint8_t>(tp_set.reg_value());
  for (unsigned i = 0; i < dp_lane_count_; i++) {
    reg_bytes[i + 1] = static_cast<uint8_t>(lane[i].reg_value());
  }
  constexpr int kAddr = dpcd::DPCD_TRAINING_PATTERN_SET;
  static_assert(kAddr + 1 == dpcd::DPCD_TRAINING_LANE0_SET, "");
  static_assert(kAddr + 2 == dpcd::DPCD_TRAINING_LANE1_SET, "");
  static_assert(kAddr + 3 == dpcd::DPCD_TRAINING_LANE2_SET, "");
  static_assert(kAddr + 4 == dpcd::DPCD_TRAINING_LANE3_SET, "");

  if (!DpcdWrite(kAddr, reg_bytes, 1 + dp_lane_count_)) {
    zxlogf(ERROR, "Failure setting TRAINING_PATTERN_SET");
    return false;
  }

  return true;
}

template <uint32_t addr, typename T>
bool DpDisplay::DpcdReadPairedRegs(hwreg::RegisterBase<T, typename T::ValueType>* regs) {
  static_assert(addr == dpcd::DPCD_LANE0_1_STATUS || addr == dpcd::DPCD_ADJUST_REQUEST_LANE0_1,
                "Bad register address");
  uint32_t num_bytes = dp_lane_count_ == 4 ? 2 : 1;
  uint8_t reg_byte[num_bytes];
  if (!DpcdRead(addr, reg_byte, num_bytes)) {
    zxlogf(ERROR, "Failure reading addr %d", addr);
    return false;
  }

  for (unsigned i = 0; i < dp_lane_count_; i++) {
    regs[i].set_reg_value(reg_byte[i / 2]);
  }

  return true;
}

bool DpDisplay::DpcdHandleAdjustRequest(dpcd::TrainingLaneSet* training,
                                        dpcd::AdjustRequestLane* adjust) {
  bool voltage_changed = false;
  uint8_t voltage_swing = 0;
  uint8_t pre_emphasis = 0;
  for (int lane_index = 0; lane_index < dp_lane_count_; ++lane_index) {
    if (adjust[lane_index].voltage_swing(lane_index).get() > voltage_swing) {
      // The cast is lossless because voltage_swing() is a 2-bit field.
      voltage_swing = static_cast<uint8_t>(adjust[lane_index].voltage_swing(lane_index).get());
    }
    if (adjust[lane_index].pre_emphasis(lane_index).get() > pre_emphasis) {
      // The cast is lossless because pre-emphasis() is a 2-bit field.
      pre_emphasis = static_cast<uint8_t>(adjust[lane_index].pre_emphasis(lane_index).get());
    }
  }

  // In the Recommended buffer translation programming for DisplayPort from the intel display
  // doc, the max voltage swing is 2/3 for DP/eDP and the max (voltage swing + pre-emphasis) is
  // 3. According to the v1.1a of the DP docs, if v + pe is too large then v should be reduced
  // to the highest supported value for the pe level (section 3.5.1.3)
  static constexpr uint32_t kMaxVoltageSwingPlusPreEmphasis = 3;
  if (voltage_swing + pre_emphasis > kMaxVoltageSwingPlusPreEmphasis) {
    voltage_swing = static_cast<uint8_t>(kMaxVoltageSwingPlusPreEmphasis - pre_emphasis);
  }
  const uint8_t max_port_voltage = controller()->igd_opregion().IsLowVoltageEdp(ddi()) ? 3 : 2;
  if (voltage_swing > max_port_voltage) {
    voltage_swing = max_port_voltage;
  }

  for (int lane_index = 0; lane_index < dp_lane_count_; lane_index++) {
    voltage_changed |= (training[lane_index].voltage_swing_set() != voltage_swing);
    training[lane_index].set_voltage_swing_set(voltage_swing);
    training[lane_index].set_max_swing_reached(voltage_swing == max_port_voltage);
    training[lane_index].set_pre_emphasis_set(pre_emphasis);
    training[lane_index].set_max_pre_emphasis_set(pre_emphasis + voltage_swing ==
                                                  kMaxVoltageSwingPlusPreEmphasis);
  }

  // Compute the index into the PHY configuration table.
  static constexpr int kFirstEntryForVoltageSwingLevel[] = {0, 4, 7, 9};

  // The array access is safe because `voltage_swing` + `pre_emphasis` is at
  // most 3. For the same reason, each (voltage_swing, pre_emphasis) index will
  // result in a different entry
  const int phy_config_index = kFirstEntryForVoltageSwingLevel[voltage_swing] + pre_emphasis;
  ZX_ASSERT(phy_config_index < 10);
  if (phy_config_index == 9) {
    // Entry 9 in the PHY configuration table is only usable for DisplayPort on
    // DDIs A and E, to support eDP displays. On DDIs B-D, entry 9 is dedicated
    // to HDMI.
    //
    // Voltage swing level 3 is only valid for eDP, so we should be on DDI A or
    // E, and should be servicing an eDP port.
    ZX_ASSERT(controller()->igd_opregion().IsLowVoltageEdp(ddi()));
    ZX_ASSERT(ddi() == 0 || ddi() == 4);
  }

  tgl_registers::DdiRegs ddi_regs(ddi());
  auto buffer_control = ddi_regs.BufferControl().ReadFrom(mmio_space());
  buffer_control.set_display_port_phy_config_kaby_lake(phy_config_index);
  buffer_control.WriteTo(mmio_space());

  return voltage_changed;
}

bool DpDisplay::LinkTrainingSetup() {
  ZX_ASSERT(capabilities_);

  tgl_registers::DdiRegs ddi_regs(ddi());

  // Tell the source device to emit the training pattern.
  auto dp_transport_control = ddi_regs.DpTransportControl().ReadFrom(mmio_space());
  dp_transport_control.set_enabled(true)
      .set_is_multi_stream(false)
      .set_sst_enhanced_framing(capabilities_->enhanced_frame_capability())
      .set_training_pattern(tgl_registers::DpTransportControl::kTrainingPattern1)
      .WriteTo(mmio_space());

  // Configure DDI PHY parameters (voltage swing and pre-emphasis).
  //
  // Kaby Lake: IHD-OS-KBL-Vol 12-1.17 pages 187-190
  // Skylake: IHD-OS-SKL-Vol 12-05.16 pages 181-183
  // TODO(fxbug.dev/31313): Read the VBT to handle unique motherboard configs for kaby lake
  uint8_t i_boost;
  const cpp20::span<const DdiPhyConfigEntry> entries =
      controller()->igd_opregion().IsLowVoltageEdp(ddi())
          ? GetEdpPhyConfigEntries(controller()->device_id(), &i_boost)
          : GetDpPhyConfigEntries(controller()->device_id(), &i_boost);
  const uint8_t i_boost_override = controller()->igd_opregion().GetIBoost(ddi(), /*is_dp=*/true);

  for (int entry_index = 0; entry_index < static_cast<int>(entries.size()); ++entry_index) {
    auto phy_config_entry1 =
        tgl_registers::DdiPhyConfigEntry1::GetDdiInstance(ddi(), entry_index).FromValue(0);
    phy_config_entry1.set_reg_value(entries[entry_index].entry1);
    if (i_boost_override) {
      phy_config_entry1.set_balance_leg_enable(1);
    }
    phy_config_entry1.WriteTo(mmio_space());

    auto phy_config_entry2 =
        tgl_registers::DdiPhyConfigEntry2::GetDdiInstance(ddi(), entry_index).FromValue(0);
    phy_config_entry2.set_reg_value(entries[entry_index].entry2).WriteTo(mmio_space());
  }

  const uint8_t i_boost_val = i_boost_override ? i_boost_override : i_boost;
  auto balance_control = tgl_registers::DdiPhyBalanceControl::Get().ReadFrom(mmio_space());
  balance_control.set_disable_balance_leg(!i_boost && !i_boost_override);
  balance_control.balance_leg_select_for_ddi(ddi()).set(i_boost_val);
  if (ddi() == tgl_registers::DDI_A && dp_lane_count_ == 4) {
    balance_control.balance_leg_select_for_ddi(tgl_registers::DDI_E).set(i_boost_val);
  }
  balance_control.WriteTo(mmio_space());

  // Enable and wait for DDI_BUF_CTL
  auto buffer_control = ddi_regs.BufferControl().ReadFrom(mmio_space());
  buffer_control.set_enabled(true)
      .set_display_port_phy_config_kaby_lake(0)
      .set_display_port_lane_count(dp_lane_count_)
      .WriteTo(mmio_space());
  zx_nanosleep(zx_deadline_after(ZX_USEC(518)));

  uint16_t link_rate_reg;
  uint8_t link_rate_val;
  if (dp_link_rate_table_idx_) {
    dpcd::LinkRateSet link_rate_set;
    link_rate_set.set_link_rate_idx(static_cast<uint8_t>(dp_link_rate_table_idx_.value()));
    link_rate_reg = dpcd::DPCD_LINK_RATE_SET;
    link_rate_val = link_rate_set.reg_value();
  } else {
    uint8_t target_bw;
    if (dp_link_rate_mhz_ == 1620) {
      target_bw = dpcd::LinkBw::k1620Mbps;
    } else if (dp_link_rate_mhz_ == 2700) {
      target_bw = dpcd::LinkBw::k2700Mbps;
    } else {
      ZX_ASSERT(dp_link_rate_mhz_ == 5400);
      target_bw = dpcd::LinkBw::k5400Mbps;
    }

    dpcd::LinkBw bw_setting;
    bw_setting.set_link_bw(target_bw);
    link_rate_reg = dpcd::DPCD_LINK_BW_SET;
    link_rate_val = bw_setting.reg_value();
  }

  // Configure the bandwidth and lane count settings
  dpcd::LaneCount lc_setting;
  lc_setting.set_lane_count_set(dp_lane_count_);
  lc_setting.set_enhanced_frame_enabled(capabilities_->enhanced_frame_capability());
  if (!DpcdWrite(link_rate_reg, &link_rate_val, 1) ||
      !DpcdWrite(dpcd::DPCD_COUNT_SET, lc_setting.reg_value_ptr(), 1)) {
    zxlogf(ERROR, "DP: Link training: failed to configure settings");
    return false;
  }

  return true;
}

// Number of times to poll with the same voltage level configured, as
// specified by the DisplayPort spec.
static const int kPollsPerVoltageLevel = 5;

bool DpDisplay::LinkTrainingStage1(dpcd::TrainingPatternSet* tp_set, dpcd::TrainingLaneSet* lanes) {
  ZX_ASSERT(capabilities_);

  // Tell the sink device to look for the training pattern.
  tp_set->set_training_pattern_set(tp_set->kTrainingPattern1);
  tp_set->set_scrambling_disable(1);

  dpcd::AdjustRequestLane adjust_req[dp_lane_count_];
  dpcd::LaneStatus lane_status[dp_lane_count_];

  int poll_count = 0;
  auto delay =
      capabilities_->dpcd_reg<dpcd::TrainingAuxRdInterval, dpcd::DPCD_TRAINING_AUX_RD_INTERVAL>();
  for (;;) {
    if (!DpcdRequestLinkTraining(*tp_set, lanes)) {
      return false;
    }

    zx_nanosleep(
        zx_deadline_after(ZX_USEC(delay.clock_recovery_delay_us(capabilities_->dpcd_revision()))));

    // Did the sink device receive the signal successfully?
    if (!DpcdReadPairedRegs<dpcd::DPCD_LANE0_1_STATUS, dpcd::LaneStatus>(lane_status)) {
      return false;
    }
    bool done = true;
    for (unsigned i = 0; i < dp_lane_count_; i++) {
      done &= lane_status[i].lane_cr_done(i).get();
    }
    if (done) {
      break;
    }

    for (unsigned i = 0; i < dp_lane_count_; i++) {
      if (lanes[i].max_swing_reached()) {
        zxlogf(ERROR, "DP: Link training: max voltage swing reached");
        return false;
      }
    }

    if (!DpcdReadPairedRegs<dpcd::DPCD_ADJUST_REQUEST_LANE0_1, dpcd::AdjustRequestLane>(
            adjust_req)) {
      return false;
    }

    if (DpcdHandleAdjustRequest(lanes, adjust_req)) {
      poll_count = 0;
    } else if (++poll_count == kPollsPerVoltageLevel) {
      zxlogf(ERROR, "DP: Link training: clock recovery step failed");
      return false;
    }
  }

  return true;
}

bool DpDisplay::LinkTrainingStage2(dpcd::TrainingPatternSet* tp_set, dpcd::TrainingLaneSet* lanes) {
  ZX_ASSERT(capabilities_);

  tgl_registers::DdiRegs ddi_regs(ddi());
  auto dp_transport_control = ddi_regs.DpTransportControl().ReadFrom(mmio_space());

  dpcd::AdjustRequestLane adjust_req[dp_lane_count_];
  dpcd::LaneStatus lane_status[dp_lane_count_];

  dp_transport_control.set_training_pattern(tgl_registers::DpTransportControl::kTrainingPattern2);
  dp_transport_control.WriteTo(mmio_space());

  tp_set->set_training_pattern_set(tp_set->kTrainingPattern2);
  int poll_count = 0;
  auto delay =
      capabilities_->dpcd_reg<dpcd::TrainingAuxRdInterval, dpcd::DPCD_TRAINING_AUX_RD_INTERVAL>();
  for (;;) {
    // lane0_training and lane1_training can change in the loop
    if (!DpcdRequestLinkTraining(*tp_set, lanes)) {
      return false;
    }

    zx_nanosleep(zx_deadline_after(ZX_USEC(delay.channel_eq_delay_us())));

    // Did the sink device receive the signal successfully?
    if (!DpcdReadPairedRegs<dpcd::DPCD_LANE0_1_STATUS, dpcd::LaneStatus>(lane_status)) {
      return false;
    }
    for (unsigned i = 0; i < dp_lane_count_; i++) {
      if (!lane_status[i].lane_cr_done(i).get()) {
        zxlogf(ERROR, "DP: Link training: clock recovery regressed");
        return false;
      }
    }

    bool symbol_lock_done = true;
    bool channel_eq_done = true;
    for (unsigned i = 0; i < dp_lane_count_; i++) {
      symbol_lock_done &= lane_status[i].lane_symbol_locked(i).get();
      channel_eq_done &= lane_status[i].lane_channel_eq_done(i).get();
    }
    if (symbol_lock_done && channel_eq_done) {
      break;
    }

    // The training attempt has not succeeded yet.
    if (++poll_count == kPollsPerVoltageLevel) {
      if (symbol_lock_done) {
        zxlogf(ERROR, "DP: Link training: symbol lock failed");
        return false;
      } else {
        zxlogf(ERROR, "DP: Link training: channel equalization failed");
        return false;
      }
    }

    if (!DpcdReadPairedRegs<dpcd::DPCD_ADJUST_REQUEST_LANE0_1, dpcd::AdjustRequestLane>(
            adjust_req)) {
      return false;
    }
    DpcdHandleAdjustRequest(lanes, adjust_req);
  }

  dp_transport_control.set_training_pattern(tgl_registers::DpTransportControl::kSendPixelData)
      .WriteTo(mmio_space());

  return true;
}

bool DpDisplay::DoLinkTraining() {
  // TODO(fxbug.dev/31313): If either of the two training steps fails, we're
  // supposed to try with a reduced bit rate.
  bool result = LinkTrainingSetup();
  if (result) {
    dpcd::TrainingPatternSet tp_set;
    dpcd::TrainingLaneSet lanes[dp_lane_count_];
    result = LinkTrainingStage1(&tp_set, lanes) && LinkTrainingStage2(&tp_set, lanes);
  }

  // Tell the sink device to end its link training attempt.
  //
  // If link training was successful, we need to do this so that the sink
  // device will accept pixel data from the source device.
  //
  // If link training was not successful, we want to do this so that
  // subsequent link training attempts can work.  If we don't unset this
  // register, subsequent link training attempts can also fail.  (This
  // can be important during development.  The sink device won't
  // necessarily get reset when the computer is reset.  This means that a
  // bad version of the driver can leave the sink device in a state where
  // good versions subsequently don't work.)
  uint32_t addr = dpcd::DPCD_TRAINING_PATTERN_SET;
  uint8_t reg_byte = 0;
  if (!DpcdWrite(addr, &reg_byte, sizeof(reg_byte))) {
    zxlogf(ERROR, "Failure setting TRAINING_PATTERN_SET");
    return false;
  }

  return result;
}

}  // namespace i915_tgl

namespace {

// Convert ratio x/y into the form used by the Link/Data M/N ratio registers.
void CalculateRatio(uint32_t x, uint32_t y, uint32_t* m_out, uint32_t* n_out) {
  // The exact values of N and M shouldn't matter too much.  N and M can be
  // up to 24 bits, and larger values will tend to represent the ratio more
  // accurately. However, large values of N (e.g. 1 << 23) cause some monitors
  // to inexplicably fail. Pick a relatively arbitrary value for N that works
  // well in practice.
  *n_out = 1 << 20;
  *m_out = static_cast<uint32_t>(static_cast<uint64_t>(x) * *n_out / y);
}

bool IsEdp(i915_tgl::Controller* controller, tgl_registers::Ddi ddi) {
  return controller && controller->igd_opregion().IsEdp(ddi);
}

}  // namespace

namespace i915_tgl {

DpDisplay::DpDisplay(Controller* controller, uint64_t id, tgl_registers::Ddi ddi,
                     DpcdChannel* dp_aux, PchEngine* pch_engine, inspect::Node* parent_node)
    : DisplayDevice(controller, id, ddi, IsEdp(controller, ddi) ? Type::kEdp : Type::kDp),
      dp_aux_(dp_aux),
      pch_engine_(type() == Type::kEdp ? pch_engine : nullptr) {
  ZX_ASSERT(dp_aux);
  if (type() == Type::kEdp) {
    ZX_ASSERT(pch_engine_ != nullptr);
  } else {
    ZX_ASSERT(pch_engine_ == nullptr);
  }

  inspect_node_ = parent_node->CreateChild(fbl::StringPrintf("dp-display-%lu", id));
  dp_lane_count_inspect_ = inspect_node_.CreateUint("dp_lane_count", 0);
  dp_link_rate_mhz_inspect_ = inspect_node_.CreateUint("dp_link_rate_mhz", 0);
}

DpDisplay::~DpDisplay() = default;

bool DpDisplay::Query() {
  // For eDP displays, assume that the BIOS has enabled panel power, given
  // that we need to rely on it properly configuring panel power anyway. For
  // general DP displays, the default power state is D0, so we don't have to
  // worry about AUX failures because of power saving mode.
  {
    auto capabilities = DpCapabilities::Read(dp_aux_, &inspect_node_);
    if (capabilities.is_error()) {
      return false;
    }

    capabilities_ = capabilities.take_value();
  }

  // TODO(fxbug.dev/31313): Add support for MST
  if (capabilities_->sink_count() != 1) {
    zxlogf(ERROR, "MST not supported");
    return false;
  }

  uint8_t lane_count = capabilities_->max_lane_count();
  // On Kaby Lake and Skylake, DDI E takes over two of DDI A's four lanes. In
  // other words, if DDI E is enabled, DDI A only has two lanes available. DDI E
  // always has two lanes available.
  //
  // Kaby Lake: IHD-OS-KBL-Vol 12-1.17 "Display Connections" > "DDIs" page 107
  // Skylake: IHD-OS-SKL-Vol 12-05.16 "Display Connections" > "DDIs" page 105
  if (ddi() == tgl_registers::DDI_A || ddi() == tgl_registers::DDI_E) {
    const bool ddi_e_enabled = !tgl_registers::DdiRegs(tgl_registers::DDI_A)
                                    .BufferControl()
                                    .ReadFrom(mmio_space())
                                    .ddi_e_disabled_kaby_lake();
    if (ddi_e_enabled) {
      lane_count = std::min<uint8_t>(lane_count, 2);
    }
  }

  dp_lane_count_ = lane_count;
  dp_lane_count_inspect_.Set(lane_count);

  ZX_ASSERT(!dp_link_rate_table_idx_.has_value());
  ZX_ASSERT(!capabilities_->supported_link_rates_mbps().empty());

  uint8_t last = static_cast<uint8_t>(capabilities_->supported_link_rates_mbps().size() - 1);
  zxlogf(INFO, "Found %s monitor (max link rate: %d MHz, lane count: %d)",
         (type() == Type::kEdp ? "eDP" : "DP"), capabilities_->supported_link_rates_mbps()[last],
         dp_lane_count_);

  return true;
}

bool DpDisplay::InitDdi() {
  ZX_ASSERT(capabilities_);

  if (type() == Type::kEdp) {
    if (!EnsureEdpPanelIsPoweredOn()) {
      return false;
    }
  }

  if (capabilities_->dpcd_revision() >= dpcd::Revision::k1_1) {
    // If the device is in a low power state, the first write can fail. It should be ready
    // within 1ms, but try a few extra times to be safe.
    dpcd::SetPower set_pwr;
    set_pwr.set_set_power_state(set_pwr.kOn);
    int count = 0;
    while (!DpcdWrite(dpcd::DPCD_SET_POWER, set_pwr.reg_value_ptr(), 1) && ++count < 5) {
      zx_nanosleep(zx_deadline_after(ZX_MSEC(1)));
    }
    if (count >= 5) {
      zxlogf(ERROR, "Failed to set dp power state");
      return ZX_ERR_INTERNAL;
    }
  }

  dpcd::LaneAlignStatusUpdate status;
  if (!DpcdRead(dpcd::DPCD_LANE_ALIGN_STATUS_UPDATED, status.reg_value_ptr(), 1)) {
    zxlogf(WARNING, "Failed to read align status on hotplug");
    return false;
  }

  // If the link is already trained, assume output is working
  if (status.interlane_align_done()) {
    return true;
  }

  // Determine the current link rate if one hasn't been assigned.
  if (dp_link_rate_mhz_ == 0) {
    ZX_ASSERT(!capabilities_->supported_link_rates_mbps().empty());

    // Pick the maximum supported link rate.
    uint8_t index = static_cast<uint8_t>(capabilities_->supported_link_rates_mbps().size() - 1);
    uint32_t link_rate = capabilities_->supported_link_rates_mbps()[index];

    zxlogf(INFO, "Selected maximum supported DisplayPort link rate: %u Mbps/lane", link_rate);
    SetLinkRate(link_rate);
    if (capabilities_->use_link_rate_table()) {
      dp_link_rate_table_idx_ = index;
    }
  }

  DpllState state = DpDpllState{
      .dp_bit_rate_mhz = dp_link_rate_mhz_,
  };

  DisplayPll* dpll = controller()->dpll_manager()->Map(ddi(), type() == Type::kEdp, state);
  if (dpll == nullptr) {
    zxlogf(ERROR, "Cannot find an available DPLL for DP display on DDI %d", ddi());
    return false;
  }

  // Enable power for this DDI.
  controller()->power()->SetDdiIoPowerState(ddi(), /* enable */ true);
  if (!PollUntil([&] { return controller()->power()->GetDdiIoPowerState(ddi()); }, zx::nsec(1),
                 20)) {
    zxlogf(ERROR, "Failed to enable IO power for ddi");
    return false;
  }

  // Do link training
  if (!DoLinkTraining()) {
    zxlogf(ERROR, "DDI %d: DisplayPort link training failed", ddi());
    return false;
  }

  return true;
}

bool DpDisplay::InitWithDpllState(const DpllState* dpll_state) {
  if (dpll_state == nullptr) {
    return false;
  }

  ZX_DEBUG_ASSERT(std::holds_alternative<DpDpllState>(*dpll_state));
  if (!std::holds_alternative<DpDpllState>(*dpll_state)) {
    zxlogf(ERROR, "Non DP dpll_state is given to DpDisplay!");
    return false;
  }

  Pipe* pipe = controller()->pipe_manager()->RequestPipeFromHardwareState(*this, mmio_space());
  if (pipe == nullptr) {
    zxlogf(ERROR, "Failed loading pipe from register!");
    return false;
  }
  set_pipe(pipe);

  auto dp_state = std::get_if<DpDpllState>(dpll_state);
  // Some display (e.g. eDP) may have already been configured by the bootloader with a
  // link clock. Assign the link rate based on the already enabled DPLL.
  if (dp_link_rate_mhz_ == 0) {
    // Since the link rate is read from the register directly, we can guarantee
    // that it is always valid.
    zxlogf(INFO, "Selected pre-configured DisplayPort link rate: %u Mbps/lane",
           dp_state->dp_bit_rate_mhz);
    SetLinkRate(dp_state->dp_bit_rate_mhz);
  }
  return true;
}

bool DpDisplay::ComputeDpllState(uint32_t pixel_clock_10khz, DpllState* config) {
  *config = DpDpllState{
      .dp_bit_rate_mhz = dp_link_rate_mhz_,
  };
  return true;
}

bool DpDisplay::DdiModeset(const display_mode_t& mode) { return true; }

bool DpDisplay::PipeConfigPreamble(const display_mode_t& mode, tgl_registers::Pipe pipe,
                                   tgl_registers::Trans trans) {
  tgl_registers::TranscoderRegs trans_regs(trans);

  // Configure Transcoder Clock Select
  if (trans != tgl_registers::TRANS_EDP) {
    auto clock_select = trans_regs.ClockSelect().ReadFrom(mmio_space());
    clock_select.set_trans_clock_select(ddi() + 1);
    clock_select.WriteTo(mmio_space());
  }

  // Pixel clock rate: The rate at which pixels are sent, in pixels per
  // second (Hz), divided by 10000.
  uint32_t pixel_clock_rate = mode.pixel_clock_10khz;

  // This is the rate at which bits are sent on a single DisplayPort
  // lane, in raw bits per second, divided by 10000.
  uint32_t link_raw_bit_rate = dp_link_rate_mhz_ * 100;
  // Link symbol rate: The rate at which link symbols are sent on a
  // single DisplayPort lane.  A link symbol is 10 raw bits (using 8b/10b
  // encoding, which usually encodes an 8-bit data byte).
  uint32_t link_symbol_rate = link_raw_bit_rate / 10;

  // Configure ratios between pixel clock/bit rate and symbol clock/bit rate
  uint32_t link_m;
  uint32_t link_n;
  CalculateRatio(pixel_clock_rate, link_symbol_rate, &link_m, &link_n);

  uint32_t pixel_bit_rate = pixel_clock_rate * kBitsPerPixel;
  uint32_t total_link_bit_rate = link_symbol_rate * 8 * dp_lane_count_;
  ZX_DEBUG_ASSERT(pixel_bit_rate <= total_link_bit_rate);  // Should be caught by CheckPixelRate

  uint32_t data_m;
  uint32_t data_n;
  CalculateRatio(pixel_bit_rate, total_link_bit_rate, &data_m, &data_n);

  auto data_m_reg = trans_regs.DataM().FromValue(0);
  data_m_reg.set_tu_or_vcpayload_size(63);  // Size - 1, default TU size is 64
  data_m_reg.set_data_m_value(data_m);
  data_m_reg.WriteTo(mmio_space());

  auto data_n_reg = trans_regs.DataN().FromValue(0);
  data_n_reg.set_data_n_value(data_n);
  data_n_reg.WriteTo(mmio_space());

  auto link_m_reg = trans_regs.LinkM().FromValue(0);
  link_m_reg.set_link_m_value(link_m);
  link_m_reg.WriteTo(mmio_space());

  auto link_n_reg = trans_regs.LinkN().FromValue(0);
  link_n_reg.set_link_n_value(link_n);
  link_n_reg.WriteTo(mmio_space());

  return true;
}

bool DpDisplay::PipeConfigEpilogue(const display_mode_t& mode, tgl_registers::Pipe pipe,
                                   tgl_registers::Trans trans) {
  tgl_registers::TranscoderRegs trans_regs(trans);
  auto msa_misc = trans_regs.MsaMisc().FromValue(0);
  msa_misc.set_sync_clock(1);
  msa_misc.set_bits_per_color(msa_misc.k8Bbc);  // kPixelFormat
  msa_misc.set_color_format(msa_misc.kRgb);     // kPixelFormat
  msa_misc.WriteTo(mmio_space());

  auto ddi_func = trans_regs.DdiFuncControl().ReadFrom(mmio_space());
  ddi_func.set_trans_ddi_function_enable(1);
  ddi_func.set_ddi_select(ddi());
  ddi_func.set_trans_ddi_mode_select(ddi_func.kModeDisplayPortSst);
  ddi_func.set_bits_per_color(ddi_func.k8bbc);  // kPixelFormat
  ddi_func.set_sync_polarity((!!(mode.flags & MODE_FLAG_VSYNC_POSITIVE)) << 1 |
                             (!!(mode.flags & MODE_FLAG_HSYNC_POSITIVE)));
  ddi_func.set_port_sync_mode_enable(0);
  ddi_func.set_dp_vc_payload_allocate(0);
  ddi_func.set_edp_input_select(
      pipe == tgl_registers::PIPE_A
          ? ddi_func.kPipeA
          : (pipe == tgl_registers::PIPE_B ? ddi_func.kPipeB : ddi_func.kPipeC));
  ddi_func.set_dp_port_width_selection(dp_lane_count_ - 1);
  ddi_func.WriteTo(mmio_space());

  auto trans_conf = trans_regs.Conf().FromValue(0);
  trans_conf.set_transcoder_enable(1);
  trans_conf.set_interlaced_mode(!!(mode.flags & MODE_FLAG_INTERLACED));
  trans_conf.WriteTo(mmio_space());

  return true;
}

bool DpDisplay::InitBacklightHw() {
  if (capabilities_ && capabilities_->backlight_aux_brightness()) {
    dpcd::EdpBacklightModeSet mode;
    mode.set_brightness_ctrl_mode(mode.kAux);
    if (!DpcdWrite(dpcd::DPCD_EDP_BACKLIGHT_MODE_SET, mode.reg_value_ptr(), 1)) {
      zxlogf(ERROR, "Failed to init backlight");
      return false;
    }
  }
  return true;
}

bool DpDisplay::SetBacklightOn(bool backlight_on) {
  if (type() != Type::kEdp) {
    return true;
  }

  if (capabilities_ && capabilities_->backlight_aux_power()) {
    dpcd::EdpDisplayCtrl ctrl;
    ctrl.set_backlight_enable(backlight_on);
    if (!DpcdWrite(dpcd::DPCD_EDP_DISPLAY_CTRL, ctrl.reg_value_ptr(), 1)) {
      zxlogf(ERROR, "Failed to enable backlight");
      return false;
    }
  } else {
    pch_engine_->SetPanelPowerTarget({
        .power_on = true,
        .backlight_on = backlight_on,
        .force_power_on = false,
        .brightness_pwm_counter_on = backlight_on,
    });
  }

  return !backlight_on || SetBacklightBrightness(backlight_brightness_);
}

bool DpDisplay::IsBacklightOn() {
  // If there is no embedded display, return false.
  if (type() != Type::kEdp) {
    return false;
  }

  if (capabilities_ && capabilities_->backlight_aux_power()) {
    dpcd::EdpDisplayCtrl ctrl;

    if (!DpcdRead(dpcd::DPCD_EDP_DISPLAY_CTRL, ctrl.reg_value_ptr(), 1)) {
      zxlogf(ERROR, "Failed to read backlight");
      return false;
    }

    return ctrl.backlight_enable();
  } else {
    return pch_engine_->PanelPowerTarget().backlight_on;
  }
}

bool DpDisplay::SetBacklightBrightness(double val) {
  if (type() != Type::kEdp) {
    return true;
  }

  backlight_brightness_ = std::max(val, controller()->igd_opregion().GetMinBacklightBrightness());
  backlight_brightness_ = std::min(backlight_brightness_, 1.0);

  if (capabilities_ && capabilities_->backlight_aux_brightness()) {
    uint16_t percent = static_cast<uint16_t>(0xffff * backlight_brightness_ + .5);

    uint8_t lsb = static_cast<uint8_t>(percent & 0xff);
    uint8_t msb = static_cast<uint8_t>(percent >> 8);
    if (!DpcdWrite(dpcd::DPCD_EDP_BACKLIGHT_BRIGHTNESS_MSB, &msb, 1) ||
        !DpcdWrite(dpcd::DPCD_EDP_BACKLIGHT_BRIGHTNESS_LSB, &lsb, 1)) {
      zxlogf(ERROR, "Failed to set backlight brightness");
      return false;
    }
  } else {
    pch_engine_->SetPanelBrightness(val);
  }

  return true;
}

double DpDisplay::GetBacklightBrightness() {
  if (!HasBacklight()) {
    return 0;
  }

  double percent = 0;

  if (capabilities_ && capabilities_->backlight_aux_brightness()) {
    uint8_t lsb;
    uint8_t msb;
    if (!DpcdRead(dpcd::DPCD_EDP_BACKLIGHT_BRIGHTNESS_MSB, &msb, 1) ||
        !DpcdRead(dpcd::DPCD_EDP_BACKLIGHT_BRIGHTNESS_LSB, &lsb, 1)) {
      zxlogf(ERROR, "Failed to read backlight brightness");
      return 0;
    }

    uint16_t brightness = static_cast<uint16_t>((lsb & 0xff) | (msb << 8));

    percent = (brightness * 1.0f) / 0xffff;

  } else {
    percent = pch_engine_->PanelBrightness();
  }

  return percent;
}

bool DpDisplay::HandleHotplug(bool long_pulse) {
  if (!long_pulse) {
    dpcd::SinkCount sink_count;
    if (!DpcdRead(dpcd::DPCD_SINK_COUNT, sink_count.reg_value_ptr(), 1)) {
      zxlogf(WARNING, "Failed to read sink count on hotplug");
      return false;
    }

    // The pulse was from a downstream monitor being connected
    // TODO(fxbug.dev/31313): Add support for MST
    if (sink_count.count() > 1) {
      return true;
    }

    // The pulse was from a downstream monitor disconnecting
    if (sink_count.count() == 0) {
      return false;
    }

    dpcd::LaneAlignStatusUpdate status;
    if (!DpcdRead(dpcd::DPCD_LANE_ALIGN_STATUS_UPDATED, status.reg_value_ptr(), 1)) {
      zxlogf(WARNING, "Failed to read align status on hotplug");
      return false;
    }

    if (status.interlane_align_done()) {
      zxlogf(DEBUG, "HPD event for trained link");
      return true;
    }

    return DoLinkTraining();
  }
  return false;
}

bool DpDisplay::HasBacklight() { return type() == Type::kEdp; }

zx::status<> DpDisplay::SetBacklightState(bool power, double brightness) {
  SetBacklightOn(power);

  brightness = std::max(brightness, 0.0);
  brightness = std::min(brightness, 1.0);

  double range = 1.0f - controller()->igd_opregion().GetMinBacklightBrightness();
  if (!SetBacklightBrightness((range * brightness) +
                              controller()->igd_opregion().GetMinBacklightBrightness())) {
    return zx::error(ZX_ERR_IO);
  }
  return zx::success();
}

zx::status<FidlBacklight::wire::State> DpDisplay::GetBacklightState() {
  return zx::success(FidlBacklight::wire::State{
      .backlight_on = IsBacklightOn(),
      .brightness = GetBacklightBrightness(),
  });
}

void DpDisplay::SetLinkRate(uint32_t value) {
  dp_link_rate_mhz_ = value;
  dp_link_rate_mhz_inspect_.Set(value);
}

bool DpDisplay::CheckPixelRate(uint64_t pixel_rate) {
  uint64_t bit_rate = (dp_link_rate_mhz_ * 1000000lu) * dp_lane_count_;
  // Multiply by 8/10 because of 8b/10b encoding
  uint64_t max_pixel_rate = (bit_rate * 8 / 10) / kBitsPerPixel;
  return pixel_rate <= max_pixel_rate;
}

uint32_t DpDisplay::LoadClockRateForTranscoder(tgl_registers::Trans transcoder) {
  tgl_registers::TranscoderRegs trans_regs(transcoder);
  uint32_t data_m = trans_regs.DataM().ReadFrom(mmio_space()).data_m_value();
  uint32_t data_n = trans_regs.DataN().ReadFrom(mmio_space()).data_n_value();

  double total_link_bit_rate_10khz = dp_link_rate_mhz_ * 100. * (8. / 10.) * dp_lane_count_;
  double res = (data_m * total_link_bit_rate_10khz) / (data_n * kBitsPerPixel);
  return static_cast<uint32_t>(round(res));
}

}  // namespace i915_tgl
