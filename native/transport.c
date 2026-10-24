#define _GNU_SOURCE
#include "dmabuf.h"
#include <sys/socket.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <drm_fourcc.h>

_Static_assert(sizeof(struct omt_frame) == 32, "wire ABI changed");
int omt_send_frame(int socket, const struct omt_frame *frame, int fd)
{
    union { struct cmsghdr align; char bytes[CMSG_SPACE(sizeof(int))]; } control = {0};
    struct iovec iov = {.iov_base = (void *)frame, .iov_len = sizeof(*frame)};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1,
        .msg_control = control.bytes, .msg_controllen = sizeof(control.bytes)};
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(fd));
    memcpy(CMSG_DATA(c), &fd, sizeof(fd));
    return sendmsg(socket, &msg, MSG_NOSIGNAL | MSG_DONTWAIT) == sizeof(*frame) ? 0 : -1;
}

int omt_recv_frame(int socket, struct omt_frame *frame, int *fd)
{
    /* Reserve room for extra descriptors so malformed packets cannot leak them. */
    union { struct cmsghdr align; char bytes[CMSG_SPACE(sizeof(int) * 8)]; } control = {0};
    struct iovec iov = {.iov_base = frame, .iov_len = sizeof(*frame)};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1,
        .msg_control = control.bytes, .msg_controllen = sizeof(control.bytes)};
    ssize_t n = recvmsg(socket, &msg, MSG_DONTWAIT | MSG_CMSG_CLOEXEC);
    *fd = -1;
    if (n < 0) return (errno == EAGAIN || errno == EWOULDBLOCK) ? 0 : -1;
    int count = 0;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&msg); c; c = CMSG_NXTHDR(&msg, c)) {
        if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS) continue;
        size_t bytes = c->cmsg_len - CMSG_LEN(0);
        for (size_t i = 0; i < bytes / sizeof(int); i++) {
            int received;
            memcpy(&received, (char *)CMSG_DATA(c) + i * sizeof(int), sizeof(int));
            if (count++ == 0) *fd = received;
            else close(received);
        }
    }
    if (n != sizeof(*frame) || (msg.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) || count != 1 ||
        frame->magic != OMT_MAGIC || !frame->width || !frame->height ||
        frame->width > OMT_MAX_SIDE || frame->height > OMT_MAX_SIDE ||
        frame->format != DRM_FORMAT_ABGR8888 || frame->stride < frame->width * 4 || frame->offset) {
        if (*fd >= 0) close(*fd);
        *fd = -1;
        return -1;
    }
    return 1;
}
