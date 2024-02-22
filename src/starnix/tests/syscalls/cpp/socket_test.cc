// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/ip_icmp.h>
#include <poll.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/un.h>
#include <unistd.h>

#include <thread>

#include <asm-generic/socket.h>
#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "fault_test.h"

#if !defined(__NR_memfd_create)
#if defined(__x86_64__)
#define __NR_memfd_create 319
#elif defined(__i386__)
#define __NR_memfd_create 356
#elif defined(__aarch64__)
#define __NR_memfd_create 279
#elif defined(__arm__)
#define __NR_memfd_create 385
#endif
#endif  // !defined(__NR_memfd_create)

TEST(UnixSocket, ReadAfterClose) {
  int fds[2];

  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  ASSERT_EQ(1, write(fds[0], "0", 1));
  ASSERT_EQ(0, close(fds[0]));
  char buf[1];
  ASSERT_EQ(1, read(fds[1], buf, 1));
  ASSERT_EQ('0', buf[0]);
  ASSERT_EQ(0, read(fds[1], buf, 1));
}

TEST(UnixSocket, ReadAfterReadShutdown) {
  int fds[2];

  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  ASSERT_EQ(1, write(fds[0], "0", 1));
  ASSERT_EQ(0, shutdown(fds[1], SHUT_RD));
  char buf[1];
  ASSERT_EQ(1, read(fds[1], buf, 1));
  ASSERT_EQ('0', buf[0]);
  ASSERT_EQ(0, read(fds[1], buf, 1));
}

TEST(UnixSocket, HupEvent) {
  int fds[2];

  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));

  int epfd = epoll_create1(0);
  ASSERT_LT(-1, epfd);
  epoll_event ev = {EPOLLIN, {.u64 = 42}};
  ASSERT_EQ(0, epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev));

  epoll_event outev = {0, {.u64 = 0}};

  int no_ready = epoll_wait(epfd, &outev, 1, 0);
  ASSERT_EQ(0, no_ready);

  close(fds[1]);

  no_ready = epoll_wait(epfd, &outev, 1, 0);
  ASSERT_EQ(1, no_ready);
  ASSERT_EQ(EPOLLIN | EPOLLHUP, outev.events);
  ASSERT_EQ(42ul, outev.data.u64);

  close(fds[0]);
  close(epfd);
}

struct read_info_spec {
  unsigned char* mem;
  size_t length;
  size_t bytes_read;
  int fd;
};

void* reader(void* arg) {
  read_info_spec* read_info = reinterpret_cast<read_info_spec*>(arg);
  while (read_info->bytes_read < read_info->length) {
    size_t to_read = read_info->length - read_info->bytes_read;
    fflush(stdout);
    ssize_t bytes_read = read(read_info->fd, read_info->mem + read_info->bytes_read, to_read);
    EXPECT_LT(-1, bytes_read);
    if (bytes_read < 0) {
      return nullptr;
    }
    read_info->bytes_read += bytes_read;
  }
  return nullptr;
}

TEST(UnixSocket, BigWrite) {
  const size_t write_size = 300000;
  unsigned char* send_mem = new unsigned char[write_size];
  ASSERT_TRUE(send_mem != nullptr);

  for (size_t i = 0; i < write_size; i++) {
    send_mem[i] = 0xff & random();
  }

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));

  read_info_spec read_info;
  read_info.mem = new unsigned char[write_size];
  bzero(read_info.mem, sizeof(unsigned char) * write_size);
  ASSERT_TRUE(read_info.mem != nullptr);
  read_info.length = write_size;
  read_info.fd = fds[1];
  read_info.bytes_read = 0;

  pthread_t read_thread;
  ASSERT_EQ(0, pthread_create(&read_thread, nullptr, reader, &read_info));
  size_t write_count = 0;
  while (write_count < write_size) {
    size_t to_send = write_size - write_count;
    ssize_t bytes_read = write(fds[0], send_mem + write_count, to_send);
    ASSERT_LT(-1, bytes_read);
    write_count += bytes_read;
  }

  ASSERT_EQ(0, pthread_join(read_thread, nullptr));

  close(fds[0]);
  close(fds[1]);

  ASSERT_EQ(write_count, read_info.bytes_read);
  ASSERT_EQ(0, memcmp(send_mem, read_info.mem, sizeof(unsigned char) * write_size));

  delete[] send_mem;
  delete[] read_info.mem;
}

