
TARGET_DIR := target/aarch64-unknown-none/debug
BUILD_DIR := .build

# Dependencies

## U-boot
UBOOT_BIN := ./u-boot/.build

#? Build U-Boot image and tools.
$(UBOOT_BIN)/u-boot.bin $(UBOOT_BIN)/tools/mkimage:
	mkdir -p $(UBOOT_BIN)
	CROSS_COMPILE=aarch64-linux-gnu- $(MAKE) -C ./u-boot O=./.build qemu_arm64_defconfig
	CROSS_COMPILE=aarch64-linux-gnu- $(MAKE) -C ./u-boot O=./.build -j all

## QEMU
QEMU_BIN := ./qemu/build
QEMU := $(QEMU_BIN)/qemu-system-aarch64
QEMU_OPTS := 

#? Build QEMU.
$(QEMU):
	mkdir -p $(QEMU_BIN) && cd $(QEMU_BIN) && ../configure --target-list=aarch64-softmmu
	$(MAKE) -C $(QEMU_BIN) -j all

all-bindep: $(QEMU) $(UBOOT_BIN)/u-boot.bin $(UBOOT_BIN)/tools/mkimage

# Building/Packaging the Kernel
build-all: $(BUILD_DIR)/kernel.img $(BUILD_DIR)/bin/init $(BUILD_DIR)/bin/api_tests $(BUILD_DIR)/bin/test_process #? Build and package everything.

build-rust: #? Build all Rust source.
	export CC_aarch64_unknown_none=aarch64-linux-gnu-gcc
	cargo build --target=aarch64-unknown-none

$(BUILD_DIR)/kernel.img: build-rust $(UBOOT_BIN)/tools/mkimage $(TARGET_DIR)/kernel
	mkdir -p $(BUILD_DIR)
	./scripts/make-image.sh $(TARGET_DIR)/kernel $(BUILD_DIR)/kernel.img

$(BUILD_DIR)/bin/init: build-rust $(TARGET_DIR)/init
	mkdir -p $(BUILD_DIR)/bin
	cp $(TARGET_DIR)/init $(BUILD_DIR)/bin/init

$(BUILD_DIR)/bin/api_tests: build-rust $(TARGET_DIR)/api_tests
	mkdir -p $(BUILD_DIR)/bin
	cp $(TARGET_DIR)/api_tests $(BUILD_DIR)/bin/api_tests

$(BUILD_DIR)/bin/test_process: build-rust $(TARGET_DIR)/test_process
	mkdir -p $(BUILD_DIR)/bin
	cp $(TARGET_DIR)/test_process $(BUILD_DIR)/bin/test_process

# QEMU Run/Test/Debug
run: build-all $(QEMU) $(UBOOT_BIN)/u-boot.bin #? Boots the system inside QEMU.
	./scripts/qemu-exec.sh $(BUILD_DIR) '{"init_process_path":"/volumes/root/bin/init"}' $(QEMU_OPTS)

debug: build-all $(QEMU) $(UBOOT_BIN)/u-boot.bin #? Run with QEMU in debug mode. Waits for GDB to attach before continuing.
	./scripts/qemu-exec.sh $(BUILD_DIR) '{}' -s -S $(QEMU_OPTS)

unit-test: build-all $(QEMU) $(UBOOT_BIN)/u-boot.bin $(UBOOT_BIN)/tools/mkimage #? Run unit tests.
	mkdir -p $(BUILD_DIR)
	cargo test -p kernel

api-test: build-all $(QEMU) $(UBOOT_BIN)/u-boot.bin #? Boots system inside QEMU and runs API tests.
	./scripts/qemu-exec.sh $(BUILD_DIR) '{"init_process_path":"/volumes/root/bin/api_tests"}' $(QEMU_OPTS)

test: unit-test api-test

# Rule to extract documentation comments for each rule in the Makefile
# Comments are `#?` after the rule head.
help: #? Display this help message.
	@awk -F ':|#\\?' '/^[^\t].+?:.*?#\\?/ { printf "\033[36m%-30s\033[0m %s\n", $$1, $$NF }' $(MAKEFILE_LIST)

.PHONY: build-all build-rust run test debug help
