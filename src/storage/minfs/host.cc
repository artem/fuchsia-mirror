// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// for S_IF*
#define _XOPEN_SOURCE
#include "src/storage/minfs/host.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <lib/syslog/cpp/macros.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/assert.h>

#include <limits>
#include <memory>
#include <string_view>
#include <type_traits>
#include <utility>

#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/minfs/format.h"
#include "src/storage/minfs/minfs.h"
#include "src/storage/minfs/runner.h"

namespace {

zx_status_t do_stat(const fbl::RefPtr<fs::Vnode>& vn, struct stat* s) {
  fs::VnodeAttributes a;
  zx_status_t status = vn->GetAttributes(&a);
  if (status == ZX_OK) {
    memset(s, 0, sizeof(struct stat));
    s->st_mode = static_cast<mode_t>(a.mode);
    s->st_size = static_cast<off_t>(a.content_size);
    s->st_ino = a.inode;
    s->st_ctime = static_cast<time_t>(a.creation_time);
    s->st_mtime = static_cast<time_t>(a.modification_time);
  }
  return status;
}

struct HostFile {
  fbl::RefPtr<fs::Vnode> vn;
  uint64_t off = 0;
  fs::VdirCookie dircookie;
};

constexpr int kMaxFd = 64;

static HostFile fdtab[kMaxFd];

constexpr int kFdMagic = 0x45AB0000;

HostFile* file_get(int fd) {
  if ((fd & 0xFFFF0000) != kFdMagic) {
    return nullptr;
  }
  fd &= 0x0000FFFF;
  if ((fd < 0) || (fd >= kMaxFd)) {
    return nullptr;
  }
  if (fdtab[fd].vn == nullptr) {
    return nullptr;
  }
  return fdtab + fd;
}

int status_to_errno(zx_status_t status) {
  switch (status) {
    case ZX_OK:
      return 0;
    case ZX_ERR_FILE_BIG:
      return EFBIG;
    case ZX_ERR_NO_SPACE:
      return ENOSPC;
    case ZX_ERR_ALREADY_EXISTS:
      return EEXIST;
    default:
      return EIO;
  }
}

#define FAIL(err)          \
  do {                     \
    errno = (err);         \
    return errno ? -1 : 0; \
  } while (0)
#define STATUS(status) FAIL(status_to_errno(status))

// Ensure the order of these global destructors are ordered.
// TODO(planders): Host-side tools should avoid using globals.
struct FakeFs {
  std::unique_ptr<fs::Vfs> fake_vfs = nullptr;
  fbl::RefPtr<minfs::VnodeMinfs> fake_root = nullptr;  // Must be destroyed before fake_vfs.
} fake_fs;

}  // namespace

int emu_mkfs(const char* path) {
  fbl::unique_fd fd(open(path, O_RDWR));
  if (!fd) {
    FX_LOGS(ERROR) << "error: could not open path " << path;
    return -1;
  }

  struct stat s;
  if (fstat(fd.get(), &s) < 0) {
    FX_LOGS(ERROR) << "error: minfs could not find end of file/device";
    return -1;
  }

  off_t size = s.st_size / minfs::kMinfsBlockSize;

  auto bc_or = minfs::Bcache::Create(std::move(fd), static_cast<uint32_t>(size));
  if (bc_or.is_error()) {
    FX_LOGS(ERROR) << "error: cannot create block cache: " << bc_or.error_value();
    return -1;
  }

  return Mkfs(bc_or.value().get()).status_value();
}

int emu_mount_bcache(std::unique_ptr<minfs::Bcache> bc) {
  auto fs = minfs::Runner::Create(nullptr, std::move(bc), minfs::MountOptions());
  if (fs.is_error()) {
    return -1;
  }
  auto root = fs->minfs().OpenRootNode();
  if (root.is_error()) {
    return -1;
  }
  fake_fs.fake_root = std::move(root).value();
  fake_fs.fake_vfs = std::move(fs).value();
  return 0;
}

