// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_PSEUDO_FILE_H_
#define LIB_VFS_CPP_PSEUDO_FILE_H_

#include <lib/vfs/cpp/internal/connection.h>
#include <lib/vfs/cpp/internal/file.h>

namespace vfs {

// Buffered pseudo-file.
//
// This variant is optimized for incrementally reading and writing properties
// which are larger than can typically be read or written by the client in
// a single I/O transaction.
//
// In read mode, the pseudo-file invokes its read handler when the file is
// opened and retains the content in a buffer which the client incrementally
// reads from and can seek within.
//
// In write mode, the client incrementally writes into and seeks within the
// buffer which the pseudo-file delivers as a whole to the write handler when
// the file is closed(if there were any writes).  Truncation is also supported.
//
// This class is thread-hostile.
//
//  # Simple usage
//
// Instances of this class should be owned and managed on the same thread
// that services their connections.
//
// # Advanced usage
//
// You can use a background thread to service connections provided:
// async_dispatcher_t for the background thread is stopped or suspended
// prior to destroying the file.
class PseudoFile final : public vfs::internal::File {
 public:
  // Handler called to read from the pseudo-file.
  using ReadHandler = fit::function<zx_status_t(std::vector<uint8_t>* output, size_t max_bytes)>;

  // Handler called to write into the pseudo-file.
  using WriteHandler = fit::function<zx_status_t(std::vector<uint8_t> input)>;

  // Creates a buffered pseudo-file.
  //
  // |read_handler| cannot be null. If the |write_handler| is null, then the
  // pseudo-file is considered not writable. The |max_file_size|
  // determines the maximum number of bytes which can be written to and read from
  // the pseudo-file's input buffer when it it opened for writing/reading.
  PseudoFile(size_t max_file_size, ReadHandler read_handler = ReadHandler(),
             WriteHandler write_handler = WriteHandler());

  ~PseudoFile() override;

 protected:
  // |Node| implementations:
  zx_status_t GetAttr(fuchsia::io::NodeAttributes* out_attributes) const override;

  zx_status_t CreateConnection(fuchsia::io::OpenFlags flags,
                               std::unique_ptr<vfs::internal::Connection>* connection) override;

  fuchsia::io::OpenFlags GetAllowedFlags() const override;

 private:
  class Content final : public vfs::internal::Connection, public File {
   public:
    Content(PseudoFile* file, fuchsia::io::OpenFlags flags, std::vector<uint8_t> content);
    ~Content() override;

    // |File| implementations:
    zx_status_t ReadAt(uint64_t count, uint64_t offset, std::vector<uint8_t>* out_data) override;
    zx_status_t WriteAt(std::vector<uint8_t> data, uint64_t offset, uint64_t* out_actual) override;
    zx_status_t Truncate(uint64_t length) override;

    uint64_t GetLength() override;

    size_t GetCapacity() override;

    // Connection implementation:
    zx_status_t BindInternal(zx::channel request, async_dispatcher_t* dispatcher) override;

    // |Node| implementations:
    std::unique_ptr<Connection> Close(Connection* connection) override;

    zx_status_t PreClose(Connection* connection) override;

    void Clone(fuchsia::io::OpenFlags flags, fuchsia::io::OpenFlags parent_flags,
               zx::channel request, async_dispatcher_t* dispatcher) override;

    zx_status_t GetAttr(fuchsia::io::NodeAttributes* out_attributes) const override;

   protected:
    void SendOnOpenEvent(zx_status_t status) override;

    fuchsia::io::OpenFlags GetAllowedFlags() const override;
    fuchsia::io::OpenFlags GetProhibitiveFlags() const override;

   private:
    zx_status_t TryFlushIfRequired();

    void SetInputLength(size_t length);

    PseudoFile* const file_;

    std::vector<uint8_t> buffer_;
    fuchsia::io::OpenFlags flags_ = {};

    // true if the file was written into
    bool dirty_ = false;
  };

  // |File| implementations:
  uint64_t GetLength() override;

  size_t GetCapacity() override;

  ReadHandler const read_handler_;
  WriteHandler const write_handler_;
  const size_t max_file_size_;
};

}  // namespace vfs

#endif  // LIB_VFS_CPP_PSEUDO_FILE_H_
