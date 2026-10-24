#define _GNU_SOURCE
#include "dmabuf.h"
#include <assert.h>
#include <sys/socket.h>
#include <sys/mman.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>
#include <drm_fourcc.h>
#include <dirent.h>

static int descriptor_count(void)
{
    DIR *dir = opendir("/proc/self/fd"); assert(dir);
    int n = 0; while (readdir(dir)) n++;
    closedir(dir); return n;
}

int main(void)
{
    int sockets[2]; assert(socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, sockets) == 0);
    int original = memfd_create("test-plane", MFD_CLOEXEC); assert(original >= 0);
    struct omt_frame frame = {.magic = OMT_MAGIC, .width=64, .height=32,
        .format=DRM_FORMAT_ABGR8888, .stride=256, .modifier=0};
    struct omt_frame got; int fd;
    assert(omt_recv_frame(sockets[1], &got, &fd) == 0);
    assert(omt_send_frame(sockets[0], &frame, original) == 0);
    assert(omt_recv_frame(sockets[1], &got, &fd) == 1);
    assert(!memcmp(&got, &frame, sizeof(frame)));
    assert(fcntl(fd, F_GETFD) & FD_CLOEXEC);
    close(fd);
    frame.stride = 1; /* metadata invalid even though FD is valid */
    assert(omt_send_frame(sockets[0], &frame, original) == 0);
    assert(omt_recv_frame(sockets[1], &got, &fd) == -1 && fd == -1);
    assert(send(sockets[0], &frame, sizeof(frame), 0) == sizeof(frame));
    assert(omt_recv_frame(sockets[1], &got, &fd) == -1 && fd == -1);
    /* A malformed packet with multiple FDs must close every received FD. */
    char control[CMSG_SPACE(sizeof(int)*2)] = {0};
    struct iovec iov = {.iov_base=&frame, .iov_len=sizeof(frame)};
    struct msghdr msg = {.msg_iov=&iov, .msg_iovlen=1, .msg_control=control, .msg_controllen=sizeof(control)};
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg); c->cmsg_level=SOL_SOCKET; c->cmsg_type=SCM_RIGHTS;
    c->cmsg_len=CMSG_LEN(sizeof(int)*2);
    int fds[] = {original, original}; memcpy(CMSG_DATA(c), fds, sizeof(fds));
    int before = descriptor_count();
    for (int i=0; i<64; i++) {
        assert(sendmsg(sockets[0], &msg, 0) == sizeof(frame));
        assert(omt_recv_frame(sockets[1], &got, &fd) == -1 && fd == -1);
    }
    assert(descriptor_count() == before);
    close(original); close(sockets[0]);
    assert(omt_recv_frame(sockets[1], &got, &fd) == -1);
    close(sockets[1]);
    return 0;
}
