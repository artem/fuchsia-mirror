// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/crash_reports/report.h"

#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include <string>

namespace forensics {
namespace crash_reports {
namespace {

std::optional<SizedData> MakeAttachment(const fuchsia::mem::Buffer& buffer) {
  if (!buffer.vmo.is_valid()) {
    return std::nullopt;
  }

  SizedData data(buffer.size, 0u);
  if (const zx_status_t status = buffer.vmo.read(data.data(), /*offset=*/0u, /*len=*/data.size());
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to read vmo";
    return std::nullopt;
  }
  return data;
}

}  // namespace

fpromise::result<Report> Report::MakeReport(const ReportId report_id,
                                            const std::string& program_shortname,
                                            const AnnotationMap& annotations,
                                            std::map<std::string, fuchsia::mem::Buffer> attachments,
                                            std::string snapshot_uuid,
                                            std::optional<fuchsia::mem::Buffer> minidump,
                                            const bool is_hourly_report) {
  std::map<std::string, SizedData> attachment_copies;
  for (const auto& [k, v] : attachments) {
    if (k.empty()) {
      return fpromise::error();
    }

    auto attachment = MakeAttachment(v);
    if (!attachment) {
      return fpromise::error();
    }
    attachment_copies.emplace(k, std::move(attachment.value()));
  }

  std::optional<SizedData> minidump_copy =
      minidump.has_value() ? MakeAttachment(minidump.value()) : std::nullopt;

  return fpromise::ok(Report(report_id, program_shortname, annotations,
                             std::move(attachment_copies), std::move(snapshot_uuid),
                             std::move(minidump_copy), is_hourly_report));
}

Report::Report(const ReportId report_id, const std::string& program_shortname,
               const AnnotationMap& annotations, std::map<std::string, SizedData> attachments,
               std::string snapshot_uuid, std::optional<SizedData> minidump,
               const bool is_hourly_report)
    : id_(report_id),
      program_shortname_(program_shortname),
      annotations_(annotations),
      attachments_(std::move(attachments)),
      snapshot_uuid_(std::move(snapshot_uuid)),
      minidump_(std::move(minidump)),
      is_hourly_report_(is_hourly_report) {}

}  // namespace crash_reports
}  // namespace forensics
