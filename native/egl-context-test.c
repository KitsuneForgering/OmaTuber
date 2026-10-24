/* Exercise context selection/failure reporting without GPU hardware. The fake
 * driver offers an RGBA8 config without pbuffer support, as GBM drivers may. */
#define eglBindAPI test_bind_api
#define eglChooseConfig test_choose_config
#define eglCreateContext test_create_context
#define eglMakeCurrent test_make_current
#include "exporter.c"
#include <assert.h>

static int failure;
EGLBoolean test_bind_api(EGLenum api)
{
    assert(api == EGL_OPENGL_ES_API);
    return failure == 1 ? EGL_FALSE : EGL_TRUE;
}
EGLBoolean test_choose_config(EGLDisplay display, const EGLint *attrs,
    EGLConfig *config, EGLint size, EGLint *count)
{
    (void)display;
    assert(size == 1);
    /* EGL defaults to WINDOW_BIT if the attribute is omitted, so explicitly
     * requiring no surface bits matters as much as removing PBUFFER_BIT. */
    int surface_bits = EGL_WINDOW_BIT;
    for (int i = 0; attrs[i] != EGL_NONE; i += 2)
        if (attrs[i] == EGL_SURFACE_TYPE) surface_bits = attrs[i+1];
    *count = (surface_bits == 0 && failure != 3) ? 1 : 0;
    *config = (EGLConfig)(uintptr_t)1;
    return failure == 2 ? EGL_FALSE : EGL_TRUE;
}
EGLContext test_create_context(EGLDisplay display, EGLConfig config,
    EGLContext share, const EGLint *attrs)
{
    (void)display;
    assert(config && share == EGL_NO_CONTEXT);
    assert(attrs[0] == EGL_CONTEXT_CLIENT_VERSION && attrs[1] == 2);
    return failure == 4 ? EGL_NO_CONTEXT : (EGLContext)(uintptr_t)2;
}
EGLBoolean test_make_current(EGLDisplay display, EGLSurface draw,
    EGLSurface read, EGLContext context)
{
    (void)display;
    assert(draw == EGL_NO_SURFACE && read == EGL_NO_SURFACE && context);
    return failure == 5 ? EGL_FALSE : EGL_TRUE;
}
int main(void)
{
    struct omt_gpu gpu = {0};
    assert(create_context(&gpu) == NULL);
    const char *expected[] = {NULL, "eglBindAPI", "eglChooseConfig", "no EGL config", "eglCreateContext", "eglMakeCurrent"};
    for (failure = 1; failure <= 5; failure++) {
        const char *error = create_context(&gpu);
        assert(error && strstr(error, expected[failure]));
    }
    puts("Surfaceless context selection and distinct failure stages passed");
    return 0;
}
