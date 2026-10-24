#define _GNU_SOURCE
#include <obs-module.h>
#include <util/platform.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
#include <errno.h>
#include <stdatomic.h>
#include "../dmabuf.h"
#include <drm_fourcc.h>

OBS_DECLARE_MODULE()
MODULE_EXPORT const char *obs_module_description(void) { return "OmaTuber DMA-BUF source (Linux)"; }

enum connection_status { WAITING, INACTIVE, NO_PRODUCER, UNSUPPORTED, PEER_REJECTED, RECEIVING, STREAMING, DISCONNECTED, TIMED_OUT, IMPORT_FAILED };
static const char *status_text(int status)
{
    switch (status) {
    case INACTIVE: return "Fonte oculta ou fora das cenas exibidas; captura suspensa.";
    case NO_PRODUCER: return "O aplicativo não está oferecendo frames. Abra o OmaTuber pelo launcher instalado (omatuber).";
    case UNSUPPORTED: return "A GPU do OBS não oferece importação DMA-BUF ABGR8888 linear.";
    case PEER_REJECTED: return "Conexão rejeitada: produtor e OBS precisam executar com o mesmo usuário.";
    case RECEIVING: return "Conectado ao OmaTuber; aguardando o primeiro frame.";
    case STREAMING: return "Frames recebidos e importados. Se a imagem continuar transparente, confira as imagens na prévia do OmaTuber e a transformação da fonte.";
    case DISCONNECTED: return "O produtor desconectou ou enviou uma resposta inválida.";
    case TIMED_OUT: return "O produtor não respondeu em dois segundos.";
    case IMPORT_FAILED: return "A GPU do OBS falhou ao importar o DMA-BUF. Confira o log do OBS e o render node selecionado.";
    default: return "Aguardando conexão com o OmaTuber.";
    }
}
struct source {
    obs_source_t *owner;
    int socket;
    atomic_int status;
    _Atomic uint64_t frames;
    gs_texture_t *texture;
    uint32_t width, height;
    uint64_t retry_at, requested_at;
    bool pending, warned;
    char path[sizeof(((struct sockaddr_un *)0)->sun_path)];
};
static const char *name(void *unused) { (void)unused; return "OmaTuber (DMA-BUF)"; }
static void set_status(struct source *s, enum connection_status status)
{
    if (atomic_exchange(&s->status, status) != (int)status)
        blog(LOG_INFO, "[OmaTuber] %s", status_text(status));
}
static void disconnect_source(struct source *s)
{
    if (s->socket >= 0) close(s->socket);
    s->socket = -1; s->pending = false;
    gs_texture_destroy(s->texture); s->texture = NULL;
    s->retry_at = os_gettime_ns() + 1000000000ULL;
}
static void *create(obs_data_t *settings, obs_source_t *source)
{
    (void)settings;
    struct source *s = calloc(1, sizeof(*s));
    if (!s) return NULL;
    atomic_init(&s->status, WAITING); atomic_init(&s->frames, 0);
    s->owner = source;
    s->socket = -1; s->width = s->height = 1;
    const char *runtime = getenv("XDG_RUNTIME_DIR");
    if (!runtime || snprintf(s->path, sizeof(s->path), "%s/omatuber/frames.sock", runtime) >= (int)sizeof(s->path)) {
        blog(LOG_ERROR, "[OmaTuber] invalid XDG_RUNTIME_DIR"); free(s); return NULL;
    }
    return s;
}
static void destroy(void *data)
{
    struct source *s = data;
    obs_enter_graphics(); disconnect_source(s); obs_leave_graphics(); free(s);
}
static bool supports_linear(void)
{
    enum gs_dmabuf_flags flags;
    uint32_t *formats = NULL; size_t count = 0;
    bool found = false;
    if (gs_query_dmabuf_capabilities(&flags, &formats, &count))
        for (size_t i = 0; i < count; i++) if (formats[i] == DRM_FORMAT_ABGR8888) found = true;
    bfree(formats);
    if (!found) return false;
    uint64_t *modifiers = NULL; count = 0; found = false;
    if (gs_query_dmabuf_modifiers_for_format(DRM_FORMAT_ABGR8888, &modifiers, &count))
        for (size_t i = 0; i < count; i++) if (modifiers[i] == 0) found = true;
    bfree(modifiers); return found;
}
static void tick(void *data, float seconds)
{
    (void)seconds;
    struct source *s = data;
    uint64_t now = os_gettime_ns();
    obs_enter_graphics();
    if (!obs_source_showing(s->owner)) {
        if (s->socket >= 0) disconnect_source(s);
        set_status(s, INACTIVE);
        goto done;
    }
    if (s->socket < 0 && now >= s->retry_at) {
        s->retry_at = now + 1000000000ULL;
        if (!supports_linear()) {
            if (!s->warned) blog(LOG_ERROR, "[OmaTuber] OBS GPU backend does not support linear ABGR8888 DMA-BUF import");
            s->warned = true;
            set_status(s, UNSUPPORTED);
            goto done;
        }
        int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
        if (fd < 0) { set_status(s, NO_PRODUCER); goto done; }
        struct sockaddr_un address = {.sun_family = AF_UNIX};
        snprintf(address.sun_path, sizeof(address.sun_path), "%s", s->path);
        if (connect(fd, (struct sockaddr *)&address, sizeof(address)) < 0) {
            close(fd); set_status(s, NO_PRODUCER); goto done;
        }
        struct ucred peer; socklen_t len = sizeof(peer);
        if (getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &peer, &len) || peer.uid != geteuid()) {
            close(fd); set_status(s, PEER_REJECTED); goto done;
        }
        s->socket = fd;
        set_status(s, RECEIVING);
    }
    if (s->socket < 0) goto done;
    if (s->pending) {
        struct omt_frame f; int fd = -1;
        int result = omt_recv_frame(s->socket, &f, &fd);
        if (result < 0 || (result == 0 && now - s->requested_at > 2000000000ULL)) {
            set_status(s, result < 0 ? DISCONNECTED : TIMED_OUT);
            disconnect_source(s); goto done;
        }
        if (!result) goto done;
        gs_texture_t *next = NULL;
        if (f.modifier == 0)
            next = gs_texture_create_from_dmabuf(f.width, f.height, f.format, GS_RGBA,
                1, &fd, &f.stride, &f.offset, &f.modifier);
        close(fd);
        if (!next) {
            set_status(s, IMPORT_FAILED);
            disconnect_source(s); goto done;
        }
        gs_texture_destroy(s->texture); s->texture = next;
        atomic_fetch_add(&s->frames, 1); set_status(s, STREAMING);
        s->width = f.width; s->height = f.height; s->pending = false;
    }
    if (send(s->socket, "NEXT", 4, MSG_DONTWAIT | MSG_NOSIGNAL) != 4) {
        set_status(s, DISCONNECTED);
        disconnect_source(s); goto done;
    }
    s->pending = true; s->requested_at = now;
