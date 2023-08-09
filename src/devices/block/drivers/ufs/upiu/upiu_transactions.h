// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_UPIU_TRANSACTIONS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_UPIU_TRANSACTIONS_H_

#include <endian.h>

#include <optional>

#include <fbl/algorithm.h>
#include <hwreg/bitfields.h>

#include "src/devices/block/drivers/ufs/transfer_request_descriptor.h"

namespace ufs {

// UPIU requires 64-bit alignment.
constexpr uint16_t kUpiuAlignment = 8;

// for test
namespace ufs_mock_device {
class TransferRequestProcessor;
class QueryRequestProcessor;
class ScsiCommandProcessor;
class UfsMockDevice;
}  // namespace ufs_mock_device

// UFS Specification Version 3.1, section 10.5 "UPIU Transactions".
enum UpiuTransactionCodes {
  kNopOut = 0x00,
  kCommand = 0x01,
  kDataOut = 0x02,
  kTaskManagementRequest = 0x04,
  kQueryRequest = 0x16,
  kNopIn = 0x20,
  kResponse = 0x21,
  kDataIn = 0x22,
  kTaskManagementResponse = 0x24,
  kReadyToTransfer = 0x31,
  kQueryResponse = 0x36,
  kRejectUpiu = 0x3f,
};

enum UpiuCommandSetType {
  kScsi = 0x00,
  kUfsSpecificCommandSet = 0x01,
  kVendorSpecificSet = 0x08,
};

enum UpiuHeaderResponse {
  kTargetSuccess = 0x00,
  kTargetFailure = 0x01,
};

// UFS Specification Version 3.1, section 10.6.2 "Basic Header Format".
struct UpiuHeader {
  // dword 0
  uint8_t trans_type = 0;
  uint8_t flags = 0;
  uint8_t lun = 0;
  uint8_t task_tag = 0;

  // dword 1
  uint8_t cmd_set_type_and_initiator_id = 0;
  uint8_t function = 0;
  uint8_t response = UpiuHeaderResponse::kTargetSuccess;
  uint8_t status = 0;

  // dword 2
  uint8_t ehs_length = 0;
  uint8_t device_infomation = 0;
  uint16_t data_segment_length = 0;  // (Big-endian)

  DEF_SUBFIELD(trans_type, 5, 0, trans_code);
  DEF_SUBBIT(trans_type, 6, dd);
  DEF_SUBBIT(trans_type, 7, hd);

  DEF_SUBBIT(flags, 2, cp);
  DEF_SUBFIELD(flags, 1, 0, task_attribute);

  DEF_SUBFIELD(cmd_set_type_and_initiator_id, 3, 0, command_set_type);
  DEF_SUBFIELD(cmd_set_type_and_initiator_id, 7, 4, initiator_id);
} __PACKED;
static_assert(sizeof(UpiuHeader) == 12, "UpiuHeader struct must be 12 bytes");

class AbstractUpiu {
 public:
  // TODO(fxbug.dev/124835): Make |AbstractUpiu| to template class for removing the |Data| struct
  // within |AbstractUpiu|. Currently each derived class must have a |UpiuHeader| as the first
  // member in its |Data| struct.
  struct Data {
    UpiuHeader header;
  } __PACKED;

  explicit AbstractUpiu() = default;
  explicit AbstractUpiu(void* data) : data_ptr_(data) {}

  virtual ~AbstractUpiu() = default;

  // Used to read or write the request descriptor in the UPIU.
  template <typename T = void>
  T* GetData() {
    return reinterpret_cast<T*>(data_ptr_);
  }
  UpiuHeader& GetHeader() {
    ZX_ASSERT(GetData());
    return GetData<Data>()->header;
  }

 protected:
  void SetData(void* data) { data_ptr_ = data; }

 private:
  void* data_ptr_ = nullptr;
};

template <typename RequestData, typename ResponseData>
class AbstractRequestUpiu : public AbstractUpiu {
 public:
  explicit AbstractRequestUpiu() {
    data_ = std::make_unique<RequestData>();
    SetData(data_.get());
  }
  explicit AbstractRequestUpiu(void* data) = delete;

  ~AbstractRequestUpiu() override = default;

  // Get the direction of the data transfer to be written to the request descriptor. The
  // TransferRequestDescriptorDataDirection determines whether the target device will read or write
  // the system memory area pointed to by the PRDT.
  virtual TransferRequestDescriptorDataDirection GetDataDirection() const {
    return TransferRequestDescriptorDataDirection::kNone;
  }

  // Get the offset that ResponseUpiu will be written to.
  uint16_t GetResponseOffset() const { return sizeof(RequestData); }

  // Get the length of the ResponseUpiu.
  uint16_t GetResponseLength() const { return sizeof(ResponseData); }

