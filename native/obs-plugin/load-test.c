/* ABI/registration smoke test without a display or graphics context. The
 * registration function is intercepted; this is not an OBS rendering test. */
#include <obs.h>
#include <assert.h>
#include <dlfcn.h>
#include <string.h>
#include <stdio.h>

static struct obs_source_info registered;
void obs_register_source_s(const struct obs_source_info *info, size_t size)
{
    assert(size == sizeof(registered));
    registered = *info;
}
int main(int argc, char **argv)
{
    assert(argc == 2);
    void *module = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!module) { fprintf(stderr, "%s\n", dlerror()); return 1; }
    bool (*load)(void) = (bool (*)(void))dlsym(module, "obs_module_load");
    assert(load && load());
    assert(!strcmp(registered.id, "omatuber_dmabuf"));
    assert(!strcmp(registered.get_name(NULL), "OmaTuber (DMA-BUF)"));
    assert(registered.video_tick && registered.video_render && registered.destroy);
    assert(registered.output_flags == OBS_SOURCE_VIDEO);
    assert(registered.get_properties);
    obs_properties_t *props = registered.get_properties(NULL);
    const char *description = obs_property_description(obs_properties_get(props, "connection_status"));
    assert(description && strstr(description, "Frames importados: 0"));
    assert(obs_properties_get(props, "refresh_status"));
    obs_properties_destroy(props);
    dlclose(module);
    puts("Plugin ABI and source registration passed (rendering not exercised)");
    return 0;
}