int emu_create_bcache(const char* path, std::unique_ptr<minfs::Bcache>* out_bc) {
  fbl::unique_fd fd(open(path, O_RDWR));
  if (!fd) {
    FX_LOGS(ERROR) << "error: could not open path " << path;
    return -1;
  }

  struct stat s;
  if (fstat(fd.get(), &s) < 0) {
    FX_LOGS(ERROR) << "error: minfs could not find end of file/device";
    return 0;
  }

  off_t size = s.st_size / minfs::kMinfsBlockSize;

  auto bc_or = minfs::Bcache::Create(std::move(fd), static_cast<uint32_t>(size));
  if (bc_or.is_error()) {
    FX_LOGS(ERROR) << "error: cannot create block cache: " << bc_or.status_value();
    return -1;
  }

  *out_bc = std::move(bc_or.value());
  return 0;
}

int emu_mount(const char* path) {
  std::unique_ptr<minfs::Bcache> bc;
  if (emu_create_bcache(path, &bc) != 0) {
    return -1;
  }
  return emu_mount_bcache(std::move(bc));
}

int emu_get_used_resources(const char* path, uint64_t* out_data_size, uint64_t* out_inodes,
                           uint64_t* out_used_size) {
  std::unique_ptr<minfs::Bcache> bc;
  if (emu_create_bcache(path, &bc) != 0) {
    return -1;
  }

  auto data_size_or = minfs::UsedDataSize(bc);
  if (data_size_or.is_error()) {
    return -1;
  }

  auto inodes_or = minfs::UsedInodes(bc);
  if (inodes_or.is_error()) {
    return -1;
  }

  auto used_size_or = minfs::UsedSize(bc);
  if (used_size_or.is_error()) {
    return -1;
  }

  *out_data_size = data_size_or.value();
  *out_inodes = inodes_or.value();
  *out_used_size = used_size_or.value();

  return 0;
}

bool emu_is_mounted() { return fake_fs.fake_root != nullptr; }

// Converts POSIX open() flags to |VnodeConnectionOptions|.
fs::VnodeConnectionOptions fdio_flags_to_connection_options(uint32_t flags) {
  fs::VnodeConnectionOptions options;

  switch (flags & O_ACCMODE) {
    case O_RDONLY:
      options.rights |= fuchsia_io::kRStarDir;
      break;
    case O_WRONLY:
      options.rights |= fuchsia_io::kWStarDir;
      break;
    case O_RDWR:
      options.rights |= fuchsia_io::kRwStarDir;
      break;
  }
#ifdef O_PATH
  if (flags & O_PATH) {
    options.flags |= fuchsia_io::OpenFlags::kNodeReference;
  }
#endif
#ifdef O_DIRECTORY
  if (flags & O_DIRECTORY) {
    options.flags |= fuchsia_io::OpenFlags::kDirectory;
  }
#endif
  if (flags & O_CREAT) {
    options.flags |= fuchsia_io::OpenFlags::kCreate;
  }
  if (flags & O_EXCL) {
    options.flags |= fuchsia_io::OpenFlags::kCreateIfAbsent;
  }
  if (flags & O_TRUNC) {
    options.flags |= fuchsia_io::OpenFlags::kTruncate;
  }
  if (flags & O_APPEND) {
    options.flags |= fuchsia_io::OpenFlags::kAppend;
  }

  return options;
}

int emu_open(const char* path, int flags) {
  // TODO: fdtab lock
  ZX_DEBUG_ASSERT_MSG(!host_path(path), "'emu_' functions can only operate on target paths");
  if (flags & O_APPEND) {
    errno = ENOTSUP;
    return -1;
  }
  for (int fd = 0; fd < kMaxFd; fd++) {
    if (fdtab[fd].vn == nullptr) {
      std::string_view str(path + PREFIX_SIZE);
      fs::VnodeConnectionOptions options = fdio_flags_to_connection_options(flags);
      auto result =
          fake_fs.fake_vfs->Open(fake_fs.fake_root, str, options, fs::Rights::ReadWrite());
      if (result.is_error()) {
        STATUS(result.error());
      }
      fdtab[fd].vn = fbl::RefPtr<fs::Vnode>::Downcast(result.ok().vnode);
      return fd | kFdMagic;
    }
  }
  FAIL(EMFILE);
}

