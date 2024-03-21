// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <ctype.h>
#include <dirent.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/fit/defer.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <zircon/types.h>

#include <algorithm>
#include <functional>
#include <iomanip>
#include <iostream>
#include <optional>
#include <ostream>
#include <string>
#include <tuple>
#include <utility>
#include <vector>

#include "performance-domain.h"

using ListCb = std::function<void(const char*)>;

constexpr char kCpuDevicePath[] = "/dev/class/cpu-ctrl";
constexpr char kCpuDeviceFormat[] = "/dev/class/cpu-ctrl/%s";
constexpr size_t kMaxPathLen = 24;  // strlen("/dev/class/cpu-ctrl/000\0")

// TODO(gkalsi): Maybe parameterize these?
constexpr uint64_t kDefaultStressTestIterations = 1000;
constexpr uint64_t kDefaultStressTestTimeoutMs =
    100;  // Milliseconds to wait before issuing another dvfs opp.

void print_frequency(const cpuctrl::wire::CpuOperatingPointInfo& info) {
  if (info.frequency_hz == cpuctrl::wire::kFrequencyUnknown) {
    std::cout << "(unknown)";
  } else {
    std::cout << info.frequency_hz << "hz";
  }
}

void print_voltage(const cpuctrl::wire::CpuOperatingPointInfo& info) {
  if (info.voltage_uv == cpuctrl::wire::kVoltageUnknown) {
    std::cout << "(unknown)";
  } else {
    std::cout << info.voltage_uv << "uv";
  }
}

// Print help message to stderr.
void usage(const char* cmd) {
  // Purely aesthetic, but create a buffer of space characters so multiline subtask
  // descriptions can be justified with the preceeding line.
  const size_t kCmdLen = strlen(cmd) + 1;
  const auto spaces_cleanup = std::make_unique<char[]>(kCmdLen);
  char* spaces = spaces_cleanup.get();
  memset(spaces, ' ', kCmdLen);  // Fill buffer with spaces.
  spaces[kCmdLen - 1] = '\0';    // Null terminate.

  // clang-format off
  fprintf(stderr, "\nInteract with the CPU\n");
  fprintf(stderr, "\t%s help                     Print this message and quit.\n", cmd);
  fprintf(stderr, "\t%s list                     List this system's performance domains\n", cmd);
  fprintf(stderr, "\t%s describe [domain]        Describes a given performance domain's operating points\n",
          cmd);
  fprintf(stderr, "\t%s                          describes all domains if `domain` is omitted.\n",
          spaces);

  fprintf(stderr, "\t%s opp <domain> [opp]       Set the CPU's operating point to `opp`. \n",
          cmd);
  fprintf(stderr, "\t%s                          Returns the current opp if `opp` is omitted.\n",
          spaces);

  fprintf(stderr, "\t%s stress [-d domains] [-t timeout] [-c count]\n", cmd);
  fprintf(stderr, "\t%s                          ex: %s stress -d /dev/class/cpu/000,/dev/class/cpu/001 -c 100 -t 10\n", spaces, cmd);
  fprintf(stderr, "\t%s                          Stress test by rapidly and randomly assigning opps.\n", spaces);
  fprintf(stderr, "\t%s                          `domains` is a commas separated list of performance domains to test\n", spaces);
  fprintf(stderr, "\t%s                          If `domains` is omitted, all domains are tested.\n", spaces);
  fprintf(stderr, "\t%s                          `timeout` defines the number of milliseconds to wait before assigning a domain\n", spaces);
  fprintf(stderr, "\t%s                          If `timeout` is omitted, a default value of %lu is used.\n", spaces, kDefaultStressTestTimeoutMs);
  fprintf(stderr, "\t%s                          `count` defines the number of iterations the stress test should run for\n", spaces);
  fprintf(stderr, "\t%s                          If `count` is omitted, a default value of %lu is used.\n", spaces, kDefaultStressTestIterations);
  // clang-format on
}

constexpr int64_t kBadParse = -1;
int64_t parse_positive_long(const char* number) {
  char* end;
  int64_t result = strtol(number, &end, 10);
  if (end == number || *end != '\0' || result < 0) {
    return kBadParse;
  }
  return result;
}

// Call `ListCb cb` with all the names of devices in kCpuDevicePath. Each of
// these devices represent a single performance domain.
zx_status_t list(const ListCb& cb) {
  DIR* dir = opendir(kCpuDevicePath);
  if (!dir) {
    fprintf(stderr, "Failed to open CPU device at '%s'\n", kCpuDevicePath);
    return -1;
  }

  auto cleanup = fit::defer([&dir]() { closedir(dir); });

  struct dirent* de = nullptr;
  while ((de = readdir(dir)) != nullptr) {
    cb(de->d_name);
  }
  return ZX_OK;
}