 private:
  std::unique_ptr<RequestData> data_ = nullptr;
};

using AbstractResponseUpiu = AbstractUpiu;

struct ResponseUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3
  uint32_t residual_transfer_count = 0;  // (Big-endian)

  // dword 4 ~ 6
  uint8_t reserved[16] = {0};

  // Sense Data
  uint16_t sense_data_len = 0;  // (Big-endian)
  uint8_t sense_data[18] = {0};

  // Add padding to align the kUpiuAlignment.
  uint8_t padding[4] = {0};

  DEF_SUBBIT(header.flags, 6, header_flags_o);
  DEF_SUBBIT(header.flags, 5, header_flags_u);
  DEF_SUBBIT(header.flags, 4, header_flags_d);
} __PACKED;
static_assert(sizeof(ResponseUpiuData) == 56, "ResponseUpiu struct, must be 56 bytes");
static_assert(sizeof(ResponseUpiuData) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.2 "RESPONSE UPIU".
class ResponseUpiu : public AbstractResponseUpiu {
 public:
  explicit ResponseUpiu(void* data) : AbstractResponseUpiu(data) {}

  ~ResponseUpiu() override = default;

  uint8_t* GetSenseData() { return GetData<ResponseUpiuData>()->sense_data; }

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::ScsiCommandProcessor;
  friend class UfsTest;
};

struct CommandUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3
  uint32_t expected_data_transfer_length = 0;  // (Big-endian)

  // dword 4 ~ 7
  uint8_t cdb[16] = {0};

  DEF_SUBBIT(header.flags, 6, header_flags_r);
  DEF_SUBBIT(header.flags, 5, header_flags_w);
  DEF_SUBBIT(header.flags, 2, header_flags_cp);
  DEF_SUBFIELD(header.flags, 1, 0, header_flags_attr);
} __PACKED;
static_assert(sizeof(CommandUpiuData) == 32, "CommandUpiu struct must be 32 bytes");
static_assert(sizeof(CommandUpiuData) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.1 "COMMAND UPIU".
class CommandUpiu : public AbstractRequestUpiu<CommandUpiuData, ResponseUpiuData> {
 public:
  explicit CommandUpiu() { GetHeader().set_trans_code(UpiuTransactionCodes::kCommand); }

  explicit CommandUpiu(UpiuCommandSetType command_set_type) : CommandUpiu() {
    GetHeader().set_command_set_type(command_set_type);
  }

  ~CommandUpiu() override = default;

  void SetExpectedDataTransferLength(uint32_t length) {
    GetData<CommandUpiuData>()->expected_data_transfer_length = htobe32(length);
  }

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::ScsiCommandProcessor;
  friend class UfsTest;
};

struct TaskManagementResponseUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3
  uint32_t param1 = 0;  // (Big-endian)
  // dword 4
  uint32_t param2 = 0;  // (Big-endian)
  // dword 5 ~ 7
  uint8_t reserved[12] = {0};
} __PACKED;
static_assert(sizeof(TaskManagementResponseUpiuData) == 32,
              "TaskManagementResponseUpiu struct must be 32 bytes");
static_assert(sizeof(TaskManagementResponseUpiuData) % kUpiuAlignment == 0,
              "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.7 "TASK MANAGEMENT RESPONSE UPIU".
class TaskManagementResponseUpiu : public AbstractResponseUpiu {
 public:
  explicit TaskManagementResponseUpiu(void* data) : AbstractResponseUpiu(data) {}

  ~TaskManagementResponseUpiu() override = default;
};

struct TaskManagementRequestUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3
  uint32_t param1 = 0;  // (Big-endian)
  // dword 4
  uint32_t param2 = 0;  // (Big-endian)
  // dword 5
  uint32_t param3 = 0;  // (Big-endian)
  // dword 6 ~ 7
  uint8_t reserved[8] = {0};
} __PACKED;
static_assert(sizeof(TaskManagementRequestUpiuData) == 32,
              "TaskManagementRequestUpiu struct must be 32 bytes");
