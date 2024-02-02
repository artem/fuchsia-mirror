// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/cpuperf_provider/importer.h"

#include <assert.h>
#include <inttypes.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <zircon/syscalls.h>

#include <atomic>
#include <iterator>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/performance/cpuperf_provider/categories.h"
#include "src/performance/lib/perfmon/reader.h"
#include "src/performance/lib/perfmon/writer.h"

namespace cpuperf_provider {

// Mock process for cpus. The infrastructure only supports processes and
// threads.
constexpr zx_koid_t kCpuProcess = 1u;

Importer::Importer(trace_context_t* context, const TraceConfig* trace_config,
                   trace_ticks_t start_time, trace_ticks_t stop_time)
#define MAKE_STRING(literal) trace_context_make_registered_string_literal(context, literal)
    : context_(context),
      trace_config_(trace_config),
      start_time_(start_time),
      stop_time_(stop_time),
      cpu_string_ref_(MAKE_STRING("cpu")),
      cpuperf_category_ref_(MAKE_STRING("cpu:perf")),
      count_name_ref_(MAKE_STRING("count")),
      value_name_ref_(MAKE_STRING("value")),
      rate_name_ref_(MAKE_STRING("rate")),
      aspace_name_ref_(MAKE_STRING("aspace")),
      pc_name_ref_(MAKE_STRING("pc")) {
  for (unsigned cpu = 0; cpu < std::size(cpu_thread_refs_); ++cpu) {
    // +1 because index thread refs start at 1
    trace_thread_index_t index = cpu + 1;
    cpu_thread_refs_[cpu] = trace_make_indexed_thread_ref(index);
    // Note: Thread ids of zero are invalid. We use "thread 0" (aka cpu 0)
    // for system-wide events.
    trace_context_write_thread_record(context, index, kCpuProcess, cpu);
    if (cpu == 0) {
      cpu_name_refs_[0] = trace_context_make_registered_string_literal(context, "system");
    } else {
      std::string name = fxl::StringPrintf("cpu%u", cpu - 1);
      cpu_name_refs_[cpu] =
          trace_context_make_registered_string_copy(context, name.c_str(), name.size());
    }
    // TODO(dje): In time emit "cpuN" for thread names, but it won't do any
    // good at the moment as we use "Count" records which ignore the thread.
  }
#undef MAKE_STRING
}

Importer::~Importer() = default;

bool Importer::Import(perfmon::Reader& reader, const perfmon::Config& perfmon_config) {
  trace_context_write_process_info_record(context_, kCpuProcess, &cpu_string_ref_);

  auto start = zx::clock::get_monotonic();

  uint32_t record_count = ImportRecords(reader, perfmon_config);

  FX_LOGS(INFO) << "Import of " << record_count << " cpu perf records took "
                << (zx::clock::get_monotonic() - start).to_usecs() << " us";

  return true;
}

uint64_t Importer::ImportRecords(perfmon::Reader& reader, const perfmon::Config& perfmon_config) {
  EventTracker event_data(start_time_);
  uint32_t record_count = 0;
  // Only print these warnings once, and then again at the end with
  // the total count. We don't want a broken trace to flood the screen
  // with messages.
  uint32_t printed_zero_period_warning_count = 0;
  uint32_t printed_old_time_warning_count = 0;
  uint32_t printed_late_record_warning_count = 0;

  uint32_t cpu;
  perfmon::SampleRecord record;

  uint64_t sample_rate = trace_config_->sample_rate();
  bool is_tally_mode = sample_rate == 0;
  trace_ticks_t current_time = reader.time();

  while (reader.ReadNextRecord(&cpu, &record) == perfmon::ReaderStatus::kOk) {
    FX_DCHECK(cpu < kMaxNumCpus);
    perfmon::EventId event_id = record.event();
    uint64_t ticks_per_second = reader.ticks_per_second();

    // There can be millions of records. This log message is useful for small
    // test runs, but otherwise is too painful. The verbosity level is chosen
    // to recognize that.
    FX_LOGS(DEBUG) << fxl::StringPrintf("Import: cpu=%u, event=0x%x, time=%" PRIu64, cpu, event_id,
                                        current_time);

    if (record.type() == perfmon::kRecordTypeTime) {
      current_time = reader.time();
      if (event_id == perfmon::kEventIdNone) {
        // This is just a time update, not a combined time+tick record.
        ++record_count;
        continue;
      }
    }

    // Get the time we last saw this event.
    trace_ticks_t prev_time = event_data.GetTime(cpu, event_id);

    if (current_time < prev_time) {
      if (printed_old_time_warning_count == 0) {
        FX_LOGS(WARNING) << "cpu " << cpu << ": record time " << current_time << " < previous time "
                         << prev_time << " (further such warnings are omitted)";
      }
      ++printed_old_time_warning_count;
    } else if (current_time == prev_time) {
      if (printed_zero_period_warning_count == 0) {
        FX_LOGS(WARNING) << "cpu " << cpu << ": empty interval at time " << current_time
                         << " (further such warnings are omitted)";
      }
      ++printed_zero_period_warning_count;
    } else if (current_time > stop_time_) {
      if (printed_late_record_warning_count == 0) {
        FX_LOGS(WARNING) << "Record has time > stop_time: " << current_time
                         << " (further such warnings are omitted)";
      }
      ++printed_late_record_warning_count;
    } else {
      switch (record.type()) {
        case perfmon::kRecordTypeTime:
          FX_DCHECK(event_id != perfmon::kEventIdNone);
          __FALLTHROUGH;
        case perfmon::kRecordTypeTick:
          if (is_tally_mode) {
            event_data.AccumulateCount(cpu, event_id, sample_rate);
          } else {
            ImportSampleRecord(cpu, record, prev_time, current_time, ticks_per_second, sample_rate);
          }
          break;
        case perfmon::kRecordTypeCount:
          if (is_tally_mode) {
            event_data.AccumulateCount(cpu, event_id, record.count->count);
          } else {
            ImportSampleRecord(cpu, record, prev_time, current_time, ticks_per_second,
                               record.count->count);
          }
          break;
        case perfmon::kRecordTypeValue:
          if (is_tally_mode) {
            event_data.UpdateValue(cpu, event_id, record.value->value);
          } else {
            ImportSampleRecord(cpu, record, prev_time, current_time, ticks_per_second,
                               record.value->value);
          }
          break;
        case perfmon::kRecordTypePc:
          if (!is_tally_mode) {
            ImportSampleRecord(cpu, record, prev_time, current_time, ticks_per_second, sample_rate);
          }
          break;
        case perfmon::kRecordTypeLastBranch:
          if (!is_tally_mode) {
            EmitLastBranchRecordBlob(cpu, record, current_time);
          }
          break;
        default:
          // The reader shouldn't be returning unknown records.
          FX_NOTREACHED();
          break;
      }
    }

    event_data.UpdateTime(cpu, event_id, current_time);
    ++record_count;
  }

  if (is_tally_mode) {
    EmitTallyCounts(reader, perfmon_config, event_data);
  }

  if (printed_old_time_warning_count > 0) {
    FX_LOGS(WARNING) << printed_old_time_warning_count
                     << " total occurrences of records going back in time";
  }
  if (printed_zero_period_warning_count > 0) {
    FX_LOGS(WARNING) << printed_zero_period_warning_count
                     << " total occurrences of records with an empty interval";
  }
  if (printed_late_record_warning_count > 0) {
    FX_LOGS(WARNING) << printed_late_record_warning_count
                     << " total occurrences of records with late times";
  }

  return record_count;
}

void Importer::ImportSampleRecord(trace_cpu_number_t cpu, const perfmon::SampleRecord& record,
                                  trace_ticks_t previous_time, trace_ticks_t current_time,
                                  uint64_t ticks_per_second, uint64_t event_value) {
  perfmon::EventId event_id = record.event();
  const perfmon::EventDetails* details;
  // Note: Errors here are generally rare, so at present we don't get clever
  // with minimizing the noise.
  if (trace_config_->model_event_manager()->EventIdToEventDetails(event_id, &details)) {
    EmitSampleRecord(cpu, details, record, previous_time, current_time, ticks_per_second,
                     event_value);
  } else {
    FX_LOGS(ERROR) << "Invalid event id: " << event_id;
  }
}

void Importer::EmitSampleRecord(trace_cpu_number_t cpu, const perfmon::EventDetails* details,
                                const perfmon::SampleRecord& record, trace_ticks_t start_time,
                                trace_ticks_t end_time, uint64_t ticks_per_second, uint64_t value) {
  FX_DCHECK(start_time < end_time);
  trace_thread_ref_t thread_ref{GetCpuThreadRef(cpu, details->id)};
  trace_string_ref_t name_ref{
      trace_context_make_registered_string_literal(context_, details->name)};
#if 0
  // Count records are "process wide" so we need some way to distinguish
  // each cpu. Thus while it might be nice to use this for "id" we don't.
  uint64_t id = record.event();
#else
  // Add one as zero doesn't get printed.
  uint64_t id = cpu + 1;
#endif

  // While the count of events is cumulative, it's more useful to report some
  // measure that's useful within each time period. E.g., a rate.
  uint64_t interval_ticks = end_time - start_time;
  FX_DCHECK(interval_ticks > 0);
  double rate_per_second = 0;
  // rate_per_second = value / (interval_ticks / ticks_per_second)
  // ticks_per_second could be zero if there's bad data in the buffer.
  // Don't crash because of it. If it's zero just punt and compute the rate
  // per tick.
  // TODO(dje): Perhaps the rate calculation should be done in the report
  // generator, but it's done this way so that catapult reports in chrome
  // are usable. Maybe add a new phase type to the catapult format?
  rate_per_second = static_cast<double>(value) / interval_ticks;
  if (ticks_per_second != 0) {
    rate_per_second *= ticks_per_second;
  }

  trace_arg_t args[3];
  size_t n_args;
  switch (record.type()) {
    case perfmon::kRecordTypeTick:
      args[0] = {trace_make_arg(rate_name_ref_, trace_make_double_arg_value(rate_per_second))};
      n_args = 1;
      break;
    case perfmon::kRecordTypeCount:
      args[0] = {trace_make_arg(rate_name_ref_, trace_make_double_arg_value(rate_per_second))};
      n_args = 1;
      break;
    case perfmon::kRecordTypeValue:
      // We somehow need to mark the value as not being a count. This is
      // important for some consumers to guide how to print the value.
      // Do this by using a different name for the value.
      args[0] = {trace_make_arg(value_name_ref_, trace_make_uint64_arg_value(value))};
      n_args = 1;
      break;
    case perfmon::kRecordTypePc:
      args[1] = {trace_make_arg(aspace_name_ref_, trace_make_uint64_arg_value(record.pc->aspace))};
      args[2] = {trace_make_arg(pc_name_ref_, trace_make_uint64_arg_value(record.pc->pc))};
      n_args = 3;
      break;
    default:
      FX_NOTREACHED();
      return;
  }

#if 0
  // TODO(dje): This is a failed experiment to use something other than
  // counters. It is kept, for now, to allow easy further experimentation.
  trace_context_write_async_begin_event_record(
      context_, start_time,
      &thread_ref, &cpuperf_category_ref_, &name_ref,
      id, nullptr, 0);
  trace_context_write_async_end_event_record(
      context_, end_time,
      &thread_ref, &cpuperf_category_ref_, &name_ref,
      id, &args[0], n_args);
#else
  // Chrome interprets the timestamp we give it as the start of the
  // interval, which for a count makes sense: this is the value of the count
  // from this point on until the next count record. We're abusing this record
  // type to display a rate.
  trace_context_write_counter_event_record(context_, start_time, &thread_ref,
                                           &cpuperf_category_ref_, &name_ref, id, &args[0], n_args);
#endif
}

void Importer::EmitLastBranchRecordBlob(trace_cpu_number_t cpu, const perfmon::SampleRecord& record,
                                        trace_ticks_t time) {
  // Use the cpu's name as the blob's name.
  auto cpu_name_ref{GetCpuNameRef(cpu)};
  uint16_t num_branches = record.last_branch->num_branches;
  size_t size = perfmon::LastBranchRecordBlobSize(num_branches);
  void* ptr = trace_context_begin_write_blob_record(context_, TRACE_BLOB_TYPE_LAST_BRANCH,
                                                    &cpu_name_ref, size);
  if (ptr) {
    auto rec = reinterpret_cast<perfmon::LastBranchRecordBlob*>(ptr);
    rec->cpu = cpu;
    rec->num_branches = record.last_branch->num_branches;
    rec->reserved = 0;
    rec->event_time = time;
    rec->aspace = record.last_branch->aspace;
    static_assert(sizeof(rec->branches[0]) == sizeof(record.last_branch->branches[0]), "");
    memcpy(&rec->branches[0], &record.last_branch->branches[0],
           num_branches * sizeof(record.last_branch->branches[0]));
  }
}

// Chrome interprets the timestamp we give Count records as the start
// of the interval with that count, which for a count makes sense: this is
// the value of the count from this point on until the next count record.
// But if we emit a value of zero at the start (or don't emit any initial
// value at all) Chrome shows the entire trace of having the value zero and
// the count record at the end of the interval is very hard to see.
// OTOH the data is correct, it's just the display that's hard to read.
// Text display of the results is unaffected.
// One important reason for providing a value at the start is because there's
// currently no other way to communicate the start time of the trace in a
// json output file, and thus there would otherwise be no way for the
// report printer to know the duration over which the count was collected.
void Importer::EmitTallyCounts(perfmon::Reader& reader, const perfmon::Config& perfmon_config,
                               const EventTracker& event_data) {
  unsigned num_cpus = zx_system_get_num_cpus();

  for (unsigned cpu = 0; cpu < num_cpus; ++cpu) {
    perfmon_config.IterateOverEvents(
        [this, &event_data, &cpu](const perfmon::Config::EventConfig& event) {
          perfmon::EventId event_id = event.event;
          if (event_data.HaveValue(cpu, event_id)) {
            uint64_t value = event_data.GetCountOrValue(cpu, event_id);
            if (event_data.IsValue(cpu, event_id)) {
              EmitTallyRecord(cpu, event_id, stop_time_, true, value);
            } else {
              EmitTallyRecord(cpu, event_id, start_time_, false, 0);
              EmitTallyRecord(cpu, event_id, stop_time_, false, value);
            }
          }
        });
  }
}

void Importer::EmitTallyRecord(trace_cpu_number_t cpu, perfmon::EventId event_id,
                               trace_ticks_t time, bool is_value, uint64_t value) {
  trace_thread_ref_t thread_ref{GetCpuThreadRef(cpu, event_id)};
  trace_arg_t args[1] = {
      {trace_make_arg(is_value ? value_name_ref_ : count_name_ref_,
                      trace_make_uint64_arg_value(value))},
  };
  const perfmon::EventDetails* details;
  if (trace_config_->model_event_manager()->EventIdToEventDetails(event_id, &details)) {
    trace_string_ref_t name_ref{
        trace_context_make_registered_string_literal(context_, details->name)};
    trace_context_write_counter_event_record(context_, time, &thread_ref, &cpuperf_category_ref_,
                                             &name_ref, event_id, &args[0], std::size(args));
  } else {
    FX_LOGS(WARNING) << "Invalid event id: " << event_id;
  }
}

trace_string_ref_t Importer::GetCpuNameRef(trace_cpu_number_t cpu) {
  FX_DCHECK(cpu < std::size(cpu_name_refs_));
  return cpu_name_refs_[cpu + 1];
}

trace_thread_ref_t Importer::GetCpuThreadRef(trace_cpu_number_t cpu, perfmon::EventId id) {
  FX_DCHECK(cpu < std::size(cpu_thread_refs_));
  // TODO(dje): Misc events are currently all system-wide, not attached
  // to any specific cpu. That won't always be the case.
  if (perfmon::GetEventIdGroup(id) == perfmon::kGroupMisc)
    cpu = 0;
  else
    ++cpu;
  return cpu_thread_refs_[cpu];
}

}  // namespace cpuperf_provider