// Print each performance domain to stdout.
void print_performance_domain(const char* path) {
  // Paths take the form /dev/class/cpu/NNN so we expect the
  // path to be exactly 3 characters long.
  if (strnlen(path, 4) == 3) {
    printf("Domain %s\n", path);
  } else {
    // Why isn't the path 3 characters?
    printf("Domain ???\n");
  }
}

void describe(const char* domain_name) {
  char path[kMaxPathLen];
  snprintf(path, kMaxPathLen, kCpuDeviceFormat, domain_name);

  zx::result domain = CpuPerformanceDomain::CreateFromPath(path);
  if (domain.is_error()) {
    std::cerr << "Failed to connect to performance domain device '" << domain_name << "'"
              << " st = " << domain.status_string() << std::endl;
    return;
  }

  CpuPerformanceDomain& client = domain.value();
  const auto [core_count_status, core_count] = client.GetNumLogicalCores();

  std::cout << "Domain " << domain_name << std::endl;
  if (core_count_status == ZX_OK) {
    std::cout << core_count << " logical cores" << std::endl;
  }

  const auto [status, opps] = client.GetOperatingPoints();
  if (status != ZX_OK) {
    std::cerr << "Failed to get operating points, st = " << status << std::endl;
    return;
  }

  for (size_t i = 0; i < opps.size(); i++) {
    std::cout << " + opps: " << i << std::endl;

    std::cout << "   - freq: ";
    print_frequency(opps[i]);
    std::cout << std::endl;

    std::cout << "   - volt: ";
    print_voltage(opps[i]);
    std::cout << std::endl;
  }
}

void set_current_operating_point(const char* domain_name, const char* opp) {
  char path[kMaxPathLen];
  snprintf(path, kMaxPathLen, kCpuDeviceFormat, domain_name);

  zx::result domain = CpuPerformanceDomain::CreateFromPath(path);
  if (domain.is_error()) {
    std::cerr << "Failed to connect to performance domain device '" << domain_name << "'"
              << " st = " << domain.status_string() << std::endl;
    return;
  }
  CpuPerformanceDomain& client = domain.value();

  const auto [opp_count_status, opp_count] = client.GetOperatingPointCount();

  if (opp_count_status != ZX_OK) {
    std::cerr << "Failed to get operating point counts, st = " << opp_count_status << std::endl;
    return;
  }

  int64_t desired_opp_l = parse_positive_long(opp);
  if (desired_opp_l < 0 || desired_opp_l > opp_count) {
    std::cerr << "Bad opp '%s', must be a positive integer between 0 and %u\n" << opp << opp_count;
    return;
  }

  uint32_t desired_opp = static_cast<uint32_t>(desired_opp_l);

  zx_status_t status = client.SetCurrentOperatingPoint(static_cast<uint32_t>(desired_opp));
  if (status != ZX_OK) {
    std::cerr << "Failed to set operating point, st = " << status << std::endl;
    return;
  }

  std::cout << "PD: " << domain_name << " set opp to " << desired_opp << std::endl;

  const auto [st, opps] = client.GetOperatingPoints();
  if (st != ZX_OK) {
    std::cerr << "Failed to get operating points, st = " << st << std::endl;
    return;
  }

  if (desired_opp < opps.size()) {
    std::cout << "freq: ";
    print_frequency(opps[desired_opp]);
    std::cout << " ";

    std::cout << "volt: ";
    print_voltage(opps[desired_opp]);
    std::cout << std::endl;
  }
}

void get_current_operating_point(const char* domain_name) {
  char path[kMaxPathLen];
  snprintf(path, kMaxPathLen, kCpuDeviceFormat, domain_name);

  zx::result domain = CpuPerformanceDomain::CreateFromPath(path);
  if (domain.is_error()) {
    std::cerr << "Failed to connect to performance domain device '" << domain_name << "'"
              << " st = " << domain.status_string() << std::endl;
    return;
  }

  CpuPerformanceDomain& client = domain.value();
  const auto [status, ps_index, opp] = client.GetCurrentOperatingPoint();

  if (status != ZX_OK) {
    std::cout << "Failed to get current operating point, st = " << status << std::endl;
    return;
  }

  std::cout << "Current opp = " << ps_index << std::endl;
  std::cout << "  Frequency: ";
  print_frequency(opp);
  std::cout << std::endl;
  std::cout << "    Voltage: ";
  print_voltage(opp);
  std::cout << std::endl;
}

zx_status_t describe_all() {
  list(describe);
  return ZX_OK;
}

