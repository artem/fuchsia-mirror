// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <ctype.h>
#include <errno.h>
#include <fidl/fuchsia.hardware.hrtimer/cpp/fidl.h>
#include <fidl/fuchsia.hardware.hrtimer/cpp/natural_ostream.h>
#include <getopt.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fdio.h>
#include <zircon/status.h>

#include <filesystem>

constexpr char kHrtimerClassDir[] = "/dev/class/hrtimer";

constexpr char kUsageSummary[] = R"""(
High resolution timers driver control.

Usage:
  hrtimer-ctl [-d|--device <device>] -i|--id <id> -e|--event <timer_seconds>
  hrtimer-ctl [-d|--device <device>] -i|--id <id> -r|--runtime
  hrtimer-ctl [-d|--device <device>] -i|--id <id> -s|--stop
  hrtimer-ctl [-d|--device <device>] -i|--id <id> [-k|--keep_alive <keep_alive_seconds>]
              -w|--wait <timer_seconds>
  hrtimer-ctl [-d|--device <device>] -p|--properties
  hrtimer-ctl -h|--help
)""";

constexpr char kUsageDetails[] = R"""(
Options:
  -d, --device      Specifies the device path, either full path specified e.g.
                    /dev/class/hrtimer/1234abcd, or only the devfs name specified e.g. 1234abcd, or
                    unspecified (picks the first device in /dev/class/hrtimer).
                    If specified, must be listed before other options.
  -e, --event       Starts timer for <timer_seconds> and waits for its expiration via event if
                    supported.
  -w, --wait        Starts timer for <timer_seconds> and waits for its expiration if supported.
  -h, --help        Show list of command-line options
  -i, --id          Specifies the timer id. Must be listed before --event, --wait, --runtime and
                    --stop.
  -k, --keep_alive  Specifies an amount of seconds to hold a keep alive lease returned by --wait.
                    Optionally listed before --wait.
  -p, --properties  Retrieves properties of the driver.
  -r, --runtime     Retrieves a timer runtime in ticks.
  -s, --stop        Stops a timer. Note it does not cancel an event wait by --event or --wait.

Examples:

Start timer id 2 for 20 seconds and waits for its expiration via event:
$ hrtimer-ctl --id 2 --event 20
Executing on device /dev/class/hrtimer/9d14a831
Setting event...
Starting timer...
Waiting on event...
Event trigged
$

Start timer id 0 for 5 seconds and waits for its expiration, preventing system suspension
$ hrtimer-ctl --id 0 --keep_alive 15 --wait 5
Executing on device /dev/class/hrtimer/9d14a831
Starting timer...
Timer triggered
Holding keep alive...
Keep alive released
$
)""";

template <typename T>
std::string ToString(const T& value) {
  std::ostringstream buf;
  buf << value;
  return buf.str();
}
template <typename T>
std::string FidlString(const T& value) {
  return ToString(fidl::ostream::Formatted<T>(value));
}

void ShowUsage(bool show_details) {
  std::cout << kUsageSummary;
  if (!show_details) {
    std::cout << std::endl << "Use `hrtimer-ctl --help` to see full help text" << std::endl;
    return;
  }
  std::cout << kUsageDetails;
}

fidl::SyncClient<fuchsia_hardware_hrtimer::Device> GetHrtimerClient(std::string path) {
  if (!path.size()) {
    for (const auto& entry : std::filesystem::directory_iterator(kHrtimerClassDir)) {
      path = entry.path().string();
      break;
    }
  }

  std::cout << "Executing on device " << path << std::endl;
  zx::result connector = component::Connect<fuchsia_hardware_hrtimer::Device>(path.c_str());
  if (connector.is_error()) {
    std::cerr << "could not connect to:" << path << " status:" << connector.status_string();
    return {};
  }

  return fidl::SyncClient<fuchsia_hardware_hrtimer::Device>(std::move(connector.value()));
}

