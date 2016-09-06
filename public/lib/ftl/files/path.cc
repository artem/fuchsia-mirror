// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/ftl/files/path.h"

#include <functional>
#include <memory>
#include <vector>

#include <dirent.h>
#include <errno.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include "lib/ftl/files/directory.h"

#ifdef __Fuchsia__
// TODO(qsr): lstat doesn't exist yet on fuchsia
#define LSTAT stat
#else
#define LSTAT lstat
#endif

namespace files {
namespace {

size_t ResolveParentDirectoryTraversal(const std::string& path, size_t put) {
  if (put >= 2) {
    size_t previous_separator = path.rfind('/', put - 2);
    if (previous_separator != std::string::npos) return previous_separator + 1;
  }
  if (put == 1 && path[0] == '/') {
    return put;
  }
  return 0;
}

void SafeCloseDir(DIR* dir) {
  if (dir) closedir(dir);
}

bool ForEachEntry(const std::string& path,
                  std::function<bool(const std::string& path)> callback) {
  std::unique_ptr<DIR, decltype(&SafeCloseDir)> dir(opendir(path.c_str()),
                                                    SafeCloseDir);
  if (!dir.get()) return false;
  for (struct dirent* entry = readdir(dir.get()); entry != nullptr;
       entry = readdir(dir.get())) {
    char* name = entry->d_name;
    if (name[0]) {
      if (name[0] == '.') {
        if (!name[1] || (name[1] == '.' && !name[2])) {
          // . or ..
          continue;
        }
      }
      if (!callback(path + "/" + name)) return false;
    }
  }
  return true;
}

}  // namespace

std::string SimplifyPath(std::string path) {
  if (path.empty()) return ".";

  size_t put = 0;
  size_t get = 0;
  size_t traversal_root = 0;
  size_t component_start = 0;

  if (path[0] == '/') {
    put = 1;
    get = 1;
    component_start = 1;
  }

  while (get < path.size()) {
    char c = path[get];

    if (c == '.' && (get == component_start || get == component_start + 1)) {
      // We've seen "." or ".." so far in this component. We need to continue
      // searching.
      ++get;
      continue;
    }

    if (c == '/') {
      if (get == component_start || get == component_start + 1) {
        // We've found a "/" or a "./", which we can elide.
        ++get;
        component_start = get;
        continue;
      }
      if (get == component_start + 2) {
        // We've found a "../", which means we need to remove the previous
        // component.
        if (put == traversal_root) {
          path[put++] = '.';
          path[put++] = '.';
          path[put++] = '/';
          traversal_root = put;
        } else {
          put = ResolveParentDirectoryTraversal(path, put);
        }
        ++get;
        component_start = get;
        continue;
      }
    }

    size_t next_separator = path.find('/', get);
    if (next_separator == std::string::npos) {
      // We've reached the last component.
      break;
    }
    size_t next_component_start = next_separator + 1;
    ++next_separator;
    size_t component_size = next_component_start - component_start;
    if (put != component_start && component_size > 0) {
      path.replace(put, component_size,
                   path.substr(component_start, component_size));
    }
    put += component_size;
    get = next_component_start;
    component_start = next_component_start;
  }

  size_t last_component_size = path.size() - component_start;
  if (last_component_size == 1 && path[component_start] == '.') {
    // The last component is ".", which we can elide.
  } else if (last_component_size == 2 && path[component_start] == '.' &&
             path[component_start + 1] == '.') {
    // The last component is "..", which means we need to remove the previous
    // component.
    if (put == traversal_root) {
      path[put++] = '.';
      path[put++] = '.';
      path[put++] = '/';
      traversal_root = put;
    } else {
      put = ResolveParentDirectoryTraversal(path, put);
    }
  } else {
    // Otherwise, we need to copy over the last component.
    if (put != component_start && last_component_size > 0) {
      path.replace(put, last_component_size,
                   path.substr(component_start, last_component_size));
    }
    put += last_component_size;
  }

  if (put >= 2 && path[put - 1] == '/')
    --put;  // Trim trailing /
  else if (put == 0)
    return ".";  // Use . for otherwise empty paths to treat them as relative.

  path.resize(put);
  return path;
}

// Returns the directory name component of the given path.
std::string GetDirectoryName(std::string path) {
  size_t separator = path.rfind('/');
  if (separator == 0u) return "/";
  if (separator == std::string::npos) return std::string();
  return path.substr(0, separator);
}

bool DeletePath(const std::string& path, bool recursive) {
  struct stat stat_buffer;
  if (LSTAT(path.c_str(), &stat_buffer) != 0)
    return (errno == ENOENT || errno == ENOTDIR);
  if (!S_ISDIR(stat_buffer.st_mode)) return (unlink(path.c_str()) == 0);
  if (!recursive) return (rmdir(path.c_str()) == 0);

  std::vector<std::string> directories;
  directories.push_back(path);
  for (size_t index = 0; index < directories.size(); ++index) {
    if (!ForEachEntry(directories[index],
                      [&directories](const std::string& child) {
                        if (IsDirectory(child)) {
                          directories.push_back(child);
                        } else {
                          if (unlink(child.c_str()) != 0) return false;
                        }
                        return true;
                      })) {
      return false;
    }
  }
  for (auto it = directories.rbegin(); it != directories.rend(); ++it) {
    if (rmdir(it->c_str()) != 0) return false;
  }
  return true;
}

}  // namespace files