static_assert(sizeof(TaskManagementRequestUpiuData) % kUpiuAlignment == 0,
              "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.6 "TASK MANAGEMENT REQUEST UPIU".
class TaskManagementRequestUpiu
    : public AbstractRequestUpiu<TaskManagementRequestUpiuData, TaskManagementResponseUpiuData> {
 public:
  explicit TaskManagementRequestUpiu() {
    GetHeader().set_trans_code(UpiuTransactionCodes::kTaskManagementRequest);
  }

  ~TaskManagementRequestUpiu() override = default;
};

enum class QueryFunction {
  kStandardReadRequest = 0x01,
  kStandardWriteRequest = 0x81,
};

enum class QueryOpcode {
  kNop = 0,
  kReadDescriptor,
  kWriteDescriptor,
  kReadAttribute,
  kWriteAttribute,
  kReadFlag,
  kSetFlag,
  kClearFlag,
  kToggleFlag,
};

struct QueryResponseUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3
  uint8_t opcode = 0;
  uint8_t idn = 0;
  uint8_t index = 0;
  uint8_t selector = 0;

  // dword 4
  uint8_t reserved1[2] = {0};
  uint16_t length = 0;  // (Big-endian)

  // dword 5
  union {
    uint32_t value = 0;  // (Big-endian)
    struct {
      uint8_t reserved2[3];
      uint8_t flag_value;
    };
  } __PACKED;

  // dword 6
  uint8_t reserved3[4] = {0};

  // dword 7
  uint8_t reserved4[4] = {0};
  std::array<uint8_t, 256> command_data = {0};
} __PACKED;
static_assert(sizeof(QueryResponseUpiuData) == 288, "QueryResponseUpiu struct must be 288 bytes");
static_assert(sizeof(QueryResponseUpiuData) % kUpiuAlignment == 0,
              "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.9 "QUERY RESPONSE UPIU".
class QueryResponseUpiu : public AbstractResponseUpiu {
 public:
  explicit QueryResponseUpiu(void* data) : AbstractResponseUpiu(data) {}

  ~QueryResponseUpiu() override = default;

  template <typename U>
  constexpr U& GetResponse() {
    static_assert(std::is_base_of<QueryResponseUpiu, U>::value);
    static_assert(sizeof(U) == sizeof(QueryResponseUpiu));
    return *reinterpret_cast<U*>(this);
  }

  // for test
  uint8_t GetOpcode() { return GetData<QueryResponseUpiuData>()->opcode; }
  uint8_t GetIdn() { return GetData<QueryResponseUpiuData>()->idn; }
  uint8_t GetIndex() { return GetData<QueryResponseUpiuData>()->index; }

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::QueryRequestProcessor;
  friend class UfsTest;
};

struct QueryRequestUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3
  uint8_t opcode = 0;
  uint8_t idn = 0;
  uint8_t index = 0;
  uint8_t selector = 0;

  // dword 4
  uint8_t reserved1[2] = {0};
  uint16_t length = 0;  // (Big-endian)

  // dword 5
  uint32_t value = 0;  // (Big-endian)

  // dword 6
  uint8_t reserved2[4] = {0};

  // dword 7
  uint8_t reserved3[4] = {0};
  std::array<uint8_t, 256> command_data = {0};
} __PACKED;
static_assert(sizeof(QueryRequestUpiuData) == 288, "QueryRequestUpiu struct must be 288 bytes");
static_assert(sizeof(QueryRequestUpiuData) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.8 "QUERY REQUEST UPIU".
class QueryRequestUpiu : public AbstractRequestUpiu<QueryRequestUpiuData, QueryResponseUpiuData> {
 public:
  explicit QueryRequestUpiu(QueryFunction query_function, QueryOpcode query_opcode, uint8_t type,
                            uint8_t index = 0) {
    GetHeader().set_trans_code(UpiuTransactionCodes::kQueryRequest);

    GetHeader().function = static_cast<uint8_t>(query_function);

    GetData<QueryRequestUpiuData>()->opcode = static_cast<uint8_t>(query_opcode);
    GetData<QueryRequestUpiuData>()->idn = type;
    GetData<QueryRequestUpiuData>()->index = index;
  }

  ~QueryRequestUpiu() override = default;

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class ufs_mock_device::QueryRequestProcessor;
  friend class UfsTest;
};

struct NopInUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3 ~ 7
  uint8_t reserved[20] = {0};
} __PACKED;
static_assert(sizeof(NopInUpiuData) == 32, "NopInUpiu struct must be 32 bytes");
static_assert(sizeof(NopInUpiuData) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.12 "NOP IN UPIU".
class NopInUpiu : public AbstractResponseUpiu {
 public:
  explicit NopInUpiu(void* data) : AbstractResponseUpiu(data) {}

  ~NopInUpiu() override = default;

 private:
  // for test
  friend class ufs_mock_device::TransferRequestProcessor;
  friend class UfsTest;
};

struct NopOutUpiuData {
  // dword 0 ~ 2
  UpiuHeader header;
  // dword 3 ~ 7
  uint8_t reserved[20] = {0};
} __PACKED;
static_assert(sizeof(NopOutUpiuData) == 32, "NopOutUpiu struct must be 32 bytes");
static_assert(sizeof(NopOutUpiuData) % kUpiuAlignment == 0, "UPIU requires 64-bit alignment");

// UFS Specification Version 3.1, section 10.7.11 "NOP OUT UPIU".
class NopOutUpiu : public AbstractRequestUpiu<NopOutUpiuData, NopInUpiuData> {
 public:
  explicit NopOutUpiu() { GetHeader().set_trans_code(UpiuTransactionCodes::kNopOut); }

  ~NopOutUpiu() override = default;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UPIU_UPIU_TRANSACTIONS_H_