int StartTimer(std::string path, std::optional<uint64_t> id, bool is_wait, int64_t timer_seconds,
               std::optional<uint64_t> keep_alive_seconds) {
  auto client = GetHrtimerClient(path);

  bool supports_event = false;
  bool supports_wait = false;
  uint64_t resolution_nsecs = 0;
  auto result = client->GetProperties();
  if (result.is_error()) {
    std::cerr << "GetProperties failed: " << result.error_value().FormatDescription() << std::endl;
    return -1;
  }
  if (result->properties().timers_properties()) {
    for (auto& i : result->properties().timers_properties().value()) {
      if (i.id() == id) {
        if (i.supports_event() && i.supports_event().value() == true) {
          supports_event = true;
        }
        if (i.supports_wait() && i.supports_wait().value() == true) {
          supports_wait = true;
        }
        if (i.supported_resolutions() && i.supported_resolutions()->size()) {
          // Pick the last resolution, arbitrary.
          resolution_nsecs = i.supported_resolutions()->back().duration().value();
        }
      }
    }
  }

  zx::event event;
  if (!is_wait) {
    if (!supports_event) {
      std::cerr << "Events not supported" << std::endl;
      return -1;
    }
    std::cout << "Setting event..." << std::endl;
    if (zx::event::create(0, &event) != ZX_OK) {
      std::cout << "Could not create event" << std::endl;
      return -1;
    }
    zx::event duplicate_event;
    event.duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate_event);
    auto result_set_event = client->SetEvent({*id, std::move(duplicate_event)});
    if (!result_set_event.is_ok()) {
      std::cerr << "SetEvent failed: " << result_set_event.error_value().FormatDescription()
                << std::endl;
      return -1;
    }
  }
  std::cout << "Starting timer..." << std::endl;
  uint64_t ticks = static_cast<uint64_t>(zx::sec(timer_seconds).to_nsecs() / resolution_nsecs);

  if (is_wait) {
    if (!supports_wait) {
      std::cerr << "Waiting on timer not supported" << std::endl;
      return -1;
    }
    {
      auto result_wait = client->StartAndWait(
          {*id, fuchsia_hardware_hrtimer::Resolution::WithDuration(resolution_nsecs), ticks});
      if (!result_wait.is_ok()) {
        std::cerr << "StartAndWait failed: " << result_wait.error_value().FormatDescription()
                  << std::endl;
        return -1;
      }
      std::cout << "Timer triggered" << std::endl;
      if (keep_alive_seconds) {
        std::cout << "Holding keep alive..." << std::endl;
        zx::nanosleep(zx::deadline_after(zx::sec(*keep_alive_seconds)));
      }
    }
    std::cout << "Keep alive released" << std::endl;  // result_wait's scope ended.
  } else {
    auto result_start = client->Start(
        {*id, fuchsia_hardware_hrtimer::Resolution::WithDuration(resolution_nsecs), ticks});
    if (!result_start.is_ok()) {
      std::cerr << "Start failed: " << result_start.error_value().FormatDescription() << std::endl;
      return -1;
    }
  }
  if (!is_wait) {
    if (supports_event) {
      std::cout << "Waiting on event..." << std::endl;
      zx_signals_t signals;
      event.wait_one(ZX_EVENT_SIGNALED, zx::time::infinite(), &signals);
      std::cout << "Event trigged" << std::endl;
    }
  }
  return 0;
}

int main(int argc, char** argv) {
  std::string path = {};
  std::optional<uint64_t> id;
  std::optional<uint64_t> keep_alive_seconds;

  while (true) {
    static struct option long_options[] = {
        {"help", no_argument, 0, 'h'},        {"device", required_argument, 0, 'd'},
        {"runtime", no_argument, 0, 'r'},     {"stop", no_argument, 0, 's'},
        {"event", required_argument, 0, 'e'}, {"wait", required_argument, 0, 'w'},
        {"id", required_argument, 0, 'i'},    {"keep_alive", required_argument, 0, 'k'},
        {"properties", no_argument, 0, 'p'},  {0, 0, 0, 0}};

    int c = getopt_long(argc, argv, "hd:rse:w:k:i:p", long_options, 0);
    if (c == -1)
      break;

    switch (c) {
      case 'h':
        ShowUsage(true);
        return 0;
        break;
      case 'd': {
        // Allows using only the devfs name, for instance "1234abcd" instead of
        // "/dev/class/hrtimer/1234abcd".
        path = optarg;
        int node_number = -1;
        if (sscanf(path.c_str(), "%x", &node_number) == 1) {
          path = std::string(kHrtimerClassDir) + "/" + path;
        }
      } break;

      case 'i': {
        uint64_t local_id;
        if (sscanf(optarg, "%lu", &local_id) != 1) {
          ShowUsage(false);
          return -1;
        }
        id.emplace(local_id);
      } break;

      case 'k': {
        uint64_t local_keep_alive_seconds;
        if (sscanf(optarg, "%lu", &local_keep_alive_seconds) != 1) {
          ShowUsage(false);
          return -1;
        }
        keep_alive_seconds.emplace(local_keep_alive_seconds);
      } break;

      case 'r': {
        if (!id) {
          break;
        }

        auto result = GetHrtimerClient(path)->GetTicksLeft(*id);
        if (result.is_error()) {
          std::cerr << "GetTicksLeft failed: " << result.error_value().FormatDescription()
                    << std::endl;
          return -1;
        }
        std::cout << FidlString(result->ticks()) << std::endl;
        return 0;
      } break;

      case 's': {
        if (!id) {
          break;
        }

        auto result = GetHrtimerClient(path)->Stop(*id);
        if (result.is_error()) {
          std::cerr << "Stop failed: " << result.error_value().FormatDescription() << std::endl;
          return -1;
        }
        return 0;
      } break;

      case 'e':
        [[fallthrough]];
      case 'w': {
        if (!id) {
          break;
        }

        int64_t timer_seconds = 0;
        if (sscanf(optarg, "%ld", &timer_seconds) != 1) {
          ShowUsage(false);
          return -1;
        }

        return StartTimer(path, id, c == 'w', timer_seconds, keep_alive_seconds);
      }

      case 'p': {
        auto result = GetHrtimerClient(path)->GetProperties();
        if (result.is_error()) {
          std::cerr << "Get properties failed: " << result.error_value().FormatDescription()
                    << std::endl;
          return -1;
        }
        std::cout << FidlString(result->properties()) << std::endl;
        return 0;
      }

      default:
        ShowUsage(false);
        return -1;
    }
  }

  ShowUsage(false);
  return -1;
}
