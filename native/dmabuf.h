#ifndef OMATUBER_DMABUF_H
#define OMATUBER_DMABUF_H
#include <stdint.h>
#include <stddef.h>
/* Local Linux ABI, SOCK_SEQPACKET. One immutable ABGR8888 plane + one FD.
 * Consumers request one frame at a time with the exact four bytes NEXT. */
#define OMT_MAGIC 0x31544d4fu
#define OMT_MAX_SIDE 4096u
struct omt_frame {
    uint32_t magic, width, height, format, stride, offset;
    uint64_t modifier;
};
int omt_send_frame(int socket, const struct omt_frame *frame, int fd);
int omt_recv_frame(int socket, struct omt_frame *frame, int *fd);
struct omt_gpu;
struct omt_gpu *omt_gpu_create(const char *node, char *error, size_t size);
void omt_gpu_destroy(struct omt_gpu *gpu);
uint32_t omt_upload(const uint8_t *rgba, uint32_t width, uint32_t height);
void omt_delete_texture(uint32_t texture);
int omt_begin(struct omt_gpu *gpu, uint32_t width, uint32_t height);
void omt_draw(struct omt_gpu *gpu, uint32_t texture, float x, float y, float w, float h);
int omt_publish(struct omt_gpu *gpu, int socket);
#endif