TEST(UnixSocket, ConnectZeroBacklog) {
  char* tmp = getenv("TEST_TMPDIR");
  auto socket_path =
      tmp == nullptr ? "/tmp/socktest_connect" : std::string(tmp) + "/socktest_connect";
  struct sockaddr_un sun;
  sun.sun_family = AF_UNIX;
  strcpy(sun.sun_path, socket_path.c_str());
  struct sockaddr* addr = reinterpret_cast<struct sockaddr*>(&sun);

  auto server = socket(AF_UNIX, SOCK_STREAM, 0);
  ASSERT_EQ(bind(server, addr, sizeof(sun)), 0);
  ASSERT_EQ(listen(server, 0), 0);

  auto client = socket(AF_UNIX, SOCK_STREAM, 0);
  ASSERT_GT(client, -1);
  ASSERT_EQ(connect(client, addr, sizeof(sun)), 0);

  ASSERT_EQ(unlink(socket_path.c_str()), 0);
  ASSERT_EQ(close(client), 0);
  ASSERT_EQ(close(server), 0);
}

TEST(UnixSocket, ConnectLargeSize) {
  struct sockaddr_un sun;
  sun.sun_family = AF_UNIX;
  strcpy(sun.sun_path, "/bogus/path/value");
  struct sockaddr* addr = reinterpret_cast<struct sockaddr*>(&sun);

  auto client = socket(AF_UNIX, SOCK_STREAM, 0);
  ASSERT_GT(client, -1);
  ASSERT_EQ(connect(client, addr, sizeof(struct sockaddr_un) + 1), -1);
  EXPECT_EQ(errno, EINVAL);
}

TEST(InetSocket, ConnectLargeSize) {
  struct sockaddr_in in;
  in.sin_family = AF_INET;
  struct sockaddr* addr = reinterpret_cast<struct sockaddr*>(&in);

  auto client = socket(AF_INET, SOCK_STREAM, 0);
  ASSERT_GT(client, -1);
  ASSERT_EQ(connect(client, addr, sizeof(struct sockaddr_storage) + 1), -1);
  EXPECT_EQ(errno, EINVAL);
}

class UnixSocketTest : public testing::Test {
  // SetUp() - make socket
 protected:
  void SetUp() override {
    char* tmp = getenv("TEST_TMPDIR");
    socket_path_ = tmp == nullptr ? "/tmp/socktest" : std::string(tmp) + "/socktest";
    struct sockaddr_un sun;
    sun.sun_family = AF_UNIX;
    strcpy(sun.sun_path, socket_path_.c_str());
    struct sockaddr* addr = reinterpret_cast<struct sockaddr*>(&sun);

    server_ = socket(AF_UNIX, SOCK_STREAM, 0);
    ASSERT_GT(server_, -1);
    ASSERT_EQ(bind(server_, addr, sizeof(sun)), 0);
    ASSERT_EQ(listen(server_, 1), 0);

    client_ = socket(AF_UNIX, SOCK_STREAM, 0);
    ASSERT_GT(client_, -1);
    ASSERT_EQ(connect(client_, addr, sizeof(sun)), 0);
  }

  void TearDown() override {
    ASSERT_EQ(unlink(socket_path_.c_str()), 0);
    ASSERT_EQ(close(client_), 0);
    ASSERT_EQ(close(server_), 0);
  }

  int client() const { return client_; }

 private:
  int client_ = 0;
  int server_ = 0;
  std::string socket_path_;
};

TEST_F(UnixSocketTest, ImmediatePeercredCheck) {
  struct ucred cred;
  socklen_t cred_size = sizeof(cred);
  ASSERT_EQ(getsockopt(client(), SOL_SOCKET, SO_PEERCRED, &cred, &cred_size), 0);
  ASSERT_NE(cred.pid, 0);
  ASSERT_NE(cred.uid, static_cast<uid_t>(-1));
  ASSERT_NE(cred.uid, static_cast<gid_t>(-1));
}

