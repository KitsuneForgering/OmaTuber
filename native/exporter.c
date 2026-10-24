#define _GNU_SOURCE
#define EGL_EGLEXT_PROTOTYPES
#define GL_GLEXT_PROTOTYPES
#include "dmabuf.h"
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <gbm.h>
#include <drm_fourcc.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct omt_gpu {
    int fd;
    struct gbm_device *device;
    EGLDisplay display;
    EGLContext context;
    GLuint program, framebuffer, target;
    GLint position, texcoord;
    struct gbm_bo *bo;
    EGLImageKHR image;
    uint32_t width, height;
    PFNEGLCREATEIMAGEKHRPROC create_image;
    PFNEGLDESTROYIMAGEKHRPROC destroy_image;
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_texture;
};

static GLuint shader(GLenum type, const char *source)
{
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &source, NULL); glCompileShader(s);
    GLint ok = 0; glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) { glDeleteShader(s); return 0; }
    return s;
}

static void end_frame(struct omt_gpu *g)
{
    if (g->framebuffer) glDeleteFramebuffers(1, &g->framebuffer);
    if (g->target) glDeleteTextures(1, &g->target);
    if (g->image) g->destroy_image(g->display, g->image);
    if (g->bo) gbm_bo_destroy(g->bo);
    g->framebuffer = g->target = 0; g->image = EGL_NO_IMAGE_KHR; g->bo = NULL;
}

