.DEFAULT_GOAL := all
CARGO ?= cargo
BUILD_DIR ?= target
BINDIR ?= $(HOME)/.local/bin
DATADIR ?= $(if $(XDG_DATA_HOME),$(XDG_DATA_HOME),$(HOME)/.local/share)
OBS_PLUGIN_DIR ?= $(HOME)/.config/obs-studio/plugins/omatuber-dmabuf

# The normal workflow prepares the complete integration for this user.
all: install

build:
	$(CARGO) build --locked --features sound,dmabuf --bin omatuber --target-dir "$(BUILD_DIR)"
	$(MAKE) -C native/obs-plugin BUILD_DIR="$(abspath $(BUILD_DIR))/obs-plugin" all

install: build
	$(MAKE) -C native/obs-plugin BUILD_DIR="$(abspath $(BUILD_DIR))/obs-plugin" OBS_PLUGIN_DIR="$(OBS_PLUGIN_DIR)" install
	install -Dm755 "$(BUILD_DIR)/debug/omatuber" "$(BINDIR)/omatuber-bin"
	install -Dm755 packaging/omatuber-launch "$(BINDIR)/omatuber"
	install -Dm644 packaging/omatuber.desktop "$(DATADIR)/applications/omatuber.desktop"
	install -Dm644 packaging/omatuber.svg "$(DATADIR)/icons/hicolor/scalable/apps/omatuber.svg"
	@echo "OmaTuber e plugin instalados. Reinicie o OBS e abra omatuber."

run: build
	OMATUBER_DMABUF=1 "$(BUILD_DIR)/debug/omatuber"

test:
	$(MAKE) -C native/obs-plugin BUILD_DIR="$(abspath $(BUILD_DIR))/obs-plugin" test
	$(CARGO) test --locked --all-features --tests --target-dir "$(BUILD_DIR)"

gpu-test:
	$(MAKE) -C native/obs-plugin BUILD_DIR="$(abspath $(BUILD_DIR))/obs-plugin" gpu-test

.PHONY: all build install run test gpu-test