TEST(UnixSocket, SendZeroFds) {
  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));

  char data[] = "a";
  struct iovec iov[] = {{
      .iov_base = data,
      .iov_len = 1,
  }};
  char buf[CMSG_SPACE(0)];
  struct msghdr msg = {
      .msg_iov = iov,
      .msg_iovlen = 1,
      .msg_control = buf,
      .msg_controllen = sizeof(buf),
  };
  *CMSG_FIRSTHDR(&msg) = (struct cmsghdr){
      .cmsg_len = CMSG_LEN(0),
      .cmsg_level = SOL_SOCKET,
      .cmsg_type = SCM_RIGHTS,
  };
  ASSERT_EQ(sendmsg(fds[0], &msg, 0), 1);

  memset(data, 0, sizeof(data));
  memset(buf, 0, sizeof(buf));
  ASSERT_EQ(recvmsg(fds[1], &msg, 0), 1);
  EXPECT_EQ(data[0], 'a');
  EXPECT_EQ(msg.msg_controllen, 0u);
}

#if defined(__NR_memfd_create)
TEST(UnixSocket, SendMemFd) {
  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));

  int memfd = static_cast<int>(syscall(__NR_memfd_create, "test_memfd", 0));

  char data[] = "";
  struct iovec iov[] = {{
      .iov_base = data,
      .iov_len = 1,
  }};
  char buf[CMSG_SPACE(sizeof(int))];
  struct msghdr msg = {
      .msg_iov = iov,
      .msg_iovlen = 1,
      .msg_control = buf,
      .msg_controllen = sizeof(buf),
  };
  struct cmsghdr* cmsg = CMSG_FIRSTHDR(&msg);
  *cmsg = (struct cmsghdr){
      .cmsg_len = CMSG_LEN(sizeof(int)),
      .cmsg_level = SOL_SOCKET,
      .cmsg_type = SCM_RIGHTS,
  };
  memmove(CMSG_DATA(cmsg), &memfd, sizeof(int));
  msg.msg_controllen = cmsg->cmsg_len;

  ASSERT_EQ(sendmsg(fds[0], &msg, 0), 1);

  memset(data, 0, sizeof(data));
  memset(buf, 0, sizeof(buf));
  ASSERT_EQ(recvmsg(fds[1], &msg, 0), 1);
  EXPECT_EQ(data[0], '\0');
  EXPECT_GT(msg.msg_controllen, 0u);
}
#endif  // defined(__NR_memfd_create)

// This test verifies that we can concurrently attempt to create the same type of socket from
// multiple threads.
TEST(Socket, ConcurrentCreate) {
  std::atomic_int barrier{0};
  std::atomic_int child_ready{0};
  auto child = std::thread([&] {
    child_ready.store(1);
    while (barrier.load() == 0) {
    }
    fbl::unique_fd fd;
    EXPECT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_STREAM, 0))) << strerror(errno);
  });
  while (child_ready.load() == 0) {
  }
  barrier.store(1);

  fbl::unique_fd fd;
  EXPECT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_STREAM, 0))) << strerror(errno);
  child.join();
}

class SocketFault : public FaultTest, public testing::WithParamInterface<std::pair<int, int>> {
 protected:
  void SetUp() override {
    const auto [type, protocol] = GetParam();

    if (type == SOCK_DGRAM && protocol == IPPROTO_ICMP && getuid() != 0) {
      GTEST_SKIP() << "Ping sockets require root.";
    }

    sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_addr = {htonl(INADDR_LOOPBACK)},
    };
    socklen_t addrlen = sizeof(addr);
    ASSERT_TRUE(recv_fd_ = fbl::unique_fd(socket(AF_INET, type, protocol))) << strerror(errno);
    ASSERT_EQ(bind(recv_fd_.get(), reinterpret_cast<sockaddr*>(&addr), addrlen), 0)
        << strerror(errno);
    ASSERT_EQ(getsockname(recv_fd_.get(), reinterpret_cast<sockaddr*>(&addr), &addrlen), 0)
        << strerror(errno);
    ASSERT_EQ(addrlen, sizeof(addr));
    if (type == SOCK_STREAM) {
      ASSERT_EQ(listen(recv_fd_.get(), 0), 0) << strerror(errno);
      listen_fd_ = std::move(recv_fd_);
    }