void omt_gpu_destroy(struct omt_gpu *g)
{
    if (!g) return;
    if (g->context != EGL_NO_CONTEXT) {
        end_frame(g);
        if (g->program) glDeleteProgram(g->program);
        eglMakeCurrent(g->display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        eglDestroyContext(g->display, g->context);
    }
    if (g->display != EGL_NO_DISPLAY) eglTerminate(g->display);
    if (g->device) gbm_device_destroy(g->device);
    if (g->fd >= 0) close(g->fd);
    free(g);
}

/* No window or pbuffer is used: rendering goes exclusively to an FBO.
 * Requesting PBUFFER_BIT here excludes valid GBM configs on some drivers. */
static const char *create_context(struct omt_gpu *g)
{
    if (!eglBindAPI(EGL_OPENGL_ES_API)) return "eglBindAPI(GLES) failed";
    const EGLint attrs[] = {EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_SURFACE_TYPE, 0,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE};
    EGLConfig config; EGLint count = 0;
    if (!eglChooseConfig(g->display, attrs, &config, 1, &count))
        return "eglChooseConfig(GLES2, surfaceless) failed";
    if (!count) return "no EGL config matches GLES2 RGBA8 (surfaceless, surface_type=0)";
    const EGLint ctx[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    g->context = eglCreateContext(g->display, config, EGL_NO_CONTEXT, ctx);
    if (g->context == EGL_NO_CONTEXT) return "eglCreateContext(GLES2) failed";
    if (!eglMakeCurrent(g->display, EGL_NO_SURFACE, EGL_NO_SURFACE, g->context))
        return "eglMakeCurrent(surfaceless) failed";
    return NULL;
}

struct omt_gpu *omt_gpu_create(const char *node, char *error, size_t size)
{
    const char *why = "allocation failed";
    EGLint egl_error;
    struct omt_gpu *g = calloc(1, sizeof(*g));
    if (!g) { snprintf(error, size, "%s", why); return NULL; }
    g->fd = -1; g->display = EGL_NO_DISPLAY; g->context = EGL_NO_CONTEXT;
    why = "cannot open DRM render node";
    g->fd = open(node, O_RDWR | O_CLOEXEC);
    if (g->fd < 0) goto fail;
    why = "cannot create GBM device";
    g->device = gbm_create_device(g->fd);
    if (!g->device) goto fail;
    why = "cannot initialize EGL on GBM";
    PFNEGLGETPLATFORMDISPLAYEXTPROC platform_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (!platform_display) goto fail;
    g->display = platform_display(EGL_PLATFORM_GBM_KHR, g->device, NULL);
    if (g->display == EGL_NO_DISPLAY || !eglInitialize(g->display, NULL, NULL)) goto fail;
    const char *extensions = eglQueryString(g->display, EGL_EXTENSIONS);
    why = "EGL requires dma_buf_import, dma_buf_import_modifiers and surfaceless_context";
    if (!extensions || !strstr(extensions, "EGL_EXT_image_dma_buf_import_modifiers") ||
        !strstr(extensions, "EGL_KHR_surfaceless_context")) goto fail;
    g->create_image = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    g->destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    g->image_texture = (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (!g->create_image || !g->destroy_image || !g->image_texture) goto fail;
    why = create_context(g);
    if (why) goto fail;
    why = "cannot compile composition shader";
    GLuint vs = shader(GL_VERTEX_SHADER,
        "attribute vec2 p; attribute vec2 uv; varying vec2 v; void main(){gl_Position=vec4(p,0.,1.);v=uv;}");
    GLuint fs = shader(GL_FRAGMENT_SHADER,
        "precision mediump float; varying vec2 v; uniform sampler2D image; void main(){gl_FragColor=texture2D(image,v);}");
    if (!vs || !fs) { if (vs) glDeleteShader(vs); if (fs) glDeleteShader(fs); goto fail; }
    g->program = glCreateProgram(); glAttachShader(g->program, vs); glAttachShader(g->program, fs);
    glLinkProgram(g->program); glDeleteShader(vs); glDeleteShader(fs);
    GLint ok; glGetProgramiv(g->program, GL_LINK_STATUS, &ok); if (!ok) goto fail;
    g->position = glGetAttribLocation(g->program, "p"); g->texcoord = glGetAttribLocation(g->program, "uv");
    return g;
fail:
    egl_error = eglGetError();
    if (egl_error == EGL_SUCCESS) snprintf(error, size, "%s", why);
    else snprintf(error, size, "%s (EGL 0x%x)", why, egl_error);
    omt_gpu_destroy(g); return NULL;
}

uint32_t omt_upload(const uint8_t *rgba, uint32_t width, uint32_t height)
{
    GLuint t = 0; glGenTextures(1, &t); glBindTexture(GL_TEXTURE_2D, t);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, width, height, 0, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    if (glGetError() != GL_NO_ERROR) { glDeleteTextures(1, &t); return 0; }
    return t;
}
void omt_delete_texture(uint32_t texture) { glDeleteTextures(1, &texture); }

int omt_begin(struct omt_gpu *g, uint32_t width, uint32_t height)
{
    end_frame(g);
    if (!width || !height || width > OMT_MAX_SIDE || height > OMT_MAX_SIDE) return -1;
    g->width = width; g->height = height;
    /* Linear is a deliberately narrow first-version interoperability contract. */
    const uint64_t modifier = DRM_FORMAT_MOD_LINEAR;
    g->bo = gbm_bo_create_with_modifiers2(g->device, width, height, DRM_FORMAT_ABGR8888,
        &modifier, 1, GBM_BO_USE_RENDERING);
    if (!g->bo || gbm_bo_get_plane_count(g->bo) != 1) return -1;
    int fd = gbm_bo_get_fd(g->bo); if (fd < 0) return -1;
    const EGLint attrs[] = {EGL_WIDTH, (EGLint)width, EGL_HEIGHT, (EGLint)height,
        EGL_LINUX_DRM_FOURCC_EXT, DRM_FORMAT_ABGR8888, EGL_DMA_BUF_PLANE0_FD_EXT, fd,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0, EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)gbm_bo_get_stride(g->bo),
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, 0, EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, 0, EGL_NONE};
    g->image = g->create_image(g->display, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attrs);
    close(fd);
    if (g->image == EGL_NO_IMAGE_KHR) return -1;
    glGenTextures(1, &g->target); glBindTexture(GL_TEXTURE_2D, g->target);
    g->image_texture(GL_TEXTURE_2D, g->image);
    glGenFramebuffers(1, &g->framebuffer); glBindFramebuffer(GL_FRAMEBUFFER, g->framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, g->target, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return -1;
    glViewport(0, 0, width, height); glClearColor(0, 0, 0, 0); glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(g->program); glActiveTexture(GL_TEXTURE0);
    glUniform1i(glGetUniformLocation(g->program, "image"), 0);
    glEnable(GL_BLEND); glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    return 0;
}

void omt_draw(struct omt_gpu *g, uint32_t texture, float x, float y, float w, float h)
{
    /* Logical top-left is physical buffer row zero, matching libobs imports. */
    float l = x * 2 / g->width - 1, r = (x+w) * 2 / g->width - 1;
    float t = y * 2 / g->height - 1, b = (y+h) * 2 / g->height - 1;
    const GLfloat vertices[] = {l,t,0,0, r,t,1,0, l,b,0,1, r,b,1,1};
    glBindTexture(GL_TEXTURE_2D, texture);
    glVertexAttribPointer(g->position, 2, GL_FLOAT, GL_FALSE, 4*sizeof(GLfloat), vertices);
    glVertexAttribPointer(g->texcoord, 2, GL_FLOAT, GL_FALSE, 4*sizeof(GLfloat), vertices+2);
    glEnableVertexAttribArray(g->position); glEnableVertexAttribArray(g->texcoord);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

int omt_publish(struct omt_gpu *g, int socket)
{
    /* Complete writes before passing the FD. No buffer is ever written again,
     * so the consumer needs no release fence or timeout-based reuse rule. */
    glFinish();
    if (glGetError() != GL_NO_ERROR) return -1;
    int fd = gbm_bo_get_fd(g->bo); if (fd < 0) return -1;
    struct omt_frame f = {.magic=OMT_MAGIC, .width=g->width, .height=g->height,
        .format=DRM_FORMAT_ABGR8888, .stride=gbm_bo_get_stride(g->bo),
        .offset=0, .modifier=gbm_bo_get_modifier(g->bo)};
    int result = omt_send_frame(socket, &f, fd); close(fd); end_frame(g);
    return result;
}
