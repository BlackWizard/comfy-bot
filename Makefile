SHELL := /bin/sh

REMOTE ?= rig
REMOTE_DIR ?= /data/comfy-bot
TARGET ?= x86_64-unknown-linux-musl
BIN_NAME := comfy-bot
LOCAL_BIN := dist/$(BIN_NAME)-$(TARGET)
REMOTE_BIN := $(REMOTE_DIR)/$(BIN_NAME)

.PHONY: linux build-linux clean

# Build the static musl binary and upload it to $(REMOTE):$(REMOTE_DIR).
linux:
	./scripts/build-linux-musl.sh
	scp "$(LOCAL_BIN)" "$(REMOTE):$(REMOTE_DIR)/$(BIN_NAME).new"
	@echo "Uploaded $(LOCAL_BIN) -> $(REMOTE):$(REMOTE_BIN)"
	ssh $(REMOTE) "sudo /usr/bin/systemctl restart comfy-bot"

# Build only, without touching the server.
build-linux:
	./scripts/build-linux-musl.sh

clean:
	cargo clean
	rm -rf dist
