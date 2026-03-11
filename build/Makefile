ARCH        := x86_64
ROOT        := $(shell cd .. && pwd)
KERNEL      := $(ROOT)/target/x86_64-unknown-none/release/squirrel-kernel
ISO_DIR     := $(ROOT)/target/iso
ISO         := $(ROOT)/target/squirrel.iso
LIMINE_DIR  := $(ROOT)/build/limine
QEMU        := qemu-system-x86_64
QEMU_FLAGS  := -m 4G -serial stdio -vga std -cpu Haswell

# For QEMU development disk (SVFS storage)
DISK_IMG    := $(ROOT)/target/squirrel-disk.img
DISK_SIZE   := 2G

.PHONY: build wasm run run-e1000 run-rtl8139 debug iso disk clean

# Build WASM capability modules first (kernel embeds them via include_bytes!)
wasm:
	@for module in hello-module display-module input-module storage-module settings-module; do \
		echo "[WASM] Building $$module..."; \
		cd "$(ROOT)/modules/$$module" && cargo build --release --target wasm32-unknown-unknown; \
	done

# Build the kernel (depends on WASM modules being compiled first)
build: wasm
	cd "$(ROOT)" && cargo build --release -p squirrel-kernel

run: iso disk
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom "$(ISO)" \
		-drive "file=$(DISK_IMG),format=raw,if=virtio" \
		-netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
		-enable-kvm 2>/dev/null || \
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom "$(ISO)" \
		-drive "file=$(DISK_IMG),format=raw,if=virtio" \
		-netdev user,id=net0 -device virtio-net-pci,netdev=net0

# Test with emulated Intel e1000 (same driver works on real Intel NICs)
run-e1000: iso disk
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom "$(ISO)" \
		-drive "file=$(DISK_IMG),format=raw,if=virtio" \
		-netdev user,id=net0 -device e1000,netdev=net0 \
		-enable-kvm 2>/dev/null || \
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom "$(ISO)" \
		-drive "file=$(DISK_IMG),format=raw,if=virtio" \
		-netdev user,id=net0 -device e1000,netdev=net0

# Test with emulated Realtek RTL8139
run-rtl8139: iso disk
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom "$(ISO)" \
		-drive "file=$(DISK_IMG),format=raw,if=virtio" \
		-netdev user,id=net0 -device rtl8139,netdev=net0 \
		-enable-kvm 2>/dev/null || \
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom "$(ISO)" \
		-drive "file=$(DISK_IMG),format=raw,if=virtio" \
		-netdev user,id=net0 -device rtl8139,netdev=net0

debug: iso disk
	$(QEMU) $(QEMU_FLAGS) \
		-cdrom "$(ISO)" \
		-drive "file=$(DISK_IMG),format=raw,if=virtio" \
		-netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
		-s -S &
	gdb "$(KERNEL)" \
		-ex "set arch i386:x86-64" \
		-ex "target remote :1234" \
		-ex "break _start"

iso: build
	rm -rf "$(ISO_DIR)"
	mkdir -p "$(ISO_DIR)/boot/limine"
	cp "$(KERNEL)" "$(ISO_DIR)/boot/squirrel-kernel"
	cp "$(ROOT)/build/limine.conf" "$(ISO_DIR)/boot/limine/limine.conf"
	cp "$(ROOT)/build/limine.conf" "$(ISO_DIR)/limine.conf"
	cp "$(LIMINE_DIR)/limine-bios.sys" "$(ISO_DIR)/boot/limine/"
	cp "$(LIMINE_DIR)/limine-bios-cd.bin" "$(ISO_DIR)/boot/limine/"
	cp "$(LIMINE_DIR)/limine-uefi-cd.bin" "$(ISO_DIR)/boot/limine/"
	mkdir -p "$(ISO_DIR)/EFI/BOOT"
	cp "$(LIMINE_DIR)/BOOTX64.EFI" "$(ISO_DIR)/EFI/BOOT/"
	xorriso -as mkisofs -R \
		-b boot/limine/limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot boot/limine/limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image \
		--protective-msdos-label \
		"$(ISO_DIR)" -o "$(ISO)"
	"$(LIMINE_DIR)/limine" bios-install "$(ISO)"

disk:
	@if [ ! -f "$(DISK_IMG)" ]; then \
		echo "Creating disk image $(DISK_SIZE)..."; \
		dd if=/dev/zero of="$(DISK_IMG)" bs=1M count=2048 status=progress; \
	fi

clean:
	cd "$(ROOT)" && cargo clean
	rm -rf "$(ISO_DIR)" "$(ISO)"