    ASSERT_TRUE(send_fd_ = fbl::unique_fd(socket(AF_INET, type, protocol))) << strerror(errno);
    ASSERT_EQ(connect(send_fd_.get(), reinterpret_cast<const sockaddr*>(&addr), sizeof(addr)), 0)
        << strerror(errno);

    if (type == SOCK_STREAM) {
      ASSERT_TRUE(recv_fd_ = fbl::unique_fd(accept(listen_fd_.get(), nullptr, nullptr)))
          << strerror(errno);
    } else if (protocol == IPPROTO_ICMP) {
      // ICMP sockets only get the packet on the sending socket since sockets do not
      // receive ICMP requests, only replies. Note that the netstack internally
      // responds to ICMP requests without any user-application needing to handle
      // requests.
      ASSERT_TRUE(recv_fd_ = fbl::unique_fd(dup(send_fd_.get()))) << strerror(errno);
    }
  }

  void TearDown() override {
    send_fd_.reset();
    recv_fd_.reset();
    listen_fd_.reset();
  }

  void SetRecvFdNonBlocking() {
    int flags = fcntl(recv_fd_.get(), F_GETFL, 0);
    ASSERT_GE(flags, 0) << strerror(errno);
    ASSERT_EQ(fcntl(recv_fd_.get(), F_SETFL, flags | O_NONBLOCK), 0) << strerror(errno);
  }

  fbl::unique_fd recv_fd_;
  fbl::unique_fd listen_fd_;
  fbl::unique_fd send_fd_;
};

// Test sending a packet from invalid memory.
TEST_P(SocketFault, Write) {
  EXPECT_EQ(write(send_fd_.get(), faulting_ptr_, kFaultingSize_), -1);
  EXPECT_EQ(errno, EFAULT);
}

// Test receiving a packet to invalid memory.
TEST_P(SocketFault, Read) {
  // First send a valid message that we can read.
  //
  // We send an ICMP message since this test is generic over UDP/TCP/ICMP.
  // UDP/TCP do not care about the shape of the payload but ICMP does so we just
  // use an ICMP compatible payload for simplicity.
  constexpr icmphdr kSendIcmp = {
      .type = ICMP_ECHO,
  };
  ASSERT_EQ(write(send_fd_.get(), &kSendIcmp, sizeof(kSendIcmp)),
            static_cast<ssize_t>(sizeof(kSendIcmp)));

  pollfd p = {
      .fd = recv_fd_.get(),
      .events = POLLIN,
  };
  ASSERT_EQ(poll(&p, 1, -1), 1);
  ASSERT_EQ(p.revents, POLLIN);

  static_assert(kFaultingSize_ >= sizeof(kSendIcmp));
  EXPECT_EQ(read(recv_fd_.get(), faulting_ptr_, sizeof(kSendIcmp)), -1);
  EXPECT_EQ(errno, EFAULT);
}

