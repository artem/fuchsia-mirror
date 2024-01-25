// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include "sdk/lib/fdio/fdio_unistd.h"
#include "sdk/lib/fdio/internal.h"

// checkfile, checkfileat, and checkfd let us error out if the object
// doesn't exist, which allows the stubs to be a little more 'real'
static int seterr(int err) {
  if (err) {
    errno = err;
    return -1;
  }
  return 0;
}

static int checkfile(const char* path, int err) {
  struct stat s;
  if (stat(path, &s)) {
    return -1;
  }
  return seterr(err);
}

static int checkfileat(int fd, const char* path, int flags, int err) {
  struct stat s;
  if (fstatat(fd, path, &s, flags)) {
    return -1;
  }
  return seterr(err);
}

static bool fdok(int fd) { return fd_to_io(fd) != nullptr; }

static int checkfd(int fd, int err) {
  if (!fdok(fd)) {
    return ERRNO(EBADF);
  }
  return seterr(err);
}

static int checkfilefd(const char* path, int fd, int err) {
  struct stat s;
  if (stat(path, &s)) {
    return -1;
  }
  return checkfd(fd, err);
}

static int checkdir(DIR* dir, int err) {
  if (dirfd(dir) < 0) {
    errno = EBADF;
    return -1;
  }
  return seterr(err);
}

// not supported by any filesystems yet
__EXPORT
int symlink(const char* existing, const char* newpath) {
  errno = ENOSYS;
  return -1;
}
__EXPORT
ssize_t readlink(const char* __restrict path, char* __restrict buf, size_t bufsize) {
  // EINVAL = not symlink
  return checkfile(path, EINVAL);
}

// creating things we don't have plumbing for yet
__EXPORT
int mkfifo(const char* path, mode_t mode) {
  errno = ENOSYS;
  return -1;
}
__EXPORT
int mknod(const char* path, mode_t mode, dev_t dev) {
  errno = ENOSYS;
  return -1;
}

// no permissions support yet
__EXPORT
int chown(const char* path, uid_t owner, gid_t group) { return checkfile(path, ENOSYS); }
__EXPORT
int fchown(int fd, uid_t owner, gid_t group) { return checkfd(fd, ENOSYS); }
__EXPORT
int lchown(const char* path, uid_t owner, gid_t group) { return checkfile(path, ENOSYS); }

// no permissions support, but treat rwx bits as don't care rather than error
__EXPORT
int chmod(const char* path, mode_t mode) {
  mode &= 07777;  // only last 4 octals are relevant to chmod
  return checkfile(path, (mode & (~0777)) ? ENOSYS : 0);
}
__EXPORT
int fchmod(int fd, mode_t mode) {
  mode &= 07777;  // only last 4 octals are relevant to chmod
  return checkfd(fd, (mode & (~0777)) ? ENOSYS : 0);
}
__EXPORT
int fchmodat(int fd, const char* path, mode_t mode, int flags) {
  if (flags & ~AT_SYMLINK_NOFOLLOW) {
    errno = EINVAL;
    return -1;
  }

  return checkfileat(fd, path, flags, (mode & (~0777)) ? ENOSYS : 0);
}

__EXPORT
int access(const char* path, int mode) { return checkfile(path, 0); }

__EXPORT
void sync(void) {}

__EXPORT
int rmdir(const char* path) { return unlinkat(AT_FDCWD, path, AT_REMOVEDIR); }

// tty stubbing.
__EXPORT
int ttyname_r(int fd, char* name, size_t size) {
  if (!isatty(fd)) {
    return ENOTTY;
  }

  return checkfd(fd, ENOSYS);
}

__EXPORT
int sendmmsg(int fd, struct mmsghdr* msgvec, unsigned int vlen, unsigned int flags) {
  return checkfd(fd, ENOSYS);
}

__EXPORT
int recvmmsg(int fd, struct mmsghdr* msgvec, unsigned int vlen, unsigned int flags,
             struct timespec* timeout) {
  return checkfd(fd, ENOSYS);
}

__EXPORT
int sockatmark(int fd) {
  // ENOTTY is intentional for non-socket objects, but needs more investigation for sockets.
  // See https://fxbug.dev/42165442.
  return checkfd(fd, ENOTTY);
}

__EXPORT
int fchownat(int fd, const char* path, uid_t uid, gid_t gid, int flag) {
  return checkfd(fd, ENOSYS);
}

__EXPORT
int symlinkat(const char* existing, int fd, const char* newpath) {
  return checkfilefd(existing, fd, ENOSYS);
}

__EXPORT
ssize_t readlinkat(int fd, const char* __restrict path, char* __restrict buf, size_t bufsize) {
  return checkfilefd(path, fd, ENOSYS);
}

__EXPORT
void seekdir(DIR* dir, long loc) {}

__EXPORT
long telldir(DIR* dir) { return checkdir(dir, ENOSYS); }

__EXPORT
int posix_fadvise(int fd, off_t base, off_t len, int advice) { return fdok(fd) ? ENOSYS : EBADF; }

__EXPORT
int posix_fallocate(int fd, off_t base, off_t len) { return fdok(fd) ? ENOSYS : EBADF; }

__EXPORT
int readdir_r(DIR* dir, struct dirent* entry, struct dirent** result) {
  return dirfd(dir) < 0 ? EBADF : ENOSYS;
}