int emu_close(int fd) {
  // TODO: fdtab lock
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }
  f->vn->Close();
  f->vn.reset();
  f->off = 0;
  f->dircookie = fs::VdirCookie();
  return 0;
}

ssize_t emu_write(int fd, const void* buf, size_t count) {
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }
  size_t actual = 0;
  zx_status_t status = f->vn->Write(buf, count, f->off, &actual);
  if (status == ZX_OK) {
    f->off += actual;
    ZX_DEBUG_ASSERT(actual <= std::numeric_limits<ssize_t>::max());
    return static_cast<ssize_t>(actual);
  }

  ZX_DEBUG_ASSERT(status < 0);
  STATUS(status);
}

ssize_t emu_pwrite(int fd, const void* buf, size_t count, off_t off) {
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }
  size_t actual;
  zx_status_t status = f->vn->Write(buf, count, off, &actual);
  if (status == ZX_OK) {
    ZX_DEBUG_ASSERT(actual <= std::numeric_limits<ssize_t>::max());
    return static_cast<ssize_t>(actual);
  }

  ZX_DEBUG_ASSERT(status < 0);
  STATUS(status);
}

ssize_t emu_read(int fd, void* buf, size_t count) {
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }
  size_t actual = 0;
  zx_status_t status = f->vn->Read(buf, count, f->off, &actual);
  if (status == ZX_OK) {
    f->off += actual;
    ZX_DEBUG_ASSERT(actual <= std::numeric_limits<ssize_t>::max());
    return static_cast<ssize_t>(actual);
  }
  ZX_DEBUG_ASSERT(status < 0);
  STATUS(status);
}

ssize_t emu_pread(int fd, void* buf, size_t count, off_t off) {
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }
  size_t actual;
  zx_status_t status = f->vn->Read(buf, count, off, &actual);
  if (status == ZX_OK) {
    ZX_DEBUG_ASSERT(actual <= std::numeric_limits<ssize_t>::max());
    return static_cast<ssize_t>(actual);
  }
  ZX_DEBUG_ASSERT(status < 0);
  STATUS(status);
}

int emu_ftruncate(int fd, off_t len) {
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }
  int r = f->vn->Truncate(len);
  return r < 0 ? -1 : r;
}

off_t emu_lseek(int fd, off_t offset, int whence) {
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }

  uint64_t old = f->off;

  switch (whence) {
    case SEEK_SET: {
      if (offset < 0) {
        FAIL(EINVAL);
      }
      f->off = offset;
      break;
    }
    case SEEK_END: {
      fs::VnodeAttributes a;
      if (f->vn->GetAttributes(&a)) {
        FAIL(EINVAL);
      }
      old = a.content_size;
      __FALLTHROUGH;
    }
    case SEEK_CUR: {
      uint64_t n = old + offset;
      if (offset < 0) {
        if (n >= old) {
          FAIL(EINVAL);
        }
      } else {
        if (n < old) {
          FAIL(EINVAL);
        }
      }
      f->off = n;
      break;
    }
    default: {
      FAIL(EINVAL);
    }
  }
  return static_cast<off_t>(f->off);
}

int emu_fstat(int fd, struct stat* s) {
  HostFile* f = file_get(fd);
  if (f == nullptr) {
    return -1;
  }
  STATUS(do_stat(f->vn, s));
}