TEST_P(SocketFault, ReadV) {
  // First send a valid message that we can read.
  //
  // We send an ICMP message since this test is generic over UDP/TCP/ICMP.
  // UDP/TCP do not care about the shape of the payload but ICMP does so we just
  // use an ICMP compatible payload for simplicity.
  constexpr icmphdr kSendIcmp = {
      .type = ICMP_ECHO,
  };
  ASSERT_EQ(write(send_fd_.get(), &kSendIcmp, sizeof(kSendIcmp)),
            static_cast<ssize_t>(sizeof(kSendIcmp)));

  pollfd p = {
      .fd = recv_fd_.get(),
      .events = POLLIN,
  };
  ASSERT_EQ(poll(&p, 1, -1), 1);
  ASSERT_EQ(p.revents, POLLIN);

  char base0[1];
  char base2[sizeof(kSendIcmp) - 1];
  iovec iov[] = {
      {
          .iov_base = base0,
          .iov_len = sizeof(base0),
      },
      {
          .iov_base = faulting_ptr_,
          .iov_len = sizeof(kFaultingSize_),
      },
      {
          .iov_base = base2,
          .iov_len = sizeof(base2),
      },
  };

  // Read once with iov holding the invalid pointer.
  ASSERT_EQ(readv(recv_fd_.get(), iov, std::size(iov)), -1);
  EXPECT_EQ(errno, EFAULT);

  // Read again after clearing the invalid buffer. This read will fail on UDP/ICMP
  // sockets since they deque the message before checking the validity of buffers
  // but TCP sockets will not remove bytes from the unread bytes held by the kernel
  // if any buffer faults. Note that what UDP/ICMP does is ~acceptable since they are
  // not meant to be a reliable protocol and the behaviour for TCP also makes sense
  // because when the socket returns EFAULT, there is no way to know how many
  // bytes the kernel write into our buffers. Since the kernel has no way to tell us
  // how many bytes were read when a fault occurred, it has no other option than to
  // keep the bytes before the fault to prevent userspace from dropping part of a
  // byte stream.
  ASSERT_NO_FATAL_FAILURE(SetRecvFdNonBlocking());
  const auto [type, protocol] = GetParam();
  iov[1] = iovec{};
  if (type == SOCK_STREAM) {
    ASSERT_EQ(readv(recv_fd_.get(), iov, std::size(iov)), static_cast<ssize_t>(sizeof(kSendIcmp)));
  } else {
    ASSERT_EQ(readv(recv_fd_.get(), iov, std::size(iov)), -1);
    EXPECT_EQ(errno, EAGAIN);
  }
}

TEST_P(SocketFault, WriteV) {
  icmphdr kSendIcmp = {
      .type = ICMP_ECHO,
  };
  constexpr size_t kBase0Size = 1;
  iovec iov[] = {
      {
          .iov_base = &kSendIcmp,
          .iov_len = kBase0Size,
      },
      {
          .iov_base = faulting_ptr_,
          .iov_len = sizeof(kFaultingSize_),
      },
      {
          .iov_base = reinterpret_cast<char*>(&kSendIcmp) + kBase0Size,
          .iov_len = sizeof(kSendIcmp) - kBase0Size,
      },
  };
  ASSERT_EQ(writev(send_fd_.get(), iov, std::size(iov)), -1);
  EXPECT_EQ(errno, EFAULT);

  // Reading should fail since nothing should have been written.
  ASSERT_NO_FATAL_FAILURE(SetRecvFdNonBlocking());
  char recv_buf[sizeof(kSendIcmp)];
  ASSERT_EQ(read(recv_fd_.get(), &recv_buf, sizeof(recv_buf)), -1);
  EXPECT_EQ(errno, EAGAIN);
}

INSTANTIATE_TEST_SUITE_P(SocketFault, SocketFault,
                         testing::Values(std::make_pair(SOCK_DGRAM, 0),
                                         std::make_pair(SOCK_DGRAM, IPPROTO_ICMP),
                                         std::make_pair(SOCK_STREAM, 0)));
class SndRcvBufSockOpt : public testing::TestWithParam<int> {};

// This test asserts that the value of SO_RCVBUF and SO_SNDBUF are doubled on
// set, and this doubled value is returned on get, as described in the Linux
// socket(7) man page.
TEST_P(SndRcvBufSockOpt, DoubledOnGet) {
  fbl::unique_fd fd;
  EXPECT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_STREAM, 0))) << strerror(errno);

  int buf_size;
  socklen_t optlen = sizeof(buf_size);
  ASSERT_EQ(getsockopt(fd.get(), SOL_SOCKET, GetParam(), &buf_size, &optlen), 0) << strerror(errno);

  ASSERT_EQ(setsockopt(fd.get(), SOL_SOCKET, GetParam(), &buf_size, optlen), 0) << strerror(errno);

  int new_buf_size;
  ASSERT_EQ(getsockopt(fd.get(), SOL_SOCKET, GetParam(), &new_buf_size, &optlen), 0)
      << strerror(errno);
  ASSERT_EQ(new_buf_size, 2 * buf_size);
}

INSTANTIATE_TEST_SUITE_P(SndRcvBufSockOpt, SndRcvBufSockOpt, testing::Values(SO_SNDBUF, SO_RCVBUF));