done:
    obs_leave_graphics();
}
static void render(void *data, gs_effect_t *effect)
{
    (void)effect;
    struct source *s = data;
    if (!s->texture) return;
    gs_blend_state_push(); gs_blend_function(GS_BLEND_ONE, GS_BLEND_INVSRCALPHA);
    obs_source_draw(s->texture, 0, 0, 0, 0, false);
    gs_blend_state_pop();
}
static bool refresh_status(obs_properties_t *props, obs_property_t *button, void *data)
{
    (void)button;
    struct source *s = data;
    char text[768];
    snprintf(text, sizeof(text), "%s\nFrames importados: %llu\nSocket: %s",
        status_text(s ? atomic_load(&s->status) : WAITING),
        (unsigned long long)(s ? atomic_load(&s->frames) : 0), s ? s->path : "ainda não criado");
    obs_property_set_description(obs_properties_get(props, "connection_status"), text);
    return true;
}
static obs_properties_t *properties(void *data)
{
    obs_properties_t *props = obs_properties_create();
    obs_properties_add_text(props, "connection_status", "", OBS_TEXT_INFO);
    obs_properties_add_button2(props, "refresh_status", "Atualizar diagnóstico", refresh_status, data);
    refresh_status(props, NULL, data);
    return props;
}
static uint32_t width(void *data) { return ((struct source *)data)->width; }
static uint32_t height(void *data) { return ((struct source *)data)->height; }
bool obs_module_load(void)
{
    struct obs_source_info info = {
        .id = "omatuber_dmabuf", .type = OBS_SOURCE_TYPE_INPUT,
        .output_flags = OBS_SOURCE_VIDEO,
        .get_properties = properties,
        .get_name = name, .create = create, .destroy = destroy,
        .video_tick = tick, .video_render = render, .get_width = width, .get_height = height,
    };
    obs_register_source(&info); return true;
}