void stress(std::vector<std::string> names, const size_t iterations, const uint64_t timeout) {
  // Default is all domains.
  if (names.empty()) {
    list([&names](const char* path) { names.push_back(path); });
  }

  std::vector<CpuPerformanceDomain> domains;
  for (const auto& name : names) {
    std::string device_path = std::string(kCpuDevicePath) + std::string("/") + name;

    zx::result domain = CpuPerformanceDomain::CreateFromPath(device_path);
    if (domain.is_error()) {
      std::cerr << "Failed to connect to performance domain device '" << name << "'"
                << " st = " << domain.status_string() << std::endl;
      continue;
    }

    domains.push_back(std::move(domain.value()));
  }

  // Put things back the way they were before the test started.
  std::vector<fit::deferred_action<std::function<void(void)>>> autoreset;
  for (auto& domain : domains) {
    const auto current_opp = domain.GetCurrentOperatingPoint();
    if (std::get<0>(current_opp) != ZX_OK) {
      std::cerr << "Could not get initial opp for domain, won't reset when finished" << std::endl;
      continue;
    }

    autoreset.emplace_back([&domain, current_opp]() {
      uint32_t opp = static_cast<uint32_t>(std::get<1>(current_opp));
      zx_status_t st = domain.SetCurrentOperatingPoint(opp);
      if (st != ZX_OK) {
        std::cerr << "Failed to reset initial opp" << std::endl;
      }
    });
  }

  std::cout << "Stress testing " << domains.size() << " domain[s]." << std::endl;

  for (size_t i = 0; i < iterations; i++) {
    // Pick a random domain.
    const size_t selected_domain_idx = rand() % domains.size();

    // Pick a random operating point for this domain.
    auto& selected_domain = domains[selected_domain_idx];
    const auto [opp_count_status, opp_count] = selected_domain.GetOperatingPointCount();
    if (opp_count_status != ZX_OK) {
      std::cerr << "Failed to get operating point counts, st = " << opp_count_status << std::endl;
      return;
    }
    const uint32_t selected_opp = rand() % opp_count;
    zx_status_t status = selected_domain.SetCurrentOperatingPoint(selected_opp);
    if (status != ZX_OK) {
      std::cout << "Stress test failed to drive domain " << selected_domain_idx << " into opp "
                << selected_opp << std::endl;
      return;
    }

    if ((i % 10) == 0) {
      std::cout << "[" << std::setw(4) << i << "/" << std::setw(4) << iterations << "] "
                << "Stress tests completed." << std::endl;
    }

    zx_nanosleep(zx_deadline_after(ZX_MSEC(timeout)));
  }
}

char* get_option(char* argv[], const int argc, const std::string& option) {
  char** end = argv + argc;
  char** res = std::find(argv, end, option);

  if (res == end || (res + 1) == end) {
    return nullptr;
  }

  return *(res + 1);
}

int main(int argc, char* argv[]) {
  const char* cmd = argv[0];
  if (argc == 1) {
    usage(argv[0]);
    return -1;
  }

  const char* subcmd = argv[1];

  if (!strncmp(subcmd, "help", 4)) {
    usage(cmd);
    return 0;
  }
  if (!strncmp(subcmd, "list", 4)) {
    return list(print_performance_domain) == ZX_OK ? 0 : -1;
  }
  if (!strncmp(subcmd, "describe", 8)) {
    if (argc >= 3) {
      describe(argv[2]);
      return 0;
    }
    return describe_all() == ZX_OK ? 0 : -1;
  }
  if (!strncmp(subcmd, "opp", 6)) {
    if (argc == 4) {
      set_current_operating_point(argv[2], argv[3]);
    } else if (argc == 3) {
      get_current_operating_point(argv[2]);
    } else {
      fprintf(stderr, "opp <domain> [opp]\n");
      usage(cmd);
      return -1;
    }
  } else if (!strncmp(subcmd, "stress", 6)) {
    char* timeout_c = get_option(argv, argc, std::string("-t"));
    char* iterations_c = get_option(argv, argc, std::string("-c"));
    char* domains_c = get_option(argv, argc, std::string("-d"));

    int64_t timeout = kDefaultStressTestTimeoutMs;
    int64_t iterations = kDefaultStressTestIterations;

    if (timeout_c != nullptr) {
      timeout = parse_positive_long(timeout_c);
      if (timeout < 0) {
        fprintf(stderr, "'timeout' argument must be a positive integer");
        usage(cmd);
        return -1;
      }
    }

    if (iterations_c != nullptr) {
      iterations = parse_positive_long(iterations_c);
      if (iterations < 0) {
        fprintf(stderr, "'iterations' argument must be a positive integer");
        usage(cmd);
        return -1;
      }
    }

    std::vector<std::string> domains;
    if (domains_c != nullptr) {
      char* token = strtok(domains_c, ",");
      do {
        domains.push_back(token);
      } while ((token = strtok(nullptr, ",")));
    }

    stress(domains, iterations, timeout);
  }

  return 0;
}
