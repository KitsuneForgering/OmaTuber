CC ?= cc
PKG_CONFIG ?= pkg-config
OBS_INCLUDE := $(shell $(PKG_CONFIG) --variable=includedir libobs)/obs
BUILD_DIR ?= ../../target/obs-plugin
OBS_PLUGIN_DIR ?= $(HOME)/.config/obs-studio/plugins/omatuber-dmabuf
CFLAGS += -std=c11 -Wall -Wextra -Werror -fPIC
CPPFLAGS += -I$(OBS_INCLUDE) $(shell $(PKG_CONFIG) --cflags libobs libdrm)
LDLIBS += $(shell $(PKG_CONFIG) --libs libobs libdrm)

all: $(BUILD_DIR)/omatuber-dmabuf.so

$(BUILD_DIR)/omatuber-dmabuf.so: plugin.c ../transport.c ../dmabuf.h
	mkdir -p $(BUILD_DIR)
	$(CC) $(CFLAGS) $(CPPFLAGS) -shared plugin.c ../transport.c $(LDLIBS) -o $@

# OBS's per-user plugin path, separate from the distro's files.
install: all
	install -d "$(OBS_PLUGIN_DIR)/data"
	install -Dm755 $(BUILD_DIR)/omatuber-dmabuf.so "$(OBS_PLUGIN_DIR)/bin/64bit/omatuber-dmabuf.so"

.PHONY: all install

test: all
	$(CC) $(CFLAGS) $(CPPFLAGS) -rdynamic load-test.c $(LDLIBS) -ldl -o $(BUILD_DIR)/load-test
	$(BUILD_DIR)/load-test $(abspath $(BUILD_DIR)/omatuber-dmabuf.so)
	$(CC) $(CFLAGS) $(CPPFLAGS) ../transport.c ../transport-test.c -o $(BUILD_DIR)/transport-test
	$(BUILD_DIR)/transport-test
	$(CC) $(CFLAGS) $(shell $(PKG_CONFIG) --cflags egl glesv2 gbm libdrm) ../egl-context-test.c ../transport.c $(shell $(PKG_CONFIG) --libs egl glesv2 gbm libdrm) -o $(BUILD_DIR)/egl-context-test
	$(BUILD_DIR)/egl-context-test

.PHONY: test

# Optional; exit 77 means the requested DRM node/context is unavailable.
DRM_NODE ?= /dev/dri/renderD128
gpu-test:
	mkdir -p $(BUILD_DIR)
	$(CC) $(CFLAGS) $(shell $(PKG_CONFIG) --cflags egl glesv2 gbm libdrm) ../exporter.c ../transport.c ../gpu-test.c $(shell $(PKG_CONFIG) --libs egl glesv2 gbm libdrm) -o $(BUILD_DIR)/gpu-test
	$(BUILD_DIR)/gpu-test $(DRM_NODE)

.PHONY: gpu-test
