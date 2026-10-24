/* Optional hardware test. Readback is confined to this test oracle. */
#include "dmabuf.h"
#include <GLES2/gl2.h>
#include <sys/socket.h>
#include <unistd.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv)
{
    char error[256];
    struct omt_gpu *gpu = omt_gpu_create(argc > 1 ? argv[1] : "/dev/dri/renderD128", error, sizeof(error));
    if (!gpu) { fprintf(stderr, "GPU test unavailable: %s\n", error); return 77; }
    int result = 1, sockets[2] = {-1, -1};
    /* Different channels, positions and alpha detect swizzling/flips/blending. */
    const unsigned char pixels[] = {255,0,0,255, 0,128,0,128, 0,0,255,255, 0,0,0,0};
    unsigned char got[sizeof(pixels)] = {0};
    uint32_t texture = omt_upload(pixels, 2, 2);
    if (!texture || omt_begin(gpu, 2, 2)) goto done;
    omt_draw(gpu, texture, 0, 0, 2, 2);
    glReadPixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, got);
    if (glGetError() || memcmp(pixels, got, sizeof(pixels))) {
        fprintf(stderr, "GPU pixel oracle failed (orientation/channels/premultiplied alpha)\n"); goto done;
    }
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sockets)) goto done;
    if (omt_publish(gpu, sockets[0])) goto done;
    struct omt_frame frame; int fd = -1;
    if (omt_recv_frame(sockets[1], &frame, &fd) != 1) goto done;
    close(fd);
    if (frame.width != 2 || frame.height != 2 || frame.modifier != 0) goto done;
    puts("GPU composition + DMA-BUF export passed; OBS import still needs a live test");
    result = 0;
done:
    if (sockets[0] >= 0) close(sockets[0]);
    if (sockets[1] >= 0) close(sockets[1]);
    omt_delete_texture(texture); omt_gpu_destroy(gpu);
    return result;
}