int emu_stat(const char* fn, struct stat* s) {
  ZX_DEBUG_ASSERT_MSG(!host_path(fn), "'emu_' functions can only operate on target paths");
  fbl::RefPtr<fs::Vnode> vn = fake_fs.fake_root;
  fbl::RefPtr<fs::Vnode> cur = fake_fs.fake_root;

  const char* nextpath = nullptr;

  fn += PREFIX_SIZE;
  do {
    while (fn[0] == '/') {
      fn++;
    }
    if (fn[0] == 0) {
      break;
    }
    size_t len = strlen(fn);
    nextpath = strchr(fn, '/');
    if (nextpath != nullptr) {
      len = nextpath - fn;
      nextpath++;
    }
    fbl::RefPtr<fs::Vnode> vn_fs;
    zx_status_t status = cur->Lookup(std::string_view(fn, len), &vn_fs);
    if (status != ZX_OK) {
      FAIL(ENOENT);
    }
    vn = fbl::RefPtr<fs::Vnode>::Downcast(vn_fs);
    cur = vn;
    fn = nextpath;
  } while (nextpath != nullptr);

  zx_status_t status = do_stat(vn, s);
  STATUS(status);
}

constexpr size_t kDirBufSize = 2048;

struct MinDir {
  ~MinDir() {
    if (vn)
      vn->Close();
  }

  uint64_t magic = minfs::kMinfsMagic0;
  fbl::RefPtr<fs::Vnode> vn;
  fs::VdirCookie cookie;
  uint8_t* ptr = nullptr;
  uint8_t data[kDirBufSize] = {0};
  size_t size = 0;
  dirent de = {};
};

int emu_mkdir(const char* path) {
  ZX_DEBUG_ASSERT_MSG(!host_path(path), "'emu_' functions can only operate on target paths");
  int fd = emu_open(path, O_CREAT | O_EXCL | O_DIRECTORY);
  if (fd >= 0) {
    emu_close(fd);
    return 0;
  } else {
    return fd;
  }
}

DIR* emu_opendir(const char* name) {
  ZX_DEBUG_ASSERT_MSG(!host_path(name), "'emu_' functions can only operate on target paths");
  std::string_view path(name + PREFIX_SIZE);
  fs::VnodeConnectionOptions options{.flags = fuchsia_io::OpenFlags::kPosixWritable,
                                     .rights = fuchsia_io::kRStarDir};
  auto result = fake_fs.fake_vfs->Open(fake_fs.fake_root, path, options, fs::Rights::ReadWrite());
  if (result.is_error()) {
    return nullptr;
  }
  MinDir* dir = new MinDir();
  dir->vn = fbl::RefPtr<fs::Vnode>::Downcast(result.ok().vnode);
  return reinterpret_cast<DIR*>(dir);
}

dirent* emu_readdir(DIR* dirp) {
  MinDir* dir = reinterpret_cast<MinDir*>(dirp);
  for (;;) {
// TODO(b/293947862): Remove use of deprecated `vdirent_t` when transitioning ReadDir to Enumerate
// as part of io2 migration.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    if (dir->size >= sizeof(vdirent_t)) {
      vdirent_t* vde = reinterpret_cast<vdirent_t*>(dir->ptr);
      dirent* ent = &dir->de;
      size_t name_len = vde->size;
      size_t entry_len = vde->size + sizeof(vdirent_t);
      ZX_DEBUG_ASSERT(dir->size >= entry_len);
      memcpy(ent->d_name, vde->name, name_len);
      ent->d_name[name_len] = '\0';
      ent->d_type = vde->type;
      dir->ptr += entry_len;
      dir->size -= entry_len;
      return ent;
    }
#pragma clang diagnostic pop
    size_t actual = 0;
    zx_status_t status = dir->vn->Readdir(&dir->cookie, &dir->data, kDirBufSize, &actual);
    if (status != ZX_OK || actual == 0) {
      break;
    }
    dir->ptr = dir->data;
    dir->size = actual;
  }
  return nullptr;
}

void emu_rewinddir(DIR* dirp) {
  MinDir* dir = reinterpret_cast<MinDir*>(dirp);
  dir->size = 0;
  dir->ptr = NULL;
  dir->cookie.n = 0;
}

int emu_closedir(DIR* dirp) {
  if (reinterpret_cast<uint64_t*>(dirp)[0] != minfs::kMinfsMagic0) {
    return closedir(dirp);
  }

  MinDir* dir = reinterpret_cast<MinDir*>(dirp);
  delete dir;

  return 0;
}
